//! Once warm, input costs no allocation on the Rust heap: pointer motion,
//! buttons, the wheel, and keys with their text all go from the socket to the
//! app's systems without allocating. Allocations are counted only inside
//! `step`, on the test thread; the scripted compositor's thread is not
//! counted. (libwayland itself, in C, mallocs a closure per event.)
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::Duration;

use static_ecs::prelude::*;
use static_window::*;
use support::compositor::{Compositor, Globals};

struct Counting;
thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
fn note() {
    if COUNTING.get() {
        ALLOCATIONS.set(ALLOCATIONS.get() + 1);
    }
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations made by `f` on this thread.
fn counted(f: impl FnOnce()) -> usize {
    ALLOCATIONS.set(0);
    COUNTING.set(true);
    f();
    COUNTING.set(false);
    ALLOCATIONS.get()
}

/// What the app saw, without allocating.
#[derive(Default, Clone, Copy, Debug, PartialEq)]
struct Tally {
    keys: u32,
    text: u32,
    moves: u32,
    buttons: u32,
    scrolls: u32,
}

const NOW: Option<Duration> = Some(Duration::ZERO);
const A: u32 = 30;
const BTN_LEFT: u32 = 0x110;

#[test]
fn warm_input_events_allocate_nothing() {
    let mut app = App::new()
        .add_plugins(WindowPlugin)
        .init_resource::<Tally>()
        .add_systems(label::<KeyInput>(), |_: &KeyInput, mut t: ResMut<Tally>| t.keys += 1)
        .add_systems(label::<TextInput>(), |e: &TextInput, mut t: ResMut<Tally>| t.text += e.text().len() as u32)
        .add_systems(label::<PointerMoved>(), |_: &PointerMoved, mut t: ResMut<Tally>| t.moves += 1)
        .add_systems(label::<PointerButton>(), |_: &PointerButton, mut t: ResMut<Tally>| t.buttons += 1)
        .add_systems(label::<Scroll>(), |_: &Scroll, mut t: ResMut<Tally>| t.scrolls += 1)
        .build();
    let (compositor, socket) = Compositor::spawn(Globals::default());
    let mut lp = WindowLoop::from_socket(socket).unwrap();
    app.world.spawn_bundle((Window::new("alloc").size(64, 32),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 0, 0, &[]));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        s.keyboard_enter(0, &[]);
        s.pointer_enter(0, 1.0, 1.0);
    });
    lp.step(&mut app, NOW);

    // Each round: a motion, a click, a wheel notch, and a typed `a`.
    let mut round = |app: &mut _, lp: &mut WindowLoop, i: u32| {
        compositor.with(move |s, _| {
            s.motion(f64::from(i % 50), 2.0);
            s.button(BTN_LEFT, true);
            s.button(BTN_LEFT, false);
            s.wheel(10.0, 120);
            s.key(A, true);
            s.key(A, false);
        });
        counted(|| {
            lp.step(app, NOW);
        })
    };
    for i in 0..64 {
        round(&mut app, &mut lp, i);
    }
    let before = *app.world.resource::<Tally>().unwrap();
    let allocations: Vec<usize> = (64..256).map(|i| round(&mut app, &mut lp, i)).collect();
    let after = *app.world.resource::<Tally>().unwrap();
    let n = allocations.len() as u32;
    let seen = Tally {
        keys: after.keys - before.keys,
        text: after.text - before.text,
        moves: after.moves - before.moves,
        buttons: after.buttons - before.buttons,
        scrolls: after.scrolls - before.scrolls,
    };
    assert_eq!(seen, Tally { keys: 2 * n, text: n, moves: n, buttons: 2 * n, scrolls: n }, "every event arrived");
    assert!(allocations.iter().all(|&a| a == 0), "allocations per round: {allocations:?}");
}
