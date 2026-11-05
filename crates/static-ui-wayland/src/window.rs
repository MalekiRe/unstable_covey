//! The Wayland window: one wakeup handles every pending event, emits them as
//! `Input` labels, runs `UiFrame`, rasters the damaged rectangles into a
//! free shared-memory buffer and commits them. Frames are requested only
//! while the UI says it is animating; otherwise the loop sleeps in `poll`
//! on the Wayland socket, the behaviors' wakeup eventfd, and the next timer.
use std::os::fd::{AsFd, OwnedFd};
use std::sync::Arc;
use std::time::Instant;

use rustix::event::{PollFd, PollFlags, Timespec};
use rustix::mm::{MapFlags, ProtFlags};
use static_ecs::prelude::*;
use static_ecs::{Dispatch as Handles, Wakeup};
use static_ui::input::{key_from_evdev, key_text};
use static_ui::layout::Rect;
use static_ui::{ClearColor, Input, Key, Modifiers, MouseButton, Redraw, RenderList, UiFrame, UiLayout, Viewport};
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::wl_buffer::{self, WlBuffer};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_keyboard::{self, WlKeyboard};
use wayland_client::protocol::wl_pointer::{self, WlPointer};
use wayland_client::protocol::wl_registry::WlRegistry;
use wayland_client::protocol::wl_seat::{self, WlSeat};
use wayland_client::protocol::wl_shm::{self, WlShm};
use wayland_client::protocol::wl_shm_pool::WlShmPool;
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, QueueHandle, WEnum, delegate_noop};
use wayland_protocols::wp::presentation_time::client::wp_presentation::{self, WpPresentation};
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{self, WpPresentationFeedback};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

use crate::canvas::Canvas;
use crate::font::{Atlas, Fonts};
use crate::paint::paint;

const BUFFERS: usize = 2;

/// How the window opens.
#[derive(Clone, Debug)]
pub struct WindowOptions {
    pub title: String,
    pub app_id: String,
    pub width: u32,
    pub height: u32,
    /// Close after this many committed frames (for tests and benchmarks).
    pub max_frames: Option<u64>,
    /// Close after this long.
    pub run_for: Option<std::time::Duration>,
}
impl Default for WindowOptions {
    fn default() -> Self {
        Self {
            title: "static-ui".into(),
            app_id: "static-ecs.static-ui".into(),
            width: 800,
            height: 600,
            max_frames: None,
            run_for: None,
        }
    }
}

/// What the loop did, for measuring an idle UI and latency.
#[derive(Clone, Debug, Default)]
pub struct Stats {
    /// Returns from `poll`.
    pub wakeups: u64,
    /// `UiFrame` emits, and why: input, a frame callback or buffer
    /// release, a behavior's wakeup, a timer.
    pub frames: u64,
    pub input_frames: u64,
    pub callback_frames: u64,
    pub woken_frames: u64,
    pub timer_frames: u64,
    /// Surface commits with new pixels.
    pub commits: u64,
    /// Pixels rastered over the run.
    pub pixels: u64,
    /// For commits that followed input: input received to pixels on screen,
    /// in nanoseconds, from `wp_presentation` (CLOCK_MONOTONIC).
    pub input_to_present_ns: Vec<u64>,
    /// Time from launch to the first frame being committed, in nanoseconds.
    pub first_commit_ns: u64,
}

fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Protocol state the event handlers update.
#[derive(Default)]
struct State {
    configured: bool,
    running: bool,
    size: Option<(u32, u32)>,
    busy: [bool; BUFFERS],
    frame_done: bool,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    modifiers: Modifiers,
    /// Input since the last frame, with when the first one arrived.
    input: Vec<Input>,
    input_at: Option<u64>,
    clock: Option<u32>,
    presented: Vec<(u64, u64)>,
}

/// Shared-memory buffers of one size.
struct Buffers {
    pixels: *mut u32,
    bytes: usize,
    width: usize,
    height: usize,
    buffers: Vec<WlBuffer>,
    /// Per buffer: what changed since it was last drawn.
    pending: [Vec<Rect>; BUFFERS],
    _fd: OwnedFd,
}
impl Buffers {
    fn new(shm: &WlShm, qh: &QueueHandle<State>, width: usize, height: usize) -> Self {
        let frame = width * height * 4;
        let bytes = frame * BUFFERS;
        let fd = rustix::fs::memfd_create("static-ui", rustix::fs::MemfdFlags::CLOEXEC).expect("memfd_create");
        rustix::fs::ftruncate(&fd, bytes as u64).expect("ftruncate");
        // SAFETY: a fresh shared mapping of the whole memfd, unmapped on drop.
        let pixels = unsafe {
            rustix::mm::mmap(std::ptr::null_mut(), bytes, ProtFlags::READ | ProtFlags::WRITE, MapFlags::SHARED, &fd, 0)
        }
        .expect("mmap") as *mut u32;
        let pool = shm.create_pool(fd.as_fd(), bytes as i32, qh, ());
        let buffers = (0..BUFFERS)
            .map(|i| pool.create_buffer((i * frame) as i32, width as i32, height as i32, (width * 4) as i32, wl_shm::Format::Xrgb8888, qh, i))
            .collect();
        pool.destroy();
        let whole = Rect::new(0.0, 0.0, width as f32, height as f32);
        Self { pixels, bytes, width, height, buffers, pending: [vec![whole], vec![whole]], _fd: fd }
    }
    fn frame(&mut self, index: usize) -> &mut [u32] {
        // SAFETY: buffer `index` is inside the mapping and not attached.
        unsafe { std::slice::from_raw_parts_mut(self.pixels.add(index * self.width * self.height), self.width * self.height) }
    }
}
impl Drop for Buffers {
    fn drop(&mut self) {
        for buffer in &self.buffers {
            buffer.destroy();
        }
        // SAFETY: the mapping made in `new`.
        unsafe {
            let _ = rustix::mm::munmap(self.pixels.cast(), self.bytes);
        }
    }
}

/// Open a window and run `app` in it until it is closed (or `options` end
/// the run). The app needs `UiPlugin<Fonts>`.
pub fn run<S: Storage, L>(app: &mut App<S, L>, options: WindowOptions) -> Stats
where
    L: Handles<Input, World<S>> + Handles<UiFrame, World<S>>,
{
    let launched = now_ns();
    let started = Instant::now();
    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init::<State>(&conn).expect("registry");
    let qh = queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let presentation: Option<WpPresentation> = globals.bind(&qh, 1..=1, ()).ok();
    let _seat: Option<WlSeat> = globals.bind(&qh, 1..=7, ()).ok();
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title(options.title.clone());
    toplevel.set_app_id(options.app_id.clone());
    surface.commit();
    conn.flush().expect("flush");

    // Behaviors woken from anywhere write here; the loop polls it.
    let wake: Arc<OwnedFd> = Arc::new(
        rustix::event::eventfd(0, rustix::event::EventfdFlags::CLOEXEC | rustix::event::EventfdFlags::NONBLOCK).expect("eventfd"),
    );
    let waker = wake.clone();
    app.world.insert_resource(Wakeup(Arc::new(move || {
        let _ = rustix::io::write(&*waker, &1u64.to_ne_bytes());
    })));
    app.world.insert_resource(Viewport { width: options.width as f32, height: options.height as f32 });

    let mut state = State { running: true, ..State::default() };
    let mut stats = Stats::default();
    let mut atlas = Atlas::default();
    let mut buffers: Option<Buffers> = None;
    let mut woken = true;
    let mut feedbacks = Vec::new();
    while state.running {
        queue.dispatch_pending(&mut state).expect("dispatch");
        let deadline = app.world.resource::<Redraw>().and_then(|r| r.deadline);
        let due = deadline.is_some_and(|d| d <= Instant::now());
        let has_work = state.configured && (woken || due || state.frame_done || !state.input.is_empty() || buffers.is_none());
        if !has_work {
            conn.flush().expect("flush");
            let Some(guard) = queue.prepare_read() else { continue };
            let timeout = match (deadline, options.run_for) {
                (Some(d), _) => Some(d.saturating_duration_since(Instant::now())),
                (None, Some(limit)) => Some(limit.saturating_sub(started.elapsed())),
                (None, None) => None,
            };
            let timespec = timeout.map(|t| Timespec { tv_sec: t.as_secs() as i64, tv_nsec: t.subsec_nanos() as i64 });
            let socket_fd = guard.connection_fd();
            let mut fds = [PollFd::new(&socket_fd, PollFlags::IN), PollFd::new(&*wake, PollFlags::IN)];
            match rustix::event::poll(&mut fds, timespec.as_ref()) {
                Ok(_) | Err(rustix::io::Errno::INTR) => {}
                Err(e) => panic!("poll: {e}"),
            }
            stats.wakeups += 1;
            let (socket, eventfd) = (fds[0].revents(), fds[1].revents());
            if socket.contains(PollFlags::IN) {
                let _ = guard.read();
            } else {
                drop(guard);
            }
            if eventfd.contains(PollFlags::IN) {
                let mut count = [0u8; 8];
                let _ = rustix::io::read(&*wake, &mut count);
                woken = true;
            }
            if options.run_for.is_some_and(|limit| started.elapsed() >= limit) {
                break;
            }
            continue;
        }
        stats.input_frames += !state.input.is_empty() as u64;
        stats.callback_frames += state.frame_done as u64;
        stats.woken_frames += woken as u64;
        stats.timer_frames += due as u64;
        woken = false;
        state.frame_done = false;
        // Size: the compositor's, or ours.
        let (width, height) = state.size.filter(|&(w, h)| w > 0 && h > 0).unwrap_or((options.width, options.height));
        if buffers.as_ref().is_none_or(|b| b.width != width as usize || b.height != height as usize) {
            buffers = Some(Buffers::new(&shm, &qh, width as usize, height as usize));
            state.busy = [false; BUFFERS];
            app.world.insert_resource(Viewport { width: width as f32, height: height as f32 });
        }
        let input_at = state.input_at.take();
        for event in state.input.drain(..) {
            app.emit(&event);
        }
        app.emit(&UiFrame);
        stats.frames += 1;
        let damage = app.world.resource_mut::<RenderList>().map(|mut list| list.take_damage()).unwrap_or_default();
        let buffers = buffers.as_mut().unwrap();
        for pending in &mut buffers.pending {
            pending.extend_from_slice(&damage);
        }
        let animating = app.world.resource::<Redraw>().is_some_and(|r| r.animating);
        let free = (0..BUFFERS).find(|&i| !state.busy[i] && !buffers.pending[i].is_empty());
        if let Some(index) = free {
            let regions = merge_all(std::mem::take(&mut buffers.pending[index]));
            let (w, h) = (buffers.width, buffers.height);
            let world = &app.world;
            let (Some(list), Some(ui), Some(fonts)) = (world.resource::<RenderList>(), world.resource::<UiLayout>(), world.resource::<Fonts>()) else {
                panic!("the app needs `UiPlugin<Fonts>`");
            };
            let clear = world.resource::<ClearColor>().copied().unwrap_or_default();
            let mut canvas = Canvas { pixels: buffers.frame(index), width: w, height: h, stride: w, touched: 0 };
            let painted = paint(&mut canvas, list, ui, fonts, &mut atlas, clear.0, &regions);
            stats.pixels += painted.pixels;
            if let (Some(presentation), Some(at)) = (&presentation, input_at) {
                feedbacks.push(presentation.feedback(&surface, &qh, at));
            }
            surface.attach(Some(&buffers.buffers[index]), 0, 0);
            for r in &regions {
                surface.damage_buffer(r.x as i32, r.y as i32, r.width.ceil() as i32, r.height.ceil() as i32);
            }
            if animating {
                surface.frame(&qh, ());
            }
            surface.commit();
            state.busy[index] = true;
            stats.commits += 1;
            if stats.commits == 1 {
                conn.flush().expect("flush");
                stats.first_commit_ns = now_ns() - launched;
            }
        } else if animating || buffers.pending.iter().any(|p| !p.is_empty()) {
            // Nothing free, or nothing to draw this time: come back on the
            // next frame (a release also wakes the loop).
            surface.frame(&qh, ());
            surface.commit();
        }
        if options.max_frames.is_some_and(|max| stats.commits >= max) {
            break;
        }
    }
    conn.flush().ok();
    drop(feedbacks);
    let _ = queue.roundtrip(&mut state);
    if state.clock == Some(1) {
        stats.input_to_present_ns = state.presented.iter().map(|&(input, shown)| shown.saturating_sub(input)).collect();
    }
    stats
}

fn merge_all(mut rects: Vec<Rect>) -> Vec<Rect> {
    rects.retain(|r| !r.is_empty());
    let mut changed = true;
    while changed {
        changed = false;
        'outer: for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                if !rects[i].intersect(rects[j]).is_empty() {
                    rects[i] = rects[i].union(rects[j]);
                    rects.swap_remove(j);
                    changed = true;
                    break 'outer;
                }
            }
        }
    }
    rects
}

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(_: &mut Self, _: &WlRegistry, _: <WlRegistry as wayland_client::Proxy>::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(State: WlCompositor);
delegate_noop!(State: WlShmPool);
delegate_noop!(State: ignore WlShm);
delegate_noop!(State: ignore WlSurface);

impl Dispatch<XdgWmBase, ()> for State {
    fn event(_: &mut Self, wm: &XdgWmBase, event: xdg_wm_base::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for State {
    fn event(state: &mut Self, xdg: &XdgSurface, event: xdg_surface::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            state.configured = true;
        }
    }
}
impl Dispatch<XdgToplevel, ()> for State {
    fn event(state: &mut Self, _: &XdgToplevel, event: xdg_toplevel::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            xdg_toplevel::Event::Close => state.running = false,
            xdg_toplevel::Event::Configure { width, height, .. } => {
                state.size = Some((width.max(0) as u32, height.max(0) as u32));
            }
            _ => {}
        }
    }
}
impl Dispatch<WlBuffer, usize> for State {
    fn event(state: &mut Self, _: &WlBuffer, event: wl_buffer::Event, index: &usize, _: &Connection, _: &QueueHandle<Self>) {
        if let wl_buffer::Event::Release = event {
            state.busy[*index] = false;
            // A buffer came back with damage still queued for it.
            state.frame_done = true;
        }
    }
}
impl Dispatch<WlCallback, ()> for State {
    fn event(state: &mut Self, _: &WlCallback, event: wl_callback::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wl_callback::Event::Done { .. } = event {
            state.frame_done = true;
        }
    }
}
impl Dispatch<WpPresentation, ()> for State {
    fn event(state: &mut Self, _: &WpPresentation, event: wp_presentation::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            state.clock = Some(clk_id);
        }
    }
}
impl Dispatch<WpPresentationFeedback, u64> for State {
    fn event(state: &mut Self, _: &WpPresentationFeedback, event: wp_presentation_feedback::Event, input: &u64, _: &Connection, _: &QueueHandle<Self>) {
        if let wp_presentation_feedback::Event::Presented { tv_sec_hi, tv_sec_lo, tv_nsec, .. } = event {
            let seconds = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
            state.presented.push((*input, seconds * 1_000_000_000 + u64::from(tv_nsec)));
        }
    }
}
impl Dispatch<WlSeat, ()> for State {
    fn event(state: &mut Self, seat: &WlSeat, event: wl_seat::Event, _: &(), _: &Connection, qh: &QueueHandle<Self>) {
        if let wl_seat::Event::Capabilities { capabilities: WEnum::Value(caps) } = event {
            if caps.contains(wl_seat::Capability::Keyboard) && state.keyboard.is_none() {
                state.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Pointer) && state.pointer.is_none() {
                state.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}
impl State {
    fn push(&mut self, input: Input) {
        self.input_at.get_or_insert_with(now_ns);
        self.input.push(input);
    }
}
impl Dispatch<WlKeyboard, ()> for State {
    fn event(state: &mut Self, _: &WlKeyboard, event: wl_keyboard::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_keyboard::Event::Key { key, state: WEnum::Value(key_state), .. } => {
                let pressed = key_state == wl_keyboard::KeyState::Pressed;
                let key = key_from_evdev(key);
                match key {
                    Key::Shift => state.modifiers.shift = pressed,
                    Key::Control => state.modifiers.control = pressed,
                    Key::Alt => state.modifiers.alt = pressed,
                    _ => {}
                }
                let modifiers = state.modifiers;
                state.push(Input::Key { key, pressed, modifiers });
                if pressed
                    && !modifiers.control
                    && let Some(text) = key_text(key, modifiers.shift)
                {
                    state.push(Input::Text(text));
                }
            }
            wl_keyboard::Event::Leave { .. } => {
                state.modifiers = Modifiers::default();
                state.push(Input::FocusLost);
            }
            _ => {}
        }
    }
}
impl Dispatch<WlPointer, ()> for State {
    fn event(state: &mut Self, _: &WlPointer, event: wl_pointer::Event, _: &(), _: &Connection, _: &QueueHandle<Self>) {
        match event {
            wl_pointer::Event::Enter { surface_x, surface_y, .. } | wl_pointer::Event::Motion { surface_x, surface_y, .. } => {
                state.push(Input::PointerMoved { x: surface_x as f32, y: surface_y as f32 });
            }
            wl_pointer::Event::Leave { .. } => state.push(Input::PointerLeft),
            wl_pointer::Event::Button { button, state: WEnum::Value(button_state), .. } => {
                let button = match button {
                    0x110 => MouseButton::Left,
                    0x111 => MouseButton::Right,
                    0x112 => MouseButton::Middle,
                    other => MouseButton::Other(other as u16),
                };
                state.push(Input::Button { button, pressed: button_state == wl_pointer::ButtonState::Pressed });
            }
            wl_pointer::Event::Axis { axis: WEnum::Value(axis), value, .. } => {
                let (dx, dy) = match axis {
                    wl_pointer::Axis::HorizontalScroll => (value as f32, 0.0),
                    _ => (0.0, value as f32),
                };
                state.push(Input::Scroll { dx, dy });
            }
            _ => {}
        }
    }
}
