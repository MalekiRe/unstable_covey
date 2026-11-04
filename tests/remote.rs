use static_ecs::SyncPoint;
use static_ecs::prelude::*;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::thread::{self, Thread};
use std::time::{Duration, Instant};

struct Update;
impl Schedule for Update {}
#[derive(Debug, PartialEq)]
struct A(u32);
struct B(u32);
struct Player;

struct ThreadWaker(Thread);
impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
}
/// Poll on this thread until ready, parking between wakes. `queued` counts
/// the first pending poll, i.e. once the request is queued.
fn block_on<F: Future>(future: F, queued: Option<&AtomicUsize>) -> F::Output {
    let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    let mut first = true;
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        if std::mem::take(&mut first) {
            queued.map(|q| q.fetch_add(1, Ordering::SeqCst));
        }
        thread::park();
    }
}
fn wait_until(mut condition: impl FnMut() -> bool) {
    let start = Instant::now();
    while !condition() {
        assert!(start.elapsed() < Duration::from_secs(10), "timed out");
        thread::yield_now();
    }
}

#[test]
fn bridges_run_at_the_sync_points_position_and_apply_commands_there() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let (before, after) = (log.clone(), log.clone());
    let (point, remote) = sync_point();
    let mut app = App::new()
        .add_systems(Update, move |mut q: Query<&A>| {
            before
                .lock()
                .unwrap()
                .push(("before", q.iter_mut().count()))
        })
        .add_systems(Update, point)
        .add_systems(Update, move |mut q: Query<&A, With<Player>>| {
            after.lock().unwrap().push(("after", q.iter_mut().count()))
        })
        .build();
    let queued = AtomicUsize::new(0);
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let mut local = 0;
            let entity = block_on(
                remote.bridge(|mut commands: Commands<_>| {
                    local += 1; // captures the caller's locals mutably
                    commands.spawn((A(5), Player))
                }),
                Some(&queued),
            )
            .unwrap();
            assert_eq!(local, 1);
            entity
        });
        wait_until(|| queued.load(Ordering::SeqCst) == 1);
        app.emit(&Update);
        let entity = worker.join().unwrap();
        assert_eq!(app.world.get::<A>(entity), Some(&A(5)));
    });
    assert_eq!(*log.lock().unwrap(), [("before", 0), ("after", 1)]);
}

#[test]
fn disjoint_bridges_run_in_parallel_in_one_wave() {
    let (point, remote) = sync_point();
    let mut app = App::new()
        .register::<A>()
        .register::<B>()
        .add_systems(Update, point)
        .build();
    app.world.spawn_bundle((A(0), B(0)));
    let barrier = Barrier::new(2);
    let queued = AtomicUsize::new(0);
    thread::scope(|scope| {
        // Each closure waits for the other: only a shared wave completes.
        scope.spawn(|| {
            block_on(
                remote.bridge(|mut q: Query<&mut A>| {
                    barrier.wait();
                    q.iter_mut().for_each(|mut a| a.0 += 1);
                }),
                Some(&queued),
            )
            .unwrap()
        });
        scope.spawn(|| {
            block_on(
                remote.bridge(|mut q: Query<&mut B>| {
                    barrier.wait();
                    q.iter_mut().for_each(|mut b| b.0 += 1);
                }),
                Some(&queued),
            )
            .unwrap()
        });
        wait_until(|| queued.load(Ordering::SeqCst) == 2);
        app.emit(&Update);
    });
}

struct InFlight {
    now: AtomicUsize,
    max: AtomicUsize,
}
impl InFlight {
    fn new() -> Self {
        Self {
            now: AtomicUsize::new(0),
            max: AtomicUsize::new(0),
        }
    }
    fn enter(&self) {
        let now = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.max.fetch_max(now, Ordering::SeqCst);
        thread::sleep(Duration::from_millis(20));
        self.now.fetch_sub(1, Ordering::SeqCst);
    }
}

#[test]
fn conflicting_shared_state_and_exclusive_bridges_never_share_a_wave() {
    let (point, remote) = sync_point();
    let mut app = App::new()
        .register::<A>()
        .register::<B>()
        .add_systems(Update, point)
        .build();
    app.world.spawn_bundle((A(0), B(0)));
    let state = remote.state();
    for case in 0..3 {
        let flight = InFlight::new();
        let queued = AtomicUsize::new(0);
        thread::scope(|scope| {
            for side in 0..2 {
                let (remote, state, flight, queued) = (&remote, state.clone(), &flight, &queued);
                scope.spawn(move || match (case, side) {
                    // Two writers of A.
                    (0, _) => block_on(
                        remote.bridge(|_: Query<&mut A>| flight.enter()),
                        Some(queued),
                    )
                    .unwrap(),
                    // Two readers sharing one state.
                    (1, _) => {
                        block_on(state.bridge(|_: Query<&B>| flight.enter()), Some(queued)).unwrap()
                    }
                    // An exclusive bridge and a disjoint reader.
                    (_, 0) => block_on(
                        remote.bridge(|_: &mut World<_>| flight.enter()),
                        Some(queued),
                    )
                    .unwrap(),
                    _ => block_on(remote.bridge(|_: Query<&B>| flight.enter()), Some(queued))
                        .unwrap(),
                });
            }
            wait_until(|| queued.load(Ordering::SeqCst) == 2);
            // Both requests were queued before this drive: two waves, one emit.
            app.emit(&Update);
        });
        assert_eq!(flight.max.load(Ordering::SeqCst), 1, "case {case}");
    }
}

#[test]
fn invalid_parameters_fail_and_until_retries_at_later_drives() {
    let (point, remote) = sync_point();
    let mut app = App::new()
        .register::<A>()
        .register::<Player>()
        .add_systems(Update, point)
        .build();
    let queued = AtomicUsize::new(0);
    let done = AtomicBool::new(false);
    thread::scope(|scope| {
        scope.spawn(|| {
            let missing = block_on(
                remote.bridge(|_: Single<&A, With<Player>>| ()),
                Some(&queued),
            );
            assert_eq!(missing, Err(BridgeError::Invalid));
            let value = block_on(
                remote.until(|a: Single<&A, With<Player>>| a.0),
                Some(&queued),
            );
            assert_eq!(value, Ok(9));
            done.store(true, Ordering::SeqCst);
        });
        wait_until(|| queued.load(Ordering::SeqCst) == 1);
        app.emit(&Update);
        wait_until(|| queued.load(Ordering::SeqCst) == 2);
        for _ in 0..3 {
            app.emit(&Update);
        }
        assert!(!done.load(Ordering::SeqCst));
        app.world.spawn_bundle((A(9), Player));
        wait_until(|| {
            app.emit(&Update);
            done.load(Ordering::SeqCst)
        });
    });
}

#[test]
fn a_loop_runs_once_per_drive_while_a_chain_may_finish_in_one() {
    let (point, remote) = sync_point();
    let mut app = App::new()
        .register::<A>()
        .add_systems(Update, point)
        .build();
    let runs = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let queued = AtomicUsize::new(0);
    thread::scope(|scope| {
        scope.spawn(|| {
            let waker = Waker::from(Arc::new(ThreadWaker(thread::current())));
            let mut cx = Context::from_waker(&waker);
            loop {
                let mut bridge = pin!(remote.bridge(|_: Query<&A>| {
                    runs.fetch_add(1, Ordering::SeqCst);
                    stop.load(Ordering::SeqCst)
                }));
                let done = loop {
                    match bridge.as_mut().poll(&mut cx) {
                        Poll::Ready(output) => break output.unwrap(),
                        Poll::Pending => {
                            queued.fetch_add(1, Ordering::SeqCst);
                            thread::park();
                        }
                    }
                };
                if done {
                    break;
                }
            }
        });
        for emit in 1..=5 {
            wait_until(|| queued.load(Ordering::SeqCst) >= emit);
            app.emit(&Update);
            assert!(runs.load(Ordering::SeqCst) <= emit);
        }
        stop.store(true, Ordering::SeqCst);
        wait_until(|| {
            app.emit(&Update);
            runs.load(Ordering::SeqCst) > 5
        });
    });
}

#[test]
fn dropping_the_app_or_a_granted_bridge_never_strands_anyone() {
    // A request queued when its app drops fails, as does any later one.
    let (point, remote) = sync_point();
    let app = App::new()
        .register::<A>()
        .add_systems(Update, point)
        .build();
    let queued = AtomicUsize::new(0);
    thread::scope(|scope| {
        let waiter = scope.spawn(|| block_on(remote.bridge(|_: Query<&A>| ()), Some(&queued)));
        wait_until(|| queued.load(Ordering::SeqCst) == 1);
        drop(app);
        assert_eq!(waiter.join().unwrap(), Err(BridgeError::Closed));
    });
    assert_eq!(
        block_on(remote.bridge(|_: Query<&A>| ()), None),
        Err(BridgeError::Closed)
    );

    // A bridge granted, then dropped without running, releases the driver.
    let (point, remote) = sync_point();
    let mut app = App::new()
        .register::<A>()
        .add_systems(Update, point)
        .build();
    let woken = Arc::new(AtomicBool::new(false));
    struct Flag(Arc<AtomicBool>);
    impl Wake for Flag {
        fn wake(self: Arc<Self>) {
            self.0.store(true, Ordering::SeqCst);
        }
    }
    let waker = Waker::from(Arc::new(Flag(woken.clone())));
    let mut bridge = Box::pin(remote.bridge(|_: Query<&A>| unreachable!()));
    assert!(
        bridge
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    // Spurious polls before the grant change nothing.
    assert!(
        bridge
            .as_mut()
            .poll(&mut Context::from_waker(&waker))
            .is_pending()
    );
    thread::scope(|scope| {
        let emitter = scope.spawn(|| app.emit(&Update));
        wait_until(|| woken.load(Ordering::SeqCst));
        drop(bridge);
        emitter.join().unwrap();
    });
}

#[test]
#[should_panic(expected = "inside a task, use `AsyncWorld::bridge`")]
fn awaiting_a_remote_bridge_inside_a_task_panics_instead_of_deadlocking() {
    let (point, remote) = sync_point();
    let mut app = App::new()
        .register::<A>()
        .add_systems(Update, point)
        .add_plugins(task(move |_: AsyncWorld<_>| async move {
            let _ = remote.bridge(|_: Query<&A>| ()).await;
        }))
        .build();
    app.emit(&Update);
}

/// Each side waits up to five seconds for the other; true if both met.
fn rendezvous(mine: &AtomicBool, theirs: &AtomicBool) -> bool {
    mine.store(true, Ordering::SeqCst);
    let start = Instant::now();
    while !theirs.load(Ordering::SeqCst) {
        if start.elapsed() > Duration::from_secs(5) {
            return false;
        }
        thread::yield_now();
    }
    true
}

#[test]
fn an_envelope_point_runs_beside_disjoint_systems_and_infers_its_components() {
    let (bridge_side, system_side) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    let met = Arc::new(AtomicUsize::new(0));
    let (mine, theirs, system_met) = (system_side.clone(), bridge_side.clone(), met.clone());
    WorkerPool::new(3).install(|| {
        // `A` is stored only because the envelope mentions it.
        let (point, remote) = SyncPoint::within::<Query<&mut A>>();
        let mut app = App::new()
            .add_systems(
                Update,
                (point, move |mut q: Query<&mut B>| {
                    if rendezvous(&mine, &theirs) {
                        system_met.fetch_add(1, Ordering::SeqCst);
                    }
                    q.iter_mut().for_each(|mut b| b.0 += 1);
                })
                    .parallel(),
            )
            .build();
        let entity = app.world.spawn_bundle((A(1), B(1)));
        let queued = AtomicUsize::new(0);
        thread::scope(|scope| {
            scope.spawn(|| {
                block_on(
                    remote.bridge(|mut q: Query<&mut A>| {
                        if rendezvous(&bridge_side, &system_side) {
                            met.fetch_add(1, Ordering::SeqCst);
                        }
                        q.iter_mut().for_each(|mut a| a.0 += 1);
                    }),
                    Some(&queued),
                )
                .unwrap()
            });
            wait_until(|| queued.load(Ordering::SeqCst) == 1);
            app.emit(&Update);
        });
        assert_eq!(app.world.get::<A>(entity), Some(&A(2)));
        assert_eq!(app.world.get::<B>(entity).map(|b| b.0), Some(2));
    });
    assert_eq!(
        met.load(Ordering::SeqCst),
        2,
        "bridge and system overlapped"
    );
}

#[test]
fn envelope_commands_apply_when_the_emit_ends() {
    let seen = Arc::new(AtomicUsize::new(usize::MAX));
    let during = seen.clone();
    let (point, remote) = SyncPoint::within::<Query<&A>>();
    let mut app = App::new()
        .add_systems(
            Update,
            (point, move |mut q: Query<&A>| {
                during.store(q.iter_mut().count(), Ordering::SeqCst)
            })
                .chain(),
        )
        .build();
    let queued = AtomicUsize::new(0);
    thread::scope(|scope| {
        scope.spawn(|| {
            block_on(
                remote.bridge(|mut commands: Commands<_>| {
                    commands.spawn((A(3),));
                }),
                Some(&queued),
            )
            .unwrap()
        });
        wait_until(|| queued.load(Ordering::SeqCst) == 1);
        app.emit(&Update);
    });
    assert_eq!(seen.load(Ordering::SeqCst), 0);
    assert_eq!(app.world.query::<&A>().iter_mut().count(), 1);
}

#[test]
fn a_waiting_envelope_point_runs_work_published_to_its_worker() {
    let started = Arc::new(AtomicBool::new(false));
    let y_ran = Arc::new(AtomicBool::new(false));
    let y_thread = Arc::new(Mutex::new(None));
    let caller = thread::current().id();
    let (gate_started, y_flag, y_where) = (started.clone(), y_ran.clone(), y_thread.clone());
    WorkerPool::new(2).install(|| {
        let (point, remote) = SyncPoint::within::<Query<&mut A>>();
        // The driver lends its worker before granting, so once the bridge runs,
        // a fork can hand work to the driver.
        let gate = move || wait_until(|| gate_started.load(Ordering::SeqCst));
        // Holds the other worker until `y` ran: only the driver can run `y`.
        let x_waits = y_ran.clone();
        let x = move |_: Query<&B>| wait_until(|| x_waits.load(Ordering::SeqCst));
        let y = move |_: Query<&B>| {
            *y_where.lock().unwrap() = Some(thread::current().id());
            y_flag.store(true, Ordering::SeqCst);
        };
        let mut app = App::new()
            .add_systems(
                Update,
                (point, (gate, (x, y).parallel()).chain()).parallel(),
            )
            .build();
        app.world.spawn_bundle((A(0), B(0)));
        let queued = AtomicUsize::new(0);
        thread::scope(|scope| {
            scope.spawn(|| {
                block_on(
                    remote.bridge(|_: Query<&mut A>| {
                        started.store(true, Ordering::SeqCst);
                        wait_until(|| y_ran.load(Ordering::SeqCst));
                    }),
                    Some(&queued),
                )
                .unwrap()
            });
            wait_until(|| queued.load(Ordering::SeqCst) == 1);
            app.emit(&Update);
        });
    });
    // The other worker was busy in `x`: the waiting driver (the calling
    // thread) ran `y`. A driver that blocked would leave `x` waiting forever.
    assert_eq!(*y_thread.lock().unwrap(), Some(caller));
}
