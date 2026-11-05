//! Joins allocate nothing once warm: every join shape, both storages, the
//! closure and lending forms, and `cascade` through its reused order buffer.
//! Devtools record every tick and frame, so they allocate by design.
#![cfg(not(feature = "devtools"))]
use static_ecs::prelude::*;
use static_ecs::{From, To};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Process-wide: systems may run on worker threads. This binary's only test.
struct Counting;
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Relaxed);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations(run: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.load(Relaxed);
    run();
    ALLOCATIONS.load(Relaxed) - before
}

struct Update;
impl Schedule for Update {}
struct DockedAt;
impl Relation for DockedAt {
    type Kind = OneToMany;
    const ACYCLIC: bool = true;
}
struct InSquad;
impl Relation for InSquad {
    type Kind = OneToMany;
    const FRAGMENTING: bool = true;
}
struct Likes;
impl Relation for Likes {
    type Kind = ManyToMany;
}
struct Fuel(u64);

type Gather = (
    From<DockedAt, &'static Fuel>,
    From<InSquad, &'static Fuel>,
    To<Likes, &'static Fuel>,
);
type Docked = (To<DockedAt, &'static Fuel>, &'static mut Fuel);
type Refuel = (
    &'static Fuel,
    To<DockedAt, &'static mut Fuel>,
    To<InSquad, Entity>,
);

#[test]
fn warm_joins_allocate_nothing() {
    assert_eq!(allocations(|| drop(std::hint::black_box(Box::new(1)))), 1);
    let mut app = App::new()
        .relation::<DockedAt>()
        .relation::<InSquad>()
        .relation::<Likes>()
        .register::<Fuel>()
        .add_systems(Update, |mut q: Query<Refuel>| {
            q.each(|(own, station, squad)| {
                if let (Some(mut station), Some(_)) = (station, squad) {
                    station.0 += own.0;
                }
            });
        })
        .add_systems(Update, |mut q: Query<Gather>| {
            let mut rows = q.iter_lending();
            while let Some((docked, squad, likes)) = rows.fetch_next() {
                let all = Iterator::chain(Iterator::chain(docked, squad), likes);
                std::hint::black_box(all.map(|f| f.0).sum::<u64>());
            }
        })
        .add_systems(Update, |mut q: Query<Docked>| {
            q.cascade(|(up, mut fuel)| fuel.0 = fuel.0.wrapping_add(up.map_or(0, |u| u.0)));
        })
        .build();
    let world = &mut app.world;
    let stations: Vec<_> = (0..4).map(|i| world.spawn_bundle((Fuel(i),))).collect();
    let squads: Vec<_> = (0..3).map(|i| world.spawn_bundle((Fuel(i),))).collect();
    for i in 0..64 {
        let ship = world.spawn_bundle((Fuel(1),));
        world.relate(ship, DockedAt, stations[i % 4]);
        world.relate(ship, InSquad, squads[i % 3]);
        world.relate(ship, Likes, stations[i % 2]);
        world.relate(ship, Likes, squads[i % 3]);
    }
    // Stations dock at each other: cascade has depth.
    world.relate(stations[1], DockedAt, stations[0]);
    world.relate(stations[2], DockedAt, stations[1]);
    // Warm: query states and the cascade order buffer reach their size.
    app.emit(&Update);
    app.emit(&Update);
    assert_eq!(allocations(|| (0..8).for_each(|_| app.emit(&Update))), 0);
}
