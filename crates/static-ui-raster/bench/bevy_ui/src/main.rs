//! The settings list of static-ui-raster's `examples/ui_bench.rs`, in Bevy
//! UI (5 elements per row: row, icon, column, two texts).
//!
//! - `--layout N`: N elements; each frame toggles one row's width and times
//!   the `UiSystems::Layout` set, then times it with nothing changed.
//! - `--first-frame`: exit after the first frame; print ms since `UI_T0_NS`.
//! - `--launch K`: spawn K `--first-frame` runs and report.
//! - `--idle SECONDS`: reactive (`desktop_app`) mode, report CPU and context
//!   switches after the first second, and peak RSS.
use std::time::{Duration, Instant};

use bevy::prelude::*;
use bevy::ui::UiSystems;
use bevy::winit::WinitSettings;

const WORDS: [&str; 8] = ["Settings", "Volume", "Brightness", "Network", "Wi-Fi", "Bluetooth", "Display", "About"];

fn now_ns() -> u64 {
    let mut t = libc_timespec();
    // SAFETY: CLOCK_MONOTONIC into a valid timespec.
    unsafe { clock_gettime(1, &mut t) };
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}
#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}
fn libc_timespec() -> Timespec {
    Timespec { tv_sec: 0, tv_nsec: 0 }
}
unsafe extern "C" {
    fn clock_gettime(clock: i32, out: *mut Timespec) -> i32;
}
fn usage() -> (f64, u64, u64) {
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap();
    let fields: Vec<&str> = stat.rsplit(')').next().unwrap().split_whitespace().collect();
    let ticks = fields[11].parse::<u64>().unwrap() + fields[12].parse::<u64>().unwrap();
    let status = std::fs::read_to_string("/proc/self/status").unwrap();
    let field = |name: &str| status.lines().find(|l| l.starts_with(name)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse().ok()).unwrap_or(0);
    (ticks as f64 / 100.0, field("voluntary_ctxt_switches:"), field("VmHWM:"))
}

#[derive(Resource)]
struct Bench {
    elements: usize,
    mode: Mode,
    frames: u32,
    layout_start: Option<Instant>,
    forced: Vec<f64>,
    unchanged: Vec<f64>,
    toggled_row: Option<Entity>,
    started: Instant,
    settle: Option<(f64, u64)>,
}
#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Layout,
    FirstFrame(u64),
    Idle(f64),
}

fn setup(mut commands: Commands, mut bench: ResMut<Bench>) {
    commands.spawn(Camera2d);
    let panel = commands
        .spawn((
            Node { width: percent(100.0), height: percent(100.0), flex_direction: FlexDirection::Column, padding: UiRect::all(px(8.0)), row_gap: px(4.0), overflow: Overflow::scroll_y(), ..default() },
            BackgroundColor(Color::srgb_u8(245, 245, 248)),
        ))
        .id();
    for r in 0..bench.elements / 5 {
        let row = commands
            .spawn((
                Node { width: percent(100.0), flex_direction: FlexDirection::Row, column_gap: px(6.0), align_items: AlignItems::Center, ..default() },
                BackgroundColor(Color::WHITE),
                ChildOf(panel),
            ))
            .id();
        commands.spawn((Node { width: px(24.0), height: px(24.0), ..default() }, BackgroundColor(Color::srgb_u8(66, 133, 244)), ChildOf(row)));
        let column = commands.spawn((Node { flex_direction: FlexDirection::Column, flex_grow: 1.0, ..default() }, ChildOf(row))).id();
        commands.spawn((Text::new(WORDS[r % 8]), TextFont { font_size: bevy::text::FontSize::Px(14.0), ..default() }, ChildOf(column)));
        commands.spawn((Text::new(WORDS[(r + 3) % 8]), TextFont { font_size: bevy::text::FontSize::Px(12.0), ..default() }, ChildOf(column)));
        bench.toggled_row.get_or_insert(row);
    }
}

fn before_layout(mut bench: ResMut<Bench>) {
    bench.layout_start = Some(Instant::now());
}
fn after_layout(mut bench: ResMut<Bench>) {
    let Some(start) = bench.layout_start.take() else { return };
    let us = start.elapsed().as_secs_f64() * 1e6;
    let frame = bench.frames;
    if bench.mode == Mode::Layout && frame > 20 {
        if frame <= 220 {
            bench.forced.push(us);
        } else {
            bench.unchanged.push(us);
        }
    }
}
fn toggle(mut bench: ResMut<Bench>, mut nodes: Query<&mut Node>) {
    bench.frames += 1;
    if bench.mode == Mode::Layout && bench.frames <= 220 {
        if let Some(mut node) = bench.toggled_row.and_then(|row| nodes.get_mut(row).ok()) {
            node.width = if bench.frames % 2 == 0 { px(900.0) } else { percent(100.0) };
        }
    }
}
fn finish(mut bench: ResMut<Bench>, mut exit: MessageWriter<AppExit>) {
    match bench.mode {
        Mode::Layout if bench.frames > 420 => {
            let median = |v: &mut Vec<f64>| {
                v.sort_by(f64::total_cmp);
                v[v.len() / 2]
            };
            let (forced, unchanged) = (median(&mut bench.forced), median(&mut bench.unchanged));
            println!("{} elements: layout with one width changed {forced:.1} µs, unchanged {unchanged:.1} µs", bench.elements);
            exit.write(AppExit::Success);
        }
        Mode::FirstFrame(t0) if bench.frames >= 1 => {
            println!("{:.3}", now_ns().saturating_sub(t0) as f64 / 1e6);
            exit.write(AppExit::Success);
        }
        Mode::Idle(seconds) => {
            let elapsed = bench.started.elapsed().as_secs_f64();
            if bench.settle.is_none() && elapsed >= 1.0 {
                let (cpu, switches, _) = usage();
                bench.settle = Some((cpu, switches));
            }
            if elapsed >= seconds {
                let (cpu, switches, rss) = usage();
                let (cpu0, switches0) = bench.settle.unwrap();
                println!(
                    "idle {:.0} s after the first: {} frames total, {:.3} s CPU, {} voluntary context switches; peak RSS {:.1} MiB",
                    seconds - 1.0,
                    bench.frames,
                    cpu - cpu0,
                    switches - switches0,
                    rss as f64 / 1024.0
                );
                exit.write(AppExit::Success);
            }
        }
        _ => {}
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).map(|v| v.parse::<f64>().unwrap());
    if let Some(n) = arg("--launch") {
        let mut times = Vec::new();
        for _ in 0..n as usize {
            let t0 = now_ns();
            let out = std::process::Command::new(&args[0]).arg("--first-frame").env("UI_T0_NS", t0.to_string()).output().unwrap();
            times.push(String::from_utf8_lossy(&out.stdout).trim().lines().last().unwrap().parse::<f64>().unwrap());
            std::thread::sleep(Duration::from_millis(100));
        }
        times.sort_by(f64::total_cmp);
        println!("first frame, ms after spawn: median {:.2}, min {:.2}, max {:.2}", times[times.len() / 2], times[0], times[times.len() - 1]);
        return;
    }
    let (mode, elements) = if let Some(n) = arg("--layout") {
        (Mode::Layout, n as usize)
    } else if args.iter().any(|a| a == "--first-frame") {
        (Mode::FirstFrame(std::env::var("UI_T0_NS").unwrap().parse().unwrap()), 200)
    } else {
        (Mode::Idle(arg("--idle").unwrap_or(10.0)), 200)
    };
    let mut app = App::new();
    app.add_plugins(DefaultPlugins.set(WindowPlugin {
        primary_window: Some(Window { title: "Bevy UI bench".into(), resolution: (1280, 800).into(), ..default() }),
        ..default()
    }))
    .insert_resource(Bench { elements, mode, frames: 0, layout_start: None, forced: Vec::new(), unchanged: Vec::new(), toggled_row: None, started: Instant::now(), settle: None })
    .add_systems(Startup, setup)
    .add_systems(Update, toggle)
    .add_systems(PostUpdate, (before_layout.before(UiSystems::Layout), after_layout.after(UiSystems::Layout)))
    .add_systems(Last, finish);
    if let Mode::Idle(_) = mode {
        app.insert_resource(WinitSettings::desktop_app());
    } else {
        app.insert_resource(WinitSettings::continuous());
    }
    app.run();
}
