# static-window: the plan

A windowing crate for static-ecs, in the role winit plays for Bevy, but
expressed in this ECS's own primitives: windows are entities, requests are
component writes, window-system events are schedule labels (and behavior
mailbox deliveries), and the event loop is the thing that calls `emit`.
Wayland on Linux only, for now.

Template: StereoKit's windowing layer, [sk_app](https://github.com/StereoKit/sk_app)
(read at `c80a9ae`), and the way StereoKit drives it
(`StereoKitC/xr_backends/window.cpp`, `systems/frame_pacer.*`). Code below
marked **SPECULATIVE** is the target API, not code that exists.

## Status: implemented

Every phase below is implemented, in `crates/static-window`
([README](crates/static-window/README.md)). Measurements against the
budgets are in [WINDOW_BENCHMARKS.md](WINDOW_BENCHMARKS.md). Where the code
differs from the **SPECULATIVE** parts:

- **No `Update` label.** `Step` is a plain label, and the step's timing is the
  `FrameTime` resource (`now_ns`, `display_ns`, `dt`), set before it.
  Behaviors can be polled only under plain labels (`behavior::<B, _>(Step)`).
  Layers above the window hook in after `Step` through
  `WindowLoop::run_with`/`step_with`:
  - static-ui-raster's `ui_frame` emits `UiUpdate` and `UiFrame`;
  - flappy emits its `Tick` from `FrameTime::dt`.
- **Protocol state lives in the loop, not in a `Native` component.** Each
  window's protocol objects are kept by the `WindowLoop`. Requests go out
  from the components it reads after `Step`, so no system needs protocol
  access. The cursor is the per-window `CursorIcon` component, not a
  resource.
- **Rendering.** The plan's `framebuffer.back()` is `Framebuffer::target()`,
  which returns `Target { pixels, width, height, scale, regions }`.
  `Framebuffer::at_scale(Scale::ONE)` keeps a fixed resolution that the
  viewport stretches; flappy uses it. Viewport destination and buffer scale
  are sent only when they change.
- **Additions.**
  - `Window::passthrough` (an empty input region);
  - `Stats::first_commit_ns` and opt-in latency recording;
  - `Entity::PLACEHOLDER` in static-ecs, for synthetic events in tests;
  - `UiClock::Frame`: during a window frame, `ui_frame` sets the UI's clock to
    the frame's predicted display time. This is how "`UiClock` takes the
    step" (phase 8) came out.
- **Budgets missed.**
  - Launch is about 0.3 ms over budget. Loading libwayland and libffi with
    `dlopen` costs that much, and the old pure-Rust clients never paid it.
  - Allocation-free holds for our code, not for libwayland: libwayland
    mallocs one closure per message, and wayland-rs allocates for each new
    object and for events with more than four arguments.

  Details are in WINDOW_BENCHMARKS.md.

---

## 1. Why a crate, and why now

Before this crate, two hand-written Wayland loops existed, and they had diverged:

| | `static-ui-wayland/src/window.rs` (546 lines) | `demo/flappy/src/main.rs` (451 lines) |
|---|---|---|
| Wakes on | socket, behaviors' eventfd, timer deadline | socket only (`blocking_dispatch`) |
| Draws | damaged rectangles, two buffers, per-buffer pending damage | whole frame every callback |
| Input | US-layout evdev table, no repeat, no compose | spacebar only |
| Scale | none (1×) | none |
| Couples to | `static-ui` (`UiFrame`, `RenderList`, `Fonts`) | the game's `Draw`/`Tick` |

Neither handles scale, key repeat, keyboard layouts, cursors, the clipboard,
multiple windows, or a compositor that resizes the window during a drag
(configure storms). sk_app handles all of these in ~2,300 lines of Wayland
backend; this crate ports its decisions, not its C.

## 2. What we take from sk_app / StereoKit

Each item is a decision sk_app made (file in parentheses) that we adopt:

1. **One platform library, consumers on top.** sk_app owns windows, input
   state, cursor, clipboard; StereoKit's `window.cpp` only polls events and
   feeds its own input and renderer. Here: `static-window` owns the
   protocol; `static-ui`, flappy and any renderer are plugins on top.
2. **Logical sizes everywhere, pixels only for the framebuffer**
   (`sk_app.h` "DPI is not opt-in"). Scale is kept in exact 120ths
   (fractional-scale-v1's unit), drawable size rounds *up*
   (`ska_wl_apply_scale`), the buffer is described by `wp_viewport`; integer
   `wl_output.scale` via `wl_surface.enter/leave` is the fallback.
3. **Resize is reported only on a real change.** Compositors resend
   configure with the same size throughout an interactive drag; sk_app
   dedupes in one place (`ska_wl_apply_scale`) and treats sizes reported
   while the window is being created as initial values, not changes.
4. **A window maps before a renderer exists** with a 1×1 placeholder buffer
   scaled by the viewport (`ska_wl_attach_placeholder`), and once a renderer
   presents, the platform layer stops committing on its own
   (`app_presents`, `ska_wl_commit_if_idle`) so it never stretches a
   presented frame.
5. **Bounded first configure** (`ska_wl_wait_configured`): a compositor that
   never answers cannot hang the app.
6. **Keyboard: physical key + separate text.** Key events carry a scancode
   from level 0 of the keymap; text events carry only insertable text, never
   control characters (`ska_event_text_t`); dead keys go through compose;
   repeat is generated client-side from `repeat_info`, only for the newest
   key, without bursts after a stall (`ska_wl_pump_key_repeat`); all keys
   are released on focus loss and held keys pressed on enter
   (`keyboard_enter/leave`).
7. **Modifiers derived from tracked key state**, because
   `wl_keyboard.modifiers` arrives after the key (`ska_wl_emit_key`).
8. **Live state is what the compositor reports**, not the last request
   (`ska_window_get_fullscreen` "tracks reality").
9. **Optional globals are reported once** with what each absence costs
   (`ska_wl_log_optional`), and only `wl_compositor`, `wl_shm`,
   `xdg_wm_base` are required.
10. **Cursor shapes via `wp_cursor_shape_v1`**, an XCursor theme only as
    fallback; **relative pointer + pointer lock** as the only capture
    mechanism (no warp).
11. **Vblank from frame callbacks** and **refresh from `wl_output.mode`**
    (`ska_window_get_vblank_ns`, `get_refresh_rate`).
12. **Display-driven time step** (StereoKit `frame_pacer_step`): the next
    frame's `dt` is the gap between the display times of consecutive frames,
    so animation moves as far as the screen will show it moving; wall clock
    only when no presentation report exists.
13. **A dead connection becomes a quit event once**, instead of a spinning
    loop (`ska_wl_pump_events`).
14. **Title bars, sk_app's way** (`ska_wl_init_decorations`,
    `ska_wl_build_toplevel`, `ska_decor.c`): ask the compositor for
    server-side decorations through `xdg-decoration` (COSMIC, KDE, wlroots);
    only when it has none (GNOME) `dlopen` `libdecor-0.so.0` and let it draw
    the frame, so other desktops never load it. libdecor then owns the
    `xdg_surface`/`xdg_toplevel`: its configure replaces ours, reports the
    content size without the frame, and its commit callback always commits.
    No libdecor installed: no frame, one warning. A borderless window skips
    both. An environment variable forces the libdecor path, to test it on a
    desktop that decorates.

What we do not take: X11/Win32/macOS/Android/web backends, the 1 ms
sleep-polling `ska_event_wait` (we `poll` on fds), the global 16-window
array (entities), file dialogs, kvpstore, asset paths. Like sk_app we load
libwayland and libdecor with `dlopen` (§5).

## 3. The ECS shape

### 3.1 Windows are entities

```rust
// SPECULATIVE
let mut app = App::new().add_plugins((WindowPlugin, Notes)).build();
app.world.spawn_bundle((Window::new("Notes").size(800, 600),));
static_window::run(&mut app); // returns when no Window entity remains, or on Quit
```

| Component | Written by | Meaning |
|---|---|---|
| `Window` | app | Requested config: title, app_id, min/max size, resizable, decorated. `Changed<Window>` → protocol requests (change summaries, so idle is one load). |
| `WindowMode` | app | Requested: `Windowed`/`Maximized`/`Fullscreen`, plus one-shot `minimize()`. |
| `Surface` | crate, `Immutable` | Live logical size, pixel size, scale (120ths). `OnInsert::<Surface>` hooks see every real change (item 3). |
| `WindowState` | crate, `Immutable` | Live maximized / fullscreen / activated / suspended. |
| `KeyboardFocus`, `PointerOver` | crate, `Sparse` | Markers on the focused / hovered window. |
| `Animating` | app, `Sparse` | Keep requesting frame callbacks (a running animation). |
| `CursorIcon` | app | Cursor shape while the pointer is over this window. |
| `Framebuffer` | crate | Shared-memory swapchain (§4); present only for software rendering. |
| `Native` (private) | crate | `WlSurface`, `XdgSurface`/`XdgToplevel` *or* the libdecor frame, decoration object, viewport, fractional scale. |

Spawning `Window` creates the surface (an `OnAdd::<Window>` hook queues it
for the loop); despawning, or removing `Window`, destroys it (`OnRemove`).
Wayland proxies are `Send + Sync` (checked), so `Native` is an ordinary
component and the request systems call protocol methods directly; only
event dispatch needs the loop's `&mut` protocol state.

### 3.2 Events are labels, and mailbox deliveries

Each event kind is its own payload label (`type In = Ref<Self>`), so an app
with no key handling compiles no key program, and emitting a label nobody
listens to costs nothing. Each is also `deliver`ed to its window entity, so
a behavior can write `cx.on::<CloseRequested>(window).await`.

```rust
// SPECULATIVE
pub struct CloseRequested { pub window: Entity }
pub struct PointerMoved   { pub window: Entity, pub x: f32, pub y: f32 }   // logical
pub struct PointerButton  { pub window: Entity, pub button: MouseButton, pub pressed: bool }
pub struct Scroll         { pub window: Entity, pub dx: f32, pub dy: f32, pub lines: Option<(f32, f32)> }
pub struct KeyInput       { pub window: Entity, pub code: KeyCode, pub key: Key, pub pressed: bool, pub repeat: bool, pub modifiers: Modifiers }
pub struct TextInput      { pub window: Entity, pub text: InlineStr<32> }
pub struct FocusChanged   { pub window: Entity, pub focused: bool }
pub struct ClipboardText  { pub text: Box<str> }   // reply to Clipboard::request
```

Input *state* (sk_app's `ska_keyboard_get_state`, `ska_mouse_get_state`,
`ska_mouse_get_delta`) is resources: `Keys` (pressed bitset by `KeyCode`,
modifiers), `Pointer` (position, buttons, accumulated delta, window under
it). `Clipboard` and `Cursor` are resources with request methods.

### 3.3 One wakeup

StereoKit's `window_step_begin`/`_end`, in labels:

```mermaid
flowchart LR
  P[poll: socket, Wakeup eventfd, next deadline] --> D[dispatch protocol into event ring]
  D --> E[emit each event label + deliver]
  E --> U[emit Update]
  U --> R[request systems: Changed Window/WindowMode/CursorIcon]
  R --> F[emit Redraw per window that needs pixels]
  F --> C[attach + damage + commit, frame callback if Animating]
```

- A window needs pixels when it was resized or rescaled, its renderer
  reported damage, or a frame callback arrived while `Animating`.
- `Update` carries the step: `now`, `dt` (display-driven, item 12) and the
  predicted display time of the frame being produced.
- Deadlines: a `WakeAt` resource (earliest of any plugin's timers, key
  repeat) bounds the `poll` timeout. With no deadline, no `Animating`, and
  no input, the loop sleeps in `poll` forever; ten thousand idle behaviors
  cost nothing (their wakers go through the existing `Wakeup` resource).
- The event ring and damage lists are reused; no allocation per event or
  per frame once warm.

`run`'s bounds name every label it emits; a blanket trait
(`WindowLabels<S>`) spells them once so callers write one bound.

## 4. Rendering contract

- **Software (now):** `Framebuffer` owns a memfd mapping with two `XRGB8888`
  buffers and, per buffer, the damage since it was last drawn (moved from
  `static-ui-wayland`). Under `Redraw { window }` a renderer calls
  `framebuffer.back(&mut self) -> Target { pixels, width, height, stride,
  scale, redraw: &[Rect] }` and reports what it changed; the loop attaches
  and commits only that damage. A busy pair defers the draw until a release
  (today's `waiting_for_buffer`), and resizing reallocates once per real
  resize, between frames (StereoKit reconciles swapchain size between
  frames for the same reason: a Wayland window takes a new size only when a
  buffer commits).
- **First frame during the configure roundtrip** (today's trick, worth
  0.2 ms): draw at the requested size before configure arrives; commit if
  the size came back the same.
- **GPU (later):** the crate runs on libwayland (§5), so
  `wl_display*` and `wl_surface*` exist to hand Vulkan or wgpu, as
  `ska_vk_create_surface` does. Not built in this plan.

## 5. Decorations, the libwayland backend, and keyboard layouts

**Decorations.** libdecor is C and takes a `wl_display*`, which the
pure-Rust `wayland-client` backend does not have. Its `system` backend
(libwayland, loaded with `dlopen`, as sk_app does) has one. A throwaway spike
confirmed the combination:
- libdecor decorated a wayland-rs surface on weston headless (no
  `xdg-decoration`, like GNOME), and our buffer went up after its configure.
- libdecor's objects sit on libwayland's default queue and ours on
  wayland-rs's queue. One poll serves both: the loop reads the socket once,
  dispatches our queue, then calls `libdecor_dispatch(ctx, 0)`.
- Decoration surfaces get pointer events on libdecor's own seat listener;
  ours ignores surfaces it doesn't own, as `ska_wl_window_from_surface`
  does.

| libdecor plugin (weston headless) | launch to first frame |
|---|---|
| GTK (libdecor's default choice when GTK is installed) | 66–69 ms |
| cairo | 15 ms |

That cost is paid only where the compositor will not decorate. COSMIC and
KDE use `xdg-decoration` and never load libdecor. Plugin choice is left to
libdecor, as in sk_app.

**One backend: libwayland, like sk_app.** `wayland-client`'s `system`
backend with `dlopen`, everywhere. Static binaries are given up: `dlopen`
from a fully static binary is not dependable (a static glibc process would
load a second `libc.so` with the library; static musl cannot `dlopen` at
all). flappy becomes an ordinary dynamically linked binary, like every
other binary in the workspace.

**Keyboard layouts: kbvm** (pure Rust, with its `compose` feature for dead
keys), replacing the current US-layout evdev table.

## 6. Testing without sleeps

- **Pure logic, unit-tested:** scale and rounding, resize dedupe, key
  repeat scheduling on a manual clock, modifier derivation, held/released
  keys across focus, display-driven `dt`.
- **A scripted compositor** (`tests/support/compositor.rs`, on the
  pure-Rust `wayland-server`, over a socketpair): implements
  `wl_compositor`, `wl_shm`, `xdg_wm_base`, `wl_seat`, `wl_output`,
  fractional scale, viewporter, cursor shape, data device. Tests script
  configures, scale changes, key and pointer events, and assert on the ECS
  (components, emitted labels, deliveries) and on what the client sent
  (commits, damage, buffer pixels, cursor shape). Steps are driven by
  roundtrips, so tests are deterministic. The client side connects with
  `Connection::from_socket`.
- **Decoration choice, scripted:** with `xdg-decoration` offered, the
  window asks for server-side mode, libdecor stays unloaded, and the
  compositor answering client-side is reported. Without it, the libdecor
  path is taken, or an undecorated window plus one warning when the loader
  finds no library (the loader is injectable for this test).
- **Real compositors (smoke, not in `cargo test`):** `weston
  --backend=headless` (installed; has `xdg_wm_base`, `wp_presentation`,
  `wp_viewporter`, no seat) for create/configure/commit/present, and COSMIC
  for everything, including real key injection through
  `zwp_virtual_keyboard_manager_v1`, which COSMIC offers. That closes the
  gap in `UI_BENCHMARKS.md`, where input latency was measured with a
  behavior wakeup instead of a key press. Weston headless has no
  `xdg-decoration`, so it also runs the libdecor path for real: configure,
  content size, commit callback, close.

## 7. Phases

Each phase lands as one commit with its tests; the repo stays green.

### Phase 0: crate and pure core
`crates/static-window`: components, event labels, `Keys`/`Pointer`,
`WindowPlugin`, scale math, resize dedupe, repeat scheduler, `dt`. No
Wayland yet; unit tests only.

### Phase 1: scripted compositor
The `wayland-server` test harness of §6, with a smoke test that a raw
`wayland-client` connection binds globals through it.

### Phase 2: windows
Connect, required/optional globals (report once), window entities via hooks,
configure/ack with bounded first wait, placeholder map, close → event,
`Window`/`WindowMode` requests, live `Surface`/`WindowState`, several
windows, connection loss → quit once, `run` with `poll` on socket +
`Wakeup` + `WakeAt`. Acceptance: an idle window makes zero commits and
wakes only for compositor traffic.

### Phase 3: software rendering
`Framebuffer`, `Redraw`, per-buffer damage, busy-buffer deferral, resize
between frames, viewport + fractional scale, first frame during configure,
frame callbacks only while `Animating`.

### Phase 4: input
Seat hot-plug, pointer (enter/leave/motion/button, `axis_value120`, events
grouped by `wl_pointer.frame`), keyboard via kbvm (keymap from the
compositor's fd, repeat, compose, text, focus semantics of §2.6–7), input
state resources, deliveries to window entities.

### Phase 5: timing
Frame-callback vblank, `wp_presentation` feedback, refresh rate,
display-driven `dt` and predicted display time in `Update`, `LoopStats`
(today's `Stats`: wakeups, commits, frame reasons, input-to-commit and
input-to-present).

### Phase 6: cursor, pointer capture, clipboard
`CursorIcon` via `wp_cursor_shape_v1` with an XCursor fallback (pure-Rust
`xcursor` crate), hidden cursor, relative pointer + lock, clipboard text
get/set through `wl_data_device` (the read is a pipe polled by the loop,
answered with `ClipboardText`).

### Phase 7: title bars
Item 14 of §2:
- `xdg-decoration` server-side mode, plus the compositor's answer.
- Otherwise libdecor, loaded with `dlopen`.
  Its frame replaces our `xdg_surface`/`xdg_toplevel` and our configure
  path, and `Window`/`WindowMode` requests go through its API: title,
  app_id, min/max content size, maximize, fullscreen, minimize.
- `Window::decorated = false` skips both.
- `STATIC_WINDOW_FORCE_CSD=1` forces the libdecor path, like
  `SKA_WAYLAND_FORCE_CSD`.
- Acceptance: a weston-headless smoke run with no `xdg-decoration`, and
  COSMIC with the variable set.

### Phase 8: cutover
- `static-ui`: its `Input` enum and `Redraw` resource are replaced by
  static-window's labels, `WakeAt` and `Animating`; `Viewport` comes from
  `Surface`; `UiClock` takes `Update`'s step.
- `static-ui-wayland` no longer touches Wayland: it becomes
  `static-ui-raster` (fonts, glyphs, canvas, paint, and a plugin drawing the
  `RenderList` under `Redraw`); `window.rs` is deleted.
- flappy: its loop is deleted; `Draw` runs under `Redraw`, `Tick` under
  `Update`. Its fully static build goes away (§5).
- Examples and benches move; docs and `UI_BENCHMARKS.md` update.

### Phase 9: measure
`WINDOW_BENCHMARKS.md`, on COSMIC, against the loops it replaced and against
winit 0.30 + softbuffer drawing the same frame:

| Measure | Budget |
|---|---|
| Idle window, 60 s | 0 commits, ≤ 2 wakeups, 0.000 s CPU |
| Launch to first commit | flappy ≤ its current number; static-ui widgets ≤ 3.2 ms; libdecor path reported separately |
| Key press (virtual keyboard) to commit sent | ≤ 0.1 ms median |
| Allocations per event and per frame, warm | 0 |
| Stripped binary size, empty window | report against winit + softbuffer |

## 8. Not in this plan

`xdg_popup` (menus and tooltips outside the window; a `PopupOf` relation
with `LINKED` fits it), layer shell, text-input-v3 / IME, drag and drop,
primary selection, xdg-activation, idle inhibit, file dialogs (a portal
over D-Bus), GPU surfaces, other platforms.

## 9. Decisions

1. **Keyboard layouts:** kbvm.
2. **Title bars:** the compositor's when it offers them, libdecor's when it
   does not, as sk_app does (§5, phase 7).
3. **Crate names:** `static-window`; `static-ui-wayland` becomes
   `static-ui-raster` at cutover.
