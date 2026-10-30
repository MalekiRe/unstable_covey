//! Persistent workers for typed continuations. Straight-line work is ordinary
//! inlined code; only a published branch crosses threads through an erased entry.
use std::{
    any::Any,
    cell::{Cell, UnsafeCell},
    hint,
    marker::PhantomData,
    mem::MaybeUninit,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    sync::{
        Arc, Condvar, LazyLock, Mutex, PoisonError,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering, fence},
    },
    thread::{self, JoinHandle},
};

type Panic = Box<dyn Any + Send>;
thread_local! {
    static CURRENT: Cell<*const WorkerPool> = const { Cell::new(std::ptr::null()) };
    static SERIAL: Cell<bool> = const { Cell::new(false) };
}
static DEFAULT: LazyLock<WorkerPool> =
    LazyLock::new(|| WorkerPool::new(thread::available_parallelism().map_or(1, usize::from)));
fn pool() -> &'static WorkerPool {
    // The installed pool outlives its install scope; invocations never escape it.
    CURRENT
        .with(|current| unsafe { current.get().as_ref() })
        .unwrap_or_else(|| &DEFAULT)
}
/// Whether a group with this static weight should use workers on this thread.
/// Serial scopes are checked first, so serial execution never creates the pool.
#[inline]
pub(crate) fn parallel(weight: usize) -> bool {
    !SERIAL.with(Cell::get) && {
        let pool = pool();
        pool.workers() > 1 && weight >= pool.min_parallel_work
    }
}
/// Run with every nested group executing directly on this thread.
pub(crate) fn serially<T>(body: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            SERIAL.with(|serial| serial.set(self.0));
        }
    }
    let _restore = Restore(SERIAL.with(|serial| serial.replace(true)));
    body()
}

/// Idle iterations before a worker or waiting caller blocks.
const SPIN: u32 = 1 << 12;
const IDLE: usize = 0;
const READY: usize = 1;
const RUNNING: usize = 2;
struct Branch {
    context: *const (),
    call: unsafe fn(*const (), Flow, &AtomicUsize),
    flow: Flow,
}
#[repr(align(64))]
struct Slot {
    state: AtomicUsize,
    branch: UnsafeCell<MaybeUninit<Branch>>,
}
// IDLE -> RUNNING gives the owner or a publisher exclusive write access;
// READY -> RUNNING transfers the initialized branch to exactly one reader.
unsafe impl Sync for Slot {}
unsafe impl Send for Slot {}
impl Slot {
    fn take(&self) -> Option<Branch> {
        self.state
            .compare_exchange(READY, RUNNING, Ordering::Acquire, Ordering::Relaxed)
            .ok()?;
        Some(unsafe { (*self.branch.get()).assume_init_read() })
    }
}
struct Shared {
    slots: Box<[Slot]>,
    closing: AtomicBool,
    started: AtomicUsize,
    sleeping: AtomicBool,
    lock: Mutex<()>,
    wake: Condvar,
}
impl Shared {
    /// Run one published branch as worker `me`: its own slot first, then steal.
    fn help(&self, me: usize) -> bool {
        let own = &self.slots[me];
        // Branch entries release our slot themselves, before their lease.
        if let Some(branch) = own.take() {
            unsafe { (branch.call)(branch.context, branch.flow, &own.state) };
            return true;
        }
        for offset in 1..self.slots.len() {
            let other = &self.slots[(me + offset) % self.slots.len()];
            if other.state.load(Ordering::Relaxed) != READY {
                continue;
            }
            // Hold our own slot so nothing is published to us while busy.
            if own
                .state
                .compare_exchange(IDLE, RUNNING, Ordering::Acquire, Ordering::Relaxed)
                .is_err()
            {
                return true; // Work was just published to us.
            }
            let Some(branch) = other.take() else {
                own.state.store(IDLE, Ordering::Release);
                continue;
            };
            other.state.store(IDLE, Ordering::Release);
            unsafe { (branch.call)(branch.context, branch.flow, &own.state) };
            return true;
        }
        false
    }
    fn wake_caller(&self) {
        fence(Ordering::SeqCst);
        if self.sleeping.load(Ordering::Relaxed) {
            drop(self.lock.lock().unwrap_or_else(PoisonError::into_inner));
            self.wake.notify_one();
        }
    }
}

/// Persistent workers; the calling thread is worker 0.
pub struct WorkerPool {
    shared: Arc<Shared>,
    threads: Vec<JoinHandle<()>>,
    submission: Mutex<()>,
    min_parallel_work: usize,
}
impl WorkerPool {
    pub fn new(workers: usize) -> Self {
        assert!(workers > 0, "worker count must be positive");
        let mut pool = Self {
            shared: Arc::new(Shared {
                slots: (0..workers)
                    .map(|_| Slot {
                        state: AtomicUsize::new(IDLE),
                        branch: UnsafeCell::new(MaybeUninit::uninit()),
                    })
                    .collect(),
                closing: AtomicBool::new(false),
                started: AtomicUsize::new(0),
                sleeping: AtomicBool::new(false),
                lock: Mutex::new(()),
                wake: Condvar::new(),
            }),
            threads: Vec::with_capacity(workers - 1),
            submission: Mutex::new(()),
            min_parallel_work: 1,
        };
        for worker in 1..workers {
            let shared = pool.shared.clone();
            let thread = thread::Builder::new()
                .name(format!("ecs-{worker}"))
                .spawn(move || {
                    shared.started.fetch_add(1, Ordering::Release);
                    let mut idle = 0;
                    while !shared.closing.load(Ordering::Acquire) {
                        if shared.help(worker) {
                            idle = 0;
                        } else if idle < SPIN {
                            idle += 1;
                            hint::spin_loop();
                        } else {
                            idle = 0;
                            // Publication precedes unpark; the token survives this window.
                            thread::park();
                        }
                    }
                })
                .expect("cannot start ECS worker");
            pool.threads.push(thread);
        }
        // Worker startup allocates; finish it before the first measured frame.
        while pool.shared.started.load(Ordering::Acquire) != workers - 1 {
            thread::yield_now();
        }
        pool
    }
    pub fn workers(&self) -> usize {
        self.shared.slots.len()
    }
    /// Minimum static branch weight eligible for publication. Functions weigh
    /// one; groups sum their children. This is not a timing estimate.
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

/// A compile-time path to one child of the invocation root.
#[doc(hidden)]
pub trait Project<R> {
    type Target;
    /// Preferred worker for branches starting here; stable across frames.
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
pub trait Inner {
    type Inner;
    /// # Safety
    /// The wrapper allocation is live; only the wrapped value may be borrowed.
    unsafe fn inner(this: *mut Self) -> *mut Self::Inner;
}
#[doc(hidden)]
pub struct First<P>(PhantomData<P>);
#[doc(hidden)]
pub struct Second<P>(PhantomData<P>);
#[doc(hidden)]
pub struct Within<P>(PhantomData<P>);
impl<R, P: Project<R, Target: Fields>> Project<R> for First<P> {
    type Target = <P::Target as Fields>::Left;
    const KEY: usize = P::KEY.wrapping_mul(2).wrapping_add(1);
    unsafe fn get(root: *mut R) -> *mut Self::Target {
        unsafe { P::Target::left(P::get(root)) }
    }
}
impl<R, P: Project<R, Target: Fields>> Project<R> for Second<P> {
    type Target = <P::Target as Fields>::Right;
    const KEY: usize = P::KEY.wrapping_mul(2).wrapping_add(2);
    unsafe fn get(root: *mut R) -> *mut Self::Target {
        unsafe { P::Target::right(P::get(root)) }
    }
}
impl<R, P: Project<R, Target: Inner>> Project<R> for Within<P> {
    type Target = <P::Target as Inner>::Inner;
    const KEY: usize = P::KEY;
    unsafe fn get(root: *mut R) -> *mut Self::Target {
        unsafe { P::Target::inner(P::get(root)) }
    }
}

/// A path's Lamport change clock (latest tick it has observed or issued) and
/// whether any system on it ran, packed into one register.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct Flow(u64);
impl Flow {
    #[inline(always)]
    pub(crate) fn new(clock: u64, ran: bool) -> Self {
        Self(clock << 1 | ran as u64)
    }
    #[inline(always)]
    pub(crate) fn clock(self) -> u64 {
        self.0 >> 1
    }
    #[inline(always)]
    pub(crate) fn ran(self) -> bool {
        self.0 & 1 != 0
    }
}

/// The next outlined continuation, returned instead of called so stack depth
/// stays constant however many joins a frame passes through.
#[doc(hidden)]
pub struct Tail<S, R>(Option<(Entry<S, R>, Flow)>);
type Entry<S, R> = unsafe fn(&Context<'_, S, R>, Flow) -> Tail<S, R>;
impl<S, R> Tail<S, R> {
    pub(crate) const DONE: Self = Self(None);
    /// Continue with K's single shared out-of-line copy.
    #[inline(always)]
    pub(crate) fn to<K: Continue<S, R>>(flow: Flow) -> Self {
        Self(Some((outlined::<S, R, K>, flow)))
    }
    #[inline(always)]
    pub(crate) fn or(self, other: Self) -> Self {
        Self(self.0.or(other.0))
    }
}
#[inline(never)]
unsafe fn outlined<S, R, K: Continue<S, R>>(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
    unsafe { K::resume(context, flow) }
}
/// Run K's shared outlined copy and every continuation it hands back.
#[inline]
pub(crate) unsafe fn run<S, R, K: Continue<S, R>>(context: &Context<'_, S, R>, flow: Flow) {
    let mut tail = Tail::to::<K>(flow);
    while let Some((entry, flow)) = tail.0 {
        tail = unsafe { entry(context, flow) };
    }
}

/// What runs after a region finishes.
#[doc(hidden)]
pub trait Continue<S, R> {
    /// # Safety
    /// Every predecessor of this continuation has relinquished its accesses.
    unsafe fn resume(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R>;
}
/// Enter the system at P, then K.
#[doc(hidden)]
pub struct Then<P, K>(PhantomData<(P, K)>);
impl<S: Send + Sync, R: Send, P: Project<R, Target: crate::System<S>>, K: Continue<S, R>>
    Continue<S, R> for Then<P, K>
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
        unsafe { <P::Target as crate::System<S>>::enter::<R, P, K>(context, flow) }
    }
}
/// K reachable from several sites, emitted once out of line.
#[doc(hidden)]
pub struct Outlined<K>(PhantomData<K>);
impl<S, R, K: Continue<S, R>> Continue<S, R> for Outlined<K> {
    #[inline(always)]
    unsafe fn resume(_: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
        Tail::to::<K>(flow)
    }
}
#[doc(hidden)]
pub struct Done;
impl<S, R> Continue<S, R> for Done {
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
        context.result.store(flow.0, Ordering::Relaxed);
        Tail::DONE
    }
}

/// A join whose predecessors may finish on different workers: remaining
/// arrivals in the low half of `count`, executed arrivals above, and the
/// latest clock among arrivals.
#[doc(hidden)]
#[derive(Default)]
pub struct Join {
    count: AtomicU64,
    clock: AtomicU64,
}
impl Join {
    #[inline]
    pub(crate) fn reset(&self, arrivals: usize) {
        self.count.store(arrivals as u64, Ordering::Relaxed);
        self.clock.store(0, Ordering::Relaxed);
    }
    /// The merged flow, for the last arrival only.
    #[inline]
    pub(crate) fn arrive(&self, flow: Flow) -> Option<Flow> {
        self.clock.fetch_max(flow.clock(), Ordering::Relaxed);
        // Remaining is at least one, so the low half never borrows.
        let delta = if flow.ran() { (1 << 32) - 1 } else { u64::MAX };
        // AcqRel orders every arrival's clock before the last arrival's load.
        let old = self.count.fetch_add(delta, Ordering::AcqRel);
        (old as u32 == 1).then(|| {
            Flow::new(
                self.clock.load(Ordering::Relaxed),
                flow.ran() || old >> 32 != 0,
            )
        })
    }
}

/// One invocation: the root, its world, and branch leases.
#[doc(hidden)]
pub struct Context<'a, S, R> {
    pub(crate) nodes: *mut R,
    pub(crate) world: *mut S,
    pub(crate) event: *const (),
    pool: &'a WorkerPool,
    /// Live leases: the root path plus every published, unfinished branch.
    pending: AtomicUsize,
    result: AtomicU64,
    cancelled: AtomicBool,
    panic: Mutex<Vec<Panic>>,
}
// Typed paths hand disjoint children to branches; static access checks and
// joins order world accesses. The context outlives every lease.
unsafe impl<S: Send + Sync, R: Send> Sync for Context<'_, S, R> {}
impl<S: Send + Sync, R: Send> Context<'_, S, R> {
    /// Publish C to an idle worker. False means run it here instead.
    #[inline(always)]
    pub(crate) fn offer<C: Continue<S, R>>(&self, flow: Flow, weight: usize, key: usize) -> bool {
        let branch = Branch {
            context: std::ptr::from_ref(self).cast(),
            call: invoke::<S, R, C>,
            flow,
        };
        weight >= self.pool.min_parallel_work
            && self
                .pool
                .publish(branch, key, &self.cancelled, &self.pending)
    }
}
impl WorkerPool {
    // Shared by every fork site; only the branch entry is monomorphized.
    fn publish(
        &self,
        branch: Branch,
        key: usize,
        cancelled: &AtomicBool,
        pending: &AtomicUsize,
    ) -> bool {
        if cancelled.load(Ordering::Relaxed) {
            return false;
        }
        let slots = &self.shared.slots;
        for offset in 0..slots.len() {
            let worker = key.wrapping_add(offset) % slots.len();
            let slot = &slots[worker];
            // Load first: when every worker is busy, a fork costs no atomic writes.
            if slot.state.load(Ordering::Relaxed) != IDLE
                || slot
                    .state
                    .compare_exchange(IDLE, RUNNING, Ordering::Acquire, Ordering::Relaxed)
                    .is_err()
            {
                continue;
            }
            unsafe { (*slot.branch.get()).write(branch) };
            // The publisher holds a lease, so pending cannot reach zero first.
            pending.fetch_add(1, Ordering::Relaxed);
            slot.state.store(READY, Ordering::Release);
            if worker == 0 {
                self.shared.wake_caller();
            } else {
                self.threads[worker - 1].thread().unpark();
            }
            return true;
        }
        false
    }
}
/// Run a branch while holding `slot`. The slot is released before the lease,
/// so a finished invocation leaves every worker it used available again.
unsafe fn invoke<S: Send + Sync, R: Send, C: Continue<S, R>>(
    pointer: *const (),
    flow: Flow,
    slot: &AtomicUsize,
) {
    let context = unsafe { &*pointer.cast::<Context<'_, S, R>>() };
    // Shared outlives this call: every worker thread owns an Arc to it.
    let shared: &Shared = &context.pool.shared;
    if !context.cancelled.load(Ordering::Relaxed) {
        let result = catch_unwind(AssertUnwindSafe(|| {
            serially(|| unsafe { run::<S, R, C>(context, flow) })
        }));
        if let Err(payload) = result {
            context
                .panic
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(payload);
            context.cancelled.store(true, Ordering::Relaxed);
        }
    }
    slot.store(IDLE, Ordering::Release);
    // The context may be freed once the last lease is released.
    if context.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
        shared.wake_caller();
    }
}

/// Run continuation C from the root as one invocation, starting from `flow`.
/// Resumes the first panic after every lease is released.
pub(crate) unsafe fn execute<S: Send + Sync, R: Send, C: Continue<S, R>>(
    nodes: *mut R,
    world: *mut S,
    event: *const (),
    flow: Flow,
) -> Flow {
    let pool = pool();
    let submission = pool
        .submission
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let context = Context {
        nodes,
        world,
        event,
        pool,
        pending: AtomicUsize::new(1),
        result: AtomicU64::new(flow.0),
        cancelled: AtomicBool::new(false),
        panic: Mutex::new(Vec::new()),
    };
    let shared = &*pool.shared;
    shared.slots[0].state.store(RUNNING, Ordering::Relaxed);
    let root = std::ptr::from_ref(&context).cast();
    unsafe { invoke::<S, R, C>(root, flow, &shared.slots[0].state) };
    let mut idle = 0;
    while context.pending.load(Ordering::Acquire) != 0 {
        if shared.help(0) {
            idle = 0;
        } else if idle < SPIN {
            idle += 1;
            hint::spin_loop();
        } else {
            idle = 0;
            shared.sleeping.store(true, Ordering::Relaxed);
            fence(Ordering::SeqCst);
            let mut guard = shared.lock.lock().unwrap_or_else(PoisonError::into_inner);
            while context.pending.load(Ordering::Acquire) != 0
                && shared.slots[0].state.load(Ordering::Acquire) != READY
            {
                guard = shared
                    .wake
                    .wait(guard)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            drop(guard);
            shared.sleeping.store(false, Ordering::Relaxed);
        }
    }
    let result = Flow(context.result.load(Ordering::Relaxed));
    let mut payloads = context
        .panic
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    drop(submission);
    if !payloads.is_empty() {
        let first = payloads.swap_remove(0);
        drop(payloads);
        resume_unwind(first);
    }
    result
}
