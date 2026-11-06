#![recursion_limit = "512"]
//!
//! - `--launch N`: spawn this example N times; each reports when its first
//!   frame was committed, in ms after the spawn (CLOCK_MONOTONIC).
//! - `--idle SECONDS`: run with nothing happening; report wakeups, frames,
//!   CPU time and context switches after the first second.
//! - `--latency SECONDS`: a thread wakes a behavior every 100 ms, which
//!   recolors a button; report wakeup-to-scanout times (`wp_presentation`).
//!
//! cargo run --release -p static-ui-raster --example ui_window_bench -- --idle 60
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_ecs::{Behavior, Cx, behavior};
use static_ui::widgets::*;
use static_ui::*;
use static_ui_raster::{Fonts, RasterPlugin, UiLabels, ui_frame, ui_window};
use static_window::{Flow, Window, WindowLoop, WindowPlugin, WindowWorld};

fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}
/// Step the window loop (with the UI's frames) until `until`, or until it
/// ends.
fn run_until<S: WindowWorld, L: UiLabels<S>>(window_loop: &mut WindowLoop, app: &mut App<S, L>, until: Instant) {
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        if window_loop.step_with(app, Some(left), ui_frame) != Flow::Continue {
            break;
        }
    }
}
/// (CPU seconds, voluntary context switches, peak RSS KiB) of this process.
fn usage() -> (f64, u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<&str> = stat.rsplit(')').next().unwrap().split_whitespace().collect();
    let ticks = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let field = |name: &str| {
        status.lines().find(|l| l.starts_with(name)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0)
    };
    (ticks as f64 / 100.0, field("voluntary_ctxt_switches:"), field("VmHWM:"))
}

/// Set from another thread: the latency behavior's wake source.
#[derive(Default)]
struct Pulse(Mutex<(u64, Option<Waker>)>);
struct PulseSlot(Arc<Pulse>);
/// Recolors its button each pulse.
struct Pulsing;
impl Behavior for Pulsing {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        let pulse = cx.with(|mut q: Query<&PulseSlot>| q.get_mut(me).unwrap().0.clone()).unwrap();
        let mut seen = 0;
        loop {
            seen = std::future::poll_fn(|c| {
                let mut state = pulse.0.lock().unwrap();
                if state.0 != seen {
                    return Poll::Ready(state.0);
                }
                state.1 = Some(c.waker().clone());
                Poll::Pending
            })
            .await;
            let color = if seen % 2 == 0 { theme::ACCENT } else { theme::BUTTON };
            cx.with(|mut q: Query<&mut Background>| q.get_mut(me).unwrap().0 = color).unwrap();
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|v| v.parse::<f64>().expect("a number"));
    if let Some(n) = arg("--launch") {
        let mut times = Vec::new();
        for _ in 0..n as usize {
            let t0 = now_ns();
            let out = std::process::Command::new(&args[0]).arg("--first-frame").env("UI_T0_NS", t0.to_string()).output().expect("spawn");
            let line = String::from_utf8_lossy(&out.stdout);
            times.push(line.trim().parse::<f64>().expect("child prints ms"));
            std::thread::sleep(Duration::from_millis(100));
        }
        times.sort_by(f64::total_cmp);
        println!("first frame committed, ms after spawn: median {:.2}, min {:.2}, max {:.2}", times[times.len() / 2], times[0], times[times.len() - 1]);
        return;
    }

    let mut app = App::new()
        .add_plugins((WindowPlugin, UiPlugin::<Fonts>::default(), InputPlugin, widgets::<Fonts>(), behavior::<Pulsing, _>(UiUpdate), RasterPlugin))
        .register::<PulseSlot>()
        .build();
    let world = &mut app.world;
    let screen = world.spawn_ui(el((Node::row().width(Sizing::GROW).height(Sizing::GROW).padding(Padding::all(24)).gap(24), Background(theme::PANEL))));
    let column = world.spawn_ui_under(screen, el((Node::column().gap(12),)));
    world.spawn_ui_under(column, button("Save"));
    world.spawn_ui_under(column, checkbox(true));
    world.spawn_ui_under(column, slider(SliderRange::default(), 0.3));
    world.spawn_ui_under(column, text_field("Hello", 220.0));
    spawn_dropdown(world, Some(column), &["Low", "Medium", "High"], 0);
    let pulse = Arc::new(Pulse::default());
    let pulsing = world.spawn_ui_under(column, button("Pulse"));
    world.insert_bundle(pulsing, (Pulsing, PulseSlot(pulse.clone())));
    spawn_virtual_list(world, Some(screen), VirtualList { count: 10_000, row_height: 24.0 }, 300.0, 500.0, || {
        el((Node::row().width(Sizing::GROW).height(Sizing::fixed(24.0)).padding(Padding::xy(8, 4)),)).children((el((Text::new("row", 14),)),))
    });

    let mut window_loop = WindowLoop::connect().expect("a Wayland compositor");
    if args.iter().any(|a| a == "--first-frame") {
        app.world.spawn_bundle(ui_window(Window::new("ui_window_bench").size(800, 600)));
        if let Some(t0) = std::env::var("UI_T0_NS").ok().and_then(|t| t.parse().ok()) {
            window_loop.launched_at(t0);
        }
        while window_loop.stats().first_commit_ns == 0 && window_loop.step_with(&mut app, None, ui_frame) == Flow::Continue {}
        println!("{:.3}", window_loop.stats().first_commit_ns as f64 / 1e6);
        return;
    }
    if let Some(seconds) = arg("--latency") {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (pulse, stopped) = (pulse.clone(), stop.clone());
        let pacer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            while !stopped.load(std::sync::atomic::Ordering::Relaxed) {
                let waker = {
                    let mut state = pulse.0.lock().unwrap();
                    state.0 += 1;
                    state.1.take()
                };
                if let Some(waker) = waker {
                    waker.wake();
                }
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        app.world.spawn_bundle(ui_window(Window::new("ui_window_bench").size(800, 600)));
        window_loop.record_latency(true);
        run_until(&mut window_loop, &mut app, Instant::now() + Duration::from_secs_f64(seconds));
        let stats = window_loop.stats().clone();
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        pacer.join().unwrap();
        let mut ms: Vec<f64> = stats.input_to_present_ns.iter().map(|&n| n as f64 / 1e6).collect();
        ms.sort_by(f64::total_cmp);
        if ms.is_empty() {
            println!("no presentation feedback (compositor without wp_presentation?)");
        } else {
            let mut ours: Vec<f64> = stats.input_to_commit_ns.iter().map(|&n| n as f64 / 1e6).collect();
            ours.sort_by(f64::total_cmp);
            println!(
                "wakeup to scanout over {} frames: median {:.2} ms, p90 {:.2} ms, min {:.2} ms; wakeup to commit sent: median {:.3} ms, max {:.3} ms",
                ms.len(),
                ms[ms.len() / 2],
                ms[ms.len() * 9 / 10],
                ms[0],
                ours[ours.len() / 2],
                ours[ours.len() - 1]
            );
        }
        return;
    }
    let seconds = arg("--idle").unwrap_or(10.0);
    // Sample usage once the window has settled for a second.
    let settled = std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(1));
        usage()
    });
    // Input-transparent: a pointer crossing the window must not count.
    app.world.spawn_bundle(ui_window(Window::new("ui_window_bench").size(800, 600).passthrough(true)));
    let start = Instant::now();
    run_until(&mut window_loop, &mut app, start + Duration::from_secs(1));
    let first = (window_loop.stats.wakeups, window_loop.stats.commits);
    run_until(&mut window_loop, &mut app, start + Duration::from_secs_f64(seconds));
    let (cpu0, switches0, _) = settled.join().unwrap();
    let (cpu1, switches1, rss) = usage();
    let stats = &window_loop.stats;
    println!(
        "{} wakeups and {} commits in the first second; in the next {:.0} s: {} wakeups, {} commits, {:.3} s CPU, {} voluntary context switches (1 is the sampling thread's); peak RSS {:.1} MiB",
        first.0,
        first.1,
        seconds - 1.0,
        stats.wakeups - first.0,
        stats.commits - first.1,
        cpu1 - cpu0,
        switches1 - switches0,
        rss as f64 / 1024.0
    );
}
