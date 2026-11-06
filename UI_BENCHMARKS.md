# UI benchmarks

static-ui (static-ecs, Clay layout, CPU raster on Wayland) measured against
the budgets of [UI_PLAN.md](UI_PLAN.md), against clay.h, and against the same
UI in Bevy UI 0.19.1. Ryzen 9 5900X, Arch Linux, COSMIC (cosmic-comp) at
120 Hz; release builds (`lto = "thin"`, `codegen-units = 1`); timings are
medians, headless ones pinned to one core (`taskset -c 2`). The window
numbers (startup, idle, latency) were taken with the window loop phase 3
built, since replaced by `crates/static-window`; current ones are in
[WINDOW_BENCHMARKS.md](WINDOW_BENCHMARKS.md).

## Summary

| Budget (UI_PLAN.md §1) | Result |
|---|---|
| An idle UI does nothing | **0 commits, 0.000 s CPU** over 59 s idle; 2 wakeups (a buffer release, the end of the run) |
| Layout ≤ 50 µs for 1,000 visible elements, ≤ 0.5 ms for 10,000 | Clay passes **43.7 µs** and **464 µs** (settings list, real fonts); the whole layout system 53.1 µs and 557 µs |
| Within 1.5× of clay.h on the same tree | **0.25–0.30×** clay.h's time (4× faster), boxes identical on 100,000 random trees |
| Hidden UI is free | an unchanged frame costs **0.37 µs** at 1,000 or 10,000 elements; 10,000 idle behaviors add **0.01 µs** |
| Toggling a 500-entity panel ≤ 100 µs, no allocation warm | close **47 µs**, open **44 µs**; allocation-free (`tests/toggle_alloc.rs`) |
| Input to pixels ≤ 1 ms for small damage | wakeup to commit **0.064 ms** median (our share); wakeup to scanout 12.6 ms (compositor's vblank) |
| Small redraws | a restyled button rasters only its box (`tests/raster.rs`); ~32,000 pixels per pulse frame |
| Startup in the range of `demo/flappy` (1.5 ms) | first frame committed **3.2 ms** after spawn (flappy: 1.5 ms for a simpler scene) |
| Mistakes don't compile | widget behaviors' `cx.with` access is checked like systems; a missing `Inbox<E>` is a compile error |

## Layout against clay.h

`static-ui-clay-oracle` builds the same UI-shaped tree (a panel of rows: icon,
two labels) through clay.h v0.14 and static-ui-layout, with the same text
measurement, and declares plus lays it out each frame, Clay's only mode.
"Layout only" re-lays-out an already declared tree, which is what static-ui
does when nothing structural changed.

| elements | clay.h | static-ui-layout | ratio | layout only |
|---:|---:|---:|---:|---:|
| 1,001 | 243.8 µs | 59.8 µs | 0.25× | 36.6 µs |
| 10,001 | 2,374.8 µs | 719.3 µs | 0.30× | 417.7 µs |

clay.h also hashes element ids and builds render commands in that time;
static-ui builds its render list separately, only when something changed.

Correctness: `cargo test -p static-ui-clay-oracle` lays out 100,000 random
trees (sizing kinds, min/max, padding, gaps, alignment, both directions,
wrapping text, floating elements on every attach point, clipping, aspect
ratios) through both and compares every box and text line. All match. 191
of the 100,000 trees make clay.h loop forever (a child already below its
minimum size during compression); static-ui-layout stops those loops
instead (`Tree::stalls`), the one deliberate deviation, and the test skips
them.

## Layout in the ECS (settings list, real fonts)

`examples/ui_bench.rs` (static-ui-raster): a scrolling panel of rows, five
elements per row, text measured with the bundled Liberation Sans.

| elements | flatten + layout frame | relayout frame (one width changed) | layout system | Clay passes | unchanged frame |
|---:|---:|---:|---:|---:|---:|
| 1,002 | 106.6 µs | 70.6 µs | 53.1 µs | 43.7 µs | 0.39 µs |
| 10,002 | 1,128.8 µs | 772.0 µs | 556.6 µs | 464.4 µs | 0.37 µs |

A frame is `UiUpdate` plus `UiFrame` (behaviors, layout, render list).
"Flatten" rebuilds the flattened tree (structure changed); a changed node is
patched in place instead. An unchanged frame skips layout and the render
list after one check per column: UI components use change summaries
(`Components<T, true>`, declared by the plugin with `type Storage`), which
is what made it constant (before: 5.3 µs and 48.9 µs).

## Opening and closing

| panel entities | close | open | next frame (flatten + layout) |
|---:|---:|---:|---:|
| 11 | 1.1 µs | 1.0 µs | 1.8 µs |
| 101 | 8.4 µs | 8.2 µs | 10.6 µs |
| 501 | 47.0 µs | 43.9 µs | 50.5 µs |
| 2,001 | 200.3 µs | 193.4 µs | 207.7 µs |

Closing inserts `Closed` (a `Disabling` component) down the subtree: one
table move per entity, linear in the subtree. Every query of the pipeline
then skips the panel without mentioning it.

## Behaviors and hit testing

| | 0 behaviors | 10,000 idle behaviors |
|---|---:|---:|
| idle frame | 0.32 µs | 0.33 µs |

| elements | hit test (one pointer move) |
|---:|---:|
| 1,002 | 0.64 µs |
| 10,002 | 5.80 µs |

Hit testing walks the visible tree topmost first and skips subtrees whose
bounds (computed after each layout) miss the pointer; without that pruning it
took 6.3 µs and 87.8 µs.

## Against Bevy UI

The same settings list in Bevy UI 0.19.1 (`bench/bevy_ui`, features `ui`,
Taffy layout and cosmic-text), timed around Bevy's `UiSystems::Layout` set,
in a real window. static-ui's numbers are its layout system (the same work:
change detection, layout, write-back).

| | static-ui | Bevy UI 0.19.1 |
|---|---:|---:|
| layout, 1,000 elements, one width changed | **53.1 µs** | 628.4 µs |
| layout, 10,000 elements, one width changed | **556.6 µs** | 3,773.8 µs |
| layout, nothing changed (1,000 / 10,000) | **0 µs** (skipped; frame 0.39 µs) | 525.0 / 3,294.7 µs |
| first frame after spawn | **3.2 ms** (committed) | 362 ms (first frame run) |
| idle 59 s: CPU time | **0.000 s** | 0.090 s (`WinitSettings::desktop_app()`) |
| idle 59 s: voluntary context switches | **2** | 326 |
| peak RSS | **6.0 MiB** | 296.4 MiB |
| binary, stripped | **1.93 MiB** (dynamic), 2.76 MiB (static) | 56.0 MiB |

Bevy renders with wgpu on the GPU; static-ui rasters damaged rectangles on
the CPU, so draw costs are not compared. Bevy's first-frame time is to its
first `Last` schedule run, which precedes presentation; static-ui's is to the
first commit.

## Window

`examples/ui_window_bench.rs` (static-ui-raster), a window with a button,
checkbox, slider, text field, dropdown and a 10,000-row virtual list:

- **Startup**: first frame committed 3.23 ms after spawn (median of 20; min
  2.76 ms). The first frame is laid out and drawn while the compositor
  answers the new window, and committed when it is configured.
- **Idle**: in the 59 s after the first second, 2 wakeups, 0 commits,
  0.000 s CPU, 2 voluntary context switches (one is the sampling thread's);
  peak RSS 6.0 MiB. The window is made input-transparent for this run, so a
  pointer crossing it on a live desktop does not count.
- **Latency**: a thread wakes a behavior every 100 ms (through its waker, the
  eventfd path any external future takes), which recolors a button. From the
  wakeup to the commit leaving the process: 0.064 ms median, 1.4 ms max;
  to scanout (`wp_presentation`): 12.6 ms median, p90 14.9 ms, which is the
  compositor's frame scheduling. On average 31,600 pixels were rastered per
  frame (the button's box). Pointer input takes the same path from the
  Wayland socket; no input injector was available to time it directly.

## Binary size

The widget gallery (`examples/widgets.rs`), including the 401 KiB bundled
font:

| | unstripped | stripped |
|---|---:|---:|
| dynamic | 3.2 MiB | 1.93 MiB |
| static (`+crt-static`, before static-window loaded libwayland with `dlopen`) | 4.1 MiB | 2.76 MiB |

## Fonts: our rasterizer or fontdue

Phase 3 chose between ttf-parser plus our own rasterizer and fontdue by
binary size and glyph quality. On the 62 glyphs `a–z A–Z 0–9` of Liberation
Sans:

| | code added to a probe binary | mean coverage difference from fontdue |
|---|---:|---:|
| ttf-parser + our rasterizer | 39.7 KB | 2.1 / 1.9 / 1.6 of 255 at 12 / 16 / 24 px |
| fontdue 0.9 | 68.4 KB | — |

Coverage differs by at most 39/255 at a few edge pixels (total ink within
2–3%). We kept ttf-parser and our signed-area accumulation rasterizer.

## Reproduce

```sh
cargo test -p static-ui-clay-oracle --release                     # 100,000 trees
cargo run --release -p static-ui-clay-oracle --example bench      # vs clay.h
cargo run --release -p static-ui-raster --example ui_bench       # headless
cargo run --release -p static-ui-raster --example ui_window_bench -- --launch 20
cargo run --release -p static-ui-raster --example ui_window_bench -- --idle 60
cargo run --release -p static-ui-raster --example ui_window_bench -- --latency 10
cd crates/static-ui-raster/bench/bevy_ui && cargo run --release -- --layout 10000
```
