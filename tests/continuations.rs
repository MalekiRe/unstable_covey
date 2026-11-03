use static_ecs::prelude::*;
use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};
struct Update;
struct Prefix;
struct Fork;
struct Join;

#[test]
fn narrow_prefix_reactivates_workers_and_nested_groups_join() {
    let pool = WorkerPool::new(3);
    pool.install(|| {
        let rendezvous = Arc::new(Barrier::new(2));
        let left = rendezvous.clone();
        let right = rendezvous;
        let count = Arc::new(AtomicUsize::new(0));
        let a = count.clone();
        let b = count.clone();
        let observed = count.clone();
        let mut app = App::new()
            .add_systems(
                Update,
                ready((
                    node(Prefix, || {}),
                    node(
                        Fork,
                        (
                            move || {
                                left.wait();
                                a.fetch_add(1, Ordering::SeqCst);
                            },
                            move || {
                                right.wait();
                                b.fetch_add(1, Ordering::SeqCst);
                            },
                        )
                            .parallel(),
                    )
                    .after(Prefix),
                    node(Join, move |mut invocation: Local<usize>| {
                        *invocation += 1;
                        assert_eq!(observed.load(Ordering::SeqCst), *invocation * 2);
                    })
                    .after(Fork),
                )),
            )
            .build();
        for _ in 0..100 {
            app.emit(&Update);
        }
        assert_eq!(count.load(Ordering::SeqCst), 200);
    });
}

#[test]
fn flattened_condition_skips_preserve_locals_and_release_dependents() {
    WorkerPool::new(3).install(|| {
        let count = Arc::new(AtomicUsize::new(0));
        let a = count.clone();
        let b = count.clone();
        let observed = count.clone();
        let visits = Arc::new(Mutex::new(Vec::new()));
        let output = visits.clone();
        let mut app = App::new()
            .add_systems(
                Update,
                ready((
                    node(
                        Fork,
                        (
                            move |mut local: Local<usize>| {
                                *local += 1;
                                a.fetch_add(*local, Ordering::SeqCst);
                            },
                            move |mut local: Local<usize>| {
                                *local += 1;
                                b.fetch_add(*local, Ordering::SeqCst);
                            },
                        )
                            .parallel()
                            .run_if(|gate: Single<&bool>| **gate),
                    ),
                    node(Join, move || {
                        output.lock().unwrap().push(observed.load(Ordering::SeqCst));
                    })
                    .after(Fork),
                )),
            )
            .build();
        let gate = app.world.spawn_bundle((true,));
        for enabled in [true, false, false, true] {
            *app.world.get_mut::<bool>(gate).unwrap() = enabled;
            app.emit(&Update);
        }
        assert_eq!(*visits.lock().unwrap(), vec![2, 2, 2, 6]);
    });
}

#[test]
fn dynamic_reentry_is_direct_and_pool_binding_restores_after_unwind() {
    let pool = WorkerPool::new(2);
    pool.install(|| {
        let count = Arc::new(AtomicUsize::new(0));
        let a = count.clone();
        let b = count.clone();
        let mut inner = App::new()
            .add_systems(
                Update,
                (
                    move || {
                        a.fetch_add(1, Ordering::SeqCst);
                    },
                    move || {
                        b.fetch_add(2, Ordering::SeqCst);
                    },
                )
                    .parallel(),
            )
            .build();
        let mut outer = App::new()
            .add_systems(Update, (move || inner.emit(&Update), || {}).parallel())
            .build();
        for _ in 0..20 {
            outer.emit(&Update);
        }
        assert_eq!(count.load(Ordering::SeqCst), 60);
        let nested = WorkerPool::new(1);
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(
                || nested.install(|| panic!("restore binding"))
            ))
            .is_err()
        );
        let barrier = Arc::new(Barrier::new(2));
        let a = barrier.clone();
        let b = barrier;
        let mut concurrent = App::new()
            .add_systems(
                Update,
                (
                    move || {
                        a.wait();
                    },
                    move || {
                        b.wait();
                    },
                )
                    .parallel(),
            )
            .build();
        concurrent.emit(&Update);
    });
}

#[test]
fn explicit_work_threshold_runs_small_groups_on_caller() {
    let caller = std::thread::current().id();
    let count = Arc::new(AtomicUsize::new(0));
    let a = count.clone();
    let b = count.clone();
    WorkerPool::new(3).with_min_parallel_work(3).install(|| {
        let mut app = App::new()
            .add_systems(
                Update,
                (
                    move || {
                        assert_eq!(std::thread::current().id(), caller);
                        a.fetch_add(1, Ordering::SeqCst);
                    },
                    move || {
                        assert_eq!(std::thread::current().id(), caller);
                        b.fetch_add(1, Ordering::SeqCst);
                    },
                )
                    .parallel(),
            )
            .build();
        for _ in 0..20 {
            app.emit(&Update);
        }
    });
    assert_eq!(count.load(Ordering::SeqCst), 40);
}

#[test]
fn conflict_order_respects_dependencies_not_registration_order() {
    WorkerPool::new(3).install(|| {
        let mut app = App::new()
            .add_systems(
                Update,
                ready((
                    node(Join, |mut value: Single<&mut u32>| {
                        **value *= 10;
                    })
                    .after(Fork),
                    node(Prefix, |mut value: Single<&mut u32>| {
                        **value += 2;
                    }),
                    node(Fork, |mut value: Single<&mut u32>| {
                        **value += 3;
                    })
                    .after(Prefix),
                )),
            )
            .build();
        let entity = app.world.spawn_bundle((1_u32,));
        app.emit(&Update);
        assert_eq!(app.world.get::<u32>(entity), Some(&60));
    });
}

#[test]
fn multiple_panics_release_payloads_on_the_caller_and_pool_is_reusable() {
    struct Payload(Arc<Mutex<Vec<std::thread::ThreadId>>>);
    impl Drop for Payload {
        fn drop(&mut self) {
            self.0.lock().unwrap().push(std::thread::current().id());
        }
    }
    let pool = WorkerPool::new(2);
    let caller = std::thread::current().id();
    let dropped = Arc::new(Mutex::new(Vec::new()));
    pool.install(|| {
        for _ in 0..4 {
            let barrier = Arc::new(Barrier::new(2));
            let a = barrier.clone();
            let b = barrier;
            let x = dropped.clone();
            let y = dropped.clone();
            let mut app = App::new()
                .add_systems(
                    Update,
                    (
                        move || {
                            a.wait();
                            std::panic::panic_any(Payload(x.clone()));
                        },
                        move || {
                            b.wait();
                            std::panic::panic_any(Payload(y.clone()));
                        },
                    )
                        .parallel(),
                )
                .build();
            let payload =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.emit(&Update)))
                    .unwrap_err();
            drop(payload);
        }
    });
    assert_eq!(*dropped.lock().unwrap(), vec![caller; 8]);
}

#[test]
fn simultaneous_callers_keep_worlds_and_invocation_lifetimes_separate() {
    let pool = WorkerPool::new(3);
    std::thread::scope(|scope| {
        for seed in [10_u32, 1000] {
            let pool = &pool;
            scope.spawn(move || {
                pool.install(|| {
                    let mut app = App::new()
                        .add_systems(
                            Update,
                            (
                                |mut value: Single<&mut u32>| {
                                    **value += 1;
                                },
                                |mut value: Single<&mut u64>| {
                                    **value += 2;
                                },
                            )
                                .parallel(),
                        )
                        .build();
                    let entity = app.world.spawn_bundle((seed, 0_u64));
                    for _ in 0..30 {
                        app.emit(&Update);
                    }
                    assert_eq!(app.world.get::<u32>(entity), Some(&(seed + 30)));
                    assert_eq!(app.world.get::<u64>(entity), Some(&60));
                })
            });
        }
    });
}

#[test]
fn cancelled_invocations_return_only_after_every_published_branch_finishes() {
    // One branch panics while its sibling keeps forking. emit must not return
    // while any published branch can still run (and touch freed invocation state).
    let iterations = if cfg!(miri) { 3 } else { 300 };
    WorkerPool::new(4).install(|| {
        let ran = Arc::new(AtomicUsize::new(0));
        let count = |ran: &Arc<AtomicUsize>| {
            let ran = ran.clone();
            move || {
                ran.fetch_add(1, Ordering::SeqCst);
            }
        };
        let mut app = App::new()
            .add_systems(
                Update,
                (
                    || panic!("cancel"),
                    (
                        (count(&ran), count(&ran)).parallel(),
                        (count(&ran), count(&ran)).parallel(),
                        (count(&ran), count(&ran)).parallel(),
                    ),
                )
                    .parallel(),
            )
            .build();
        for _ in 0..iterations {
            let emitted =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| app.emit(&Update)));
            assert!(emitted.is_err());
            let settled = ran.load(Ordering::SeqCst);
            for _ in 0..100 {
                std::thread::yield_now();
            }
            assert_eq!(ran.load(Ordering::SeqCst), settled);
        }
    });
}

#[test]
fn later_registrations_observe_every_branch_of_earlier_parallel_ones() {
    WorkerPool::new(3).install(|| {
        let mut app = App::new()
            .add_systems(
                Update,
                (
                    |mut a: Single<&mut u32>| {
                        std::thread::yield_now();
                        **a += 1;
                    },
                    |mut b: Single<&mut u64>| **b += 1,
                )
                    .parallel(),
            )
            .add_systems(Update, |a: Single<&u32>, b: Single<&u64>| {
                assert_eq!(u64::from(**a), **b)
            })
            .add_systems(
                Update,
                (
                    |mut a: Single<&mut u32>| **a += 1,
                    |mut b: Single<&mut u64>| {
                        std::thread::yield_now();
                        **b += 1;
                    },
                )
                    .parallel(),
            )
            .build();
        let entity = app.world.spawn_bundle((0_u32, 0_u64));
        for _ in 0..200 {
            app.emit(&Update);
        }
        assert_eq!(app.world.get::<u32>(entity), Some(&400));
        assert_eq!(app.world.get::<u64>(entity), Some(&400));
    });
}

#[test]
fn non_send_systems_and_listeners_still_run_parallel_groups() {
    use std::{cell::Cell, rc::Rc};
    let local = Rc::new(Cell::new(0));
    let (system, listener) = (local.clone(), local.clone());
    WorkerPool::new(2).install(|| {
        let mut app = App::new()
            .add_systems(
                Update,
                (
                    move || system.set(system.get() + 1),
                    (
                        |mut a: Single<&mut u32>| **a += 1,
                        |mut b: Single<&mut u64>| **b += 1,
                    )
                        .parallel(),
                ),
            )
            .on(move |_: &Update, world: &mut World<_>| {
                listener.set(listener.get() + **world.single::<&u32>().unwrap());
            })
            .build();
        let entity = app.world.spawn_bundle((0_u32, 0_u64));
        for _ in 0..3 {
            app.emit(&Update);
        }
        assert_eq!(app.world.get::<u64>(entity), Some(&3));
    });
    assert_eq!(local.get(), 3 + (1 + 2 + 3));
}
