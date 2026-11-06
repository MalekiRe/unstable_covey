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
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_decoration_manager_v1::ZxdgDecorationManagerV1;
use wayland_protocols::xdg::decoration::zv1::client::zxdg_toplevel_decoration_v1::{self, ZxdgToplevelDecorationV1};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

use crate::scale::Scale;
use crate::framebuffer::BufferId;
use crate::window::WindowState;
use wayland_client::protocol::wl_callback::{self, WlCallback};

/// Globals bound once at connect.
pub(crate) struct Globals {
    pub compositor: WlCompositor,
    pub shm: WlShm,
    pub wm: XdgWmBase,
    pub decoration: Option<ZxdgDecorationManagerV1>,
    pub viewporter: Option<WpViewporter>,
    pub fractional: Option<WpFractionalScaleManagerV1>,
}

/// An output, for the integer-scale fallback and its refresh rate.
pub(crate) struct Output {
    pub output: WlOutput,
    pub name: u32,
    pub scale: i32,
    pub refresh_mhz: i32,
}

/// A window's protocol objects and what the compositor last said about it.
pub(crate) struct Proto {
    pub entity: Entity,
    pub surface: WlSurface,
    pub xdg: XdgSurface,
    pub toplevel: XdgToplevel,
    pub decoration: Option<ZxdgToplevelDecorationV1>,
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
    /// A frame callback is outstanding.
    pub frame_requested: bool,
    /// A frame callback fired since the loop last looked.
    pub frame_done: bool,
    /// The framebuffer's buffer drawn before the first configure, committed
    /// once it arrives (if the size held).
    pub predrawn: Option<usize>,
    /// A frame is due but both buffers are with the compositor.
    pub waiting: bool,
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
}

impl Drop for Proto {
    fn drop(&mut self) {
        // Role objects before their surface; a decoration before its
        // toplevel (the reverse is a protocol error).
        if let Some(decoration) = self.decoration.take() {
            decoration.destroy();
        }
        if let Some(fractional) = self.fractional.take() {
            fractional.destroy();
        }
        if let Some(viewport) = self.viewport.take() {
            viewport.destroy();
        }
        self.toplevel.destroy();
        self.xdg.destroy();
        self.surface.destroy();
        if let Some((buffer, size)) = self.placeholder.take()
            && size != (1, 1)
        {
            buffer.destroy();
        }
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
        if let wl_callback::Event::Done { .. } = event
            && let Some(window) = state.window(entity)
        {
            window.frame_requested = false;
            window.frame_done = true;
        }
    }
}

impl Dispatch<WlBuffer, ()> for State {
    fn event(_: &mut Self, _: &WlBuffer, _: wl_buffer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {}
}

delegate_noop!(State: WlCompositor);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: WpViewporter);
delegate_noop!(State: WpViewport);
delegate_noop!(State: WpFractionalScaleManagerV1);
delegate_noop!(State: ZxdgDecorationManagerV1);
