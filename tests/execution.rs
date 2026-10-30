use static_ecs::prelude::*;
use std::{
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
struct Update;
struct A;
struct B;
struct C;
struct D;

#[test]
fn dependent_runs_before_unrelated_root_finishes() {
    let pool = WorkerPool::new(2);
    pool.install(|| {
        let rendezvous = Arc::new(Barrier::new(2));
        let a = rendezvous.clone();
        let b = rendezvous;
        let (send, receive) = mpsc::channel();
        let mut world = World::<Components<u32>>::default();
        let entity = world.spawn();
        world.insert(entity, 0_u32);
        let mut app = App::new(world).add_systems(
            Update,
            ready((
                node(A, move |mut value: Single<&mut u32>| {
                    a.wait();
                    **value += 1;
                }),
                node(B, move || {
                    b.wait();
                    assert_eq!(
                        receive.recv_timeout(Duration::from_secs(10)).unwrap() % 11,
                        1
                    );
                }),
                node(C, move |value: Single<&u32>| {
                    send.send(**value).unwrap();
                })
                .after(A),
                node(D, |mut value: Single<&mut u32>| {
                    **value += 10;
                })
                .after(A)
                .after(B),
            )),
        );
        app.emit(&Update);
        app.emit(&Update);
        assert_eq!(app.world.get::<u32>(entity), Some(&22));
    });
}

#[test]
fn conflicting_nodes_are_serialized_without_dependency_edges() {
    let pool = WorkerPool::new(3);
    pool.install(|| {
        let active = Arc::new(AtomicUsize::new(0));
        let a = active.clone();
        let b = active.clone();
        let mut world = World::<Components<u64>>::default();
        let entity = world.spawn();
        world.insert(entity, 0_u64);
        let mut app = App::new(world).add_systems(
            Update,
            ready((
                node(A, move |mut value: Single<&mut u64>| {
                    assert_eq!(a.fetch_add(1, Ordering::SeqCst), 0);
                    std::thread::yield_now();
                    **value += 1;
                    a.fetch_sub(1, Ordering::SeqCst);
                }),
                node(B, move |mut value: Single<&mut u64>| {
                    assert_eq!(b.fetch_add(1, Ordering::SeqCst), 0);
                    std::thread::yield_now();
                    **value += 2;
                    b.fetch_sub(1, Ordering::SeqCst);
                }),
            )),
        );
        for _ in 0..50 {
            app.emit(&Update);
        }
        assert_eq!(app.world.get::<u64>(entity), Some(&150));
        assert_eq!(active.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn serial_graph_releases_dependents_after_parameter_and_condition_skips() {
    type Schema = Components<u32>;
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let mut app = App::new(World::<Schema>::default())
        .with_execution_mode(ExecutionMode::Serial)
        .add_systems(
            Update,
            ready((
                node(A, move |_: Single<&u32>| {
                    observed.fetch_add(1, Ordering::SeqCst);
                }),
                node(B, (|| panic!("skipped body")).run_if(|| false)).after(A),
                node(
                    C,
                    exclusive(|world: &mut World<Schema>| {
                        if let Some(mut value) = world.single::<&mut u32>() {
                            **value += 1;
                        } else {
                            let entity = world.spawn();
                            world.insert(entity, 1_u32);
                        }
                    }),
                )
                .after(B),
            )),
        );
    app.emit(&Update);
    app.emit(&Update);
    app.emit(&Update);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(**app.world.single::<&u32>().unwrap(), 3);
}

#[test]
fn panic_joins_running_nodes_and_graph_can_run_again() {
    let pool = WorkerPool::new(2);
    pool.install(|| {
        let barrier = Arc::new(Barrier::new(2));
        let left = barrier.clone();
        let right = barrier;
        let completed = Arc::new(AtomicUsize::new(0));
        let done = completed.clone();
        let mut world = World::<Components<u32>>::default();
        let entity = world.spawn();
        world.insert(entity, 0_u32);
        let mut app = App::new(world).add_systems(
            Update,
            ready((
                node(A, move |mut attempts: Local<u32>| {
                    *attempts += 1;
                    left.wait();
                    assert!(*attempts != 1, "first invocation fails");
                }),
                node(B, move || {
                    right.wait();
                    std::thread::yield_now();
                    done.fetch_add(1, Ordering::SeqCst);
                }),
                node(C, |mut value: Single<&mut u32>| {
                    **value += 1;
                })
                .after(A)
                .after(B),
            )),
        );
        assert!(catch_unwind(AssertUnwindSafe(|| app.emit(&Update))).is_err());
        assert_eq!(completed.load(Ordering::SeqCst), 1);
        assert_eq!(app.world.get::<u32>(entity), Some(&0));
        app.emit(&Update);
        assert_eq!(completed.load(Ordering::SeqCst), 2);
        assert_eq!(app.world.get::<u32>(entity), Some(&1));
    });
}

#[test]
fn serial_mode_keeps_nested_parallel_groups_on_calling_thread() {
    let caller = std::thread::current().id();
    let visits = Arc::new(AtomicUsize::new(0));
    let a = visits.clone();
    let b = visits.clone();
    let c = visits.clone();
    let mut app = App::new(World::new(()))
        .with_execution_mode(ExecutionMode::Serial)
        .add_systems(
            Update,
            (
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
                move || {
                    assert_eq!(std::thread::current().id(), caller);
                    c.fetch_add(1, Ordering::SeqCst);
                },
            )
                .parallel()
                .run_if(|| true),
        );
    app.emit(&Update);
    assert_eq!(visits.load(Ordering::SeqCst), 3);
}

#[test]
fn single_worker_nested_graphs_complete_without_blocked_workers() {
    let pool = WorkerPool::new(1);
    pool.install(|| {
        let mut world = World::<Components<u32>>::default();
        let entity = world.spawn();
        world.insert(entity, 0_u32);
        let mut app = App::new(world).add_systems(
            Update,
            ready((
                node(
                    A,
                    ready((
                        node(C, |mut value: Single<&mut u32>| {
                            **value += 1;
                        }),
                        node(D, |mut value: Single<&mut u32>| {
                            **value *= 3;
                        })
                        .after(C),
                    )),
                ),
                node(B, |mut value: Single<&mut u32>| {
                    **value += 2;
                })
                .after(A),
            )),
        );
        app.emit(&Update);
        assert_eq!(app.world.get::<u32>(entity), Some(&5));
    });
}
