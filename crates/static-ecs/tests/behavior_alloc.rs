//! Warm behaviors allocate nothing: deliveries reuse waiter lists, wakers are
//! reused per instance, and `cx.with` keeps its parameter state per site.
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

use static_ecs::{Behavior, Cx, Inbox, behavior};

struct Frame;
impl Schedule for Frame {}
#[derive(Clone)]
struct Press;
struct Count(u32);

struct Counter;
impl Behavior for Counter {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            cx.on::<Press>(me).await;
            cx.with(|mut q: Query<&mut Count>| q.get_mut(me).unwrap().0 += 1).unwrap();
        }
    }
}

#[test]
fn delivering_and_polling_warm_behaviors_allocates_nothing() {
    let mut app = App::new()
        .add_plugins(behavior::<Counter, _>(Frame))
        .register::<Count>()
        .register::<Inbox<Press>>()
        .build();
    let buttons: Vec<Entity> = (0..100).map(|_| app.world.spawn_bundle((Counter, Count(0)))).collect();
    let mut round = |app: &mut App<_, _>| {
        for &button in buttons.iter().step_by(7) {
            app.world.deliver(button, Press);
        }
        app.emit(&Frame);
    };
    for _ in 0..3 {
        round(&mut app);
    }
    let before = ALLOCATIONS.load(Relaxed);
    for _ in 0..20 {
        round(&mut app);
    }
    assert_eq!(ALLOCATIONS.load(Relaxed) - before, 0);
    // The first round's presses came before the futures started waiting.
    assert_eq!(app.world.get::<Count>(buttons[7]).unwrap().0, 22);
}
