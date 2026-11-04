//! Remote bridges: futures on any thread or runtime reach the world at a sync
//! point. `sync_point(Label)` returns a plugin that drives the point when the
//! app reaches it while emitting `Label`, and a `Remote` handle to bridge
//! through it. The driver grants queued bridges in waves of non-conflicting
//! access: each granted bridge runs its closure on its own task's thread, in
//! parallel with the rest of the wave, then the driver applies their commands
//! in grant order. Closures never cross threads, so they need not be `Send`.
//!
//! Synchronization: one short lock guards the request list (enqueue,
//! admission, cancellation); a granted bridge reads the published wave after
//! an acquire load of its grant and reports completion with one decrement.
//! Requests live inside their pinned futures: nothing allocates per request.
use std::any::TypeId;
use std::cell::{Cell, UnsafeCell};
use std::marker::{PhantomData, PhantomPinned};
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::thread::{self, Thread};

use crate::access::{Exclusive, Filtered, QueryAccess, Read, Write};
use crate::bridge::{BridgeCell, BridgeError, BridgeFn, StateBox, StateRef, in_task, site};
use crate::change::{SystemWorld, next};
use crate::command::CommandQueue;
use crate::dispatch::{Cons, Emitted, Fits, Label, MaybeHandle, Plugins, Schedule};
use crate::executor::{self, Continue, Flow, Project, Tail};
use crate::lookup::ComponentLookup;
use crate::{Storage, World};

/// Words of an access mask: worlds of up to 256 components.
const WORDS: usize = 4;
type Mask = [u64; WORDS];

/// An access type's component bits in world `S`.
#[doc(hidden)]
pub trait Masks<S: Storage> {
    const READ: Mask = [0; WORDS];
    const WRITE: Mask = [0; WORDS];
    const EXCLUSIVE: bool = false;
}
const fn bit(index: usize) -> Mask {
    assert!(
        index < WORDS * 64,
        "remote bridges support worlds of up to 256 components"
    );
    let mut mask = [0; WORDS];
    mask[index / 64] = 1 << (index % 64);
    mask
}
const fn or(a: Mask, b: Mask) -> Mask {
    let mut i = 0;
    let mut mask = a;
    while i < WORDS {
        mask[i] |= b[i];
        i += 1;
    }
    mask
}
fn overlaps(a: &Mask, b: &Mask) -> bool {
    a.iter().zip(b).any(|(a, b)| a & b != 0)
}
impl<S: Storage> Masks<S> for () {}
impl<S: Storage> Masks<S> for Exclusive {
    const EXCLUSIVE: bool = true;
}
impl<S: ComponentLookup<T>, T: 'static> Masks<S> for Read<T> {
    const READ: Mask = bit(<S as ComponentLookup<T>>::INDEX);
}
impl<S: ComponentLookup<T>, T: 'static> Masks<S> for Write<T> {
    const WRITE: Mask = bit(<S as ComponentLookup<T>>::INDEX);
}
impl<S: Storage, A: Masks<S>, B: Masks<S>> Masks<S> for (A, B) {
    const READ: Mask = or(A::READ, B::READ);
    const WRITE: Mask = or(A::WRITE, B::WRITE);
    const EXCLUSIVE: bool = A::EXCLUSIVE || B::EXCLUSIVE;
}
impl<S: Storage, A: Masks<S>, B: Masks<S>> Masks<S> for QueryAccess<A, B> {
    const READ: Mask = or(A::READ, B::READ);
    const WRITE: Mask = or(A::WRITE, B::WRITE);
    const EXCLUSIVE: bool = A::EXCLUSIVE || B::EXCLUSIVE;
}
// Filters only narrow rows; admission stays conservative and ignores them.
impl<S: Storage, A: Masks<S>, F> Masks<S> for Filtered<A, F> {
    const READ: Mask = A::READ;
    const WRITE: Mask = A::WRITE;
    const EXCLUSIVE: bool = A::EXCLUSIVE;
}

const IDLE: u8 = 0;
const QUEUED: u8 = 1;
const GRANTED: u8 = 2;
const DONE: u8 = 3;

/// A queued request, inside its pinned `RemoteBridge`. Links, waker, and
/// epoch change only under the list lock; `slot` is written by the driver
/// before the grant that publishes it.
struct Node<S: Storage> {
    prev: Cell<*const Node<S>>,
    next: Cell<*const Node<S>>,
    state: AtomicU8,
    waker: UnsafeCell<Option<Waker>>,
    /// The drive during which this request was queued.
    epoch: Cell<u64>,
    slot: Cell<*mut CommandQueue<S>>,
    read: Mask,
    write: Mask,
    exclusive: bool,
    stamp: *const Cell<u64>,
    site: TypeId,
}

struct List<S: Storage> {
    head: *const Node<S>,
    tail: *const Node<S>,
    /// Drives started so far.
    epoch: u64,
    closed: bool,
}
impl<S: Storage> List<S> {
    /// # Safety
    /// Lock held; `node` is live, pinned, and unlinked.
    unsafe fn push(&mut self, node: &Node<S>) {
        node.prev.set(self.tail);
        node.next.set(std::ptr::null());
        match unsafe { self.tail.as_ref() } {
            Some(tail) => tail.next.set(node),
            None => self.head = node,
        }
        self.tail = node;
    }
    /// # Safety
    /// Lock held; `node` is live and linked in this list.
    unsafe fn unlink(&mut self, node: &Node<S>) {
        let (prev, next) = (node.prev.get(), node.next.get());
        match unsafe { prev.as_ref() } {
            Some(prev) => prev.next.set(next),
            None => self.head = next,
        }
        match unsafe { next.as_ref() } {
            Some(next) => next.prev.set(prev),
            None => self.tail = prev,
        }
    }
}

/// What granted bridges read: published by the driver before granting.
struct Wave<S: Storage> {
    world: *mut World<S>,
    /// The clock readers see, and the tick writers issue.
    read: u64,
    write: u64,
    driver: Option<Thread>,
}

/// A sync point's shared state.
struct Point<S: Storage> {
    list: Mutex<List<S>>,
    /// Requests in `list`, for the driver's lock-free idle check.
    queued: AtomicUsize,
    /// Granted, unfinished bridges plus one for the waiting driver.
    pending: AtomicUsize,
    wave: UnsafeCell<Wave<S>>,
}
// SAFETY: links and wakers change only under the lock; `wave` is written by
// the driver only while no bridge is granted, and read by granted bridges
// after the acquire load of their grant.
unsafe impl<S: Storage> Send for Point<S> {}
unsafe impl<S: Storage> Sync for Point<S> {}
impl<S: Storage> Point<S> {
    fn lock(&self) -> MutexGuard<'_, List<S>> {
        self.list
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
    /// A granted bridge finished or was dropped. The last one wakes the
    /// driver, then releases it, so the driver outlives the wake.
    fn complete(&self) {
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 2 {
            // SAFETY: the driver set `driver` before granting and waits for
            // `pending` to reach zero before touching `wave` again.
            unsafe { (*self.wave.get()).driver.as_ref() }
                .expect("a granting driver")
                .unpark();
            self.pending.store(0, Ordering::Release);
        }
    }
}

/// Unique wave identifiers, so shared state is granted once per wave.
static WAVES: AtomicU64 = AtomicU64::new(1);

/// Create a sync point on `label`: add the returned `SyncPoint` to an app,
/// where its position in registration order decides when, during `emit` of
/// `label`, queued bridges run; bridge from any thread through the `Remote`.
pub fn sync_point<S, E, X, LM>(_: X) -> (SyncPoint<S, E>, Remote<S, E>)
where
    S: Storage,
    E: Schedule<Route = Emitted>,
    X: Label<E, LM>,
{
    let point = Arc::new(Point {
        list: Mutex::new(List {
            head: std::ptr::null(),
            tail: std::ptr::null(),
            epoch: 0,
            closed: false,
        }),
        queued: AtomicUsize::new(0),
        pending: AtomicUsize::new(0),
        wave: UnsafeCell::new(Wave {
            world: std::ptr::null_mut(),
            read: 0,
            write: 0,
            driver: None,
        }),
    });
    let remote = Remote {
        point: point.clone(),
        label: PhantomData,
    };
    let driver = SyncPoint {
        point,
        drive: Drive::default(),
        label: PhantomData,
    };
    (driver, remote)
}

/// The driver's buffers, reused across drives so warm drives allocate nothing.
struct Drive<S: Storage> {
    grants: Vec<*const Node<S>>,
    wakers: Vec<Waker>,
    /// (task waker, bridge site) pairs granted during this drive.
    history: Vec<(*const (), TypeId)>,
    slots: Vec<CommandQueue<S>>,
}
impl<S: Storage> Default for Drive<S> {
    fn default() -> Self {
        Self {
            grants: Vec::new(),
            wakers: Vec::new(),
            history: Vec::new(),
            slots: Vec::new(),
        }
    }
}
// SAFETY: node pointers are only dereferenced while their requests are linked
// under the lock, and are cleared before the drive ends.
unsafe impl<S: Storage> Send for Drive<S> {}
unsafe impl<S: Storage> Sync for Drive<S> {}

/// A sync point's driver, as a plugin: runs queued remote bridges when the
/// app reaches it while emitting `E`. Dropping it (with its app) closes the
/// point: pending and later bridges fail with `BridgeError::Closed`.
pub struct SyncPoint<S: Storage, E> {
    point: Arc<Point<S>>,
    drive: Drive<S>,
    label: PhantomData<fn() -> E>,
}
impl<S: Storage, E> SyncPoint<S, E> {
    /// Grant waves until no queued request is admissible. Returns whether
    /// any bridge ran.
    /// # Safety
    /// `world` is live and held exclusively by this call.
    unsafe fn run(&mut self, world: *mut World<S>, clock: &mut u64) -> bool {
        let point = &*self.point;
        if point.queued.load(Ordering::Acquire) == 0 {
            return false;
        }
        let drive = &mut self.drive;
        // SAFETY: no bridge is granted between drives.
        unsafe {
            let wave = &mut *point.wave.get();
            wave.world = world;
            let current = thread::current();
            if wave.driver.as_ref().is_none_or(|t| t.id() != current.id()) {
                wave.driver = Some(current);
            }
        }
        drive.history.clear();
        let mut epoch = None;
        let mut ran = false;
        loop {
            let mut list = point.lock();
            let epoch = *epoch.get_or_insert_with(|| {
                list.epoch += 1;
                list.epoch
            });
            let id = WAVES.fetch_add(1, Ordering::Relaxed);
            let (mut read, mut write, mut writes) = ([0; WORDS], [0; WORDS], false);
            let mut node = list.head;
            // SAFETY: the lock is held, so linked nodes are live.
            while let Some(request) = unsafe { node.as_ref() } {
                node = request.next.get();
                let waker = unsafe { (*request.waker.get()).as_ref() }.expect("queued waker");
                let repeated = request.epoch.get() == epoch
                    && drive
                        .history
                        .iter()
                        .any(|&(task, site)| task == waker.data() && site == request.site);
                let conflicts = (request.exclusive && !drive.grants.is_empty())
                    || overlaps(&request.write, &read)
                    || overlaps(&request.write, &write)
                    || overlaps(&request.read, &write);
                let stamped = unsafe { request.stamp.as_ref() }.is_some_and(|s| s.get() == id);
                if repeated || conflicts || stamped {
                    continue;
                }
                unsafe { list.unlink(request) };
                if let Some(stamp) = unsafe { request.stamp.as_ref() } {
                    stamp.set(id);
                }
                read = or(read, request.read);
                write = or(write, request.write);
                writes |= request.write != [0; WORDS] || request.exclusive;
                let waker = unsafe { (*request.waker.get()).take() }.expect("queued waker");
                drive.history.push((waker.data(), request.site));
                drive.wakers.push(waker);
                drive.grants.push(request);
                if request.exclusive {
                    break;
                }
            }
            let granted = drive.grants.len();
            if granted == 0 {
                break;
            }
            ran = true;
            if drive.slots.len() < granted {
                drive.slots.resize_with(granted, CommandQueue::default);
            }
            // SAFETY: nothing is granted yet; the world is exclusively ours.
            unsafe {
                let wave = &mut *point.wave.get();
                wave.read = *clock;
                wave.write = if writes { next(*clock) } else { *clock };
                World::set_clock(world, *clock);
            }
            point.pending.store(granted + 1, Ordering::Relaxed);
            point.queued.fetch_sub(granted, Ordering::Relaxed);
            for (request, slot) in drive.grants.iter().zip(&mut drive.slots) {
                // SAFETY: unlinked under this lock: live until it completes.
                let request = unsafe { &**request };
                request.slot.set(slot);
                request.state.store(GRANTED, Ordering::Release);
            }
            drive.grants.clear();
            drop(list);
            for waker in drive.wakers.drain(..) {
                waker.wake();
            }
            // One left means the last bridge is between its unpark and its
            // release: that unpark may already be consumed, so don't park.
            loop {
                match point.pending.load(Ordering::Acquire) {
                    0 => break,
                    1 => thread::yield_now(),
                    _ => thread::park(),
                }
            }
            // SAFETY: every granted bridge has finished with the world.
            unsafe {
                let queue = &mut *World::queue(world);
                for slot in &mut drive.slots[..granted] {
                    queue.append(slot);
                }
                let write = (*point.wave.get()).write;
                World::set_clock(world, World::clock(world).max(write));
                (*world).flush();
                *clock = World::clock(world);
            }
        }
        ran
    }
}
impl<S: Storage, E> Drop for SyncPoint<S, E> {
    fn drop(&mut self) {
        let mut list = self.point.lock();
        list.closed = true;
        let mut wakers = Vec::new();
        // SAFETY: the lock is held, so linked nodes are live.
        while let Some(request) = unsafe { list.head.as_ref() } {
            unsafe { list.unlink(request) };
            wakers.extend(unsafe { (*request.waker.get()).take() });
            request.state.store(IDLE, Ordering::Release);
        }
        self.point.queued.store(0, Ordering::Relaxed);
        drop(list);
        wakers.into_iter().for_each(Waker::wake);
    }
}

impl<S: Storage, E, C: Storage, L, K> Plugins<C, L, K> for SyncPoint<S, E> {
    type Components = C;
    type Listeners = Cons<Self, L>;
    type Hooks = K;
    fn add_to(self, listeners: L, hooks: K) -> (Self::Listeners, K) {
        (Cons(self, listeners), hooks)
    }
}
impl<S: Storage, E> Fits<World<S>> for SyncPoint<S, E> {
    const VALID: bool = true;
}
impl<E: 'static, S: Storage> MaybeHandle<E, World<S>> for SyncPoint<S, E> {
    const HAS_PARALLEL: bool = false;
    const WEIGHT: usize = 0;
    #[inline]
    unsafe fn maybe_handle(&mut self, _: *const (), world: &mut World<S>) {
        let world: *mut World<S> = world;
        unsafe {
            let mut clock = World::clock(world);
            self.run(world, &mut clock);
            World::set_clock(world, clock);
        }
    }
    #[inline]
    fn maybe_defer(&mut self, _: &mut <World<S> as SystemWorld>::Queue) {}
    #[inline]
    unsafe fn maybe_enter<R: Send, P: Project<R, Target = Self>, K: Continue<World<S>, R>>(
        context: &executor::Context<'_, World<S>, R>,
        flow: Flow,
    ) -> Tail<World<S>, R>
    where
        World<S>: Send + Sync,
    {
        let mut clock = flow.clock();
        // SAFETY: registrations run in order, so this driver holds the world.
        unsafe {
            let ran = (*P::get(context.nodes)).run(context.world, &mut clock);
            K::resume(context, Flow::new(clock, flow.ran() || ran))
        }
    }
}

/// A handle for bridging into an app from any thread, at sync point `E`.
/// Cheap to clone; await its bridges on threads other than the app's, since
/// the app waits for a granted bridge to run.
pub struct Remote<S: Storage, E> {
    point: Arc<Point<S>>,
    label: PhantomData<fn() -> E>,
}
impl<S: Storage, E> Clone for Remote<S, E> {
    fn clone(&self) -> Self {
        Self {
            point: self.point.clone(),
            label: PhantomData,
        }
    }
}
impl<S: Storage, E> Remote<S, E> {
    /// Run `function` once at this sync point, with fresh parameter state,
    /// on the awaiting thread, in parallel with other bridges of its wave.
    pub fn bridge<F, M>(&self, function: F) -> RemoteBridge<S, F, M, BridgeCell<F::State>>
    where
        F: BridgeFn<S, M, Access: Masks<S>>,
    {
        RemoteBridge::new(self.point.clone(), function, BridgeCell::default(), false)
    }
    /// As `bridge`, but retry at the next drive while a parameter cannot be fetched.
    pub fn until<F, M>(&self, function: F) -> RemoteBridge<S, F, M, BridgeCell<F::State>>
    where
        F: BridgeFn<S, M, Access: Masks<S>>,
    {
        RemoteBridge::new(self.point.clone(), function, BridgeCell::default(), true)
    }
    /// Parameter state kept across this point's bridges, as `AsyncWorld::state`.
    /// Bridges sharing it are never granted in the same wave.
    pub fn state<T: Default>(&self) -> RemoteState<S, E, T> {
        RemoteState {
            shared: Arc::default(),
            remote: self.clone(),
        }
    }
}

/// Shared parameter state for remote bridges, from `Remote::state`.
pub struct RemoteState<S: Storage, E, T> {
    shared: Arc<StateBox<T>>,
    remote: Remote<S, E>,
}
impl<S: Storage, E, T> Clone for RemoteState<S, E, T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            remote: self.remote.clone(),
        }
    }
}
impl<S: Storage, E, T> RemoteState<S, E, T> {
    /// `Remote::bridge` through this state.
    pub fn bridge<F, M>(&self, function: F) -> RemoteBridge<S, F, M, Arc<StateBox<T>>>
    where
        F: BridgeFn<S, M, State = T, Access: Masks<S>>,
    {
        let point = self.remote.point.clone();
        RemoteBridge::new(point, function, self.shared.clone(), false)
    }
    /// `Remote::until` through this state.
    pub fn until<F, M>(&self, function: F) -> RemoteBridge<S, F, M, Arc<StateBox<T>>>
    where
        F: BridgeFn<S, M, State = T, Access: Masks<S>>,
    {
        let point = self.remote.point.clone();
        RemoteBridge::new(point, function, self.shared.clone(), true)
    }
}

/// A pending remote bridge. Its request lives inside it, so it is pinned
/// once polled; dropping it withdraws or completes the request.
pub struct RemoteBridge<S: Storage, F, M, St> {
    node: Node<S>,
    point: Arc<Point<S>>,
    function: Option<F>,
    state: St,
    wait: bool,
    marker: PhantomData<fn() -> M>,
    _pinned: PhantomPinned,
}
// SAFETY: the request's raw links are only followed under the point's lock.
unsafe impl<S: Storage, F: Send, M, St: Send> Send for RemoteBridge<S, F, M, St> {}
impl<S: Storage, F, M, St> RemoteBridge<S, F, M, St> {
    fn new(point: Arc<Point<S>>, function: F, state: St, wait: bool) -> Self
    where
        F: BridgeFn<S, M, Access: Masks<S>>,
        St: StateRef<F::State>,
    {
        let exclusive = F::EXCLUSIVE || <F::Access as Masks<S>>::EXCLUSIVE;
        Self {
            node: Node {
                prev: Cell::new(std::ptr::null()),
                next: Cell::new(std::ptr::null()),
                state: AtomicU8::new(IDLE),
                waker: UnsafeCell::new(None),
                epoch: Cell::new(0),
                slot: Cell::new(std::ptr::null_mut()),
                read: <F::Access as Masks<S>>::READ,
                write: <F::Access as Masks<S>>::WRITE,
                exclusive,
                stamp: state.stamp(),
                site: site::<F>(),
            },
            point,
            function: Some(function),
            state,
            wait,
            marker: PhantomData,
            _pinned: PhantomPinned,
        }
    }
    /// Queue the pinned request for the next drive, or fail if closed.
    fn enqueue(&self, waker: &Waker) -> Result<(), BridgeError> {
        let mut list = self.point.lock();
        if list.closed {
            return Err(BridgeError::Closed);
        }
        // SAFETY: under the lock; the pinned node stays put until unlinked.
        unsafe {
            *self.node.waker.get() = Some(waker.clone());
            self.node.epoch.set(list.epoch);
            list.push(&self.node);
        }
        self.node.state.store(QUEUED, Ordering::Relaxed);
        self.point.queued.fetch_add(1, Ordering::Release);
        Ok(())
    }
}
impl<S, F, M, St> Future for RemoteBridge<S, F, M, St>
where
    S: Storage,
    F: BridgeFn<S, M>,
    St: StateRef<F::State>,
{
    type Output = Result<F::Out, BridgeError>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        const {
            assert!(
                F::VALID,
                "invalid bridge: missing/ambiguous storage or conflicting access"
            );
        }
        assert!(
            !in_task(),
            "remote bridges wait for their sync point: inside a task, use `AsyncWorld::bridge`"
        );
        // SAFETY: no field is moved out of the pinned future.
        let this = unsafe { self.get_unchecked_mut() };
        loop {
            match this.node.state.load(Ordering::Acquire) {
                IDLE => {
                    return match this.enqueue(cx.waker()) {
                        Ok(()) => Poll::Pending,
                        Err(error) => {
                            this.node.state.store(DONE, Ordering::Relaxed);
                            Poll::Ready(Err(error))
                        }
                    };
                }
                QUEUED => {
                    let _list = this.point.lock();
                    if this.node.state.load(Ordering::Relaxed) != QUEUED {
                        continue; // granted or closed meanwhile
                    }
                    // SAFETY: the waker changes only under the lock.
                    let waker = unsafe { &mut *this.node.waker.get() };
                    if !waker.as_ref().is_some_and(|w| w.will_wake(cx.waker())) {
                        *waker = Some(cx.waker().clone());
                    }
                    return Poll::Pending;
                }
                GRANTED => return this.run(cx.waker()),
                _ => panic!("remote bridge polled after completion"),
            }
        }
    }
}
impl<S: Storage, F, M, St> RemoteBridge<S, F, M, St>
where
    F: BridgeFn<S, M>,
    St: StateRef<F::State>,
{
    /// Run the granted bridge on this thread, then report completion.
    fn run(&mut self, waker: &Waker) -> Poll<Result<F::Out, BridgeError>> {
        let point = &*self.point;
        let function = self
            .function
            .take()
            .expect("granted bridge has its closure");
        // SAFETY: the grant's acquire load published the wave and slot; the
        // driver waits for this bridge, whose access no wave peer conflicts
        // with, and the state is granted to it alone.
        unsafe {
            let wave = &*point.wave.get();
            let world = wave.world;
            let id = World::world_id(world);
            let now = if !F::EXCLUSIVE && <F::Access as crate::access::Access>::WRITES {
                wave.write
            } else {
                wave.read
            };
            let cell = &mut *self.state.cell();
            let window = cell.window(id, now);
            match function.call(world, &mut cell.params, window) {
                Ok(output) => {
                    cell.ran(id, now);
                    F::defer(&mut cell.params, &mut *self.node.slot.get());
                    self.node.state.store(DONE, Ordering::Relaxed);
                    point.complete();
                    Poll::Ready(Ok(output))
                }
                Err(function) if self.wait => {
                    self.function = Some(function);
                    let queued = self.enqueue(waker);
                    point.complete();
                    match queued {
                        Ok(()) => Poll::Pending,
                        Err(error) => Poll::Ready(Err(error)),
                    }
                }
                Err(_) => {
                    self.node.state.store(DONE, Ordering::Relaxed);
                    point.complete();
                    Poll::Ready(Err(BridgeError::Invalid))
                }
            }
        }
    }
}
impl<S: Storage, F, M, St> Drop for RemoteBridge<S, F, M, St> {
    fn drop(&mut self) {
        let mut state = self.node.state.load(Ordering::Acquire);
        if state == QUEUED {
            let mut list = self.point.lock();
            state = self.node.state.load(Ordering::Acquire);
            if state == QUEUED {
                // SAFETY: still linked under the lock.
                unsafe {
                    list.unlink(&self.node);
                    (*self.node.waker.get()).take();
                }
                self.point.queued.fetch_sub(1, Ordering::Relaxed);
                return;
            }
        }
        if state == GRANTED {
            // Granted but never run (or unwinding from the closure).
            self.point.complete();
        }
    }
}
