//! Typed continuations run directly. Only a published branch crosses an erased
//! thread boundary; there are no task IDs, ready queues, or dependency scans.
use crate::plan::System;
use std::{
    any::Any,
    cell::{Cell, UnsafeCell},
    marker::PhantomData,
    mem::MaybeUninit,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc, LazyLock, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::{self, JoinHandle, Thread},
};

type Panic = Box<dyn Any + Send>;
thread_local! {
    static CURRENT: Cell<*const WorkerPool> = const { Cell::new(std::ptr::null()) };
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
}
static DEFAULT: LazyLock<WorkerPool> =
    LazyLock::new(|| WorkerPool::new(thread::available_parallelism().map_or(1, usize::from)));
fn pool() -> &'static WorkerPool {
    // Used only inside the install scope; never retained by an invocation.
    CURRENT
        .with(|current| unsafe { current.get().as_ref() })
        .unwrap_or_else(|| &DEFAULT)
}
pub(crate) fn serial() -> bool {
    ACTIVE.with(Cell::get) || pool().workers() == 1
}

const IDLE: usize = 0;
const RESERVED: usize = 1;
const READY: usize = 2;
const RUNNING: usize = 3;
struct Branch {
    context: *const (),
    call: unsafe fn(*const (), bool),
    executed: bool,
}
#[repr(align(64))]
struct Slot {
    state: AtomicUsize,
    branch: UnsafeCell<MaybeUninit<Branch>>,
}
// RESERVED has one writer; READY -> RUNNING transfers the initialized branch
// to exactly one reader. IDLE is published only after that reader returns.
unsafe impl Sync for Slot {}
unsafe impl Send for Slot {}
impl Slot {
    fn new() -> Self {
        Self {
            state: AtomicUsize::new(IDLE),
            branch: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }
    fn take(&self) -> Option<Branch> {
        self.state
            .compare_exchange(READY, RUNNING, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(unsafe { (*self.branch.get()).assume_init_read() })
    }
    fn claim(&self) -> bool {
        let Some(branch) = self.take() else {
            return false;
        };
        unsafe {
            (branch.call)(branch.context, branch.executed);
        }
        self.state.store(IDLE, Ordering::Release);
        true
    }
    // The stealer holds its own slot RUNNING until this call returns. Transfer
    // the invocation lease there, freeing the original worker immediately.
    unsafe fn steal(&self) -> bool {
        let Some(branch) = self.take() else {
            return false;
        };
        self.state.store(IDLE, Ordering::Release);
        unsafe {
            (branch.call)(branch.context, branch.executed);
        }
        true
    }
}
struct Shared {
    slots: Box<[Slot]>,
    closing: AtomicBool,
    started: AtomicUsize,
}

/// Persistent workers including the calling thread. No per-system jobs or queues.
pub struct WorkerPool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    submission: Mutex<()>,
    min_parallel_work: usize,
}
impl WorkerPool {
    pub fn new(workers: usize) -> Self {
        assert!(workers > 0, "worker count must be positive");
        let shared = Arc::new(Shared {
            slots: (0..workers).map(|_| Slot::new()).collect(),
            closing: AtomicBool::new(false),
            started: AtomicUsize::new(0),
        });
        let mut pool = Self {
            shared,
            threads: Vec::with_capacity(workers - 1),
            submission: Mutex::new(()),
            min_parallel_work: 1,
        };
        for worker in 1..workers {
            let shared = pool.shared.clone();
            pool.threads.push(
                thread::Builder::new()
                    .name(format!("ecs-{worker}"))
                    .spawn(move || {
                        let _ = thread::current();
                        shared.started.fetch_add(1, Ordering::Release);
                        loop {
                            if shared.closing.load(Ordering::Acquire) {
                                return;
                            }
                            if shared.slots[worker].claim() {
                                continue;
                            }
                            // Only reserve capacity when a real branch can be stolen.
                            // Idle scanners must not make an otherwise free worker
                            // unavailable to a narrow prefix that is about to fork.
                            let mut helped = false;
                            for offset in 1..shared.slots.len() {
                                let other = (worker + offset) % shared.slots.len();
                                if shared.slots[other].state.load(Ordering::Relaxed) != READY {
                                    continue;
                                }
                                if shared.slots[worker]
                                    .state
                                    .compare_exchange(
                                        IDLE,
                                        RUNNING,
                                        Ordering::Acquire,
                                        Ordering::Relaxed,
                                    )
                                    .is_ok()
                                {
                                    helped = unsafe { shared.slots[other].steal() };
                                    shared.slots[worker].state.store(IDLE, Ordering::Release);
                                }
                                if helped {
                                    break;
                                }
                            }
                            if helped {
                                continue;
                            }
                            // Publication precedes unpark; a token survives this window.
                            thread::park();
                        }
                    })
                    .expect("cannot start ECS worker"),
            );
        }
        while pool.shared.started.load(Ordering::Acquire) != workers - 1 {
            thread::yield_now();
        }
        pool
    }
    pub fn workers(&self) -> usize {
        self.shared.slots.len()
    }
    /// Minimum static branch weight eligible for publication. Functions weigh
    /// one; sequential groups sum their weights. This is not a timing estimate.
    pub fn with_min_parallel_work(mut self, minimum: usize) -> Self {
        self.min_parallel_work = minimum;
        self
    }
    pub fn install<R>(&self, run: impl FnOnce() -> R) -> R {
        struct Restore(*const WorkerPool);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT.with(|current| current.set(self.0));
            }
        }
        let _restore = Restore(CURRENT.with(|current| current.replace(self)));
        run()
    }
}
impl Drop for WorkerPool {
    fn drop(&mut self) {
        self.shared.closing.store(true, Ordering::Release);
        for worker in &self.threads {
            worker.thread().unpark();
        }
        for worker in self.threads.drain(..) {
            let _ = worker.join();
        }
    }
}

/// A compile-time projection. It never borrows an ancestor containing another
/// executing child. Implementations are internal to the sealed system tree.
#[doc(hidden)]
pub trait Project<R> {
    type Target;
    const KEY: usize = 0;
    /// # Safety
    /// Root is live and the selected child is exclusively owned by this path.
    unsafe fn get(root: *mut R) -> *mut Self::Target;
}
#[doc(hidden)]
pub struct Root;
impl<R> Project<R> for Root {
    type Target = R;
    unsafe fn get(root: *mut R) -> *mut R {
        root
    }
}
#[doc(hidden)]
pub trait Fields {
    type Left;
    type Right;
    /// # Safety
    /// The parent allocation is live; only the selected field may be borrowed.
    unsafe fn left(this: *mut Self) -> *mut Self::Left;
    /// # Safety
    /// The parent allocation is live; only the selected field may be borrowed.
    unsafe fn right(this: *mut Self) -> *mut Self::Right;
}
#[doc(hidden)]
pub struct First<P>(PhantomData<P>);
#[doc(hidden)]
pub struct Second<P>(PhantomData<P>);
impl<R, P: Project<R>> Project<R> for First<P>
where
    P::Target: Fields,
{
    type Target = <P::Target as Fields>::Left;
    const KEY: usize = P::KEY.wrapping_mul(2).wrapping_add(1);
    unsafe fn get(root: *mut R) -> *mut Self::Target {
        unsafe { P::Target::left(P::get(root)) }
    }
}
impl<R, P: Project<R>> Project<R> for Second<P>
where
    P::Target: Fields,
{
    type Target = <P::Target as Fields>::Right;
    const KEY: usize = P::KEY.wrapping_mul(2).wrapping_add(2);
    unsafe fn get(root: *mut R) -> *mut Self::Target {
        unsafe { P::Target::right(P::get(root)) }
    }
}

#[doc(hidden)]
pub trait Continue<S, R> {
    /// # Safety
    /// Every predecessor of this continuation has relinquished its accesses.
    unsafe fn resume(context: &Context<'_, S, R>, executed: bool);
}
#[doc(hidden)]
pub struct Then<P, K>(PhantomData<(P, K)>);
impl<S: Send + Sync, R: Send, P: Project<R>, K: Continue<S, R>> Continue<S, R> for Then<P, K>
where
    P::Target: System<S>,
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, executed: bool) {
        if context.cancelled() {
            return;
        }
        unsafe {
            P::Target::enter::<R, P, K>(context, executed);
        }
    }
}
struct Done;
impl<S, R> Continue<S, R> for Done {
    unsafe fn resume(context: &Context<'_, S, R>, executed: bool) {
        context.executed.store(executed, Ordering::Relaxed);
        context.done.store(true, Ordering::Release);
        context.caller.unpark();
    }
}

/// Storage exists only at a potential cross-worker join. Local fork fallbacks
/// bypass it entirely. The high bit carries the OR of executed predecessors.
#[doc(hidden)]
#[derive(Default)]
pub struct Join(AtomicUsize);
impl Join {
    pub(crate) fn reset(&self, arrivals: usize) {
        self.0.store(arrivals, Ordering::Relaxed);
    }
    pub(crate) fn arrive(&self, executed: bool) -> Option<bool> {
        const FLAG: usize = 1 << (usize::BITS - 1);
        // Each arrival decrements the count. Setting the result bit and consuming
        // the count are one operation; no per-system completion flag is needed.
        let old = self
            .0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some((old - 1) | if executed { FLAG } else { 0 })
            })
            .unwrap();
        ((old & !FLAG) == 1).then_some(executed || old & FLAG != 0)
    }
}

#[doc(hidden)]
pub struct Context<'a, S, R> {
    pub(crate) nodes: *mut R,
    pub(crate) world: *mut S,
    pool: &'a WorkerPool,
    caller: Thread,
    done: AtomicBool,
    executed: AtomicBool,
    cancelled: AtomicBool,
    panic: Mutex<Vec<Panic>>,
}
// The typed paths transfer ownership of disjoint children. Static access proofs
// and generated joins order world accesses. Invocation storage outlives all slots.
unsafe impl<S: Send + Sync, R: Send> Sync for Context<'_, S, R> {}
impl<S, R> Context<'_, S, R> {
    #[inline]
    pub(crate) fn cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Relaxed)
    }
    fn fail(&self, payload: Panic) {
        self.panic.lock().unwrap().push(payload);
        self.cancelled.store(true, Ordering::Release);
        self.caller.unpark();
    }
}
impl<'a, S: Send + Sync, R: Send> Context<'a, S, R> {
    #[inline]
    pub(crate) fn reserve<P: Project<R>>(
        &self,
        weight: usize,
    ) -> Option<Reservation<'_, 'a, S, R>> {
        if weight < self.pool.min_parallel_work || self.cancelled() {
            return None;
        }
        let slots = &self.pool.shared.slots;
        for offset in 0..slots.len() {
            let worker = P::KEY.wrapping_add(offset) % slots.len();
            if slots[worker]
                .state
                .compare_exchange(IDLE, RESERVED, Ordering::Acquire, Ordering::Relaxed)
                .is_ok()
            {
                return Some(Reservation {
                    context: self,
                    worker,
                    published: false,
                });
            }
        }
        None
    }
    #[inline]
    pub(crate) fn offer<P, K>(&self, executed: bool, weight: usize) -> bool
    where
        P: Project<R>,
        P::Target: System<S>,
        K: Continue<S, R>,
    {
        if let Some(reservation) = self.reserve::<P>(weight) {
            reservation.publish::<P, K>(executed);
            true
        } else {
            false
        }
    }
}
pub(crate) struct Reservation<'c, 'p, S, R> {
    context: &'c Context<'p, S, R>,
    worker: usize,
    published: bool,
}
impl<S: Send + Sync, R: Send> Reservation<'_, '_, S, R> {
    pub(crate) fn publish<P, K>(mut self, executed: bool)
    where
        P: Project<R>,
        P::Target: System<S>,
        K: Continue<S, R>,
    {
        let slot = &self.context.pool.shared.slots[self.worker];
        unsafe {
            (*slot.branch.get()).write(Branch {
                context: std::ptr::from_ref(self.context).cast(),
                call: invoke::<S, R, P, K>,
                executed,
            });
        }
        self.published = true;
        slot.state.store(READY, Ordering::Release);
        if self.worker == 0 {
            self.context.caller.unpark();
        } else {
            self.context.pool.threads[self.worker - 1].thread().unpark();
        }
    }
}
impl<S, R> Drop for Reservation<'_, '_, S, R> {
    fn drop(&mut self) {
        if !self.published {
            self.context.pool.shared.slots[self.worker]
                .state
                .store(IDLE, Ordering::Release);
        }
    }
}
unsafe fn invoke<S: Send + Sync, R: Send, P: Project<R>, K: Continue<S, R>>(
    pointer: *const (),
    executed: bool,
) where
    P::Target: System<S>,
{
    let context = unsafe { &*pointer.cast::<Context<'_, S, R>>() };
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            ACTIVE.with(|active| active.set(self.0));
        }
    }
    let _restore = Restore(ACTIVE.with(|active| active.replace(true)));
    if context.cancelled() {
        return;
    }
    let result = catch_unwind(AssertUnwindSafe(|| unsafe {
        P::Target::enter::<R, P, K>(context, executed)
    }));
    if let Err(payload) = result {
        context.fail(payload);
    }
}

pub(crate) unsafe fn execute<S: Send + Sync, R: System<S> + Send>(
    nodes: *mut R,
    world: *mut S,
) -> Option<()> {
    if !R::HAS_PARALLEL || serial() || R::WEIGHT < pool().min_parallel_work {
        return unsafe { (&mut *nodes).run_serial_unchecked(world) }.map(|_| ());
    }
    let pool = pool();
    let submission = pool.submission.lock().unwrap();
    let context = Context {
        nodes,
        world,
        pool,
        caller: thread::current(),
        done: AtomicBool::new(false),
        executed: AtomicBool::new(false),
        cancelled: AtomicBool::new(false),
        panic: Mutex::new(Vec::new()),
    };
    pool.shared.slots[0].state.store(RUNNING, Ordering::Relaxed);
    unsafe {
        invoke::<S, R, Root, Done>(std::ptr::from_ref(&context).cast(), false);
    }
    pool.shared.slots[0].state.store(IDLE, Ordering::Release);
    loop {
        // Caller participation also steals an assigned but unstarted branch.
        if pool.shared.slots[0].claim() {
            continue;
        }
        let mut helped = false;
        if pool.shared.slots[0]
            .state
            .compare_exchange(IDLE, RUNNING, Ordering::Acquire, Ordering::Relaxed)
            .is_ok()
        {
            for slot in &pool.shared.slots[1..] {
                if unsafe { slot.steal() } {
                    helped = true;
                    break;
                }
            }
            pool.shared.slots[0].state.store(IDLE, Ordering::Release);
        }
        if helped {
            continue;
        }
        if context.done.load(Ordering::Acquire) || context.cancelled.load(Ordering::Acquire) {
            if pool
                .shared
                .slots
                .iter()
                .all(|slot| slot.state.load(Ordering::Acquire) == IDLE)
            {
                break;
            }
            // Completion precedes callback return. Do not park waiting for an
            // acknowledgement that has no new work to publish or wake us for.
            thread::yield_now();
        } else {
            thread::park();
        }
    }
    let executed = context.executed.load(Ordering::Relaxed);
    let mut payloads = context.panic.into_inner().unwrap();
    drop(submission);
    if !payloads.is_empty() {
        let first = payloads.swap_remove(0);
        drop(payloads);
        resume_unwind(first);
    }
    executed.then_some(())
}
