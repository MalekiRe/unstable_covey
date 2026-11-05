//! Async access to the world. A bridge runs a closure with typed parameters,
//! like a one-shot system, when its task reaches the bridge's label during
//! `emit`. Closures run on the polling thread and may borrow the task's
//! locals, including `&mut`, so nothing here requires `Send`.
use crate::init::Init;
use std::any::TypeId;
use std::cell::{Cell, UnsafeCell};
use std::marker::PhantomData;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use crate::access::{Access, Exclusive, ValidAccess};
use crate::change::{ChangeWindow, SystemWorld, next};
use crate::command::CommandQueue;
use crate::dispatch::{Emitted, Label, Schedule};
use crate::param::Param;
use crate::plan::accesses;
use crate::{Storage, World};

/// Where a task's pending bridges wait: the label whose dispatch polls it.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Parked {
    No,
    At(TypeId),
    /// Bridges on several labels: poll at every label.
    Anywhere,
}

/// Bridge sites one visit remembers; later sites wait for the next emit.
const SITES: usize = 16;

/// The task being polled on this thread: its world, label, and clock.
pub(crate) struct Scope {
    pub(crate) label: TypeId,
    pub(crate) world: *mut (),
    pub(crate) id: u64,
    pub(crate) clock: Cell<u64>,
    running: Cell<bool>,
    pub(crate) parked: Cell<Parked>,
    /// Bridge sites (closure types) that already ran during this visit.
    ran: Cell<[Option<TypeId>; SITES]>,
}
thread_local! {
    static SCOPE: Cell<*const Scope> = const { Cell::new(std::ptr::null()) };
}
/// Whether this thread is polling one of an app's tasks.
pub(crate) fn in_task() -> bool {
    !SCOPE.get().is_null()
}
impl Scope {
    pub(crate) fn new<S: Storage>(label: TypeId, world: *mut World<S>, clock: u64) -> Self {
        Self {
            label,
            world: world.cast(),
            // SAFETY: the caller's world is live.
            id: unsafe { World::world_id(world) },
            clock: Cell::new(clock),
            running: Cell::new(false),
            parked: Cell::new(Parked::No),
            ran: Cell::new([None; SITES]),
        }
    }
    /// Make this the current scope while `poll` runs.
    pub(crate) fn enter<R>(&self, poll: impl FnOnce() -> R) -> R {
        struct Restore(*const Scope);
        impl Drop for Restore {
            fn drop(&mut self) {
                SCOPE.set(self.0);
            }
        }
        let _restore = Restore(SCOPE.replace(self));
        poll()
    }
    fn park(&self, label: TypeId) {
        self.parked.set(match self.parked.get() {
            Parked::No => Parked::At(label),
            Parked::At(parked) if parked == label => Parked::At(label),
            _ => Parked::Anywhere,
        });
    }
    /// Record that bridge `site` runs now; false if it already ran during
    /// this visit (or the visit ran too many sites), so it waits.
    fn first_run(&self, site: TypeId) -> bool {
        let mut ran = self.ran.get();
        for slot in &mut ran {
            match slot {
                Some(seen) if *seen == site => return false,
                Some(_) => {}
                None => {
                    *slot = Some(site);
                    self.ran.set(ran);
                    return true;
                }
            }
        }
        false
    }
}

/// `TypeId` of `T` with lifetimes erased. Every closure expression has its
/// own type, so this identifies a bridge's call site.
pub(crate) fn site<T: ?Sized>() -> TypeId {
    trait Site {
        fn id(&self) -> TypeId
        where
            Self: 'static;
    }
    impl<T: ?Sized> Site for PhantomData<T> {
        fn id(&self) -> TypeId
        where
            Self: 'static,
        {
            TypeId::of::<T>()
        }
    }
    let marker = PhantomData::<T>;
    let erased: &dyn Site = &marker;
    // SAFETY: only the trait object's lifetime bound changes; `id` reads no
    // data and type identity ignores lifetimes.
    let erased: &(dyn Site + 'static) = unsafe { std::mem::transmute(erased) };
    erased.id()
}

/// A task's handle to its app's world, typed by the world's component set.
/// Name it `AsyncWorld<_>` in the task closure; `build` fixes the type.
pub struct AsyncWorld<S: Storage> {
    id: u64,
    schema: PhantomData<fn() -> S>,
}
impl<S: Storage> Clone for AsyncWorld<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S: Storage> Copy for AsyncWorld<S> {}
impl<S: Storage> AsyncWorld<S> {
    pub(crate) fn new(id: u64) -> Self {
        Self {
            id,
            schema: PhantomData,
        }
    }
    /// Run `function` once when this task reaches label `E`, with fresh
    /// parameter state. Its commands apply before it returns. Fails if a
    /// parameter cannot be fetched, e.g. a `Single` that does not match.
    pub fn bridge<E, X, LM, F, M>(
        &self,
        _: X,
        function: F,
    ) -> Bridge<S, E, F, M, BridgeCell<F::State>>
    where
        E: Schedule<Route = Emitted>,
        X: Label<E, LM>,
        F: BridgeFn<S, M>,
    {
        Bridge::new(*self, function, BridgeCell::default(), false)
    }
    /// As `bridge`, but wait for the next time `E` runs while a parameter
    /// cannot be fetched.
    pub fn until<E, X, LM, F, M>(
        &self,
        _: X,
        function: F,
    ) -> Bridge<S, E, F, M, BridgeCell<F::State>>
    where
        E: Schedule<Route = Emitted>,
        X: Label<E, LM>,
        F: BridgeFn<S, M>,
    {
        Bridge::new(*self, function, BridgeCell::default(), true)
    }
    /// Parameter state kept across bridges, inferred from its first use:
    /// `Local`s persist, `Changed` sees changes since the previous bridge, and
    /// query caches stay warm, so bridging through it allocates nothing.
    pub fn state<T: Init>(&self) -> BridgeState<S, T> {
        BridgeState {
            shared: Arc::default(),
            world: *self,
        }
    }
}

/// One bridge's parameter states and change cursor.
#[doc(hidden)]
pub struct BridgeCell<T> {
    pub(crate) params: T,
    pub(crate) last_run: u64,
    pub(crate) last_world: u64,
}
impl<T: Init> Default for BridgeCell<T> {
    fn default() -> Self {
        Self {
            params: T::init(),
            last_run: 0,
            last_world: 0,
        }
    }
}
impl<T> BridgeCell<T> {
    /// The change window of a run at `now` in world `id`.
    pub(crate) fn window(&self, id: u64, now: u64) -> ChangeWindow {
        let last = if self.last_world == id {
            self.last_run
        } else {
            0
        };
        ChangeWindow { last, now }
    }
    pub(crate) fn ran(&mut self, id: u64, now: u64) {
        self.last_run = now;
        self.last_world = id;
    }
}
/// Where a bridge keeps its parameter state: inline, or shared.
#[doc(hidden)]
pub trait StateRef<T> {
    fn cell(&mut self) -> *mut BridgeCell<T>;
    /// The wave stamp of shared state; null for state no other bridge sees.
    fn stamp(&self) -> *const Cell<u64> {
        std::ptr::null()
    }
}
impl<T> StateRef<T> for BridgeCell<T> {
    fn cell(&mut self) -> *mut BridgeCell<T> {
        self
    }
}

/// State shared by bridges, plus the stamp of the remote wave that last
/// granted one of them, written only by sync points.
#[doc(hidden)]
pub struct StateBox<T> {
    stamp: Cell<u64>,
    cell: UnsafeCell<BridgeCell<T>>,
}
impl<T: Init> Default for StateBox<T> {
    fn default() -> Self {
        Self {
            stamp: Cell::new(0),
            cell: UnsafeCell::new(BridgeCell::default()),
        }
    }
}
// SAFETY: a running bridge alone accesses the cell: local bridges hold the
// world exclusively and never nest, and a remote wave grants each stamp once.
// Only the sync point driving a wave touches the stamp.
unsafe impl<T: Send> Send for StateBox<T> {}
unsafe impl<T: Send> Sync for StateBox<T> {}
impl<T> StateRef<T> for Arc<StateBox<T>> {
    fn cell(&mut self) -> *mut BridgeCell<T> {
        self.cell.get()
    }
    fn stamp(&self) -> *const Cell<u64> {
        &self.stamp
    }
}

/// Shared parameter state for bridges, from `AsyncWorld::state`. Cloning
/// shares it; bridges through it never overlap.
pub struct BridgeState<S: Storage, T> {
    shared: Arc<StateBox<T>>,
    world: AsyncWorld<S>,
}
impl<S: Storage, T> Clone for BridgeState<S, T> {
    fn clone(&self) -> Self {
        Self {
            shared: self.shared.clone(),
            world: self.world,
        }
    }
}
impl<S: Storage, T> BridgeState<S, T> {
    /// `AsyncWorld::bridge` through this state.
    pub fn bridge<E, X, LM, F, M>(&self, _: X, function: F) -> Bridge<S, E, F, M, Arc<StateBox<T>>>
    where
        E: Schedule<Route = Emitted>,
        X: Label<E, LM>,
        F: BridgeFn<S, M, State = T>,
    {
        Bridge::new(self.world, function, self.shared.clone(), false)
    }
    /// `AsyncWorld::until` through this state.
    pub fn until<E, X, LM, F, M>(&self, _: X, function: F) -> Bridge<S, E, F, M, Arc<StateBox<T>>>
    where
        E: Schedule<Route = Emitted>,
        X: Label<E, LM>,
        F: BridgeFn<S, M, State = T>,
    {
        Bridge::new(self.world, function, self.shared.clone(), true)
    }
}

/// Why a bridge did not run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeError {
    /// A parameter could not be fetched, e.g. a `Single` without exactly one match.
    Invalid,
    /// The sync point is gone: its app was dropped, or it was never added.
    Closed,
}
impl std::fmt::Display for BridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Invalid => "a bridge parameter could not be fetched",
            Self::Closed => "the bridge's sync point was dropped",
        })
    }
}
impl std::error::Error for BridgeError {}

/// A closure a bridge can run: typed parameters (`Query`, `Single`,
/// `Commands`, `Local`) or `&mut World<_>` alone.
#[diagnostic::on_unimplemented(
    message = "this closure cannot run as a bridge",
    note = "bridges take up to 16 system parameters (`Query`, `Single`, `Commands`, `Local`), or `&mut World<_>` alone"
)]
pub trait BridgeFn<S: Storage, M>: Sized {
    type Out;
    #[doc(hidden)]
    type State: Init;
    #[doc(hidden)]
    type Access: Access;
    #[doc(hidden)]
    const VALID: bool;
    #[doc(hidden)]
    const EXCLUSIVE: bool;
    /// Fetch every parameter, then call; hand the closure back if one fails.
    /// # Safety
    /// Exclusive world access for the call; `window` is this run's window.
    #[doc(hidden)]
    unsafe fn call(
        self,
        world: *mut World<S>,
        state: &mut Self::State,
        window: ChangeWindow,
    ) -> Result<Self::Out, Self>;
    #[doc(hidden)]
    fn defer(_state: &mut Self::State, _queue: &mut CommandQueue<S>) {}
}

macro_rules! bridge_fn {
    ($($p:ident $i:tt),*) => {
        impl<S: Storage, Func, R, $($p: Param<World<S>>),*> BridgeFn<S, fn($($p),*) -> R> for Func
        where
            Func: FnOnce($($p),*) -> R + for<'a> FnOnce($($p::Item<'a>),*) -> R,
            accesses!($($p),*): ValidAccess,
        {
            type Out = R;
            type State = ($($p::State,)*);
            type Access = accesses!($($p),*);
            const VALID: bool = <Self::Access as ValidAccess>::VALID $(&& $p::VALID)*;
            const EXCLUSIVE: bool = false;
            #[inline]
            unsafe fn call(
                self,
                _world: *mut World<S>,
                _state: &mut Self::State,
                _window: ChangeWindow,
            ) -> Result<R, Self> {
                $(
                    #[allow(non_snake_case)]
                    let Some($p) = (unsafe { $p::fetch(_world, &mut _state.$i, _window) }) else {
                        return Err(self);
                    };
                )*
                Ok(self($($p),*))
            }
            fn defer(_state: &mut Self::State, _queue: &mut CommandQueue<S>) {
                $($p::defer(&mut _state.$i, _queue);)*
            }
        }
    };
}
bridge_fn!();
bridge_fn!(P0 0);
bridge_fn!(P0 0, P1 1);
bridge_fn!(P0 0, P1 1, P2 2);
bridge_fn!(P0 0, P1 1, P2 2, P3 3);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11, P12 12);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11, P12 12, P13 13);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11, P12 12, P13 13, P14 14);
bridge_fn!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11, P12 12, P13 13, P14 14, P15 15);

#[doc(hidden)]
pub struct Exclusively<R>(PhantomData<fn() -> R>);
impl<S: Storage, Func: FnOnce(&mut World<S>) -> R, R> BridgeFn<S, Exclusively<R>> for Func {
    type Out = R;
    type State = ();
    type Access = Exclusive;
    const VALID: bool = true;
    const EXCLUSIVE: bool = true;
    #[inline]
    unsafe fn call(self, world: *mut World<S>, _: &mut (), _: ChangeWindow) -> Result<R, Self> {
        Ok(self(unsafe { &mut *world }))
    }
}

/// A pending bridge: resolves when its task reaches label `E`.
pub struct Bridge<S: Storage, E, F, M, St> {
    function: Option<F>,
    state: St,
    world: AsyncWorld<S>,
    wait: bool,
    marker: PhantomData<fn() -> (E, M)>,
}
// Nothing is structurally pinned: the closure moves out when it runs.
impl<S: Storage, E, F, M, St> Unpin for Bridge<S, E, F, M, St> {}
impl<S: Storage, E, F, M, St> Bridge<S, E, F, M, St> {
    fn new(world: AsyncWorld<S>, function: F, state: St, wait: bool) -> Self {
        Self {
            function: Some(function),
            state,
            world,
            wait,
            marker: PhantomData,
        }
    }
}
impl<S, E, F, M, St> Future for Bridge<S, E, F, M, St>
where
    S: Storage,
    E: Schedule<Route = Emitted>,
    F: BridgeFn<S, M>,
    St: StateRef<F::State>,
{
    type Output = Result<F::Out, BridgeError>;
    fn poll(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Self::Output> {
        const {
            assert!(
                F::VALID,
                "invalid bridge: missing/ambiguous storage or conflicting access"
            );
        }
        let this = self.get_mut();
        let scope = SCOPE.get();
        assert!(
            !scope.is_null(),
            "a bridge was polled outside its app's tasks"
        );
        // SAFETY: the scope outlives the poll that installed it.
        let scope = unsafe { &*scope };
        assert_eq!(
            scope.id, this.world.id,
            "a task awaited another world's bridge"
        );
        #[cfg(not(feature = "devtools"))]
        let label = TypeId::of::<E>();
        #[cfg(feature = "devtools")]
        let label = crate::devtools::name_label::<E>();
        // A site runs once per visit: a loop around a bridge runs once per
        // emit, while a chain of different bridges completes in one.
        if scope.label != label || !scope.first_run(site::<F>()) {
            scope.park(label);
            return Poll::Pending;
        }
        assert!(!scope.running.get(), "bridges cannot nest");
        let function = this
            .function
            .take()
            .expect("bridge polled after completion");
        struct Running<'a>(&'a Cell<bool>);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        scope.running.set(true);
        let _running = Running(&scope.running);
        let world = scope.world.cast::<World<S>>();
        // SAFETY: the scope's world is `World<S>` (same id) and this task holds
        // it exclusively; bridges never nest, so the state cell is unaliased.
        unsafe {
            let cell = &mut *this.state.cell();
            let latest = scope.clock.get();
            let now = if !F::EXCLUSIVE && F::Access::WRITES {
                next(latest)
            } else {
                latest
            };
            World::set_clock(world, now);
            let window = cell.window(scope.id, now);
            match function.call(world, &mut cell.params, window) {
                Ok(output) => {
                    cell.ran(scope.id, now);
                    F::defer(&mut cell.params, &mut *World::queue(world));
                    (*world).flush();
                    scope.clock.set(World::clock(world));
                    Poll::Ready(Ok(output))
                }
                Err(function) if this.wait => {
                    this.function = Some(function);
                    scope.park(label);
                    Poll::Pending
                }
                Err(_) => Poll::Ready(Err(BridgeError::Invalid)),
            }
        }
    }
}
