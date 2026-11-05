use std::future::poll_fn;
use std::pin::pin;
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::task::{Poll, Waker};

use static_ecs::prelude::*;
use static_ecs::{Behavior, Cx, Inbox, Wakeup, behavior};

struct Frame;
impl Schedule for Frame {}
#[derive(Clone, Debug, PartialEq)]
struct Press(u32);
#[derive(Clone)]
struct Release;
struct Count(u32);

/// Polls of every `Counter` future, and drops of them.
static POLLS: AtomicUsize = AtomicUsize::new(0);
static DROPS: AtomicUsize = AtomicUsize::new(0);
struct OnDrop;
impl Drop for OnDrop {
    fn drop(&mut self) {
        DROPS.fetch_add(1, Relaxed);
    }
}

/// Counts presses into `Count`; every poll is counted.
struct Counter;
impl Behavior for Counter {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        let _guard = OnDrop;
        let mut body = pin!(async move {
            loop {
                let Press(n) = cx.on::<Press>(me).await;
                cx.with(|mut q: Query<&mut Count>| q.get_mut(me).unwrap().0 += n).unwrap();
            }
        });
        poll_fn(|context| {
            POLLS.fetch_add(1, Relaxed);
            body.as_mut().poll(context)
        })
        .await
    }
}

/// The tests share the counters: run them one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

#[test]
fn a_delivery_polls_exactly_the_awaiting_behavior() {
    let _serial = SERIAL.lock().unwrap();
    let mut app = App::new()
        .add_plugins(behavior::<Counter, _>(Frame))
        .register::<Count>()
        .register::<Inbox<Press>>()
        .register::<Inbox<Release>>()
        .build();
    let buttons: Vec<Entity> = (0..1000).map(|_| app.world.spawn_bundle((Counter, Count(0)))).collect();
    POLLS.store(0, Relaxed);
    app.emit(&Frame);
    assert_eq!(POLLS.swap(0, Relaxed), 1000, "each starts once");
    for _ in 0..5 {
        app.emit(&Frame);
    }
    assert_eq!(POLLS.swap(0, Relaxed), 0, "idle behaviors are not polled");
    assert!(app.world.deliver(buttons[17], Press(3)));
    assert!(!app.world.deliver(buttons[18], Release), "nobody awaits Release");
    app.emit(&Frame);
    assert_eq!(POLLS.swap(0, Relaxed), 1);
    assert_eq!(app.world.get::<Count>(buttons[17]).unwrap().0, 3);
    assert!(buttons.iter().enumerate().all(|(i, &b)| i == 17 || app.world.get::<Count>(b).unwrap().0 == 0));
}

#[test]
fn removing_the_marker_or_despawning_drops_the_future_and_its_waiters() {
    let _serial = SERIAL.lock().unwrap();
    let mut app = App::new()
        .add_plugins(behavior::<Counter, _>(Frame))
        .register::<Count>()
        .register::<Inbox<Press>>()
        .build();
    let (a, b) = (app.world.spawn_bundle((Counter, Count(0))), app.world.spawn_bundle((Counter, Count(0))));
    app.emit(&Frame);
    let waiters = |app: &App<_, _>, e| app.world.resource::<Inbox<Press>>().unwrap().waiters(e);
    assert_eq!((waiters(&app, a), waiters(&app, b)), (1, 1));
    DROPS.store(0, Relaxed);
    app.world.remove::<Counter>(a);
    app.world.despawn(b);
    app.emit(&Frame);
    assert_eq!(DROPS.load(Relaxed), 2);
    assert_eq!((waiters(&app, a), waiters(&app, b)), (0, 0));
    assert!(!app.world.deliver(a, Press(1)));
    // Re-adding starts a fresh future.
    app.world.insert(a, Counter);
    app.emit(&Frame);
    app.world.deliver(a, Press(4));
    app.emit(&Frame);
    assert_eq!(app.world.get::<Count>(a).unwrap().0, 4);
}

#[test]
fn hidden_entities_freeze_their_behaviors_until_shown() {
    let _serial = SERIAL.lock().unwrap();
    let mut app = App::new()
        .add_plugins(behavior::<Counter, _>(Frame))
        .register::<Count>()
        .register::<Disabled>()
        .register::<Inbox<Press>>()
        .build();
    let button = app.world.spawn_bundle((Counter, Count(0)));
    app.emit(&Frame);
    app.world.insert(button, Disabled);
    assert!(app.world.deliver(button, Press(2)));
    POLLS.store(0, Relaxed);
    app.emit(&Frame);
    app.emit(&Frame);
    assert_eq!(POLLS.load(Relaxed), 0, "frozen while disabled");
    assert_eq!(app.world.get::<Count>(button).unwrap().0, 0);
    app.world.remove::<Disabled>(button);
    app.emit(&Frame);
    assert_eq!(POLLS.load(Relaxed), 1);
    assert_eq!(app.world.get::<Count>(button).unwrap().0, 2);
    // Started while hidden: the first poll waits too.
    let hidden = app.world.spawn_bundle((Counter, Count(0), Disabled));
    POLLS.store(0, Relaxed);
    app.emit(&Frame);
    assert_eq!(POLLS.load(Relaxed), 0);
    app.world.remove::<Disabled>(hidden);
    app.emit(&Frame);
    assert_eq!(POLLS.load(Relaxed), 1);
}

/// Races `Press` against `Release` every round: the loser stops waiting.
struct Racer;
impl Behavior for Racer {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            let mut press = pin!(cx.on::<Press>(me));
            let mut release = pin!(cx.on::<Release>(me));
            let pressed = poll_fn(|context| match press.as_mut().poll(context) {
                Poll::Ready(Press(n)) => Poll::Ready(Some(n)),
                Poll::Pending => release.as_mut().poll(context).map(|_| None),
            })
            .await;
            cx.with(|mut q: Query<&mut Count>| q.get_mut(me).unwrap().0 += pressed.unwrap_or(100)).unwrap();
        }
    }
}

#[test]
fn racing_events_leaves_no_stale_waiters() {
    let mut app = App::new()
        .add_plugins(behavior::<Racer, _>(Frame))
        .register::<Count>()
        .register::<Inbox<Press>>()
        .register::<Inbox<Release>>()
        .build();
    let racer = app.world.spawn_bundle((Racer, Count(0)));
    app.emit(&Frame);
    for round in 0..50 {
        if round % 2 == 0 {
            app.world.deliver(racer, Press(1));
        } else {
            app.world.deliver(racer, Release);
        }
        app.emit(&Frame);
        assert_eq!(app.world.resource::<Inbox<Press>>().unwrap().waiters(racer), 1);
        assert_eq!(app.world.resource::<Inbox<Release>>().unwrap().waiters(racer), 1);
    }
    assert_eq!(app.world.get::<Count>(racer).unwrap().0, 25 + 25 * 100);
}

/// A one-shot signal set from another thread.
#[derive(Default)]
struct Signal(Mutex<(bool, Option<Waker>)>);
impl Signal {
    fn set(&self) {
        let mut state = self.0.lock().unwrap();
        state.0 = true;
        if let Some(waker) = state.1.take() {
            waker.wake();
        }
    }
}
struct SignalSlot(Arc<Signal>);

/// Waits for its `SignalSlot`, then counts once.
struct Waiter;
impl Behavior for Waiter {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        let signal = cx.with(|mut q: Query<&SignalSlot>| q.get_mut(me).unwrap().0.clone()).unwrap();
        poll_fn(|context| {
            let mut state = signal.0.lock().unwrap();
            if state.0 {
                return Poll::Ready(());
            }
            state.1 = Some(context.waker().clone());
            Poll::Pending
        })
        .await;
        cx.with(|mut q: Query<&mut Count>| q.get_mut(me).unwrap().0 += 1).unwrap();
    }
}

#[test]
fn external_wakers_queue_the_behavior_and_call_wakeup() {
    let mut app = App::new()
        .add_plugins(behavior::<Waiter, _>(Frame))
        .register::<Count>()
        .register::<SignalSlot>()
        .build();
    let wakeups = Arc::new(AtomicUsize::new(0));
    let counted = wakeups.clone();
    app.world.insert_resource(Wakeup(Arc::new(move || {
        counted.fetch_add(1, Relaxed);
    })));
    let signal = Arc::new(Signal::default());
    let entity = app.world.spawn_bundle((Waiter, Count(0), SignalSlot(signal.clone())));
    app.emit(&Frame);
    app.emit(&Frame);
    assert_eq!(app.world.get::<Count>(entity).unwrap().0, 0);
    std::thread::spawn(move || signal.set()).join().unwrap();
    assert_eq!(wakeups.load(Relaxed), 1, "the event loop hears about it");
    app.emit(&Frame);
    assert_eq!(app.world.get::<Count>(entity).unwrap().0, 1);
}

/// Despawns itself on its first press.
struct SelfDestruct;
impl Behavior for SelfDestruct {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        cx.on::<Press>(me).await;
        cx.with(|mut commands: Commands<S>| commands.despawn(me)).unwrap();
        // The entity is gone, but this future runs until it yields.
        assert!(cx.with(|mut q: Query<&Count>| q.get_mut(me).is_none()).unwrap());
        cx.on::<Press>(me).await;
        unreachable!("dropped before another poll");
    }
}

#[test]
fn a_behavior_may_despawn_its_own_entity() {
    let mut app = App::new()
        .add_plugins(behavior::<SelfDestruct, _>(Frame))
        .register::<Count>()
        .register::<Inbox<Press>>()
        .build();
    let entity = app.world.spawn_bundle((SelfDestruct, Count(0)));
    app.emit(&Frame);
    app.world.deliver(entity, Press(0));
    app.emit(&Frame);
    assert!(!app.world.contains(entity));
    app.emit(&Frame);
}

/// Counts a press, then waits for its release.
struct PressRelease;
impl Behavior for PressRelease {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            let Press(n) = cx.on::<Press>(me).await;
            cx.on::<Release>(me).await;
            cx.with(|mut q: Query<&mut Count>| q.get_mut(me).unwrap().0 += n).unwrap();
        }
    }
}

#[test]
fn a_burst_delivered_while_suspended_is_handled_in_order() {
    let mut app = App::new()
        .add_plugins(behavior::<PressRelease, _>(Frame))
        .register::<Count>()
        .register::<Inbox<Press>>()
        .register::<Inbox<Release>>()
        .build();
    let entity = app.world.spawn_bundle((PressRelease, Count(0)));
    app.emit(&Frame); // waits for a press; listens for releases once it awaits one
    app.world.deliver(entity, Press(1));
    app.emit(&Frame);
    // Release and the next press arrive before it runs again.
    assert!(app.world.deliver(entity, Release));
    assert!(app.world.deliver(entity, Press(10)));
    assert!(app.world.deliver(entity, Release));
    app.emit(&Frame);
    assert_eq!(app.world.get::<Count>(entity).unwrap().0, 11);
    // Events from before it last ran are not replayed.
    app.emit(&Frame);
    assert_eq!(app.world.get::<Count>(entity).unwrap().0, 11);
}
