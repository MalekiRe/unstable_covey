//! Subtree toggles allocate nothing once warm: the walk follows link lists,
//! and warm table moves reuse their columns.
#![cfg(not(feature = "devtools"))]
use static_ecs::prelude::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

/// Process-wide: this binary's only test.
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

struct ChildOf;
impl Relation for ChildOf {
    type Kind = OneToMany;
    const LINKED: bool = true;
    const ORDERED: bool = true;
}
#[derive(Default)]
struct Closed;
impl Disabling for Closed {}
struct Label(#[allow(dead_code)] u32);

#[test]
fn closing_and_opening_a_warm_panel_allocates_nothing() {
    let mut world = App::new()
        .relation::<ChildOf>()
        .register::<Label>()
        .register::<Closed>()
        .register::<DisabledRoot<Closed>>()
        .build()
        .world;
    // A 500-entity panel: 20 rows of 24 cells.
    let panel = world.spawn_bundle((Label(0),));
    for row in 0..20 {
        let row = world.spawn_bundle((Label(row), ChildOf.to(panel)));
        for cell in 0..24 {
            world.spawn_bundle((Label(cell), ChildOf.to(row)));
        }
    }
    for _ in 0..2 {
        world.disable_subtree::<Closed, ChildOf>(panel);
        world.enable_subtree::<Closed, ChildOf>(panel);
    }
    let before = ALLOCATIONS.load(Relaxed);
    for _ in 0..10 {
        world.disable_subtree::<Closed, ChildOf>(panel);
        assert_eq!(world.query::<&Label>().iter_mut().count(), 0);
        world.enable_subtree::<Closed, ChildOf>(panel);
    }
    assert_eq!(ALLOCATIONS.load(Relaxed) - before, 0);
    assert_eq!(world.query::<&Label>().iter_mut().count(), 501);
}
