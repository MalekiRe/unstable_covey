//! Hot paths of tasks allocate nothing once warm: an idle task's visit, and a
//! bridge through reusable state.
use static_ecs::prelude::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;
thread_local! {
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        ALLOCATIONS.set(ALLOCATIONS.get() + 1);
        unsafe { System.realloc(ptr, layout, size) }
    }
}
#[global_allocator]
static GLOBAL: Counting = Counting;

fn allocations(run: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.get();
    run();
    ALLOCATIONS.get() - before
}

struct Update;
impl Schedule for Update {}
struct Render;
impl Schedule for Render {}
struct Health(u32);

#[test]
fn idle_visits_and_warm_bridges_allocate_nothing() {
    // The counter sees this thread's allocations.
    assert_eq!(allocations(|| drop(std::hint::black_box(Box::new(1)))), 1);
    let mut app = App::new()
        .register::<Health>()
        .add_plugins((
            // Waits on a label that is never emitted: visited, never polled.
            task(|world: AsyncWorld<_>| async move {
                let _ = world.bridge(Render, |_: &mut World<_>| ()).await;
            }),
            task(|world: AsyncWorld<_>| async move {
                let state = world.state();
                loop {
                    state
                        .bridge(Update, |mut q: Query<&mut Health>, _: Commands<_>| {
                            for mut health in &mut q {
                                health.0 += 1;
                            }
                        })
                        .await
                        .unwrap();
                    world
                        .bridge(Update, |world: &mut World<_>| {
                            std::hint::black_box(world.archetype_count());
                        })
                        .await
                        .unwrap();
                }
            }),
        ))
        .build();
    app.world.spawn_bundle((Health(0),));
    for _ in 0..3 {
        app.emit(&Update);
    }
    assert_eq!(
        allocations(|| for _ in 0..100 {
            app.emit(&Update)
        }),
        0
    );
    let entity = app
        .world
        .query::<(Entity, &Health)>()
        .iter_mut()
        .next()
        .unwrap()
        .0;
    assert_eq!(app.world.get::<Health>(entity).unwrap().0, 103);
}

#[test]
fn warm_remote_bridges_allocate_nothing_on_either_side() {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::task::{Context, Wake, Waker};
    use std::thread;
    struct Unpark(thread::Thread);
    impl Wake for Unpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }
    let (point, remote) = sync_point();
    let mut app = App::new()
        .register::<Health>()
        .add_systems(Update, point)
        .build();
    app.world.spawn_bundle((Health(0),));
    const WARM: usize = 3;
    const ROUNDS: usize = 50;
    let (queued, finished) = (AtomicUsize::new(0), AtomicUsize::new(0));
    let wait = |counter: &AtomicUsize, at: usize| {
        while counter.load(Ordering::SeqCst) < at {
            thread::yield_now();
        }
    };
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let waker = Waker::from(Arc::new(Unpark(thread::current())));
            let mut cx = Context::from_waker(&waker);
            let state = remote.state();
            let mut round = |_| {
                let mut bridge =
                    std::pin::pin!(state.bridge(|mut q: Query<&mut Health>, _: Commands<_>| {
                        for mut health in &mut q {
                            health.0 += 1;
                        }
                    }));
                let mut first = true;
                while bridge.as_mut().poll(&mut cx).is_pending() {
                    if std::mem::take(&mut first) {
                        queued.fetch_add(1, Ordering::SeqCst);
                    }
                    thread::park();
                }
                finished.fetch_add(1, Ordering::SeqCst);
            };
            (0..WARM).for_each(&mut round);
            allocations(|| (0..ROUNDS).for_each(&mut round))
        });
        let mut drive = |round: usize| {
            wait(&queued, round + 1);
            app.emit(&Update);
            wait(&finished, round + 1);
        };
        (0..WARM).for_each(&mut drive);
        let driver = allocations(|| (WARM..WARM + ROUNDS).for_each(&mut drive));
        assert_eq!((driver, worker.join().unwrap()), (0, 0));
    });
    let mut q = app.world.query::<&Health>();
    assert_eq!(q.iter_mut().next().unwrap().0, (WARM + ROUNDS) as u32);
}
