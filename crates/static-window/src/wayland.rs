//! Protocol state: what the compositor has told us, kept per window until the
//! loop applies it to the world. Handlers only record; [`crate::run`] decides.
use std::os::fd::AsFd;

use static_ecs::Entity;
use wayland_client::globals::GlobalListContents;
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::{self, WlOutput};
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::{self, WlSurface};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{self, WpFractionalScaleV1};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::presentation_time::client::wp_presentation::{self, WpPresentation};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{self, WpPresentationFeedback};
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

use crate::clipboard::Selection;
use crate::cursor::{CursorIcon, Theme};
use crate::event::TextInput;
use wayland_client::protocol::wl_data_device_manager::WlDataDeviceManager;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::WpCursorShapeDeviceV1;
use wayland_protocols::wp::cursor_shape::v1::client::wp_cursor_shape_manager_v1::WpCursorShapeManagerV1;
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_locked_pointer_v1::ZwpLockedPointerV1;
use wayland_protocols::wp::pointer_constraints::zv1::client::zwp_pointer_constraints_v1::ZwpPointerConstraintsV1;
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1;
use wayland_protocols::wp::relative_pointer::zv1::client::zwp_relative_pointer_v1::{self, ZwpRelativePointerV1};
use crate::framebuffer::BufferId;
use crate::input::{Key, KeyCode, MouseButton};
use crate::keyboard::{Keymap, Typed};
use crate::pacer::Pacer;
use crate::repeat::Repeat;
use crate::scale::Scale;
use crate::window::WindowState;
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_seat::{self, WlSeat};

/// Globals bound once at connect.
pub(crate) struct Globals {
    pub compositor: WlCompositor,
    pub shm: WlShm,
    pub wm: XdgWmBase,
    pub decoration: Option<ZxdgDecorationManagerV1>,
    pub viewporter: Option<WpViewporter>,
    pub fractional: Option<WpFractionalScaleManagerV1>,
    pub presentation: Option<WpPresentation>,
    pub cursor_shape: Option<WpCursorShapeManagerV1>,
    pub constraints: Option<ZwpPointerConstraintsV1>,
    pub relative: Option<ZwpRelativePointerManagerV1>,
    pub data_device: Option<WlDataDeviceManager>,
}

/// An output, for the integer-scale fallback and its refresh rate.
pub(crate) struct Output {
    pub output: WlOutput,
    pub name: u32,
    pub scale: i32,
    pub refresh_mhz: i32,
}

/// A window's protocol objects and what the compositor last said about it.
/// Fields drop in order: the role before the surface it is a role of.
pub(crate) struct Proto {
    pub entity: Entity,
    pub role: crate::decor::Role,
    pub surface: OwnedSurface,
    pub viewport: Option<WpViewport>,
    pub fractional: Option<WpFractionalScaleV1>,
    /// The `Window` and mode last sent.
    pub sent: crate::Window,
    pub sent_mode: crate::WindowMode,

    /// From the latest `xdg_toplevel.configure`, applied at the next
    /// `xdg_surface.configure`.
    pub pending_size: (i32, i32),
    pub pending_state: WindowState,
    /// An `xdg_surface.configure` arrived (and was acked) since the loop
    /// last looked.
    pub configured_now: bool,
    /// The first configure has arrived.
    pub configured: bool,
    /// Logical size in effect.
    pub size: (u32, u32),
    pub state: WindowState,
    /// From fractional-scale-v1, in 120ths.
    pub fractional_scale: Option<u32>,
    /// From `wl_surface.preferred_buffer_scale` (v6).
    pub preferred_buffer_scale: Option<i32>,
    /// Outputs the surface is on, by registry name.
    pub outputs: Vec<u32>,
    /// Size or scale may have changed: re-derive the surface.
    pub dirty: bool,
    pub close_requested: bool,
    /// The compositor's answer to our decoration request.
    pub server_decorated: Option<bool>,
    /// A placeholder buffer is attached (no renderer yet).
    pub placeholder: Option<(WlBuffer, (u32, u32))>,
    /// The viewport destination and buffer scale last sent: surface state
    /// that holds across commits, so sent only when it changes.
    pub destination: Option<(i32, i32)>,
    pub buffer_scale: i32,
    /// A frame callback is outstanding.
    pub frame_requested: bool,
    /// A frame callback fired since the loop last looked.
    pub frame_done: bool,
    /// The framebuffer's buffer drawn before the first configure, committed
    /// once it arrives (if the size held).
    pub predrawn: Option<usize>,
    /// A frame is due but both buffers are with the compositor.
    pub waiting: bool,
    /// Display timing for this surface, after StereoKit's frame pacer.
    pub pacer: Pacer,
    /// Buffer commits so far: the id of each commit's presentation.
    pub commits: u64,
    /// The commit the outstanding frame callback rides on.
    pub frame_seq: u64,
    /// The pointer lock, while [`crate::PointerLock`] is on the window.
    pub lock: Option<ZwpLockedPointerV1>,
}

impl Proto {
    /// The scale in effect: fractional-scale-v1's, else the surface's
    /// preferred buffer scale, else the largest scale of its outputs.
    pub fn scale(&self, outputs: &[Output]) -> Scale {
        if let Some(scale) = self.fractional_scale {
            return Scale(scale);
        }
        if let Some(factor) = self.preferred_buffer_scale {
            return Scale::integer(factor.max(1) as u32);
        }
        let factor = outputs.iter().filter(|o| self.outputs.contains(&o.name)).map(|o| o.scale).max().unwrap_or(1);
        Scale::integer(factor.max(1) as u32)
    }

    /// Show the buffer at logical size `width × height`: through the
    /// viewport where there is one, else as an integer buffer scale.
    pub fn set_geometry(&mut self, width: u32, height: u32, scale: Scale) {
        match &self.viewport {
            Some(viewport) => {
                let destination = (width as i32, height as i32);
                if self.destination != Some(destination) {
                    viewport.set_destination(destination.0, destination.1);
                    self.destination = Some(destination);
                }
            }
            None => {
                let factor = scale.0.div_ceil(Scale::UNIT) as i32;
                if self.buffer_scale != factor {
                    self.surface.set_buffer_scale(factor);
                    self.buffer_scale = factor;
                }
            }
        }
    }
}

impl Drop for Proto {
    fn drop(&mut self) {
        // Extensions of the surface first; then (as fields) its role, then
        // the surface itself.
        if let Some(lock) = self.lock.take() {
            lock.destroy();
        }
        if let Some(fractional) = self.fractional.take() {
            fractional.destroy();
        }
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
        if let Some((buffer, size)) = self.placeholder.take()
            && size != (1, 1)
        {
            buffer.destroy();
        }
    }
}

/// A surface destroyed when dropped.
pub(crate) struct OwnedSurface(pub WlSurface);
impl std::ops::Deref for OwnedSurface {
    type Target = WlSurface;
    fn deref(&self) -> &WlSurface {
        &self.0
    }
}
impl Drop for OwnedSurface {
    fn drop(&mut self) {
        self.0.destroy();
    }
}

/// Everything the dispatch handlers update.
#[derive(Default)]
pub(crate) struct State {
    pub windows: Vec<Proto>,
    pub outputs: Vec<Output>,
    /// An output's scale or mode changed.
    pub outputs_changed: bool,
    /// Framebuffer buffers the compositor gave back.
    pub released: Vec<BufferId>,
    pub seat: Seat,
    /// Input since the loop last looked, in order.
    pub input: Vec<InputEvent>,
    /// The presentation clock (CLOCK_MONOTONIC is 1, the clock we read).
    pub clock: Option<u32>,
    /// Input-to-screen latencies, in nanoseconds, if the loop records them.
    pub presented: Option<Vec<u64>>,
    pub selection: Selection,
    /// The cursor theme, where the compositor cannot draw shapes itself.
    pub theme: Option<Theme>,
    /// libdecor, drawing title bars where the compositor won't. After the
    /// windows, so their frames go first.
    pub decor: Option<crate::decor::Libdecor>,
}

impl State {
    pub fn window(&mut self, entity: Entity) -> Option<&mut Proto> {
        self.windows.iter_mut().find(|w| w.entity == entity)
    }
    pub(crate) fn bind_output(&mut self, registry: &WlRegistry, name: u32, version: u32, qh: &QueueHandle<Self>) {
        let output = registry.bind::<WlOutput, _, _>(name, version.min(4), qh, name);
        self.outputs.push(Output { output, name, scale: 1, refresh_mhz: 0 });
    }
}

/// An opaque black buffer: shared 1×1 (stretched by a viewport) or the
/// window's pixel size without one. It maps a window before anything draws.
pub(crate) fn placeholder(shm: &WlShm, qh: &QueueHandle<State>, width: u32, height: u32) -> WlBuffer {
    let stride = width as i32 * 4;
    let bytes = stride as usize * height as usize;
    let fd = rustix::fs::memfd_create("static-window-placeholder", rustix::fs::MemfdFlags::CLOEXEC).expect("memfd_create");
    rustix::fs::ftruncate(&fd, bytes as u64).expect("ftruncate");
    // SAFETY: a fresh shared mapping of the whole memfd, unmapped below.
    unsafe {
        let pixels = rustix::mm::mmap(std::ptr::null_mut(), bytes, rustix::mm::ProtFlags::WRITE, rustix::mm::MapFlags::SHARED, &fd, 0)
            .expect("mmap");
        std::slice::from_raw_parts_mut(pixels as *mut u32, bytes / 4).fill(0xFF00_0000);
        let _ = rustix::mm::munmap(pixels, bytes);
    }
    let pool = shm.create_pool(fd.as_fd(), bytes as i32, qh, ());
    let buffer = pool.create_buffer(0, width as i32, height as i32, stride, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    buffer
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(state: &mut Self, registry: &WlRegistry, event: wl_registry::Event, _: &GlobalListContents, _: &Connection, qh: &QueueHandle<Self>) {
        match event {
            wl_registry::Event::Global { name, interface, version } if interface == WlOutput::interface().name => {
                state.bind_output(registry, name, version, qh);
            }
            wl_registry::Event::GlobalRemove { name } => {
                if let Some(i) = state.outputs.iter().position(|o| o.name == name) {
                    let output = state.outputs.swap_remove(i);
                    for window in &mut state.windows {
                        window.outputs.retain(|&o| o != name);
                        window.dirty = true;
                    }
                    if output.output.version() >= 3 {
                        output.output.release();
                    }
                }
            }
            _ => {}
        }
    }
}

impl Dispatch<WlOutput, u32> for State {
    fn event(state: &mut Self, _: &WlOutput, event: wl_output::Event, &name: &u32, _: &Connection, _: &QueueHandle<Self>) {
        let Some(output) = state.outputs.iter_mut().find(|o| o.name == name) else { return };
        match event {
            wl_output::Event::Mode { flags: WEnum::Value(flags), refresh, .. } if flags.contains(wl_output::Mode::Current) => {
                output.refresh_mhz = refresh;
            }
            wl_output::Event::Scale { factor } => output.scale = factor,
            wl_output::Event::Done => state.outputs_changed = true,
            _ => {}
        }
    }
}

impl Dispatch<WlSurface, Entity> for State {
    fn event(state: &mut Self, _: &WlSurface, event: wl_surface::Event, &entity: &Entity, _: &Connection, _: &QueueHandle<Self>) {
        let outputs = &state.outputs;
        let Some(window) = state.windows.iter_mut().find(|w| w.entity == entity) else { return };
        match event {
            wl_surface::Event::Enter { output } => {
                if let Some(o) = outputs.iter().find(|o| o.output == output)
                    && !window.outputs.contains(&o.name)
                {
                    window.outputs.push(o.name);
                    window.dirty = true;
                }
            }
            wl_surface::Event::Leave { output } => {
                if let Some(o) = outputs.iter().find(|o| o.output == output) {
                    window.outputs.retain(|&n| n != o.name);
                    window.dirty = true;
                }
            }
            wl_surface::Event::PreferredBufferScale { factor } => {
                window.preferred_buffer_scale = Some(factor);
                window.dirty = true;
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(_: &mut Self, wm: &XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, Entity> for State {
    fn event(state: &mut Self, xdg: &XdgSurface, event: xdg_surface::Event, &entity: &Entity, _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            // Acked now; the commit that follows applies it.
            xdg.ack_configure(serial);
            if let Some(window) = state.window(entity) {
                window.configured_now = true;
                window.dirty = true;
            }
        }
    }
}

impl Dispatch<XdgToplevel, Entity> for State {
    fn event(state: &mut Self, _: &XdgToplevel, event: xdg_toplevel::Event, &entity: &Entity, _: &Connection, _: &QueueHandle<Self>) {
        let Some(window) = state.window(entity) else { return };
        match event {
            xdg_toplevel::Event::Configure { width, height, states } => {
                window.pending_size = (width, height);
                let mut s = WindowState::default();
                for state in states.as_chunks::<4>().0.iter().map(|&b| u32::from_ne_bytes(b)) {
                    match xdg_toplevel::State::try_from(state) {
                        Ok(xdg_toplevel::State::Maximized) => s.maximized = true,
                        Ok(xdg_toplevel::State::Fullscreen) => s.fullscreen = true,
                        Ok(xdg_toplevel::State::Resizing) => s.resizing = true,
                        Ok(xdg_toplevel::State::Activated) => s.activated = true,
                        Ok(xdg_toplevel::State::Suspended) => s.suspended = true,
                        _ => {}
                    }
                }
                window.pending_state = s;
            }
            xdg_toplevel::Event::Close => window.close_requested = true,
            _ => {}
        }
    }
}

impl Dispatch<ZxdgToplevelDecorationV1, Entity> for State {
    fn event(state: &mut Self, _: &ZxdgToplevelDecorationV1, event: zxdg_toplevel_decoration_v1::Event, &entity: &Entity, _: &Connection, _: &QueueHandle<Self>) {
        if let zxdg_toplevel_decoration_v1::Event::Configure { mode: WEnum::Value(mode) } = event
            && let Some(window) = state.window(entity)
        {
            window.server_decorated = Some(mode == zxdg_toplevel_decoration_v1::Mode::ServerSide);
        }
    }
}

impl Dispatch<WpFractionalScaleV1, Entity> for State {
    fn event(state: &mut Self, _: &WpFractionalScaleV1, event: wp_fractional_scale_v1::Event, &entity: &Entity, _: &Connection, _: &QueueHandle<Self>) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(window) = state.window(entity)
            && window.fractional_scale != Some(scale)
        {
            window.fractional_scale = Some(scale);
            window.dirty = true;
        }
    }
}

impl Dispatch<WlBuffer, BufferId> for State {
    fn event(state: &mut Self, _: &WlBuffer, event: wl_buffer::Event, &id: &BufferId, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            state.released.push(id);
        }
    }
}

impl Dispatch<WlCallback, Entity> for State {
    fn event(state: &mut Self, _: &WlCallback, event: wl_callback::Event, &entity: &Entity, _: &Connection, _: &QueueHandle<Self>) {
        let wl_callback::Event::Done { callback_data: ms } = event else { return };
        let presentation = state.clock == Some(MONOTONIC);
        let Some(window) = state.window(entity) else { return };
        window.frame_requested = false;
        window.frame_done = true;
        // Without presentation-time, the callback's time is when the frame
        // went to the screen (milliseconds on an unnamed clock that every
        // compositor makes CLOCK_MONOTONIC, as sk_app assumes).
        if !presentation {
            let now = crate::run::now_ns();
            let delta_ms = ms.wrapping_sub((now / 1_000_000) as u32) as i32;
            if delta_ms.abs() < 1000 {
                window.pacer.presented(window.frame_seq, (now as i64 + delta_ms as i64 * 1_000_000) as u64);
            }
        }
    }
}

const MONOTONIC: u32 = 1;

/// What a presentation feedback reports on.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Feedback {
    pub window: Entity,
    pub seq: u64,
    /// When the input this frame answers arrived, for latency.
    pub input_at: Option<u64>,
}

impl Dispatch<WpPresentation, ()> for State {
    fn event(state: &mut Self, _: &WpPresentation, event: wp_presentation::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.clock = Some(clk_id);
        }
    }
}

impl Dispatch<WpPresentationFeedback, Feedback> for State {
    fn event(state: &mut Self, _: &WpPresentationFeedback, event: wp_presentation_feedback::Event, feedback: &Feedback, _: &Connection, _: &QueueHandle<Self>) {
        let wp_presentation_feedback::Event::Presented { tv_sec_hi, tv_sec_lo, tv_nsec, refresh, .. } = event else { return };
        if state.clock != Some(MONOTONIC) {
            return;
        }
        let shown = ((tv_sec_hi as u64) << 32 | tv_sec_lo as u64) * 1_000_000_000 + tv_nsec as u64;
        if let (Some(at), Some(latencies)) = (feedback.input_at, &mut state.presented) {
            latencies.push(shown.saturating_sub(at));
        }
        if let Some(window) = state.window(feedback.window) {
            window.pacer.presented(feedback.seq, shown);
            if refresh > 0 {
                window.pacer.set_refresh(refresh as u64);
            }
        }
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(_: &mut Self, _: &WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(State: WlCompositor);
delegate_noop!(State: wayland_client::protocol::wl_region::WlRegion);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: WpViewporter);
delegate_noop!(State: WpViewport);
delegate_noop!(State: WpFractionalScaleManagerV1);
delegate_noop!(State: ZxdgDecorationManagerV1);
delegate_noop!(State: WpCursorShapeManagerV1);
delegate_noop!(State: WpCursorShapeDeviceV1);
delegate_noop!(State: ZwpPointerConstraintsV1);
delegate_noop!(State: ZwpRelativePointerManagerV1);
delegate_noop!(State: ignore ZwpLockedPointerV1);
// The theme's cursor surface.
delegate_noop!(State: ignore WlSurface);

impl Dispatch<ZwpRelativePointerV1, ()> for State {
    fn event(state: &mut Self, _: &ZwpRelativePointerV1, event: zwp_relative_pointer_v1::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let zwp_relative_pointer_v1::Event::RelativeMotion { dx_unaccel, dy_unaccel, .. } = event
            && let Some(window) = state.seat.pointer_focus
        {
            state.input.push(InputEvent::Delta { window, dx: dx_unaccel as f32, dy: dy_unaccel as f32 });
        }
    }
}

/// Input, in the order the compositor sent it, for the loop to apply.
#[derive(Clone, Copy, Debug)]
pub(crate) enum InputEvent {
    /// Keyboard focus arrived; `held` are the modifiers already down.
    KeyboardEnter { window: Entity, held: [Option<KeyCode>; 8] },
    KeyboardLeave { window: Entity },
    Key { window: Entity, code: KeyCode, pressed: bool, repeat: bool, key: Key, text: Option<TextInput> },
    PointerEnter { window: Entity, x: f32, y: f32 },
    PointerLeave { window: Entity },
    Motion { window: Entity, x: f32, y: f32 },
    Button { window: Entity, button: MouseButton, pressed: bool },
    Scroll { window: Entity, dx: f32, dy: f32, lines: Option<(f32, f32)> },
    /// Unaccelerated motion, while the pointer is locked.
    Delta { window: Entity, dx: f32, dy: f32 },
}

/// Scroll gathered over a pointer frame: pixels, and wheel detents.
type PendingScroll = (f32, f32, Option<(f32, f32)>);

/// The seat: one, as in sk_app.
#[derive(Default)]
pub(crate) struct Seat {
    pub seat: Option<WlSeat>,
    pub pointer: Option<WlPointer>,
    keyboard: Option<WlKeyboard>,
    keymap: Option<Keymap>,
    /// The keymap being compiled off the loop's thread: compiling one, with
    /// the locale's compose table, takes milliseconds, and it arrives while
    /// the first frame is on its way.
    compiling: Option<std::thread::JoinHandle<Option<Keymap>>>,
    pub repeat: Repeat,
    pub keyboard_focus: Option<Entity>,
    pub pointer_focus: Option<Entity>,
    /// The serial of the last pointer enter (cursor requests quote it).
    pub pointer_serial: u32,
    /// The latest input event's serial: setting the selection quotes it.
    pub input_serial: u32,
    /// The compositor draws the cursor shape we name, if it can.
    pub cursor_device: Option<WpCursorShapeDeviceV1>,
    /// The cursor last set; reset by each pointer enter.
    pub cursor: Option<CursorIcon>,
    /// Raw motion, while some window holds the pointer locked.
    pub relative: Option<ZwpRelativePointerV1>,
    /// Scroll gathered until `wl_pointer.frame`.
    scroll: Option<PendingScroll>,
}

impl Seat {
    /// The compiled keymap, waiting for the compile if it is still running.
    pub fn keymap(&mut self) -> Option<&mut Keymap> {
        if let Some(compiling) = self.compiling.take()
            && let Ok(Some(keymap)) = compiling.join()
        {
            self.keymap = Some(keymap);
        }
        self.keymap.as_mut()
    }
    /// A key event for `code`, with what it means and types now.
    fn key(&mut self, window: Entity, code: u32, pressed: bool, repeat: bool) -> InputEvent {
        let (key, text) = match self.keymap() {
            Some(keymap) => {
                let key = keymap.key(code);
                let text = (pressed && keymap.typed(code) == Typed::Text).then(|| TextInput::new(window, keymap.text())).flatten();
                (key, text)
            }
            None => (Key::Unidentified, None),
        };
        InputEvent::Key { window, code: KeyCode(code as u16), pressed, repeat, key, text }
    }
}

impl State {
    /// A held key's repeat, if one is due at `now_ns`.
    pub fn pump_repeat(&mut self, now_ns: u64) {
        if let Some(code) = self.seat.repeat.due(now_ns)
            && let Some(window) = self.seat.keyboard_focus
        {
            let event = self.seat.key(window, code, true, true);
            self.input.push(event);
        }
    }
    fn flush_scroll(&mut self) {
        if let (Some((dx, dy, lines)), Some(window)) = (self.seat.scroll.take(), self.seat.pointer_focus) {
            self.input.push(InputEvent::Scroll { window, dx, dy, lines });
        }
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event else { return };
        let s = &mut state.seat;
        match (caps.contains(wl_seat::Capability::Pointer), s.pointer.take()) {
            (true, None) => s.pointer = Some(seat.get_pointer(qh, ())),
            (true, kept) => s.pointer = kept,
            (false, Some(pointer)) if pointer.version() >= 3 => pointer.release(),
            (false, _) => {}
        }
        match (caps.contains(wl_seat::Capability::Keyboard), s.keyboard.take()) {
            (true, None) => s.keyboard = Some(seat.get_keyboard(qh, ())),
            (true, kept) => s.keyboard = kept,
            (false, Some(keyboard)) if keyboard.version() >= 3 => keyboard.release(),
            (false, _) => {}
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Keymap { format: WEnum::Value(wl_keyboard::KeymapFormat::XkbV1), fd, size } => {
                // SAFETY: a private read-only mapping of the keymap file the
                // compositor sent, unmapped below.
                let map = unsafe {
                    rustix::mm::mmap(std::ptr::null_mut(), size as usize, rustix::mm::ProtFlags::READ, rustix::mm::MapFlags::PRIVATE, &fd, 0)
                };
                if let Ok(map) = map {
                    let bytes = unsafe { std::slice::from_raw_parts(map as *const u8, size as usize) }.to_vec();
                    unsafe {
                        let _ = rustix::mm::munmap(map, size as usize);
                    }
                    state.seat.keymap = None;
                    state.seat.compiling = Some(std::thread::spawn(move || Keymap::from_bytes(&bytes)));
                }
            }
            wl_keyboard::Event::Enter { serial, surface, keys } => {
                state.seat.input_serial = serial;
                let Some(&window) = surface.data::<Entity>() else { return };
                state.seat.keyboard_focus = Some(window);
                let mut held = [None; 8];
                let modifiers = keys.as_chunks::<4>().0.iter().map(|&b| KeyCode(u32::from_ne_bytes(b) as u16)).filter(|c| c.is_modifier());
                for (slot, code) in held.iter_mut().zip(modifiers) {
                    *slot = Some(code);
                }
                state.input.push(InputEvent::KeyboardEnter { window, held });
            }
            wl_keyboard::Event::Leave { surface, .. } => {
                let Some(&window) = surface.data::<Entity>() else { return };
                state.seat.keyboard_focus = None;
                state.seat.repeat.clear();
                if let Some(keymap) = state.seat.keymap() {
                    keymap.reset_compose();
                }
                state.input.push(InputEvent::KeyboardLeave { window });
            }
            wl_keyboard::Event::Key { serial, key, state: WEnum::Value(key_state), .. } => {
                state.seat.input_serial = serial;
                let Some(window) = state.seat.keyboard_focus else { return };
                let pressed = key_state == wl_keyboard::KeyState::Pressed;
                if pressed {
                    let repeats = state.seat.keymap().is_some_and(|k| k.repeats(key)) && !KeyCode(key as u16).is_modifier();
                    state.seat.repeat.press(key, repeats, crate::run::now_ns());
                } else {
                    state.seat.repeat.release(key);
                }
                let event = state.seat.key(window, key, pressed, false);
                state.input.push(event);
            }
            wl_keyboard::Event::Modifiers { mods_depressed, mods_latched, mods_locked, group, .. } => {
                if let Some(keymap) = state.seat.keymap() {
                    keymap.set_modifiers(mods_depressed, mods_latched, mods_locked, group);
                }
            }
            wl_keyboard::Event::RepeatInfo { rate, delay } => state.seat.repeat.set_info(rate, delay),
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, pointer: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { serial, surface, surface_x, surface_y } => {
                let Some(&window) = surface.data::<Entity>() else { return };
                state.seat.pointer_focus = Some(window);
                state.seat.pointer_serial = serial;
                state.seat.input_serial = serial;
                // The compositor hands cursor duty back on every enter.
                state.seat.cursor = None;
                state.input.push(InputEvent::PointerEnter { window, x: surface_x as f32, y: surface_y as f32 });
            }
            wl_pointer::Event::Leave { surface, .. } => {
                let Some(&window) = surface.data::<Entity>() else { return };
                state.flush_scroll();
                state.seat.pointer_focus = None;
                state.input.push(InputEvent::PointerLeave { window });
            }
            wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                if let Some(window) = state.seat.pointer_focus {
                    state.input.push(InputEvent::Motion { window, x: surface_x as f32, y: surface_y as f32 });
                }
            }
            wl_pointer::Event::Button { serial, button, state: WEnum::Value(button_state), .. } => {
                state.seat.input_serial = serial;
                if let Some(window) = state.seat.pointer_focus {
                    let pressed = button_state == wl_pointer::ButtonState::Pressed;
                    state.input.push(InputEvent::Button { window, button: MouseButton::from_evdev(button), pressed });
                }
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(axis), value, .. } => {
                let scroll = state.seat.scroll.get_or_insert((0.0, 0.0, None));
                match axis {
                    wl_pointer::Axis::VerticalScroll => scroll.1 += value as f32,
                    _ => scroll.0 += value as f32,
                }
                // Before version 5 there are no frames to wait for.
                if pointer.version() < 5 {
                    state.flush_scroll();
                }
            }
            wl_pointer::Event::AxisValue120 { axis: WEnum::Value(axis), value120 } => {
                let scroll = state.seat.scroll.get_or_insert((0.0, 0.0, None));
                let lines = scroll.2.get_or_insert((0.0, 0.0));
                match axis {
                    wl_pointer::Axis::VerticalScroll => lines.1 += value120 as f32 / 120.0,
                    _ => lines.0 += value120 as f32 / 120.0,
                }
            }
            wl_pointer::Event::Frame => state.flush_scroll(),
            _ => {}
        }
    }
}
