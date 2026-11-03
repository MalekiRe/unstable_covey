use static_ecs::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Poll, Waker};

struct Update;
impl Schedule for Update {}
struct Render;
impl Schedule for Render {}

#[derive(Debug, PartialEq)]
struct Health(u32);
struct Player;

#[test]
fn bridges_run_at_the_tasks_position_and_see_earlier_bridges_effects() {
    let log = &RefCell::new(Vec::new());
    let mut app = App::new()
        .add_systems(Update, |mut q: Query<&Health>| {
            log.borrow_mut()
                .push(format!("before {}", q.iter_mut().count()))
        })
        .add_plugins(task(|world: AsyncWorld<_>| async move {
            let spawned = world
                .bridge(Update, |mut commands: Commands<_>| {
                    commands.spawn((Health(10), Player))
                })
                .await
                .unwrap();
            // Same emit: the spawn already applied.
            let health = world
                .bridge(Update, |mut q: Query<(Entity, &Health)>| {
                    q.iter_mut().map(|(e, h)| (e, h.0)).next()
                })
                .await
                .unwrap();
            assert_eq!(health, Some((spawned, 10)));
            log.borrow_mut().push("task".to_string());
        }))
        .add_systems(Update, |mut q: Query<&Health, With<Player>>| {
            log.borrow_mut()
                .push(format!("after {}", q.iter_mut().count()))
        })
        .build();
    app.emit(&Update);
    app.emit(&Update);
    drop(app);
    assert_eq!(
        *log.borrow(),
        ["before 0", "task", "after 1", "before 1", "after 1"]
    );
}

#[test]
fn a_bridge_waits_for_its_label_and_until_waits_for_valid_parameters() {
    let steps = &RefCell::new(Vec::new());
    let mut app = App::new()
        .register::<Health>()
        .register::<Player>()
        .add_plugins(task(|world: AsyncWorld<_>| async move {
            let missing = world
                .bridge(Update, |_: Single<&Health, With<Player>>| ())
                .await;
            assert_eq!(missing, Err(BridgeError::Invalid));
            steps.borrow_mut().push("invalid");
            let hp = world
                .until(Render, |h: Single<&Health, With<Player>>| h.0)
                .await
                .unwrap();
            steps
                .borrow_mut()
                .push(if hp == 7 { "seven" } else { "other" });
        }))
        .build();
    app.emit(&Update);
    assert_eq!(*steps.borrow(), ["invalid"]);
    app.emit(&Render); // no player yet: still waiting
    app.emit(&Update);
    let player = app.world.spawn();
    app.world.insert_bundle(player, (Health(7), Player));
    app.emit(&Update); // wrong label
    assert_eq!(*steps.borrow(), ["invalid"]);
    app.emit(&Render);
    drop(app);
    assert_eq!(*steps.borrow(), ["invalid", "seven"]);
}

#[test]
fn closures_borrow_non_send_task_locals_mutably() {
    let total = Rc::new(RefCell::new(0));
    let seen = total.clone();
    let mut app = App::new()
        .register::<Health>()
        .add_plugins(task(move |world: AsyncWorld<_>| async move {
            let mut local = Vec::new();
            let shared = Rc::clone(&seen);
            world
                .bridge(Update, |w: &mut World<_>| {
                    w.spawn_bundle((Health(4),));
                    w.spawn_bundle((Health(5),));
                })
                .await
                .unwrap();
            world
                .bridge(Update, |mut q: Query<&Health>| {
                    local.extend(q.iter_mut().map(|h| h.0));
                    *shared.borrow_mut() += q.iter_mut().map(|h| h.0).sum::<u32>();
                })
                .await
                .unwrap();
            local.sort();
            assert_eq!(local, [4, 5]);
        }))
        .build();
    app.emit(&Update);
    drop(app);
    assert_eq!(*total.borrow(), 9);
}

#[test]
fn shared_state_keeps_locals_and_change_detection_between_bridges() {
    let changed = &RefCell::new(Vec::new());
    let mut app = App::new()
        .register::<Health>()
        .add_plugins(task(|world: AsyncWorld<_>| async move {
            let state = world.state();
            loop {
                let (count, ids) = state
                    .bridge(
                        Update,
                        |mut n: Local<u32>, mut q: Query<&Health, Changed<Health>>| {
                            *n += 1;
                            (*n, q.iter_mut().map(|h| h.0).collect::<Vec<_>>())
                        },
                    )
                    .await
                    .unwrap();
                changed.borrow_mut().push((count, ids));
            }
        }))
        .build();
    let a = app.world.spawn();
    app.world.insert(a, Health(1));
    app.emit(&Update);
    app.emit(&Update);
    app.world.get_mut::<Health>(a).unwrap().0 = 2;
    app.emit(&Update);
    drop(app);
    assert_eq!(*changed.borrow(), [(1, vec![1]), (2, vec![]), (3, vec![2])]);
}

/// Ready once its flag is set from another thread, which wakes the task.
struct Flag(Arc<(AtomicBool, std::sync::Mutex<Option<Waker>>)>);
impl Future for Flag {
    type Output = ();
    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<()> {
        let (done, waker) = &*self.0;
        *waker.lock().unwrap() = Some(cx.waker().clone());
        if done.load(Ordering::Acquire) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

#[test]
fn a_woken_task_resumes_at_the_next_emit_of_any_label() {
    let flag = Arc::new((AtomicBool::new(false), std::sync::Mutex::new(None)));
    let resumed = RefCell::new(false);
    let mut app = App::new()
        .add_plugins(task(|_: AsyncWorld<()>| async {
            Flag(flag.clone()).await;
            *resumed.borrow_mut() = true;
        }))
        .build();
    app.emit(&Update);
    app.emit(&Render);
    assert!(!*resumed.borrow());
    let remote = flag.clone();
    std::thread::spawn(move || {
        remote.0.store(true, Ordering::Release);
        remote.1.lock().unwrap().take().unwrap().wake();
    })
    .join()
    .unwrap();
    app.emit(&Render);
    assert!(*resumed.borrow());
}

#[test]
fn send_tasks_bridge_between_parallel_groups_on_workers() {
    use std::sync::Mutex;
    struct A(u32);
    struct B(u32);
    let log = Arc::new(Mutex::new(Vec::new()));
    let (before, during, after) = (log.clone(), log.clone(), log.clone());
    WorkerPool::new(3).install(|| {
        let mut app = App::new()
            .add_systems(
                Update,
                (
                    |mut q: Query<&mut A>| q.iter_mut().for_each(|mut a| a.0 += 1),
                    |mut q: Query<&mut B>| q.iter_mut().for_each(|mut b| b.0 += 10),
                )
                    .parallel(),
            )
            .add_systems(Update, move || before.lock().unwrap().push("before"))
            .add_plugins(task(move |world: AsyncWorld<_>| async move {
                let state = world.state();
                loop {
                    let sum = state
                        .bridge(
                            Update,
                            |mut q: Query<(&A, &B)>, mut commands: Commands<_>| {
                                let sum: u32 = q.iter_mut().map(|(a, b)| a.0 + b.0).sum();
                                commands.spawn((A(0), B(0)));
                                sum
                            },
                        )
                        .await
                        .unwrap();
                    during
                        .lock()
                        .unwrap()
                        .push(if sum == 11 { "first" } else { "later" });
                }
            }))
            .add_systems(
                Update,
                (
                    move |mut q: Query<&A>| {
                        after.lock().unwrap().push(if q.iter_mut().count() == 2 {
                            "after 2"
                        } else {
                            "after ?"
                        })
                    },
                    |_: Query<&B>| {},
                )
                    .parallel(),
            )
            .build();
        app.world.spawn_bundle((A(0), B(0)));
        app.emit(&Update);
    });
    assert_eq!(*log.lock().unwrap(), ["before", "first", "after 2"]);
}
