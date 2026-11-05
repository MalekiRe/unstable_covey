//! One system, one archetype, dense storage: the query loop should compile to
//! the same vector loop as `ideal` over raw slices. Inspect with
//! `objdump -d --disassemble=integrate_fold target/release/examples/query_loop`.
use static_ecs::prelude::*;
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

pub struct Position(f32);
pub struct Velocity(f32);
struct Update;
impl Schedule for Update {}

#[unsafe(no_mangle)]
#[inline(never)]
pub fn integrate(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut position, velocity) in &mut query {
        position.0 += velocity.0;
    }
}

/// Internal iteration: `for_each` drives `QueryIter::fold`, not `next`.
#[unsafe(no_mangle)]
#[inline(never)]
pub fn integrate_fold(mut query: Query<(&mut Position, &Velocity)>) {
    (&mut query)
        .into_iter()
        .for_each(|(mut position, velocity)| position.0 += velocity.0);
}

/// Per-table slices, per-row `Mut` stamping.
#[unsafe(no_mangle)]
#[inline(never)]
pub fn integrate_chunk_rows(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut positions, velocities) in query.contiguous_mut().unwrap() {
        for (mut position, velocity) in positions.iter_mut().zip(velocities) {
            position.0 += velocity.0;
        }
    }
}

/// Per-table slices, whole-slice stamping.
#[unsafe(no_mangle)]
#[inline(never)]
pub fn integrate_chunks(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut positions, velocities) in query.contiguous_mut().unwrap() {
        for (position, velocity) in positions.as_mut_slice().iter_mut().zip(velocities) {
            position.0 += velocity.0;
        }
    }
}

/// What the loop means: add, stamp the change tick, raise the column summary once.
#[unsafe(no_mangle)]
#[inline(never)]
pub fn ideal(
    positions: &mut [Position],
    velocities: &[Velocity],
    changed: &mut [u64],
    summary: &AtomicU64,
    tick: u64,
) {
    let n = positions.len();
    let (velocities, changed) = (&velocities[..n], &mut changed[..n]);
    for i in 0..n {
        positions[i].0 += velocities[i].0;
        changed[i] = tick;
    }
    summary.fetch_max(tick, Ordering::Relaxed);
}

const N: usize = 1 << 20;
const FRAMES: u32 = 200;

macro_rules! bench {
    ($system:expr) => {{
        let mut app = App::new().add_systems(Update, $system).build();
        app.world
            .spawn_batch((0..N).map(|i| (Position(i as f32), Velocity(1.0))));
        for _ in 0..10 {
            app.emit(black_box(&Update));
        }
        let start = Instant::now();
        for _ in 0..FRAMES {
            app.emit(black_box(&Update));
        }
        start.elapsed().as_secs_f64() * 1e9 / (FRAMES as f64 * N as f64)
    }};
}

fn main() {
    let mut positions: Vec<_> = (0..N).map(|i| Position(i as f32)).collect();
    let velocities: Vec<_> = (0..N).map(|_| Velocity(1.0)).collect();
    let mut changed = vec![0u64; N];
    let summary = AtomicU64::new(0);
    for tick in 0..10 {
        ideal(
            black_box(&mut positions),
            &velocities,
            &mut changed,
            &summary,
            tick,
        );
    }
    let start = Instant::now();
    for tick in 0..FRAMES as u64 {
        ideal(
            black_box(&mut positions),
            &velocities,
            &mut changed,
            &summary,
            tick,
        );
    }
    let raw = start.elapsed().as_secs_f64() * 1e9 / (FRAMES as f64 * N as f64);
    for (name, ns) in [
        ("ideal (raw slices)", raw),
        ("for loop (next)", bench!(integrate)),
        ("for_each (fold)", bench!(integrate_fold)),
        ("chunks, per-row Mut", bench!(integrate_chunk_rows)),
        ("chunks, as_mut_slice", bench!(integrate_chunks)),
    ] {
        println!("{name:22} {ns:6.3} ns/entity  {:5.2}x", ns / raw);
    }
}
