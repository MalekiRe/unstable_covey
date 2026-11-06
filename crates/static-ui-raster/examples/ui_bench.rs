//! Headless UI timings for UI_BENCHMARKS.md: layout of a settings-list UI
//! at 1k and 10k elements, open/close by subtree size, an idle frame with
//! 10,000 behaviors, and hit testing. Text is measured with the real fonts.
//!
//! cargo run --release -p static-ui-raster --example ui_bench
use std::hint::black_box;
use std::time::Instant;

use static_ecs::prelude::*;
use static_ecs::{Behavior, Cx, behavior};
use static_ui::*;
use static_ui_raster::Fonts;
use static_window::PointerMoved;

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}
fn time(runs: usize, mut f: impl FnMut()) -> f64 {
    for _ in 0..runs / 10 + 1 {
        f();
    }
    median((0..runs).map(|_| {
        let start = Instant::now();
        f();
        start.elapsed().as_secs_f64() * 1e6
    }).collect())
}

const WORDS: [&str; 8] = ["Settings", "Volume", "Brightness", "Network", "Wi-Fi", "Bluetooth", "Display", "About"];

/// A scrolling settings list: per row an icon, and a title over a subtitle
/// (5 elements per row).
fn settings<S: Storage>(world: &mut World<S>, elements: usize) -> (Entity, Entity) {
    let panel = world.spawn_ui(el((
        Node::column().width(Sizing::GROW).height(Sizing::GROW).padding(Padding::all(8)).gap(4),
        Background(Color::rgb(245, 245, 248)),
        Clip { horizontal: false, vertical: true },
        Scroll::default(),
    )));
    let mut first = None;
    for r in 0..elements / 5 {
        let row = world.spawn_ui_under(
            panel,
            el((Node::row().width(Sizing::GROW).gap(6).align(AlignX::Left, AlignY::Center), Background(Color::WHITE))).children((
                el((Node::row().width(Sizing::fixed(24.0)).height(Sizing::fixed(24.0)), Background(Color::rgb(66, 133, 244)))),
                el((Node::column().width(Sizing::GROW),)).children((
                    el((Text::new(WORDS[r % 8], 14),)),
                    el((Text::new(WORDS[(r + 3) % 8], 12),)),
                )),
            )),
        );
        first.get_or_insert(row);
    }
    (panel, first.unwrap())
}

macro_rules! app {
    () => {
        App::new().add_plugins((UiPlugin::<Fonts>::default(), InputPlugin)).build()
    };
}
macro_rules! frame {
    ($app:expr) => {{
        $app.emit(&UiUpdate);
        $app.emit(&UiFrame);
    }};
}

struct Idle;
impl Behavior for Idle {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            cx.on::<Click>(me).await;
        }
    }
}

fn main() {
    println!("## Layout (settings list, real fonts)\n");
    println!("| elements | flatten + layout frame | relayout frame (one width changed) | layout system | Clay passes | unchanged frame |");
    println!("|---:|---:|---:|---:|---:|---:|");
    for elements in [1_000, 10_000] {
        let mut app = app!();
        app.world.insert_resource(Viewport { width: 1280.0, height: 800.0 });
        let (_, row) = settings(&mut app.world, elements);
        let runs = if elements > 5000 { 200 } else { 2000 };
        // Full flattening and layout: force a rebuild each run.
        let rebuild = time(runs, || {
            if let Some(mut ui) = app.world.resource_mut::<UiLayout>() {
                ui.mark_structure_dirty();
            }
            frame!(app);
        });
        let mut wide = false;
        let relayout = time(runs, || {
            wide = !wide;
            app.world.insert(row, Node::row().width(if wide { Sizing::fixed(900.0) } else { Sizing::GROW }).gap(6).align(AlignX::Left, AlignY::Center));
            frame!(app);
        });
        let (mut system, mut passes) = (Vec::new(), Vec::new());
        for _ in 0..runs {
            wide = !wide;
            app.world.insert(row, Node::row().width(if wide { Sizing::fixed(900.0) } else { Sizing::GROW }).gap(6).align(AlignX::Left, AlignY::Center));
            frame!(app);
            let ui = app.world.resource::<UiLayout>().unwrap();
            system.push(ui.layout_time().as_secs_f64() * 1e6);
            passes.push(ui.pass_time().as_secs_f64() * 1e6);
        }
        let unchanged = time(runs, || frame!(app));
        let count = app.world.resource::<UiLayout>().unwrap().tree().len();
        println!("| {count} | {rebuild:.1} µs | {relayout:.1} µs | {:.1} µs | {:.1} µs | {unchanged:.2} µs |", median(system), median(passes));
    }

    println!("\n## Opening and closing a panel\n");
    println!("| panel entities | close | open | frame after (flatten + layout) |");
    println!("|---:|---:|---:|---:|");
    for size in [10usize, 100, 500, 2_000] {
        let mut app = app!();
        let (panel, _) = settings(&mut app.world, size);
        frame!(app);
        let entities = app.world.descendants::<ChildOf>(panel).count() + 1;
        let (mut close, mut open, mut after) = (Vec::new(), Vec::new(), Vec::new());
        for _ in 0..200 {
            let start = Instant::now();
            app.world.disable_subtree::<Closed, ChildOf>(panel);
            close.push(start.elapsed().as_secs_f64() * 1e6);
            frame!(app);
            let start = Instant::now();
            app.world.enable_subtree::<Closed, ChildOf>(panel);
            open.push(start.elapsed().as_secs_f64() * 1e6);
            let start = Instant::now();
            frame!(app);
            after.push(start.elapsed().as_secs_f64() * 1e6);
        }
        println!("| {entities} | {:.1} µs | {:.1} µs | {:.1} µs |", median(close), median(open), median(after));
    }

    println!("\n## Idle behaviors\n");
    println!("| behaviors | idle frame (UiUpdate + UiFrame) |");
    println!("|---:|---:|");
    for count in [0usize, 10_000] {
        let mut app = App::new().add_plugins((UiPlugin::<Fonts>::default(), InputPlugin, behavior::<Idle, _>(UiUpdate))).build();
        let list = app.world.spawn_ui(el((Node::column(),)));
        for _ in 0..count {
            app.world.spawn_ui_under(list, el((Node::row().width(Sizing::fixed(10.0)).height(Sizing::fixed(1.0)), Idle)));
        }
        frame!(app);
        println!("| {count} | {:.2} µs |", time(2000, || frame!(app)));
    }

    println!("\n## Hit testing (one pointer move)\n");
    println!("| elements | pointer move |");
    println!("|---:|---:|");
    for elements in [1_000, 10_000] {
        let mut app = app!();
        app.world.insert_resource(Viewport { width: 1280.0, height: 800.0 });
        settings(&mut app.world, elements);
        frame!(app);
        let mut y = 0.0;
        let us = time(2000, || {
            y = (y + 7.0) % 800.0;
            app.emit(black_box(&PointerMoved { window: Entity::PLACEHOLDER, x: 400.0, y }));
        });
        println!("| {} | {us:.2} µs |", app.world.resource::<UiLayout>().unwrap().tree().len());
    }
}
