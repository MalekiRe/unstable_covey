# static-window

Windows as entities for [static-ecs](../static-ecs), on Wayland. The design
follows StereoKit's windowing layer, [sk_app](https://github.com/StereoKit/sk_app):
- sizes are logical, with fractional scale kept exact;
- a resize is reported only when the size really changed;
- keys arrive as physical keys, with typed text as a separate event;
- key repeat is generated on the client;
- the time step follows the display;
- title bars come from the compositor, or from libdecor where it has none.

See [WINDOW_PLAN.md](../../WINDOW_PLAN.md) for the design and
[WINDOW_BENCHMARKS.md](../../WINDOW_BENCHMARKS.md) for measurements.

```rust,ignore
use static_ecs::prelude::*;
use static_window::*;

let mut app = App::new()
    .add_plugins(WindowPlugin)
    .add_systems(label::<KeyInput>(), |key: &KeyInput| println!("{:?}", key.key))
    .add_systems(label::<TextInput>(), |text: &TextInput| println!("typed {}", text.text()))
    .build();
app.world.spawn_bundle((Window::new("Notes").size(800, 600),));
run(&mut app)?; // until the windows are gone
```

## Windows

Spawn a `Window` to open one; despawn it to close it. To change a window,
write to its components; read what the compositor granted from the others.

| Component | Written by | |
|---|---|---|
| `Window` | app | Title, app id, size (the compositor may override it), limits, resizable, decorated, close-on-request, passthrough |
| `WindowMode` | app | `Windowed`, `Maximized`, `Fullscreen` (requests) |
| `Minimize` | app | Minimize once; removed after asking |
| `Surface` | static-window | Live logical size and scale (`Scale`, in 120ths) |
| `WindowState` | static-window | Live maximized, fullscreen, activated, resizing, suspended |
| `KeyboardFocus`, `PointerOver` | static-window | Markers on the focused / hovered window |
| `Animating` | app | Keep drawing at the display's rate |
| `CursorIcon` | app | The cursor over this window (CSS names) |
| `PointerLock` | app | Lock the pointer; raw motion arrives as `PointerDelta` |
| `Framebuffer` | app | Draw this window yourself, on the CPU |

`Surface` and `WindowState` are `Immutable`: their hooks see every real
change.

## Events

Each kind of event is its own label: `CloseRequested`, `Resized`,
`FocusChanged`, `PointerMoved`, `PointerLeft`, `PointerButton`,
`PointerDelta`, `Scroll`, `KeyInput`, `TextInput`, and `ClipboardText`. Name
them with `label::<E>()`. Each window event is also delivered to its window
entity, so a behavior can `cx.on::<CloseRequested>(window).await`. `Keys` and
`Pointer` are resources holding input state. `Clipboard` sets the clipboard's
text, or requests it (the answer is `ClipboardText`).

Keys come in two forms:
- `KeyInput::code` is the physical key: the evdev code, the same on every layout.
- `KeyInput::key` is what the layout makes of it, read through the
  compositor's keymap with [kbvm](https://crates.io/crates/kbvm).

Dead keys compose into text, and `TextInput` never carries control
characters.

## The loop

Each wakeup does the following, in order:
1. Apply what the compositor said (emitting the labels above).
2. Set `FrameTime` and emit `Step`.
3. Run the caller's frame hook, if any.
4. Send what the world wants (new, changed and despawned windows, cursors,
   locks, the clipboard).
5. Emit `Redraw { window }` for each framebuffer with something to show, then
   commit it.

With nothing animating, no deadline in `WakeAt`, and no input, the loop sleeps
in `poll`. An idle window commits nothing. `Wakeup` (from static-ecs) wakes
it from any thread.

- `run(&mut app)`: connect and run until no windows remain.
- `WindowLoop::run_with(app, frame)`: the same, calling `frame` after each
  `Step`. static-ui-raster uses it to emit the UI's update and layout.
- `WindowLoop::step_with(app, timeout, frame)`: one wakeup, for embedding the
  loop or driving it in tests.

`FrameTime::dt` is the gap between the predicted display times of
consecutive frames, after StereoKit's frame pacer. It comes from
`wp_presentation` reports, or from frame callbacks and the output's mode where
those are missing. Without any of them it falls back to the wall clock.

## Drawing

Put a `Framebuffer` on a window, and call `request(rect)` or `request_all()`
during `Step`. The loop picks whichever of its two shared-memory buffers is
free and emits `Redraw` for it. In `Redraw`, `target()` hands out that buffer
and the regions it needs repainted: everything that changed since that
buffer was last drawn.

Sizes:
- The buffers are the window's size in pixels at its scale, and the viewport
  states the logical size.
- `Framebuffer::at_scale(Scale::ONE)` keeps a fixed resolution instead, and the
  compositor stretches it.
- The first frame is drawn while the compositor answers the new window.

Pacing:
- While a frame callback is outstanding, the next frame waits for it, so an
  animation runs at the display's rate.

## Title bars

The crate asks the compositor to draw title bars through `xdg-decoration`.
Where the compositor does not support it (GNOME), libdecor draws them. It is
loaded with `dlopen` when the first window that needs a frame is created. With
neither available, windows have no frame.

To try the libdecor path on a desktop that draws title bars, set
`STATIC_WINDOW_FORCE_CSD=1`.

## Building

libwayland is loaded with `dlopen` (through wayland-client's `system`
backend), so a fully static binary is not supported.

Large apps can exceed the compiler's default recursion limit when their
plugins' vocabularies combine; `#![recursion_limit = "256"]` in the binary
fixes it (see the static-ui-raster examples).

## Tests

`tests/support/compositor.rs` is a scripted Wayland compositor (on
`wayland-server`) running on its own thread. Tests:
1. Drive it with configures, scale changes, input, presentation reports and
   clipboard offers.
2. Step the loop.
3. Assert on the world, and on what the client sent: commits, damage, buffer
   pixels, cursor shapes, locks.

Nothing in them depends on timing.
