# Window benchmarks

static-window ([WINDOW_PLAN.md](WINDOW_PLAN.md)) measured against the plan's
budgets, the hand-written loops it replaced, and winit 0.30 + softbuffer 0.4.
Ryzen 9 5900X, Arch Linux (kernel 7.1), COSMIC (cosmic-comp) at 120 Hz;
release builds (`lto = "thin"`, `codegen-units = 1`).

## Summary

| Budget (WINDOW_PLAN.md, phase 9) | Result |
|---|---|
| Idle window, 60 s: 0 commits, ≤ 2 wakeups, 0.000 s CPU | **met**: 0 commits, 1 wakeup, 0.000 s CPU |
| Launch to first commit: flappy ≤ its old loop | **missed**: 2.18 ms, against 1.83 ms (old loop, dynamic) and 1.55 ms (static) |
| Launch to first commit: static-ui widgets ≤ 3.2 ms | **missed**: 3.49 ms |
| Launch to first commit, libdecor path | reported: 117 ms (widgets), 136 ms (flappy) |
| Virtual-keyboard key press to commit sent ≤ 0.1 ms median | **met**: 0.011 ms our share; 0.059 ms from the virtual keyboard's send |
| Allocations per event and per frame, warm: 0 | **met by our code**, not by the libraries under it: per event, 0 on the Rust heap and 1 malloc in libwayland. Per frame, 7 in wayland-rs and 19 in libwayland, none in our code |
| Stripped binary size against winit + softbuffer | 2.08 MB, against 1.76 MB (no client-side title bars) and 2.27 MB (Adwaita title bars); kbvm is 385 KB of ours |

Both launch misses come from the same cost: loading libwayland with
`dlopen`. That is the price of the one-backend decision
(WINDOW_PLAN.md §5), which libdecor requires.

## Launch

From the spawn to the first buffer commit leaving the process, CLOCK_MONOTONIC
(median, min–max).

| | First commit | Before static-window |
|---|---:|---:|
| flappy (`flappy-launch 20`) | 2.18 ms (1.92–2.41) | 1.83 dynamic, 1.55 static |
| static-ui widgets (`ui_window_bench --launch 20`) | 3.49 ms (3.21–3.70) | 3.23 |
| static-ui widgets, libdecor (`STATIC_WINDOW_FORCE_CSD=1`, 10 launches) | 117 ms (112–130) | — |
| flappy, libdecor (5 launches) | 136 ms (124–139) | — |

flappy's steps:

| Step | ms after spawn |
|---|---:|
| process running (`main`) | 0.82 |
| ECS app built, game and window spawned | 0.86 |
| first frame committed | 2.18 |

Profile: 300 launches sampled at 20 kHz.
- `wayland_sys::client::WaylandClient::open` takes 7.7% of all samples: the
  `dlopen` of libwayland-client and libffi, and the symbol lookups after it.
  That is about 0.2–0.3 ms per launch.
- The kernel takes 35% and the dynamic loader 14% (exec, page faults,
  relocation).

The old flappy spoke the protocol in pure Rust and paid none of the `dlopen`.

With libdecor, the 110–135 ms is the GTK plugin starting up; COSMIC and KDE
offer `xdg-decoration` and never load it.

## Idle

`ui_window_bench --idle 60`: the widget window, input passing through it,
nothing happening. Over the 59 s after the first second:

| Wakeups | Commits | CPU | Voluntary context switches | Peak RSS |
|---:|---:|---:|---:|---:|
| 1 | 0 | 0.000 s | 0 (1 is the sampling thread's) | 9.9 MiB |

In the first second: 5 wakeups and 1 commit (configure, first frame,
presentation report).

## Key press to commit

`examples/key_latency.rs` presses F13 100 times, 50 ms apart, from a second
connection through `zwp_virtual_keyboard_v1`. The keyboard uses its own
one-key keymap, and keys are sent only while our window has focus. Each
press recolors a 64×64 square.

| Interval | Median | p90 | Min | Max |
|---|---:|---:|---:|---:|
| key arrives → commit leaves (our share) | 0.011 ms | 0.018 | 0.008 | 0.046 |
| virtual keyboard sends → commit leaves | 0.059 ms | 0.079 | 0.040 | 4.630 |
| key arrives → on screen (`wp_presentation`) | 7.7 ms | 13.1 | 0.55 | 93 |

The 4.6 ms maximum is the first press. The compositor sends the virtual
keyboard's keymap ahead of it, and the first key waits for that keymap to
compile. Time to the screen is the compositor's vblank.

## Allocations

`tests/alloc.rs` counts allocations on the loop's thread during `step` (a
counting global allocator). The window is warm and gets, per round, a motion,
a click, a wheel notch and a typed `a`. Over 192 rounds the loop made **0
allocations** on the Rust heap. The test fails on any.

Two throwaway probes counted the rest:
- **C allocations:** a `malloc`/`calloc`/`realloc` interposer, counting both
  C and Rust, on the same thread.
- **Their source:** backtraces of the Rust allocations made in warm frames.

| Per | Our code | wayland-rs | libwayland (C) |
|---|---:|---:|---:|
| event (≤ 4 arguments) | 0 | 0 | 1 |
| event with > 4 arguments (`wl_keyboard.modifiers`, `relative_motion`, `presented`) | 0 | 1 | 1 |
| frame of an animating window | 0 | 7 | 19 |

- **libwayland** mallocs a closure for every message in either direction and
  a proxy for every new object.
- **wayland-rs** allocates 3 times for each new object: the queue's proxy
  data, a liveness flag, and the proxy's user data. A frame creates two new
  objects, `wl_surface.frame` and `wp_presentation.feedback`. wayland-rs also
  allocates once to unpack an event with more than four arguments
  (`presented` has seven).

The probe found one cost that was ours: `wp_viewport.set_destination` was sent
with every frame. It now goes out only when the size changes, which took a
frame from 28 allocations to 26 and removed a request
(`tests/rendering.rs`, `the_viewport_is_sent_again_only_when_the_size_changes`).

Reaching zero would mean leaving libwayland. libdecor needs it, so it stays.

## Binary size

The same app: `examples/software.rs`, a gradient with a square sweeping
across at the display's rate. It is also written on winit and softbuffer in
`crates/static-window/bench/winit_softbuffer`. All three builds are
Wayland-only and load libwayland with `dlopen`.

| | Stripped | Unstripped |
|---|---:|---:|
| static-window | 2.08 MB | 2.75 MB |
| winit 0.30 + softbuffer 0.4 | 1.76 MB | 2.47 MB |
| winit 0.30 + softbuffer 0.4, Adwaita title bars (`--features csd`) | 2.27 MB | 3.05 MB |

The largest crates in each:

| static-window | | winit + softbuffer | |
|---|---:|---|---:|
| kbvm | 385 KB | winit | 278 KB |
| static_window | 166 KB | wayland_client | 199 KB |
| wayland_client | 177 KB | wayland_protocols | 75 KB |
| wayland_protocols | 56 KB | smithay_client_toolkit | 43 KB |
| static_ecs | 48 KB | | |

kbvm compiles the compositor's XKB keymap in Rust. winit does the same work in
libxkbcommon, which it loads with `dlopen`, so that code is not in its binary.
Without kbvm, ours would be about 1.70 MB. Our title bars come from the
compositor or from libdecor, also loaded with `dlopen`, so they add nothing.

The widget gallery grew from 2.03 MB stripped (`fa322cd`, before the cutover)
to 3.37 MB. The growth is mostly:

| | Growth |
|---|---:|
| kbvm | 385 KB |
| core and std instantiations | 154 KB |
| static-window over the old window code | 114 KB |
| wayland-client, with more protocols | 88 KB |
| unwind tables | 77 KB |

## Reproduce

```sh
cargo test -p static-window --release --test alloc
cargo run --release -p static-window --example key_latency -- 100           # focuses its window, presses F13
cargo run --release -p static-ui-raster --example ui_window_bench -- --idle 60
cargo run --release -p static-ui-raster --example ui_window_bench -- --launch 20
STATIC_WINDOW_FORCE_CSD=1 cargo run --release -p static-ui-raster --example ui_window_bench -- --launch 10
cargo build --release -p flappy && ./target/release/flappy-launch 20
cd crates/static-window/bench/winit_softbuffer && cargo build --release [--features csd]
```
