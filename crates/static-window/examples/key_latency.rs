//! Key press to commit, through the compositor: a second connection presses
//! F13 on a virtual keyboard (zwp_virtual_keyboard_v1) every 50 ms; each press
//! recolors a 64×64 square in our window. Reports, per press, our share (the
//! key's arrival to the commit leaving), the whole path from the virtual
//! keyboard's send to the commit leaving, and to the screen.
//!
//! Keys are sent only while our window has keyboard focus, and only F13 (with
//! its own one-key keymap), so nothing else on the desktop receives them.
//!
//! cargo run --release -p static-window --example key_latency -- 100
use std::os::fd::AsFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_window::*;
use wayland_client::globals::{GlobalListContents, registry_queue_init};
use wayland_client::protocol::{wl_registry, wl_seat::WlSeat};
use wayland_client::{Connection, Dispatch, QueueHandle, delegate_noop};
use wayland_protocols_misc::zwp_virtual_keyboard_v1::client::{
    zwp_virtual_keyboard_manager_v1::ZwpVirtualKeyboardManagerV1, zwp_virtual_keyboard_v1::ZwpVirtualKeyboardV1,
};

const F13: u32 = 183;
const KEYMAP: &str = "xkb_keymap {
    xkb_keycodes \"vk\" { minimum = 8; maximum = 255; <FK13> = 191; };
    xkb_types \"vk\" { type \"ONE_LEVEL\" { modifiers = none; level_name[Level1] = \"Any\"; }; };
    xkb_compatibility \"vk\" { };
    xkb_symbols \"vk\" { key <FK13> { [ F13 ] }; };
};\0";

fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

#[derive(Default)]
struct Shared {
    focused: AtomicBool,
    done: AtomicBool,
    sent_ns: AtomicU64,
}

/// The square's color, flipped by each F13 press.
#[derive(Default)]
struct Lit(bool);

const SQUARE: PixelRect = PixelRect::new(16, 16, 64, 64);

struct Injector;
impl Dispatch<wl_registry::WlRegistry, GlobalListContents> for Injector {
    fn event(_: &mut Self, _: &wl_registry::WlRegistry, _: wl_registry::Event, _: &GlobalListContents, _: &Connection, _: &QueueHandle<Self>) {}
}
delegate_noop!(Injector: ignore WlSeat);
delegate_noop!(Injector: ZwpVirtualKeyboardManagerV1);
delegate_noop!(Injector: ZwpVirtualKeyboardV1);

/// Press F13 `presses` times, 50 ms apart, while our window has focus.
fn inject(shared: &Shared, presses: usize) -> Result<(), String> {
    let conn = Connection::connect_to_env().map_err(|e| e.to_string())?;
    let (globals, mut queue) = registry_queue_init::<Injector>(&conn).map_err(|e| e.to_string())?;
    let qh = queue.handle();
    let seat: WlSeat = globals.bind(&qh, 1..=1, ()).map_err(|e| e.to_string())?;
    let manager: ZwpVirtualKeyboardManagerV1 = globals.bind(&qh, 1..=1, ()).map_err(|_| "no virtual keyboard manager".to_string())?;
    let keyboard = manager.create_virtual_keyboard(&seat, &qh, ());
    let fd = rustix::fs::memfd_create("keymap", rustix::fs::MemfdFlags::CLOEXEC).map_err(|e| e.to_string())?;
    rustix::io::write(&fd, KEYMAP.as_bytes()).map_err(|e| e.to_string())?;
    keyboard.keymap(1, fd.as_fd(), KEYMAP.len() as u32);
    queue.roundtrip(&mut Injector).map_err(|e| e.to_string())?;
    let start = Instant::now();
    while !shared.focused.load(SeqCst) {
        if start.elapsed() > Duration::from_secs(3) {
            return Err("our window never got keyboard focus; sent nothing".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let ms = |at: Instant| at.duration_since(start).as_millis() as u32;
    for _ in 0..presses {
        std::thread::sleep(Duration::from_millis(30));
        if !shared.focused.load(SeqCst) {
            return Err("focus left our window; stopped".into());
        }
        shared.sent_ns.store(now_ns(), SeqCst);
        keyboard.key(ms(Instant::now()), F13, 1);
        conn.flush().map_err(|e| e.to_string())?;
        std::thread::sleep(Duration::from_millis(20));
        keyboard.key(ms(Instant::now()), F13, 0);
        conn.flush().map_err(|e| e.to_string())?;
    }
    keyboard.destroy();
    conn.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn summary(name: &str, mut ns: Vec<u64>) {
    if ns.is_empty() {
        println!("{name:>28}: none");
        return;
    }
    ns.sort_unstable();
    let at = |q: f64| ns[((ns.len() - 1) as f64 * q) as usize] as f64 / 1e6;
    println!("{name:>28}: median {:.3} ms, p90 {:.3}, min {:.3}, max {:.3} (n = {})", at(0.5), at(0.9), at(0.0), at(1.0), ns.len());
}

fn main() {
    let presses: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(100);
    let shared = Arc::new(Shared::default());
    let focus = shared.clone();
    let mut app = App::new()
        .add_plugins(WindowPlugin)
        .init_resource::<Lit>()
        .add_systems(label::<FocusChanged>(), move |e: &FocusChanged| focus.focused.store(e.focused, SeqCst))
        .add_systems(label::<KeyInput>(), |key: &KeyInput, mut lit: ResMut<Lit>, mut fbs: Query<&mut Framebuffer>| {
            if key.code == KeyCode(F13 as u16) && key.pressed && !key.repeat {
                lit.0 = !lit.0;
                if let Some(mut fb) = fbs.get_mut(key.window) {
                    fb.request(SQUARE);
                }
            }
        })
        .add_systems(label::<Redraw>(), |redraw: &Redraw, lit: Res<Lit>, mut fbs: Query<&mut Framebuffer>| {
            let Some(mut fb) = fbs.get_mut(redraw.window) else { return };
            let Some(target) = fb.target() else { return };
            for r in target.regions {
                for y in r.y..r.y + r.height {
                    for x in r.x..r.x + r.width {
                        let inside = (SQUARE.x..SQUARE.x + SQUARE.width).contains(&x) && (SQUARE.y..SQUARE.y + SQUARE.height).contains(&y);
                        let color = match (inside, lit.0) {
                            (true, true) => 0xFFFF_C040,
                            (true, false) => 0xFF40_60C0,
                            _ => 0xFF20_2020,
                        };
                        target.pixels[(y * target.width + x) as usize] = color;
                    }
                }
            }
        })
        .build();
    let window = app.world.spawn_bundle((
        Window::new("key latency (F13)").size(96, 96).app_id("static-window.key-latency").resizable(false),
        Framebuffer::at_scale(Scale::ONE),
    ));
    let mut window_loop = WindowLoop::connect().expect("no Wayland display");
    window_loop.record_latency(true);
    let injector = {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let result = inject(&shared, presses);
            shared.done.store(true, SeqCst);
            result
        })
    };
    let mut whole = Vec::new();
    let mut seen = 0;
    while !shared.done.load(SeqCst) {
        if window_loop.step(&mut app, Some(Duration::from_millis(100))) != Flow::Continue {
            break;
        }
        let ours = window_loop.stats().input_to_commit_ns.len();
        if ours > seen {
            // The commit left during this step; the step returned right after.
            whole.push(now_ns().saturating_sub(shared.sent_ns.load(SeqCst)));
            seen = ours;
        }
    }
    // Let the last frames' presentation reports come in.
    let until = Instant::now() + Duration::from_millis(200);
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        window_loop.step(&mut app, Some(left));
    }
    app.world.despawn(window);
    if let Err(e) = injector.join().unwrap() {
        eprintln!("{e}");
    }
    let stats = window_loop.stats();
    summary("key arrival to commit (ours)", stats.input_to_commit_ns.clone());
    summary("key sent to commit", whole);
    summary("key arrival to screen", stats.input_to_present_ns.clone());
}
