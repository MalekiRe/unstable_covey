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
        let mut app = App::new(World::new(())).add_systems(
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
        );
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
        let mut world = World::<Components<bool>>::default();
        let gate = world.spawn();
        world.insert(gate, true);
        let mut app = App::new(world).add_systems(
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
        );
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
        let mut inner = App::new(World::new(())).add_systems(
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
        );
        let mut outer = App::new(World::new(()))
            .add_systems(Update, (move || inner.emit(&Update), || {}).parallel());
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
        let mut concurrent = App::new(World::new(())).add_systems(
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
        );
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
        let mut app = App::new(World::new(())).add_systems(
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
        );
        for _ in 0..20 {
            app.emit(&Update);
        }
    });
    assert_eq!(count.load(Ordering::SeqCst), 40);
}

#[test]
fn conflict_order_respects_dependencies_not_registration_order() {
    WorkerPool::new(3).install(|| {
        let mut world = World::<Components<u32>>::default();
        let entity = world.spawn();
        world.insert(entity, 1_u32);
        let mut app = App::new(world).add_systems(
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
        );
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
            let mut app = App::new(World::new(())).add_systems(
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
            );
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
                    let mut world = World::<(Components<u32>, Components<u64>)>::default();
                    let entity = world.spawn();
                    world.insert(entity, seed);
                    world.insert(entity, 0_u64);
                    let mut app = App::new(world).add_systems(
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
                    );
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
