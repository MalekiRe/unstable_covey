//! Layout time, static-ui-layout against clay.h, on UI-shaped trees of 1k
//! and 10k elements. Both declare the tree and lay it out each frame (Clay's
//! only mode); "layout only" re-lays-out an already declared tree, as
//! static-ui does when nothing structural changed.
//!
//! cargo run --release -p static-ui-clay-oracle --example bench
use std::hint::black_box;
use std::time::Instant;

use static_ui_clay_oracle::{Clay, declare, grid_case, measure};
use static_ui_layout::{TextCache, Tree};

fn median(mut samples: Vec<f64>) -> f64 {
    samples.sort_by(f64::total_cmp);
    samples[samples.len() / 2]
}
fn time(runs: usize, mut f: impl FnMut()) -> f64 {
    for _ in 0..runs / 10 + 1 {
        f();
    }
    median(
        (0..runs)
            .map(|_| {
                let start = Instant::now();
                f();
                start.elapsed().as_secs_f64() * 1e6
            })
            .collect(),
    )
}

fn main() {
    let mut clay = Clay::new(65536);
    println!("| elements | clay.h | static-ui-layout | ratio | layout only |");
    println!("|---:|---:|---:|---:|---:|");
    for count in [1_000, 10_000] {
        let case = grid_case(count);
        let runs = if count > 5000 { 300 } else { 3000 };
        let clay_us = time(runs, || clay.layout_only(black_box(&case)));
        let mut tree = Tree::new();
        let mut cache = TextCache::default();
        let ours_us = time(runs, || {
            declare(&mut tree, black_box(&case));
            tree.layout(&mut cache, measure);
        });
        let layout_us = time(runs, || tree.layout(&mut cache, measure));
        println!(
            "| {} | {clay_us:.1} µs | {ours_us:.1} µs | {:.2}× | {layout_us:.1} µs |",
            case.nodes.len(),
            ours_us / clay_us
        );
    }
}
