//! Remote bridges: futures on any thread or runtime reach the world at a sync
//! point. A sync point is a marker type; `sync_point(P)` is the system that
//! drives it, and any `Remote` handle of the world bridges to it by name:
//! `remote.bridge(P, ...)`. Placing the system declares `P` in the world's
//! inferred schema, so its queue sits at a compile-time index in the world.
//! The driver grants queued bridges in waves of non-conflicting access: each
//! granted bridge runs its closure on its own task's thread, in parallel with
//! the rest of the wave, then the driver applies their commands in grant
//! order. Closures never cross threads, so they need not be `Send`.
//!
//! Synchronization: one short lock guards the request list (enqueue,
//! admission, cancellation); a granted bridge reads the published wave after
//! an acquire load of its grant and reports completion with one decrement.
//! Requests live inside their pinned futures: nothing allocates per request.
use std::any::TypeId;
use std::cell::{Cell, UnsafeCell};
use std::marker::{PhantomData, PhantomPinned};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::task::{Context, Poll, Waker};
use std::thread::{self, Thread};

use crate::access::{
    Access, Against, Drives, Exclusive, Filtered, QueryAccess, Read, ValidAccess, Write,
};
use crate::bridge::{BridgeCell, BridgeError, BridgeFn, StateBox, StateRef, in_task, site};
use crate::change::{SystemWorld, next};
use crate::command::CommandQueue;
use crate::executor::{self, Continue, Flow, Project, Tail, Wait};
use crate::lookup::{ComponentLookup, PointLookup};
use crate::param::ParamShape;
use crate::plan::{Input, IntoSystems, NoInput, Shape, accesses, sealed};
use crate::{Storage, System, World};

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

/// One sync point's queue and wave, owned by its world.
pub(crate) struct PointState<S: Storage> {
    list: Mutex<List<S>>,
    /// Requests in `list`, for the driver's lock-free idle check.
    queued: AtomicUsize,
    /// Granted, unfinished bridges plus one for the waiting driver.
    pending: AtomicUsize,
    /// Set while a driver drives this point; a second driver skips.
    driving: AtomicBool,
    wave: UnsafeCell<Wave<S>>,
}
// SAFETY: links and wakers change only under the lock; `wave` is written by
// the one driving driver only while no bridge is granted, and read by
// granted bridges after the acquire load of their grant.
unsafe impl<S: Storage> Send for PointState<S> {}
unsafe impl<S: Storage> Sync for PointState<S> {}
impl<S: Storage> PointState<S> {
    fn new() -> Self {
        Self {
            list: Mutex::new(List {
                head: std::ptr::null(),
                tail: std::ptr::null(),
                epoch: 0,
                closed: false,
            }),
            queued: AtomicUsize::new(0),
            pending: AtomicUsize::new(0),
            driving: AtomicBool::new(false),
            wave: UnsafeCell::new(Wave {
                world: std::ptr::null_mut(),
                read: 0,
                write: 0,
                driver: None,
            }),
        }
    }
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
    /// Fail every queued request, and every later one.
    fn close(&self) {
        let mut list = self.lock();
        list.closed = true;
        let mut wakers = Vec::new();
        // SAFETY: the lock is held, so linked nodes are live.
        while let Some(request) = unsafe { list.head.as_ref() } {
            unsafe { list.unlink(request) };
            wakers.extend(unsafe { (*request.waker.get()).take() });
            request.state.store(IDLE, Ordering::Release);
        }
        self.queued.store(0, Ordering::Relaxed);
        drop(list);
        wakers.into_iter().for_each(Waker::wake);
    }
}

/// Unique wave identifiers, so shared state is granted once per wave.
static WAVES: AtomicU64 = AtomicU64::new(1);

/// A sync point: a marker type naming where remote bridges run. Place it
/// with `add_systems(Label, sync_point(Net))`; bridge to it from anywhere
/// with `remote.bridge(Net, ...)`. Bridging to a point the app never places
/// fails to compile.
/// ```
/// use static_ecs::prelude::*;
/// struct Net; // exclusive: orders against every system
/// impl SyncPoint for Net {}
/// struct Scores; // runs beside systems disjoint from its envelope
/// struct Score(u32);
/// impl SyncPoint for Scores {
///     type Env = Query<'static, &'static mut Score>;
/// }
/// ```
pub trait SyncPoint: 'static {
    /// System parameters bounding this point's bridges. The default,
    /// `Everything`, is exclusive: each wave's commands apply at once. With
    /// an envelope, the point runs in parallel with systems that do not
    /// conflict with it, its components join the inferred world, bridges
    /// reaching outside it fail to compile, and commands (always allowed)
    /// apply when the emit ends.
    type Env: Envelope = Everything;
}
/// The default envelope: exclusive world access.
pub struct Everything;

/// The system that drives sync point `P` wherever it runs: `add_systems(Update,
/// sync_point(Net))`, or inside a chain, parallel group, or graph.
pub fn sync_point<P: SyncPoint, S: Storage>(_: P) -> SyncPointSystem<P, S> {
    const {
        assert!(
            <<P::Env as Envelope>::Access as ValidAccess>::VALID,
            "invalid sync point envelope: conflicting access"
        );
    }
    SyncPointSystem {
        drive: Drive::default(),
        queue: CommandQueue::default(),
        marker: PhantomData,
    }
}

/// A world's sync point states, in schema order. Dropping the world closes
/// them: pending and later bridges fail with `BridgeError::Closed`.
pub(crate) struct Points<S: Storage> {
    pub(crate) remote: Remote<S>,
}
impl<S: Storage> Points<S> {
    pub(crate) fn new() -> Self {
        Self {
            remote: Remote {
                states: (0..S::POINTS).map(|_| PointState::new()).collect(),
            },
        }
    }
}
impl<S: Storage> Drop for Points<S> {
    fn drop(&mut self) {
        self.remote.states.iter().for_each(PointState::close);
    }
}

/// System parameters whose combined access bounds a sync point's bridges:
/// one parameter, or a tuple of up to four.
pub trait Envelope {
    type Access: Access + ValidAccess;
}
impl Envelope for Everything {
    type Access = Exclusive;
}
impl<A: ParamShape<Access: ValidAccess>> Envelope for A {
    type Access = A::Access;
}
macro_rules! envelope {
    ($($p:ident),+) => {
        impl<$($p: ParamShape),+> Envelope for ($($p,)+)
        where
            accesses!($($p),+): ValidAccess,
        {
            type Access = accesses!($($p),+);
        }
    };
}
envelope!(A, B);
envelope!(A, B, C);
envelope!(A, B, C, D);

/// Whether access `Self` stays within envelope `Env`.
#[doc(hidden)]
pub trait Within<Env> {
    const VALUE: bool;
}
impl<Env> Within<Env> for () {
    const VALUE: bool = true;
}
// A read is covered by any access to `T`; a write only by a write.
impl<Env: Against<T, true>, T: 'static> Within<Env> for Read<T> {
    const VALUE: bool = <Env as Against<T, true>>::VALUE;
}
impl<Env: Against<T, false>, T: 'static> Within<Env> for Write<T> {
    const VALUE: bool = <Env as Against<T, false>>::VALUE;
}
impl<Env: Access> Within<Env> for Exclusive {
    const VALUE: bool = Env::EXCLUSIVE;
}
impl<Env, A: Within<Env>, B: Within<Env>> Within<Env> for (A, B) {
    const VALUE: bool = A::VALUE && B::VALUE;
}
impl<Env, A: Within<Env>, B: Within<Env>> Within<Env> for QueryAccess<A, B> {
    const VALUE: bool = A::VALUE && B::VALUE;
}
impl<Env, A: Within<Env>, F> Within<Env> for Filtered<A, F> {
    const VALUE: bool = A::VALUE;
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

/// The system driving sync point `P` (see `sync_point`).
pub struct SyncPointSystem<P, S: Storage, K = NoInput> {
    drive: Drive<S>,
    /// Envelope mode: commands granted bridges recorded, until `defer`.
    queue: CommandQueue<S>,
    marker: PhantomData<fn() -> (P, K)>,
}
impl<P: SyncPoint, S: Storage, K> SyncPointSystem<P, S, K> {
    const EXCLUSIVE: bool = <<P::Env as Envelope>::Access as Access>::EXCLUSIVE;
    /// Grant waves until no queued request is admissible. Returns whether
    /// any bridge ran.
    /// # Safety
    /// `world` is live; this call holds `P`'s envelope access to it (all of it
    /// when exclusive) and `clock` is its path's clock.
    unsafe fn run(
        &mut self,
        world: *mut World<S>,
        clock: &mut u64,
        wait: impl Fn(&AtomicUsize, &mut dyn FnMut()),
    ) -> bool {
        const {
            assert!(
                <S as PointLookup<P>>::COUNT == 1,
                "sync point declared more than once"
            );
        }
        // SAFETY: the states live in their own allocation, which the world
        // owns for its whole life; no reference into the world is kept.
        let point: &PointState<S> = unsafe {
            let states: *const [PointState<S>] = &*(*world).points.remote.states;
            &*states
                .cast::<PointState<S>>()
                .add(<S as PointLookup<P>>::INDEX)
        };
        if point.queued.load(Ordering::Acquire) == 0 || point.driving.swap(true, Ordering::Acquire)
        {
            return false;
        }
        struct Driving<'a>(&'a AtomicBool);
        impl Drop for Driving<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _driving = Driving(&point.driving);
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
            // SAFETY: nothing is granted yet. Only an exclusive point may
            // touch the world's own clock.
            unsafe {
                let wave = &mut *point.wave.get();
                wave.read = *clock;
                wave.write = if writes { next(*clock) } else { *clock };
                if Self::EXCLUSIVE {
                    World::set_clock(world, *clock);
                }
            }
            point.pending.store(granted + 1, Ordering::Relaxed);
            point.queued.fetch_sub(granted, Ordering::Relaxed);
            let mut list = Some(list);
            let mut grant = || {
                for (request, slot) in drive.grants.iter().zip(&mut drive.slots) {
                    // SAFETY: unlinked under this lock: live until it completes.
                    let request = unsafe { &**request };
                    request.slot.set(slot);
                    request.state.store(GRANTED, Ordering::Release);
                }
                drive.grants.clear();
                drop(list.take());
                for waker in drive.wakers.drain(..) {
                    waker.wake();
                }
            };
            // The waiter grants once it can run other work meanwhile.
            wait(&point.pending, &mut grant);
            // SAFETY: every granted bridge has finished with the world.
            unsafe {
                let write = (*point.wave.get()).write;
                if Self::EXCLUSIVE {
                    let queue = &mut *World::queue(world);
                    for slot in &mut drive.slots[..granted] {
                        queue.append(slot);
                    }
                    World::set_clock(world, World::clock(world).max(write));
                    (*world).flush();
                    *clock = World::clock(world);
                } else {
                    for slot in &mut drive.slots[..granted] {
                        self.queue.append(slot);
                    }
                    *clock = (*clock).max(write);
                }
            }
        }
        ran
    }
}

impl<P, S: Storage, K> sealed::Sealed for SyncPointSystem<P, S, K> {}
impl<P: SyncPoint, S: Storage, K: Input> Shape for SyncPointSystem<P, S, K> {
    type Access = (Drives<P>, <P::Env as Envelope>::Access);
    type Output = ();
    type In = K;
}
/// Where a wave stands: one left means the last bridge is between its unpark
/// and its release, so its unpark may already be consumed.
fn progress(pending: &AtomicUsize) -> Wait {
    match pending.load(Ordering::Acquire) {
        0 => Wait::Done,
        1 => Wait::Soon,
        _ => Wait::Blocked,
    }
}
/// Grant, then wait for the wave's bridges by parking.
fn park(pending: &AtomicUsize, grant: &mut dyn FnMut()) {
    grant();
    loop {
        match progress(pending) {
            Wait::Done => break,
            Wait::Soon => thread::yield_now(),
            Wait::Blocked => thread::park(),
        }
    }
}
impl<P: SyncPoint, S: Storage, K: Input> System<World<S>> for SyncPointSystem<P, S, K> {
    const VALID: bool = true;
    /// Inside a frame, a waiting envelope point runs work published to its
    /// worker meanwhile; an exclusive one has nothing to share it with.
    #[inline]
    unsafe fn enter<R: Send, Path: Project<R, Target = Self>, C: Continue<World<S>, R>>(
        context: &executor::Context<'_, World<S>, R>,
        flow: Flow,
    ) -> Tail<World<S>, R>
    where
        World<S>: Send + Sync,
    {
        let mut clock = flow.clock();
        // SAFETY: the path owns this node and its envelope's access.
        let ran = unsafe {
            (*Path::get(context.nodes)).run(context.world, &mut clock, |pending, grant| {
                // Lend the worker before granting: anything forked once a
                // bridge runs can then be handed to this thread.
                match (!Self::EXCLUSIVE)
                    .then(|| executor::lend(context))
                    .flatten()
                {
                    Some(lent) => {
                        grant();
                        lent.wait(&|| progress(pending));
                    }
                    None => park(pending, grant),
                }
            })
        };
        unsafe { C::resume(context, Flow::new(clock, flow.ran() || ran)) }
    }
    #[inline]
    unsafe fn run_unchecked(
        &mut self,
        _: *const (),
        world: *mut World<S>,
        clock: &mut u64,
    ) -> Option<()> {
        // SAFETY: the scheduler grants this system its envelope's access.
        unsafe { self.run(world, clock, park) }.then_some(())
    }
    fn defer(&mut self, queue: &mut <World<S> as SystemWorld>::Queue) {
        queue.append(&mut self.queue);
    }
}
#[doc(hidden)]
pub struct SyncMarker;
impl<K: Input, P: SyncPoint, S: Storage> IntoSystems<K, SyncMarker> for SyncPointSystem<P, S> {
    type Systems = SyncPointSystem<P, S, K>;
    fn into_systems(self) -> Self::Systems {
        SyncPointSystem {
            drive: self.drive,
            queue: self.queue,
            marker: PhantomData,
        }
    }
}

/// A handle for bridging into a world from any thread, through any of its
/// sync points: `world.remote()`, or a `&Remote<_>` system parameter. Cheap
/// to clone; await its bridges on threads other than the app's, since the
/// app waits for granted bridges.
pub struct Remote<S: Storage> {
    states: Arc<[PointState<S>]>,
}
impl<S: Storage> Clone for Remote<S> {
    fn clone(&self) -> Self {
        Self {
            states: self.states.clone(),
        }
    }
}
/// Check `P` is placed in world `S` and `A` stays within its envelope;
/// returns `P`'s index.
#[inline(always)]
const fn index<S: PointLookup<P>, P: SyncPoint, A: Within<<P::Env as Envelope>::Access>>() -> usize
{
    const {
        assert!(
            <S as PointLookup<P>>::COUNT == 1,
            "this sync point is not placed in the app: add `sync_point(P)` under a label"
        );
        assert!(
            A::VALUE,
            "this bridge reaches outside its sync point's envelope"
        );
    }
    <S as PointLookup<P>>::INDEX
}
impl<S: Storage> Remote<S> {
    /// Run `function` once at sync point `P`, with fresh parameter state, on
    /// the awaiting thread, in parallel with other bridges of its wave.
    pub fn bridge<P, F, M>(&self, _: P, function: F) -> RemoteBridge<S, F, M, BridgeCell<F::State>>
    where
        P: SyncPoint,
        F: BridgeFn<S, M, Access: Masks<S> + Within<<P::Env as Envelope>::Access>>,
    {
        let index = index::<S, P, F::Access>();
        RemoteBridge::new(self.clone(), index, function, BridgeCell::default(), false)
    }
    /// As `bridge`, but retry at the next drive while a parameter cannot be fetched.
    pub fn until<P, F, M>(&self, _: P, function: F) -> RemoteBridge<S, F, M, BridgeCell<F::State>>
    where
        P: SyncPoint,
        F: BridgeFn<S, M, Access: Masks<S> + Within<<P::Env as Envelope>::Access>>,
    {
        let index = index::<S, P, F::Access>();
        RemoteBridge::new(self.clone(), index, function, BridgeCell::default(), true)
    }
    /// Parameter state kept across bridges, as `AsyncWorld::state`. Bridges
    /// sharing it are never granted in the same wave.
    pub fn state<T: Default>(&self) -> RemoteState<S, T> {
        RemoteState {
            shared: Arc::default(),
            remote: self.clone(),
        }
    }
}

/// Shared parameter state for remote bridges, from `Remote::state`.
pub struct RemoteState<S: Storage, T> {
    shared: Arc<StateBox<T>>,
    remote: Remote<S>,
}
impl<S: Storage, T> Clone for RemoteState<S, T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            remote: self.remote.clone(),
        }
    }
}
impl<S: Storage, T> RemoteState<S, T> {
    /// `Remote::bridge` through this state.
    pub fn bridge<P, F, M>(&self, _: P, function: F) -> RemoteBridge<S, F, M, Arc<StateBox<T>>>
    where
        P: SyncPoint,
        F: BridgeFn<S, M, State = T, Access: Masks<S> + Within<<P::Env as Envelope>::Access>>,
    {
        let index = index::<S, P, F::Access>();
        let remote = self.remote.clone();
        RemoteBridge::new(remote, index, function, self.shared.clone(), false)
    }
    /// `Remote::until` through this state.
    pub fn until<P, F, M>(&self, _: P, function: F) -> RemoteBridge<S, F, M, Arc<StateBox<T>>>
    where
        P: SyncPoint,
        F: BridgeFn<S, M, State = T, Access: Masks<S> + Within<<P::Env as Envelope>::Access>>,
    {
        let index = index::<S, P, F::Access>();
        let remote = self.remote.clone();
        RemoteBridge::new(remote, index, function, self.shared.clone(), true)
    }
}

/// A pending remote bridge. Its request lives inside it, so it is pinned
/// once polled; dropping it withdraws or completes the request.
pub struct RemoteBridge<S: Storage, F, M, St> {
    node: Node<S>,
    remote: Remote<S>,
    index: usize,
    function: Option<F>,
    state: St,
    wait: bool,
    marker: PhantomData<fn() -> M>,
    _pinned: PhantomPinned,
}
// SAFETY: the request's raw links are only followed under the point's lock.
unsafe impl<S: Storage, F: Send, M, St: Send> Send for RemoteBridge<S, F, M, St> {}
impl<S: Storage, F, M, St> RemoteBridge<S, F, M, St> {
    /// The sync point this bridge waits on.
    fn point(&self) -> &PointState<S> {
        &self.remote.states[self.index]
    }
    fn new(remote: Remote<S>, index: usize, function: F, state: St, wait: bool) -> Self
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
            remote,
            index,
            function: Some(function),
            state,
            wait,
            marker: PhantomData,
            _pinned: PhantomPinned,
        }
    }
    /// Queue the pinned request for the next drive, or fail if closed.
    fn enqueue(&self, waker: &Waker) -> Result<(), BridgeError> {
        let mut list = self.point().lock();
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
        self.point().queued.fetch_add(1, Ordering::Release);
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
                    let _list = this.point().lock();
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
        let point = &self.remote.states[self.index];
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
            let mut list = self.point().lock();
            state = self.node.state.load(Ordering::Acquire);
            if state == QUEUED {
                // SAFETY: still linked under the lock.
                unsafe {
                    list.unlink(&self.node);
                    (*self.node.waker.get()).take();
                }
                self.point().queued.fetch_sub(1, Ordering::Relaxed);
                return;
            }
        }
        if state == GRANTED {
            // Granted but never run (or unwinding from the closure).
            self.point().complete();
        }
    }
}
