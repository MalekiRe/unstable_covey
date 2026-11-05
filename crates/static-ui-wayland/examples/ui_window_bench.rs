//! Window measurements for UI_BENCHMARKS.md (needs a Wayland compositor):
//!
//! - `--launch N`: spawn this example N times; each reports when its first
//!   frame was committed, in ms after the spawn (CLOCK_MONOTONIC).
//! - `--idle SECONDS`: run with nothing happening; report wakeups, frames,
//!   CPU time and context switches after the first second.
//! - `--latency SECONDS`: a thread wakes a behavior every 100 ms, which
//!   recolors a button; report wakeup-to-scanout times (`wp_presentation`).
//!
//! cargo run --release -p static-ui-wayland --example ui_window_bench -- --idle 60
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};
use std::time::Duration;

use static_ecs::prelude::*;
use static_ecs::{Behavior, Cx, behavior};
use static_ui::widgets::*;
use static_ui::*;
use static_ui_wayland::{Fonts, WindowOptions, run};

fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
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
        .add_plugins((UiPlugin::<Fonts>::default(), InputPlugin, widgets::<Fonts>(), behavior::<Pulsing, _>(UiUpdate)))
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

    if args.iter().any(|a| a == "--first-frame") {
        let launched_ns = std::env::var("UI_T0_NS").ok().and_then(|t| t.parse().ok());
        let stats = run(&mut app, WindowOptions { max_frames: Some(1), launched_ns, ..WindowOptions::default() });
        println!("{:.3}", stats.first_commit_ns as f64 / 1e6);
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
        let stats = run(&mut app, WindowOptions { run_for: Some(Duration::from_secs_f64(seconds)), ..WindowOptions::default() });
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
                "wakeup to scanout over {} frames: median {:.2} ms, p90 {:.2} ms, min {:.2} ms; wakeup to commit sent: median {:.3} ms, max {:.3} ms; {} pixels rastered per frame on average",
                ms.len(),
                ms[ms.len() / 2],
                ms[ms.len() * 9 / 10],
                ms[0],
                ours[ours.len() / 2],
                ours[ours.len() - 1],
                stats.pixels / stats.commits.max(1)
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
    let stats = run(&mut app, WindowOptions { run_for: Some(Duration::from_secs_f64(seconds)), input: false, ..WindowOptions::default() });
    let (cpu0, switches0, _) = settled.join().unwrap();
    let (cpu1, switches1, rss) = usage();
    let after = |times: &[Duration]| times.iter().filter(|&&t| t >= Duration::from_secs(1)).count();
    println!(
        "{} wakeups and {} commits in the first second; in the next {:.0} s: {} wakeups, {} commits, {:.3} s CPU, {} voluntary context switches (1 is the sampling thread's); peak RSS {:.1} MiB",
        stats.wakeups - after(&stats.wakeups_at) as u64,
        stats.commits - after(&stats.commits_at) as u64,
        seconds - 1.0,
        after(&stats.wakeups_at),
        after(&stats.commits_at),
        cpu1 - cpu0,
        switches1 - switches0,
        rss as f64 / 1024.0
    );
}
