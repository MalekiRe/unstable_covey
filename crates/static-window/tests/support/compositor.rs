//! A scripted Wayland compositor for tests, on the pure-Rust `wayland-server`.
//!
//! It runs on its own thread, so the client may block in roundtrips. Tests
//! talk to it through [`Compositor::with`], which first reads every request
//! the client has already flushed, then runs the closure on the server's
//! state and flushes what it sent. Since the client flushes before its step
//! returns, nothing depends on timing.
#![allow(dead_code)]

use std::os::fd::{AsFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::thread::JoinHandle;

use rustix::event::{PollFd, PollFlags};
use wayland_protocols::wp::fractional_scale::v1::server::{wp_fractional_scale_manager_v1, wp_fractional_scale_v1};
use wayland_protocols::wp::cursor_shape::v1::server::{wp_cursor_shape_device_v1, wp_cursor_shape_manager_v1};
use wayland_protocols::wp::pointer_constraints::zv1::server::{zwp_locked_pointer_v1, zwp_pointer_constraints_v1};
use wayland_protocols::wp::presentation_time::server::{wp_presentation, wp_presentation_feedback};
use wayland_protocols::wp::relative_pointer::zv1::server::{zwp_relative_pointer_manager_v1, zwp_relative_pointer_v1};
use wayland_protocols::wp::viewporter::server::{wp_viewport, wp_viewporter};
use wayland_protocols::xdg::decoration::zv1::server::{zxdg_decoration_manager_v1, zxdg_toplevel_decoration_v1};
use wayland_protocols::xdg::shell::server::{xdg_surface, xdg_toplevel, xdg_wm_base};
use wayland_server::backend::ClientId;
use wayland_server::protocol::{
    wl_buffer, wl_callback, wl_compositor, wl_data_device, wl_data_device_manager, wl_data_offer, wl_data_source, wl_keyboard, wl_output,
    wl_pointer, wl_region, wl_seat, wl_shm, wl_shm_pool, wl_surface,
};
use wayland_server::{Client, DataInit, Dispatch, Display, DisplayHandle, GlobalDispatch, New, Resource, WEnum};

/// Which optional globals to offer.
#[derive(Clone, Copy, Debug)]
pub struct Globals {
    pub decoration: bool,
    pub viewporter: bool,
    pub fractional_scale: bool,
    pub presentation: bool,
    pub cursor_shape: bool,
    /// The keymap sent to the client's keyboard.
    pub keymap: &'static [u8],
}
impl Default for Globals {
    fn default() -> Self {
        Self {
            decoration: true,
            viewporter: true,
            fractional_scale: true,
            presentation: true,
            cursor_shape: true,
            keymap: include_bytes!("us.xkb"),
        }
    }
}

/// Runs on the server; returns the reply, sent after the server flushed what
/// the command made it send.
type Command = Box<dyn FnOnce(&mut Server, &DisplayHandle) -> Box<dyn FnOnce() + Send> + Send>;

/// The handle tests hold; dropping it stops the server.
pub struct Compositor {
    commands: Option<Sender<Command>>,
    wake: OwnedFd,
    thread: Option<JoinHandle<()>>,
}

impl Compositor {
    /// Start a compositor; returns it and the client's end of the socket.
    pub fn spawn(globals: Globals) -> (Self, UnixStream) {
        let (server_end, client_end) = UnixStream::pair().expect("socketpair");
        let (wake_read, wake) = rustix::pipe::pipe().expect("pipe");
        let (commands, received) = channel::<Command>();
        let thread = std::thread::spawn(move || serve(server_end, globals, wake_read, received));
        (Self { commands: Some(commands), wake, thread: Some(thread) }, client_end)
    }

    /// Run `f` on the server once it has read everything the client sent;
    /// whatever `f` sends is flushed before this returns.
    pub fn with<R: Send + 'static>(&self, f: impl FnOnce(&mut Server, &DisplayHandle) -> R + Send + 'static) -> R {
        let (reply, result) = channel();
        let command: Command = Box::new(move |server, handle| {
            let value = f(server, handle);
            Box::new(move || {
                let _ = reply.send(value);
            })
        });
        self.commands.as_ref().unwrap().send(command).expect("server stopped");
        rustix::io::write(&self.wake, &[1]).expect("wake server");
        result.recv().expect("server panicked")
    }
}

impl Drop for Compositor {
    fn drop(&mut self) {
        self.commands = None;
        let _ = rustix::io::write(&self.wake, &[0]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn serve(stream: UnixStream, globals: Globals, wake: OwnedFd, commands: Receiver<Command>) {
    let mut display = Display::<Server>::new().expect("display");
    let mut handle = display.handle();
    handle.create_global::<Server, wl_compositor::WlCompositor, ()>(6, ());
    handle.create_global::<Server, wl_shm::WlShm, ()>(1, ());
    handle.create_global::<Server, xdg_wm_base::XdgWmBase, ()>(6, ());
    handle.create_global::<Server, wl_output::WlOutput, ()>(4, ());
    if globals.decoration {
        handle.create_global::<Server, zxdg_decoration_manager_v1::ZxdgDecorationManagerV1, ()>(1, ());
    }
    if globals.viewporter {
        handle.create_global::<Server, wp_viewporter::WpViewporter, ()>(1, ());
    }
    if globals.fractional_scale {
        handle.create_global::<Server, wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()>(1, ());
    }
    handle.create_global::<Server, wl_seat::WlSeat, ()>(8, ());
    if globals.presentation {
        handle.create_global::<Server, wp_presentation::WpPresentation, ()>(1, ());
    }
    if globals.cursor_shape {
        handle.create_global::<Server, wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, ()>(1, ());
    }
    handle.create_global::<Server, zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, ()>(1, ());
    handle.create_global::<Server, zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, ()>(1, ());
    handle.create_global::<Server, wl_data_device_manager::WlDataDeviceManager, ()>(3, ());
    let client = handle.insert_client(stream, Arc::new(())).expect("client");
    let mut server = Server { client: Some(client.id()), keymap: globals.keymap, ..Server::default() };
    loop {
        let mut fds = [PollFd::new(&display, PollFlags::IN), PollFd::new(&wake, PollFlags::IN)];
        match rustix::event::poll(&mut fds, None) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(e) => panic!("poll: {e}"),
        }
        let woken = fds[1].revents().intersects(PollFlags::IN | PollFlags::HUP);
        let _ = display.dispatch_clients(&mut server);
        let _ = display.flush_clients();
        if woken {
            let mut byte = [0u8; 64];
            let _ = rustix::io::read(&wake, &mut byte);
            loop {
                match commands.try_recv() {
                    Ok(command) => {
                        let _ = display.dispatch_clients(&mut server);
                        let reply = command(&mut server, &handle);
                        let _ = display.flush_clients();
                        reply();
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => break,
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => return,
                }
            }
        }
    }
}

/// A rectangle in buffer pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Damage {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// What a commit applied.
#[derive(Clone, Debug, Default)]
pub struct Commit {
    /// Index into `Server::buffers`, if a buffer was attached this commit.
    pub buffer: Option<usize>,
    pub damage: Vec<Damage>,
    pub buffer_scale: i32,
    pub frame_callbacks: usize,
    /// The commit carried a viewport destination or a buffer scale.
    pub geometry: bool,
}

#[derive(Default)]
struct Pending {
    buffer: Option<Option<usize>>,
    damage: Vec<Damage>,
    buffer_scale: Option<i32>,
    callbacks: Vec<wl_callback::WlCallback>,
    viewport_destination: Option<(i32, i32)>,
}

pub struct SurfaceRecord {
    pub resource: wl_surface::WlSurface,
    pending: Pending,
    pub commits: Vec<Commit>,
    /// The attached buffer, after the last commit.
    pub buffer: Option<usize>,
    pub buffer_scale: i32,
    pub viewport_destination: Option<(i32, i32)>,
    pub has_viewport: bool,
    pub fractional_scale: Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
    /// Presentation feedback requested, oldest first.
    pub feedbacks: Vec<wp_presentation_feedback::WpPresentationFeedback>,
    /// Frame callbacks committed and not yet fired.
    pub callbacks: Vec<wl_callback::WlCallback>,
    pub destroyed: bool,
}

/// A toplevel request with no other state to keep.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ask {
    SetMaximized,
    UnsetMaximized,
    SetFullscreen,
    UnsetFullscreen,
    SetMinimized,
}

pub struct ToplevelRecord {
    pub surface: usize,
    pub xdg_surface: xdg_surface::XdgSurface,
    pub toplevel: Option<xdg_toplevel::XdgToplevel>,
    pub title: String,
    pub app_id: String,
    pub min_size: (i32, i32),
    pub max_size: (i32, i32),
    pub asks: Vec<Ask>,
    /// Serials acknowledged.
    pub acked: Vec<u32>,
    pub decoration: Option<zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1>,
    /// Decoration modes the client asked for (1 client side, 2 server side).
    pub decoration_modes: Vec<u32>,
    pub destroyed: bool,
}

pub struct BufferRecord {
    pool: Arc<OwnedFd>,
    pub resource: wl_buffer::WlBuffer,
    pub offset: i32,
    pub width: i32,
    pub height: i32,
    pub stride: i32,
    pub destroyed: bool,
}

#[derive(Default)]
pub struct Server {
    client: Option<ClientId>,
    pub surfaces: Vec<SurfaceRecord>,
    pub toplevels: Vec<ToplevelRecord>,
    pub buffers: Vec<BufferRecord>,
    pub outputs: Vec<wl_output::WlOutput>,
    pub pointer: Option<wl_pointer::WlPointer>,
    pub keyboard: Option<wl_keyboard::WlKeyboard>,
    /// Shapes set through cursor-shape-v1.
    pub shapes: Vec<wp_cursor_shape_device_v1::Shape>,
    /// `wl_pointer.set_cursor` calls: whether a surface was given.
    pub set_cursor: Vec<bool>,
    /// Pointer locks: the locked surface's index, and whether it was let go.
    pub locks: Vec<(usize, bool)>,
    pub relative: Option<zwp_relative_pointer_v1::ZwpRelativePointerV1>,
    pub data_device: Option<wl_data_device::WlDataDevice>,
    /// The client's selection: the source, its types, and the serial quoted.
    pub selection: Option<(wl_data_source::WlDataSource, Vec<String>, u32)>,
    keymap: &'static [u8],
    serial: u32,
    time: u32,
}

impl Server {
    fn next_serial(&mut self) -> u32 {
        self.serial += 1;
        self.serial
    }
    /// The toplevel of the `index`th window the client made.
    pub fn toplevel(&self, index: usize) -> &ToplevelRecord {
        &self.toplevels[index]
    }
    pub fn surface_of(&self, toplevel: usize) -> &SurfaceRecord {
        &self.surfaces[self.toplevels[toplevel].surface]
    }
    /// Send a toplevel configure and its surface configure; returns the
    /// serial. A 0 size leaves the size to the client.
    pub fn configure(&mut self, toplevel: usize, width: i32, height: i32, states: &[xdg_toplevel::State]) -> u32 {
        let serial = self.next_serial();
        let record = &self.toplevels[toplevel];
        let bytes: Vec<u8> = states.iter().flat_map(|&s| (s as u32).to_ne_bytes()).collect();
        record.toplevel.as_ref().expect("no toplevel role").configure(width, height, bytes);
        record.xdg_surface.configure(serial);
        serial
    }
    pub fn close(&mut self, toplevel: usize) {
        self.toplevels[toplevel].toplevel.as_ref().unwrap().close();
    }
    /// Answer a decoration request with `mode` (1 client side, 2 server side).
    pub fn decoration_mode(&mut self, toplevel: usize, mode: u32) {
        let mode = zxdg_toplevel_decoration_v1::Mode::try_from(mode).unwrap();
        self.toplevels[toplevel].decoration.as_ref().expect("no decoration object").configure(mode);
    }
    /// Report a fractional scale (in 120ths) for the window's surface.
    pub fn preferred_scale(&mut self, toplevel: usize, scale_120: u32) {
        let surface = &self.surfaces[self.toplevels[toplevel].surface];
        surface.fractional_scale.as_ref().expect("no fractional scale object").preferred_scale(scale_120);
    }
    /// Change output `index`'s integer scale.
    pub fn set_output_scale(&mut self, index: usize, factor: i32) {
        self.outputs[index].scale(factor);
        self.outputs[index].done();
    }
    /// Put the window's surface on output `index`.
    pub fn enter(&mut self, toplevel: usize, index: usize) {
        let surface = &self.surfaces[self.toplevels[toplevel].surface];
        surface.resource.enter(&self.outputs[index]);
    }
    fn tick(&mut self) -> (u32, u32) {
        self.time += 1;
        (self.next_serial(), self.time)
    }
    /// Give the window keyboard focus, with `keys` (evdev) already down.
    pub fn keyboard_enter(&mut self, toplevel: usize, keys: &[u32]) {
        let serial = self.next_serial();
        let surface = &self.surfaces[self.toplevels[toplevel].surface].resource;
        let keys = keys.iter().flat_map(|k| k.to_ne_bytes()).collect();
        self.keyboard.as_ref().expect("no keyboard").enter(serial, surface, keys);
    }
    pub fn keyboard_leave(&mut self, toplevel: usize) {
        let serial = self.next_serial();
        let surface = &self.surfaces[self.toplevels[toplevel].surface].resource;
        self.keyboard.as_ref().unwrap().leave(serial, surface);
    }
    pub fn key(&mut self, code: u32, pressed: bool) {
        let (serial, time) = self.tick();
        let state = if pressed { wl_keyboard::KeyState::Pressed } else { wl_keyboard::KeyState::Released };
        self.keyboard.as_ref().unwrap().key(serial, time, code, state);
    }
    pub fn modifiers(&mut self, depressed: u32, latched: u32, locked: u32) {
        let serial = self.next_serial();
        self.keyboard.as_ref().unwrap().modifiers(serial, depressed, latched, locked, 0);
    }
    pub fn repeat_info(&mut self, rate: i32, delay: i32) {
        self.keyboard.as_ref().unwrap().repeat_info(rate, delay);
    }
    pub fn pointer_enter(&mut self, toplevel: usize, x: f64, y: f64) {
        let serial = self.next_serial();
        let surface = &self.surfaces[self.toplevels[toplevel].surface].resource;
        let pointer = self.pointer.as_ref().expect("no pointer");
        pointer.enter(serial, surface, x, y);
        pointer.frame();
    }
    pub fn pointer_leave(&mut self, toplevel: usize) {
        let serial = self.next_serial();
        let surface = &self.surfaces[self.toplevels[toplevel].surface].resource;
        let pointer = self.pointer.as_ref().unwrap();
        pointer.leave(serial, surface);
        pointer.frame();
    }
    pub fn motion(&mut self, x: f64, y: f64) {
        let (_, time) = self.tick();
        let pointer = self.pointer.as_ref().unwrap();
        pointer.motion(time, x, y);
        pointer.frame();
    }
    /// `code` is evdev (`BTN_LEFT` is 0x110).
    pub fn button(&mut self, code: u32, pressed: bool) {
        let (serial, time) = self.tick();
        let state = if pressed { wl_pointer::ButtonState::Pressed } else { wl_pointer::ButtonState::Released };
        let pointer = self.pointer.as_ref().unwrap();
        pointer.button(serial, time, code, state);
        pointer.frame();
    }
    /// A wheel turn (`value120` 120 per notch) moving `dy`, as one frame.
    pub fn wheel(&mut self, dy: f64, value120: i32) {
        let (_, time) = self.tick();
        let pointer = self.pointer.as_ref().unwrap();
        pointer.axis_source(wl_pointer::AxisSource::Wheel);
        pointer.axis_value120(wl_pointer::Axis::VerticalScroll, value120);
        pointer.axis(time, wl_pointer::Axis::VerticalScroll, dy);
        pointer.frame();
    }
    /// Report the window's oldest pending commit as shown at `display_ns`
    /// (CLOCK_MONOTONIC) on an output refreshing every `refresh_ns`.
    pub fn present(&mut self, toplevel: usize, display_ns: u64, refresh_ns: u32) {
        let surface = self.toplevels[toplevel].surface;
        let feedback = self.surfaces[surface].feedbacks.remove(0);
        let secs = display_ns / 1_000_000_000;
        feedback.presented((secs >> 32) as u32, secs as u32, (display_ns % 1_000_000_000) as u32, refresh_ns, 0, 0, wp_presentation_feedback::Kind::Vsync);
    }
    /// Raw pointer motion, for a locked pointer.
    pub fn relative_motion(&mut self, dx: f64, dy: f64) {
        let relative = self.relative.as_ref().expect("no relative pointer");
        relative.relative_motion(0, 0, dx * 2.0, dy * 2.0, dx, dy);
        self.pointer.as_ref().unwrap().frame();
    }
    /// Paste the client's selection as `mime`: the pipe to read it from
    /// once the client has handled the request.
    pub fn paste(&mut self, mime: &str) -> OwnedFd {
        let (read, write) = rustix::pipe::pipe().unwrap();
        self.selection.as_ref().expect("no selection").0.send(mime.into(), write.as_fd());
        read
    }
    /// Another client puts `text` on the clipboard.
    pub fn offer_text(&mut self, handle: &DisplayHandle, text: &str) {
        let device = self.data_device.as_ref().expect("no data device");
        let client = handle.get_client(device.id()).unwrap();
        let offer = client.create_resource::<wl_data_offer::WlDataOffer, Arc<str>, Server>(handle, device.version(), text.into()).unwrap();
        device.data_offer(&offer);
        offer.offer("text/plain".into());
        offer.offer("text/plain;charset=utf-8".into());
        device.selection(Some(&offer));
        // Our client's own source, if any, is no longer the selection.
        if let Some((source, _, _)) = self.selection.take() {
            source.cancelled();
        }
    }
    /// Fire every committed frame callback with time `ms`.
    pub fn fire_frames(&mut self, toplevel: usize, ms: u32) -> usize {
        let surface = self.toplevels[toplevel].surface;
        let callbacks = std::mem::take(&mut self.surfaces[surface].callbacks);
        for callback in &callbacks {
            callback.done(ms);
        }
        callbacks.len()
    }
    /// Give a buffer back to the client.
    pub fn release(&mut self, buffer: usize) {
        self.buffers[buffer].resource.release();
    }
    /// The pixels of a buffer, row-major `width * height` (stride removed).
    pub fn pixels(&self, buffer: usize) -> Vec<u32> {
        let b = &self.buffers[buffer];
        let len = (b.offset + b.stride * b.height) as usize;
        // SAFETY: a private read-only mapping of the client's pool, dropped
        // before returning.
        let map = unsafe {
            rustix::mm::mmap(std::ptr::null_mut(), len, rustix::mm::ProtFlags::READ, rustix::mm::MapFlags::SHARED, b.pool.as_fd(), 0)
        }
        .expect("mmap pool");
        let bytes = unsafe { std::slice::from_raw_parts(map as *const u8, len) };
        let mut out = Vec::with_capacity((b.width * b.height) as usize);
        for row in 0..b.height {
            let start = (b.offset + row * b.stride) as usize;
            out.extend(bytes[start..start + b.width as usize * 4].as_chunks::<4>().0.iter().map(|&p| u32::from_ne_bytes(p)));
        }
        unsafe {
            let _ = rustix::mm::munmap(map, len);
        }
        out
    }
}

macro_rules! global {
    ($($iface:ty),*) => {$(
        impl GlobalDispatch<$iface, ()> for Server {
            fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<$iface>, _: &(), init: &mut DataInit<'_, Self>) {
                init.init(resource, ());
            }
        }
    )*};
}
global!(
    wl_compositor::WlCompositor,
    wl_shm::WlShm,
    zxdg_decoration_manager_v1::ZxdgDecorationManagerV1,
    wp_viewporter::WpViewporter,
    wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1
);

impl GlobalDispatch<xdg_wm_base::XdgWmBase, ()> for Server {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<xdg_wm_base::XdgWmBase>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}

impl GlobalDispatch<wp_presentation::WpPresentation, ()> for Server {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<wp_presentation::WpPresentation>, _: &(), init: &mut DataInit<'_, Self>) {
        // CLOCK_MONOTONIC, which the client reads too.
        init.init(resource, ()).clock_id(1);
    }
}

impl Dispatch<wp_presentation::WpPresentation, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wp_presentation::WpPresentation, request: wp_presentation::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wp_presentation::Request::Feedback { surface, callback } = request {
            let index = *surface.data::<usize>().unwrap();
            let feedback = init.init(callback, ());
            server.surfaces[index].feedbacks.push(feedback);
        }
    }
}

impl Dispatch<wp_presentation_feedback::WpPresentationFeedback, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wp_presentation_feedback::WpPresentationFeedback, _: wp_presentation_feedback::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl GlobalDispatch<wl_seat::WlSeat, ()> for Server {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<wl_seat::WlSeat>, _: &(), init: &mut DataInit<'_, Self>) {
        let seat = init.init(resource, ());
        seat.capabilities(wl_seat::Capability::Pointer | wl_seat::Capability::Keyboard);
        seat.name("seat0".into());
    }
}

impl Dispatch<wl_seat::WlSeat, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wl_seat::WlSeat, request: wl_seat::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_seat::Request::GetPointer { id } => server.pointer = Some(init.init(id, ())),
            wl_seat::Request::GetKeyboard { id } => {
                let keyboard = init.init(id, ());
                // The keymap travels as a file: a memfd holding it and a NUL.
                let fd = rustix::fs::memfd_create("keymap", rustix::fs::MemfdFlags::CLOEXEC).unwrap();
                rustix::io::write(&fd, server.keymap).unwrap();
                rustix::io::write(&fd, &[0]).unwrap();
                keyboard.keymap(wl_keyboard::KeymapFormat::XkbV1, fd.as_fd(), server.keymap.len() as u32 + 1);
                keyboard.repeat_info(25, 600);
                server.keyboard = Some(keyboard);
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_pointer::WlPointer, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wl_pointer::WlPointer, request: wl_pointer::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let wl_pointer::Request::SetCursor { surface, .. } = request {
            server.set_cursor.push(surface.is_some());
        }
    }
}

impl GlobalDispatch<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, ()> for Server {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}
impl Dispatch<wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wp_cursor_shape_manager_v1::WpCursorShapeManagerV1, request: wp_cursor_shape_manager_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wp_cursor_shape_manager_v1::Request::GetPointer { cursor_shape_device, .. } = request {
            init.init(cursor_shape_device, ());
        }
    }
}
impl Dispatch<wp_cursor_shape_device_v1::WpCursorShapeDeviceV1, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wp_cursor_shape_device_v1::WpCursorShapeDeviceV1, request: wp_cursor_shape_device_v1::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let wp_cursor_shape_device_v1::Request::SetShape { shape: WEnum::Value(shape), .. } = request {
            server.shapes.push(shape);
        }
    }
}

impl GlobalDispatch<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, ()> for Server {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}
impl Dispatch<zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &zwp_pointer_constraints_v1::ZwpPointerConstraintsV1, request: zwp_pointer_constraints_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let zwp_pointer_constraints_v1::Request::LockPointer { id, surface, .. } = request {
            let index = server.locks.len();
            server.locks.push((*surface.data::<usize>().unwrap(), false));
            init.init(id, index).locked();
        }
    }
}
impl Dispatch<zwp_locked_pointer_v1::ZwpLockedPointerV1, usize> for Server {
    fn request(server: &mut Self, _: &Client, _: &zwp_locked_pointer_v1::ZwpLockedPointerV1, request: zwp_locked_pointer_v1::Request, &index: &usize, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let zwp_locked_pointer_v1::Request::Destroy = request {
            server.locks[index].1 = true;
        }
    }
}

impl GlobalDispatch<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, ()> for Server {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}
impl Dispatch<zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &zwp_relative_pointer_manager_v1::ZwpRelativePointerManagerV1, request: zwp_relative_pointer_manager_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let zwp_relative_pointer_manager_v1::Request::GetRelativePointer { id, .. } = request {
            server.relative = Some(init.init(id, ()));
        }
    }
}
impl Dispatch<zwp_relative_pointer_v1::ZwpRelativePointerV1, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &zwp_relative_pointer_v1::ZwpRelativePointerV1, request: zwp_relative_pointer_v1::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let zwp_relative_pointer_v1::Request::Destroy = request {
            server.relative = None;
        }
    }
}

impl GlobalDispatch<wl_data_device_manager::WlDataDeviceManager, ()> for Server {
    fn bind(_: &mut Self, _: &DisplayHandle, _: &Client, resource: New<wl_data_device_manager::WlDataDeviceManager>, _: &(), init: &mut DataInit<'_, Self>) {
        init.init(resource, ());
    }
}
impl Dispatch<wl_data_device_manager::WlDataDeviceManager, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wl_data_device_manager::WlDataDeviceManager, request: wl_data_device_manager::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_data_device_manager::Request::CreateDataSource { id } => {
                init.init(id, std::sync::Mutex::new(Vec::<String>::new()));
            }
            wl_data_device_manager::Request::GetDataDevice { id, .. } => server.data_device = Some(init.init(id, ())),
            _ => {}
        }
    }
}
impl Dispatch<wl_data_device::WlDataDevice, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wl_data_device::WlDataDevice, request: wl_data_device::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let wl_data_device::Request::SetSelection { source, serial } = request {
            server.selection = source.map(|source| {
                let mimes = source.data::<std::sync::Mutex<Vec<String>>>().unwrap().lock().unwrap().clone();
                (source, mimes, serial)
            });
        }
    }
}
impl Dispatch<wl_data_source::WlDataSource, std::sync::Mutex<Vec<String>>> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_data_source::WlDataSource, request: wl_data_source::Request, mimes: &std::sync::Mutex<Vec<String>>, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let wl_data_source::Request::Offer { mime_type } = request {
            mimes.lock().unwrap().push(mime_type);
        }
    }
}
impl Dispatch<wl_data_offer::WlDataOffer, Arc<str>> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_data_offer::WlDataOffer, request: wl_data_offer::Request, text: &Arc<str>, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        // Another client's selection: write it, then close the pipe.
        if let wl_data_offer::Request::Receive { fd, .. } = request {
            rustix::io::write(&fd, text.as_bytes()).unwrap();
        }
    }
}

impl Dispatch<wl_keyboard::WlKeyboard, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_keyboard::WlKeyboard, _: wl_keyboard::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl GlobalDispatch<wl_output::WlOutput, ()> for Server {
    fn bind(server: &mut Self, _: &DisplayHandle, _: &Client, resource: New<wl_output::WlOutput>, _: &(), init: &mut DataInit<'_, Self>) {
        let output = init.init(resource, ());
        output.geometry(0, 0, 600, 340, wl_output::Subpixel::Unknown, "test".into(), "test".into(), wl_output::Transform::Normal);
        output.mode(wl_output::Mode::Current, 1920, 1080, 120_000);
        output.scale(1);
        output.done();
        server.outputs.push(output);
    }
}

impl Dispatch<wl_compositor::WlCompositor, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wl_compositor::WlCompositor, request: wl_compositor::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            wl_compositor::Request::CreateSurface { id } => {
                let index = server.surfaces.len();
                let resource = init.init(id, index);
                server.surfaces.push(SurfaceRecord {
                    resource,
                    pending: Pending::default(),
                    commits: Vec::new(),
                    buffer: None,
                    buffer_scale: 1,
                    viewport_destination: None,
                    has_viewport: false,
                    fractional_scale: None,
                    feedbacks: Vec::new(),
                    callbacks: Vec::new(),
                    destroyed: false,
                });
            }
            wl_compositor::Request::CreateRegion { id } => {
                init.init(id, ());
            }
            _ => {}
        }
    }
}

impl Dispatch<wl_region::WlRegion, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_region::WlRegion, _: wl_region::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl Dispatch<wl_surface::WlSurface, usize> for Server {
    fn request(server: &mut Self, _: &Client, _: &wl_surface::WlSurface, request: wl_surface::Request, &index: &usize, _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        let surface = &mut server.surfaces[index];
        match request {
            wl_surface::Request::Attach { buffer, .. } => {
                surface.pending.buffer = Some(buffer.map(|b| *b.data::<usize>().unwrap()));
            }
            wl_surface::Request::Damage { x, y, width, height } | wl_surface::Request::DamageBuffer { x, y, width, height } => {
                surface.pending.damage.push(Damage { x, y, width, height });
            }
            wl_surface::Request::Frame { callback } => {
                let callback = init.init(callback, ());
                surface.pending.callbacks.push(callback);
            }
            wl_surface::Request::SetBufferScale { scale } => surface.pending.buffer_scale = Some(scale),
            wl_surface::Request::Commit => {
                let pending = std::mem::take(&mut surface.pending);
                if let Some(buffer) = pending.buffer {
                    surface.buffer = buffer;
                }
                let geometry = pending.buffer_scale.is_some() || pending.viewport_destination.is_some();
                if let Some(scale) = pending.buffer_scale {
                    surface.buffer_scale = scale;
                }
                if pending.viewport_destination.is_some() {
                    surface.viewport_destination = pending.viewport_destination;
                }
                surface.commits.push(Commit {
                    buffer: pending.buffer.flatten(),
                    damage: pending.damage,
                    buffer_scale: surface.buffer_scale,
                    frame_callbacks: pending.callbacks.len(),
                    geometry,
                });
                surface.callbacks.extend(pending.callbacks);
            }
            wl_surface::Request::Destroy => surface.destroyed = true,
            _ => {}
        }
    }
}

impl Dispatch<wl_callback::WlCallback, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_callback::WlCallback, _: wl_callback::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl Dispatch<wl_shm::WlShm, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_shm::WlShm, request: wl_shm::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wl_shm::Request::CreatePool { id, fd, .. } = request {
            init.init(id, Arc::new(fd));
        }
    }
}

impl Dispatch<wl_shm_pool::WlShmPool, Arc<OwnedFd>> for Server {
    fn request(server: &mut Self, _: &Client, _: &wl_shm_pool::WlShmPool, request: wl_shm_pool::Request, pool: &Arc<OwnedFd>, _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wl_shm_pool::Request::CreateBuffer { id, offset, width, height, stride, .. } = request {
            let index = server.buffers.len();
            let resource = init.init(id, index);
            server.buffers.push(BufferRecord { pool: pool.clone(), resource, offset, width, height, stride, destroyed: false });
        }
    }
}

impl Dispatch<wl_buffer::WlBuffer, usize> for Server {
    fn request(server: &mut Self, _: &Client, _: &wl_buffer::WlBuffer, request: wl_buffer::Request, &index: &usize, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let wl_buffer::Request::Destroy = request {
            server.buffers[index].destroyed = true;
        }
    }
}

impl Dispatch<wl_output::WlOutput, ()> for Server {
    fn request(_: &mut Self, _: &Client, _: &wl_output::WlOutput, _: wl_output::Request, _: &(), _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}

impl Dispatch<xdg_wm_base::XdgWmBase, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &xdg_wm_base::XdgWmBase, request: xdg_wm_base::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let xdg_wm_base::Request::GetXdgSurface { id, surface } = request {
            let surface = *surface.data::<usize>().unwrap();
            let index = server.toplevels.len();
            let xdg_surface = init.init(id, index);
            server.toplevels.push(ToplevelRecord {
                surface,
                xdg_surface,
                toplevel: None,
                title: String::new(),
                app_id: String::new(),
                min_size: (0, 0),
                max_size: (0, 0),
                asks: Vec::new(),
                acked: Vec::new(),
                decoration: None,
                decoration_modes: Vec::new(),
                destroyed: false,
            });
        }
    }
}

impl Dispatch<xdg_surface::XdgSurface, usize> for Server {
    fn request(server: &mut Self, _: &Client, _: &xdg_surface::XdgSurface, request: xdg_surface::Request, &index: &usize, _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        match request {
            xdg_surface::Request::GetToplevel { id } => server.toplevels[index].toplevel = Some(init.init(id, index)),
            xdg_surface::Request::AckConfigure { serial } => server.toplevels[index].acked.push(serial),
            _ => {}
        }
    }
}

impl Dispatch<xdg_toplevel::XdgToplevel, usize> for Server {
    fn request(server: &mut Self, _: &Client, _: &xdg_toplevel::XdgToplevel, request: xdg_toplevel::Request, &index: &usize, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        let record = &mut server.toplevels[index];
        match request {
            xdg_toplevel::Request::SetTitle { title } => record.title = title,
            xdg_toplevel::Request::SetAppId { app_id } => record.app_id = app_id,
            xdg_toplevel::Request::SetMinSize { width, height } => record.min_size = (width, height),
            xdg_toplevel::Request::SetMaxSize { width, height } => record.max_size = (width, height),
            xdg_toplevel::Request::SetMaximized => record.asks.push(Ask::SetMaximized),
            xdg_toplevel::Request::UnsetMaximized => record.asks.push(Ask::UnsetMaximized),
            xdg_toplevel::Request::SetFullscreen { .. } => record.asks.push(Ask::SetFullscreen),
            xdg_toplevel::Request::UnsetFullscreen => record.asks.push(Ask::UnsetFullscreen),
            xdg_toplevel::Request::SetMinimized => record.asks.push(Ask::SetMinimized),
            xdg_toplevel::Request::Destroy => record.destroyed = true,
            _ => {}
        }
    }
}

impl Dispatch<zxdg_decoration_manager_v1::ZxdgDecorationManagerV1, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &zxdg_decoration_manager_v1::ZxdgDecorationManagerV1, request: zxdg_decoration_manager_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let zxdg_decoration_manager_v1::Request::GetToplevelDecoration { id, toplevel } = request {
            let index = *toplevel.data::<usize>().unwrap();
            server.toplevels[index].decoration = Some(init.init(id, index));
        }
    }
}

impl Dispatch<zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1, usize> for Server {
    fn request(server: &mut Self, _: &Client, _: &zxdg_toplevel_decoration_v1::ZxdgToplevelDecorationV1, request: zxdg_toplevel_decoration_v1::Request, &index: &usize, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let zxdg_toplevel_decoration_v1::Request::SetMode { mode: WEnum::Value(mode) } = request {
            server.toplevels[index].decoration_modes.push(mode as u32);
        }
    }
}

impl Dispatch<wp_viewporter::WpViewporter, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wp_viewporter::WpViewporter, request: wp_viewporter::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wp_viewporter::Request::GetViewport { id, surface } = request {
            let index = *surface.data::<usize>().unwrap();
            server.surfaces[index].has_viewport = true;
            init.init(id, index);
        }
    }
}

impl Dispatch<wp_viewport::WpViewport, usize> for Server {
    fn request(server: &mut Self, _: &Client, _: &wp_viewport::WpViewport, request: wp_viewport::Request, &index: &usize, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {
        if let wp_viewport::Request::SetDestination { width, height } = request {
            server.surfaces[index].pending.viewport_destination = Some((width, height));
        }
    }
}

impl Dispatch<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, ()> for Server {
    fn request(server: &mut Self, _: &Client, _: &wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, request: wp_fractional_scale_manager_v1::Request, _: &(), _: &DisplayHandle, init: &mut DataInit<'_, Self>) {
        if let wp_fractional_scale_manager_v1::Request::GetFractionalScale { id, surface } = request {
            let index = *surface.data::<usize>().unwrap();
            server.surfaces[index].fractional_scale = Some(init.init(id, index));
        }
    }
}

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, usize> for Server {
    fn request(_: &mut Self, _: &Client, _: &wp_fractional_scale_v1::WpFractionalScaleV1, _: wp_fractional_scale_v1::Request, _: &usize, _: &DisplayHandle, _: &mut DataInit<'_, Self>) {}
}
