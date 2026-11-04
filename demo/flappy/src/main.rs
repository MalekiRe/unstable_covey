//! A CPU-rendered Wayland client: flappy bird on static-ecs. Space, up, or a
//! click flaps; escape quits. With `FLAPPY_BENCH` set it exits once its first
//! frame reaches the screen and prints when each launch step finished,
//! relative to `FLAPPY_T0_NS` (the launcher's CLOCK_MONOTONIC before spawn).
mod game;

use std::os::fd::{AsFd, OwnedFd};

use game::{Canvas, Draw, Flappy, Game, HEIGHT, Tick, WIDTH};
use rustix::mm::{MapFlags, ProtFlags};
use static_ecs::prelude::*;
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
use wayland_protocols::wp::presentation_time::client::wp_presentation_feedback::{
    self, WpPresentationFeedback,
};
use wayland_protocols::xdg::shell::client::xdg_surface::{self, XdgSurface};
use wayland_protocols::xdg::shell::client::xdg_toplevel::{self, XdgToplevel};
use wayland_protocols::xdg::shell::client::xdg_wm_base::{self, XdgWmBase};

const BUFFERS: usize = 2;
const STRIDE: usize = WIDTH * 4;
const FRAME_BYTES: usize = STRIDE * HEIGHT;

fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Window state the protocol events update; the game lives beside it.
#[derive(Default)]
struct Window {
    configured: bool,
    running: bool,
    /// Buffers the compositor still reads.
    busy: [bool; BUFFERS],
    /// The last frame callback's timestamp (ms), when one arrived.
    frame_done: Option<u32>,
    flaps: u32,
    keyboard: Option<WlKeyboard>,
    pointer: Option<WlPointer>,
    clock: Option<u32>,
    /// When frames with feedback were shown (presentation clock ns), and
    /// the output's refresh period (ns): the first frame, then a later one.
    presented: Option<(u64, u32)>,
    steady: Option<u64>,
    discarded: bool,
}

/// Two XRGB8888 frames in one shared-memory pool.
struct Buffers {
    pixels: *mut u32,
    buffers: [WlBuffer; BUFFERS],
    _fd: OwnedFd,
}
impl Buffers {
    fn new(shm: &WlShm, qh: &QueueHandle<Window>) -> Self {
        let size = FRAME_BYTES * BUFFERS;
        let fd = rustix::fs::memfd_create("flappy", rustix::fs::MemfdFlags::CLOEXEC)
            .expect("memfd_create");
        rustix::fs::ftruncate(&fd, size as u64).expect("ftruncate");
        // SAFETY: a fresh shared mapping of the whole memfd, never unmapped
        // while the window lives.
        let pixels = unsafe {
            rustix::mm::mmap(
                std::ptr::null_mut(),
                size,
                ProtFlags::READ | ProtFlags::WRITE,
                MapFlags::SHARED,
                &fd,
                0,
            )
        }
        .expect("mmap") as *mut u32;
        let pool = shm.create_pool(fd.as_fd(), size as i32, qh, ());
        let buffers = std::array::from_fn(|i| {
            pool.create_buffer(
                (i * FRAME_BYTES) as i32,
                WIDTH as i32,
                HEIGHT as i32,
                STRIDE as i32,
                wl_shm::Format::Xrgb8888,
                qh,
                i,
            )
        });
        pool.destroy();
        Self {
            pixels,
            buffers,
            _fd: fd,
        }
    }
    fn frame(&self, index: usize) -> *mut u32 {
        // SAFETY: inside the mapping.
        unsafe { self.pixels.add(index * WIDTH * HEIGHT) }
    }
}

/// Point the canvas at buffer `index` and rasterize the game into it.
fn draw<S: Storage, L: static_ecs::Dispatch<Draw, World<S>>>(
    app: &mut App<S, L>,
    buffers: &Buffers,
    index: usize,
) {
    if let Some(mut canvas) = app.world.single::<&mut Canvas>() {
        // SAFETY: buffer `index` is not attached, so only we touch it until
        // the next attach.
        unsafe { canvas.target(buffers.frame(index), WIDTH, HEIGHT) };
    }
    app.emit(&Draw);
}

fn main() {
    let start = now_ns();
    let launched = std::env::var("FLAPPY_T0_NS")
        .ok()
        .and_then(|t| t.parse().ok())
        .unwrap_or(start);
    let bench = std::env::var_os("FLAPPY_BENCH").is_some();

    let conn = Connection::connect_to_env().expect("no Wayland display");
    let (globals, mut queue) = registry_queue_init::<Window>(&conn).expect("registry");
    let qh = queue.handle();
    let connected = now_ns();
    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ()).expect("wl_compositor");
    let shm: WlShm = globals.bind(&qh, 1..=1, ()).expect("wl_shm");
    let wm: XdgWmBase = globals.bind(&qh, 1..=6, ()).expect("xdg_wm_base");
    let presentation: Option<WpPresentation> = globals.bind(&qh, 1..=1, ()).ok();
    let _seat: Option<WlSeat> = globals.bind(&qh, 1..=7, ()).ok();

    // Ask for a window first; everything below overlaps the compositor's
    // reply.
    let surface = compositor.create_surface(&qh, ());
    let xdg = wm.get_xdg_surface(&surface, &qh, ());
    let toplevel = xdg.get_toplevel(&qh, ());
    toplevel.set_title("Flappy (static-ecs)".into());
    toplevel.set_app_id("static-ecs.flappy".into());
    toplevel.set_min_size(WIDTH as i32, HEIGHT as i32);
    toplevel.set_max_size(WIDTH as i32, HEIGHT as i32);
    surface.commit();
    conn.flush().expect("flush");

    let buffers = Buffers::new(&shm, &qh);
    let mut app = App::new().add_plugins(Flappy).build();
    game::spawn(&mut app.world, start);
    let built = now_ns();
    draw(&mut app, &buffers, 0);
    let drawn = now_ns();

    let mut window = Window {
        running: true,
        ..Window::default()
    };
    while !window.configured {
        queue.blocking_dispatch(&mut window).expect("dispatch");
    }
    let configured = now_ns();
    let feedback = presentation
        .as_ref()
        .map(|p| p.feedback(&surface, &qh, Shown::First));
    surface.attach(Some(&buffers.buffers[0]), 0, 0);
    surface.damage_buffer(0, 0, WIDTH as i32, HEIGHT as i32);
    surface.frame(&qh, ());
    surface.commit();
    conn.flush().expect("flush");
    window.busy[0] = true;
    let committed = now_ns();

    let mut last_frame: Option<u32> = None;
    // In bench mode, a later frame's commit-to-scanout latency too.
    const STEADY_FRAME: u32 = 10;
    let (mut frames, mut steady_commit) = (0u32, None);
    let mut steady_feedback = None;
    while window.running {
        queue.blocking_dispatch(&mut window).expect("dispatch");
        if bench && (window.steady.is_some() || window.discarded) {
            break;
        }
        let Some(time) = window.frame_done.take() else {
            continue;
        };
        let dt = last_frame.map_or(0.0, |last| time.wrapping_sub(last) as f32 / 1000.0);
        last_frame = Some(time);
        if let Some(mut game) = app.world.single::<&mut Game>() {
            game.flap |= std::mem::take(&mut window.flaps) > 0;
        }
        // Long stalls (dragging, hidden windows) must not tunnel the bird.
        app.emit(&Tick(dt.min(1.0 / 30.0)));
        let Some(index) = (0..BUFFERS).find(|&i| !window.busy[i]) else {
            surface.frame(&qh, ());
            surface.commit();
            continue;
        };
        draw(&mut app, &buffers, index);
        frames += 1;
        if bench && frames == STEADY_FRAME {
            steady_feedback = presentation
                .as_ref()
                .map(|p| p.feedback(&surface, &qh, Shown::Steady));
        }
        surface.attach(Some(&buffers.buffers[index]), 0, 0);
        surface.damage_buffer(0, 0, WIDTH as i32, HEIGHT as i32);
        surface.frame(&qh, ());
        surface.commit();
        if bench && frames == STEADY_FRAME {
            conn.flush().expect("flush");
            steady_commit = Some(now_ns());
        }
        window.busy[index] = true;
    }
    drop((feedback, steady_feedback));
    if bench {
        let ms = |t: u64| t.saturating_sub(launched) as f64 / 1e6;
        let (presented, refresh) = match (window.presented, window.clock) {
            // CLOCK_MONOTONIC: comparable with our timestamps.
            (Some((at, refresh)), Some(1)) => (format!("{:.3}", ms(at)), refresh),
            (Some((_, refresh)), _) => ("nan".into(), refresh),
            (None, _) => ("nan".into(), 0),
        };
        // A later frame: from its commit to its scanout.
        let steady = match (window.steady, steady_commit, window.clock) {
            (Some(at), Some(commit), Some(1)) => {
                format!("{:.3}", at.saturating_sub(commit) as f64 / 1e6)
            }
            _ => "nan".into(),
        };
        println!(
            "exec={:.3} connect={:.3} built={:.3} drawn={:.3} configure={:.3} commit={:.3} presented={presented} refresh={:.3} steady={steady}",
            ms(start),
            ms(connected),
            ms(built),
            ms(drawn),
            ms(configured),
            ms(committed),
            refresh as f64 / 1e6,
        );
    }
}

impl Dispatch<WlRegistry, GlobalListContents> for Window {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as wayland_client::Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}
delegate_noop!(Window: WlCompositor);
delegate_noop!(Window: WlShmPool);
delegate_noop!(Window: ignore WlShm);
delegate_noop!(Window: ignore WlSurface);

impl Dispatch<XdgWmBase, ()> for Window {
    fn event(
        _: &mut Self,
        wm: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm.pong(serial);
        }
    }
}
impl Dispatch<XdgSurface, ()> for Window {
    fn event(
        window: &mut Self,
        xdg: &XdgSurface,
        event: xdg_surface::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            xdg.ack_configure(serial);
            window.configured = true;
        }
    }
}
impl Dispatch<XdgToplevel, ()> for Window {
    fn event(
        window: &mut Self,
        _: &XdgToplevel,
        event: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_toplevel::Event::Close = event {
            window.running = false;
        }
    }
}
impl Dispatch<WlBuffer, usize> for Window {
    fn event(
        window: &mut Self,
        _: &WlBuffer,
        event: wl_buffer::Event,
        index: &usize,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_buffer::Event::Release = event {
            window.busy[*index] = false;
        }
    }
}
impl Dispatch<WlCallback, ()> for Window {
    fn event(
        window: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { callback_data } = event {
            window.frame_done = Some(callback_data);
        }
    }
}
impl Dispatch<WpPresentation, ()> for Window {
    fn event(
        window: &mut Self,
        _: &WpPresentation,
        event: wp_presentation::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_presentation::Event::ClockId { clk_id } = event {
            window.clock = Some(clk_id);
        }
    }
}
/// Which frame a presentation feedback belongs to.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    First,
    Steady,
}
impl Dispatch<WpPresentationFeedback, Shown> for Window {
    fn event(
        window: &mut Self,
        _: &WpPresentationFeedback,
        event: wp_presentation_feedback::Event,
        shown: &Shown,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wp_presentation_feedback::Event::Presented {
                tv_sec_hi,
                tv_sec_lo,
                tv_nsec,
                refresh,
                ..
            } => {
                let seconds = (u64::from(tv_sec_hi) << 32) | u64::from(tv_sec_lo);
                let at = seconds * 1_000_000_000 + u64::from(tv_nsec);
                match shown {
                    Shown::First => window.presented = Some((at, refresh)),
                    Shown::Steady => window.steady = Some(at),
                }
            }
            wp_presentation_feedback::Event::Discarded => window.discarded = true,
            _ => {}
        }
    }
}
impl Dispatch<WlSeat, ()> for Window {
    fn event(
        window: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(caps),
        } = event
        {
            if caps.contains(wl_seat::Capability::Keyboard) && window.keyboard.is_none() {
                window.keyboard = Some(seat.get_keyboard(qh, ()));
            }
            if caps.contains(wl_seat::Capability::Pointer) && window.pointer.is_none() {
                window.pointer = Some(seat.get_pointer(qh, ()));
            }
        }
    }
}
impl Dispatch<WlKeyboard, ()> for Window {
    fn event(
        window: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_keyboard::Event::Key {
            key,
            state: WEnum::Value(wl_keyboard::KeyState::Pressed),
            ..
        } = event
        {
            // Evdev codes: escape, space, up.
            match key {
                1 => window.running = false,
                57 | 103 => window.flaps += 1,
                _ => {}
            }
        }
    }
}
impl Dispatch<WlPointer, ()> for Window {
    fn event(
        window: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_pointer::Event::Button {
            state: WEnum::Value(wl_pointer::ButtonState::Pressed),
            ..
        } = event
        {
            window.flaps += 1;
        }
    }
}
