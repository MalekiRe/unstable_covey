//! Behaviors: one future per entity holding a marker component, owned by the
//! app (not the world) and polled at a fixed place in a schedule.
//!
//! - Inserting the marker starts the entity's future; removing it or
//!   despawning the entity drops it (cancellation is drop).
//! - Futures sit in chunks with stable addresses and are polled in place.
//! - A future runs only when woken: by an event delivered to an entity it
//!   awaits (`cx.on::<E>(entity)`), or by any `Waker` it handed out (timers,
//!   other runtimes). Idle behaviors cost nothing per frame.
//! - Each instance has one waker, reused across awaits: waking pushes the
//!   instance onto a lock-free list and calls the app's [`Wakeup`], so an
//!   event loop can sleep until something is due.
//! - A woken entity hidden by a `Disabling` component stays queued until it
//!   is shown again: disabling a subtree freezes its behaviors in place.
//! - `cx.with(|params...| ...)` runs a closure with system parameters right
//!   away, checked at compile time like systems; the poll step holds the
//!   world exclusively, so it needs no conflict analysis.
use std::any::{Any, TypeId};
use std::cell::Cell;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::pin::Pin;
use std::ptr;
use std::sync::atomic::{AtomicBool, AtomicPtr, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll, Wake, Waker};

use crate::access::Access;
use crate::bridge::{BridgeCell, BridgeError, BridgeFn, site};
use crate::change::{SystemWorld, next};
use crate::dispatch::{Emitted, Schedule, schedule};
use crate::hook::{OnAdd, OnRemove};
use crate::lookup::ComponentLookup;
use crate::plan::{NoInput, exclusive};
use crate::plugin::{Plugin, Systems};
use crate::{Commands, Entity, Storage, Trigger, World};

/// Async logic attached to every entity holding `Self`, a marker component.
/// The future is concrete (static dispatch, polled in place, never boxed)
/// and must be `Send`, like every system. Register it with
/// [`behavior::<Self>(label)`](behavior).
/// ```
/// use static_ecs::prelude::*;
/// use static_ecs::{Behavior, Cx, Inbox, behavior};
/// struct Frame;
/// impl Schedule for Frame {}
/// #[derive(Clone)]
/// struct Click;
/// struct Clicks(u32);
///
/// struct Counter;
/// impl Behavior for Counter {
///     async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
///         loop {
///             cx.on::<Click>(me).await;
///             cx.with(|mut q: Query<&mut Clicks>| q.get_mut(me).unwrap().0 += 1).unwrap();
///         }
///     }
/// }
///
/// let mut app = App::new()
///     .add_plugins(behavior::<Counter, _>(Frame))
///     .register::<Clicks>()
///     .register::<Inbox<Click>>()
///     .build();
/// let button = app.world.spawn_bundle((Counter, Clicks(0)));
/// app.emit(&Frame); // starts the future: it waits for a click
/// app.world.deliver(button, Click);
/// app.emit(&Frame); // polls just that one future
/// assert_eq!(app.world.get::<Clicks>(button).unwrap().0, 1);
/// ```
pub trait Behavior: Send + Sync + 'static {
    /// The future for entity `me`, started when `me` gains `Self`.
    fn start<S: Storage>(me: Entity, cx: Cx<S>) -> impl Future<Output = ()> + Send;
}

/// Called whenever a behavior is woken, from any thread: the app's event
/// loop wakes up (e.g. writes an eventfd) and emits the polling label.
/// Insert it as a resource; behaviors pick it up when they next run.
pub struct Wakeup(pub Arc<dyn Fn() + Send + Sync>);

/// Register behavior `B`, polled under plain label `label` at this place in
/// the app's registration order.
pub fn behavior<B: Behavior, E: Schedule<Route = Emitted, In = NoInput>>(label: E) -> BehaviorPlugin<B, E> {
    BehaviorPlugin(label, PhantomData)
}
/// The plugin [`behavior`] returns.
pub struct BehaviorPlugin<B, E>(E, PhantomData<fn() -> B>);

impl<B: Behavior, E: Schedule<Route = Emitted, In = NoInput>> Plugin for BehaviorPlugin<B, E> {
    type Components = (B, Wakeup);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        let shared = Arc::new(Shared::default());
        let (added, removed) = (shared.clone(), shared.clone());
        let mut slab = Slab::new(B::start::<S>);
        (
            schedule(OnAdd::<B>::new(), move |trigger: Trigger| {
                added.events.lock().unwrap().extend(trigger.iter().map(|&e| (e, true)));
            }),
            schedule(OnRemove::<B>::new(), move |trigger: Trigger| {
                removed.events.lock().unwrap().extend(trigger.iter().map(|&e| (e, false)));
            }),
            schedule(self.0, exclusive(move |world: &mut World<S>| slab.step(&shared, world))),
        )
    }
}

/// What the hooks and wakers hand the poll step.
#[derive(Default)]
struct Shared {
    /// Marker added (true) or removed (false), in order.
    events: Mutex<Vec<(Entity, bool)>>,
    /// Woken instances: a lock-free stack of `Arc<Control>` pointers.
    woken: AtomicPtr<Control>,
    wakeup: OnceLock<Arc<dyn Fn() + Send + Sync>>,
}
impl Drop for Shared {
    fn drop(&mut self) {
        drain(&self.woken, drop);
    }
}
/// Take every woken instance off `stack`, newest first.
fn drain(stack: &AtomicPtr<Control>, mut each: impl FnMut(Arc<Control>)) {
    let mut head = stack.swap(ptr::null_mut(), Ordering::Acquire);
    while !head.is_null() {
        // SAFETY: pushed by `wake` through `Arc::into_raw`; owned again here.
        let control = unsafe { Arc::from_raw(head) };
        head = control.next.load(Ordering::Relaxed);
        control.queued.store(false, Ordering::Release);
        each(control);
    }
}

/// One instance's waker: where to queue it, and its slab slot.
struct Control {
    shared: Weak<Shared>,
    slot: u32,
    queued: AtomicBool,
    next: AtomicPtr<Control>,
}
impl Wake for Control {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        let Some(shared) = self.shared.upgrade() else { return };
        if self.queued.swap(true, Ordering::AcqRel) {
            return;
        }
        let node = Arc::into_raw(self.clone()).cast_mut();
        let mut head = shared.woken.load(Ordering::Relaxed);
        loop {
            self.next.store(head, Ordering::Relaxed);
            match shared.woken.compare_exchange_weak(head, node, Ordering::Release, Ordering::Relaxed) {
                Ok(_) => break,
                Err(current) => head = current,
            }
        }
        if let Some(wakeup) = shared.wakeup.get() {
            wakeup();
        }
    }
}

const CHUNK: usize = 64;
const NONE: u32 = u32::MAX;

struct Entry<F> {
    entity: Entity,
    future: Option<F>,
    /// Wraps this slot's `Arc<Control>`. Kept when the slot frees, and
    /// reused if no clone of it survives.
    waker: Option<Waker>,
    next_free: u32,
}
impl<F> Entry<F> {
    fn reusable_waker(&self) -> bool {
        self.waker.as_ref().is_some_and(|waker| {
            // SAFETY: every slab waker wraps an `Arc<Control>`; the borrowed
            // count is not released.
            let control = std::mem::ManuallyDrop::new(unsafe { Arc::from_raw(waker.data().cast::<Control>()) });
            !control.queued.load(Ordering::Acquire) && Arc::strong_count(&control) == 1
        })
    }
}

/// Futures in fixed chunks, so each stays where it was pinned.
struct Slab<S, F> {
    chunks: Vec<Box<[Entry<F>]>>,
    free: u32,
    /// Slab slot by entity index.
    by_entity: Vec<u32>,
    /// Due this round.
    runnable: Vec<u32>,
    /// Woken or started while hidden: polled once shown again.
    frozen: Vec<u32>,
    events: Vec<(Entity, bool)>,
    /// `cx.with` parameter states, by call site.
    states: HashMap<TypeId, Box<dyn Any + Send>>,
    start: fn(Entity, Cx<S>) -> F,
}

impl<S: Storage, F: Future<Output = ()>> Slab<S, F> {
    fn new(start: fn(Entity, Cx<S>) -> F) -> Self {
        Self {
            chunks: Vec::new(),
            free: NONE,
            by_entity: Vec::new(),
            runnable: Vec::new(),
            frozen: Vec::new(),
            events: Vec::new(),
            states: HashMap::new(),
            start,
        }
    }
    fn entry(&mut self, slot: u32) -> &mut Entry<F> {
        &mut self.chunks[slot as usize / CHUNK][slot as usize % CHUNK]
    }
    fn slot_of(&self, entity: Entity) -> Option<u32> {
        let slot = *self.by_entity.get(entity.index() as usize)?;
        (slot != NONE && self.chunks[slot as usize / CHUNK][slot as usize % CHUNK].entity == entity)
            .then_some(slot)
    }
    fn allocate(&mut self, entity: Entity, shared: &Arc<Shared>) -> u32 {
        if self.free == NONE {
            let base = (self.chunks.len() * CHUNK) as u32;
            let chunk = (0..CHUNK as u32)
                .map(|i| Entry {
                    entity,
                    future: None,
                    waker: None,
                    next_free: if i + 1 < CHUNK as u32 { base + i + 1 } else { NONE },
                })
                .collect();
            self.chunks.push(chunk);
            self.free = base;
        }
        let slot = self.free;
        let entry = self.entry(slot);
        let free = entry.next_free;
        entry.entity = entity;
        if !entry.reusable_waker() {
            entry.waker = Some(Waker::from(Arc::new(Control {
                shared: Arc::downgrade(shared),
                slot,
                queued: AtomicBool::new(false),
                next: AtomicPtr::new(ptr::null_mut()),
            })));
        }
        self.free = free;
        let index = entity.index() as usize;
        if self.by_entity.len() <= index {
            self.by_entity.resize(index + 1, NONE);
        }
        self.by_entity[index] = slot;
        slot
    }
    /// Drop the slot's future in place and free the slot.
    fn release(&mut self, slot: u32) {
        let free = self.free;
        let entry = self.entry(slot);
        entry.future = None;
        entry.next_free = free;
        let index = entry.entity.index() as usize;
        self.by_entity[index] = NONE;
        self.free = slot;
    }

    /// The poll step: apply starts and stops, then poll what is due, in
    /// rounds, so behaviors woken by others during the step run in it too.
    fn step(&mut self, shared: &Arc<Shared>, world: &mut World<S>) {
        if shared.wakeup.get().is_none()
            && let Some(wakeup) = world.resource::<Wakeup>()
        {
            let _ = shared.wakeup.set(wakeup.0.clone());
        }
        let world: *mut World<S> = world;
        let scope = PollScope {
            world: world.cast(),
            // SAFETY: the world is live.
            id: unsafe { World::world_id(world) },
            states: &raw mut self.states,
            running: Cell::new(false),
        };
        scope.enter(|| {
            for _ in 0..8 {
                self.apply_events(shared, world, scope.id);
                self.collect_woken(shared, world);
                if self.runnable.is_empty() {
                    break;
                }
                let mut runnable = std::mem::take(&mut self.runnable);
                runnable.sort_unstable();
                runnable.dedup();
                for slot in runnable.drain(..) {
                    let entity = self.entry(slot).entity;
                    // SAFETY: the step holds the world exclusively, and no
                    // reference into it is live between polls.
                    let (alive, hidden) = unsafe { ((*world).contains(entity), (*world).hidden(entity)) };
                    if !alive {
                        continue; // its stop event is queued
                    }
                    if hidden {
                        if !self.frozen.contains(&slot) {
                            self.frozen.push(slot);
                        }
                        continue;
                    }
                    let entry = self.entry(slot);
                    let Some(future) = entry.future.as_mut() else { continue };
                    let waker = entry.waker.as_ref().expect("a started slot has a waker");
                    // SAFETY: chunk entries never move, and futures are
                    // dropped in place.
                    let future = unsafe { Pin::new_unchecked(future) };
                    if future.poll(&mut Context::from_waker(waker)).is_ready() {
                        self.entry(slot).future = None;
                    }
                }
                self.runnable = runnable;
            }
        });
    }
    fn apply_events(&mut self, shared: &Arc<Shared>, world: *mut World<S>, id: u64) {
        std::mem::swap(&mut self.events, &mut *shared.events.lock().unwrap());
        let mut events = std::mem::take(&mut self.events);
        for (entity, added) in events.drain(..) {
            if added {
                // SAFETY: the step holds the world exclusively.
                if self.slot_of(entity).is_some() || unsafe { !(*world).contains(entity) } {
                    continue;
                }
                let slot = self.allocate(entity, shared);
                let future = (self.start)(entity, Cx { id, schema: PhantomData });
                self.entry(slot).future = Some(future);
                self.runnable.push(slot);
            } else if let Some(slot) = self.slot_of(entity) {
                self.frozen.retain(|&s| s != slot);
                self.runnable.retain(|&s| s != slot);
                self.release(slot);
            }
        }
        self.events = events;
    }
    fn collect_woken(&mut self, shared: &Arc<Shared>, world: *mut World<S>) {
        drain(&shared.woken, |control| {
            let slot = control.slot;
            let Some(entry) = self.chunks.get(slot as usize / CHUNK).map(|chunk| &chunk[slot as usize % CHUNK]) else {
                return;
            };
            // A stale waker (its instance ended) wakes nothing.
            let current = entry.waker.as_ref().is_some_and(|w| ptr::eq(w.data(), Arc::as_ptr(&control).cast()));
            if current && entry.future.is_some() {
                self.runnable.push(slot);
            }
        });
        let mut i = 0;
        while i < self.frozen.len() {
            let slot = self.frozen[i];
            let entity = self.entry(slot).entity;
            // SAFETY: as in `step`.
            if unsafe { !(*world).hidden(entity) } {
                self.frozen.swap_remove(i);
                self.runnable.push(slot);
            } else {
                i += 1;
            }
        }
    }
}

/// The poll step running on this thread.
struct PollScope {
    world: *mut (),
    id: u64,
    states: *mut HashMap<TypeId, Box<dyn Any + Send>>,
    running: Cell<bool>,
}
thread_local! {
    static SCOPE: Cell<*const PollScope> = const { Cell::new(ptr::null()) };
}
impl PollScope {
    fn enter<R>(&self, body: impl FnOnce() -> R) -> R {
        struct Restore(*const PollScope);
        impl Drop for Restore {
            fn drop(&mut self) {
                SCOPE.set(self.0);
            }
        }
        let _restore = Restore(SCOPE.replace(self));
        body()
    }
    /// The current scope, if it polls world `id`.
    fn current<'a>(id: u64) -> Option<&'a PollScope> {
        let scope = SCOPE.get();
        // SAFETY: a scope outlives the polls it encloses.
        (!scope.is_null()).then(|| unsafe { &*scope }).filter(|scope| scope.id == id)
    }
}

/// A behavior's handle to its app's world, typed by the world's component
/// set. `Copy`: capture it freely.
pub struct Cx<S> {
    id: u64,
    schema: PhantomData<fn() -> S>,
}
impl<S> Clone for Cx<S> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<S> Copy for Cx<S> {}
impl<S: Storage> Cx<S> {
    fn scope(&self) -> &PollScope {
        PollScope::current(self.id).expect("a behavior's context was used outside its poll step")
    }
    /// Run `function` now with system parameters (`Query`, `Single`, `Res`,
    /// `Commands`, `Local`, ...) or `&mut World<S>` alone. Parameter state
    /// persists per call site: query caches stay warm, `Changed` sees changes
    /// since this site last ran, `Local`s keep their values. Commands apply
    /// before it returns. Fails if a parameter cannot be fetched.
    pub fn with<Func, M>(&self, function: Func) -> Result<Func::Out, BridgeError>
    where
        Func: BridgeFn<S, M>,
        Func::State: Send + 'static,
    {
        const {
            assert!(Func::VALID, "invalid behavior access: missing/ambiguous storage or conflicting access");
        }
        let scope = self.scope();
        assert!(!scope.running.replace(true), "`cx.with` cannot nest");
        struct Running<'a>(&'a Cell<bool>);
        impl Drop for Running<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _running = Running(&scope.running);
        let world = scope.world.cast::<World<S>>();
        // SAFETY: the poll step holds the world exclusively and no other
        // `with` runs; the states map is the step's own.
        unsafe {
            let cell = (*scope.states)
                .entry(site::<Func>())
                .or_insert_with(|| Box::new(BridgeCell::<Func::State>::default()))
                .downcast_mut::<BridgeCell<Func::State>>()
                .expect("one state type per call site");
            let latest = World::clock(world);
            let now = if !Func::EXCLUSIVE && Func::Access::WRITES { next(latest) } else { latest };
            World::set_clock(world, now);
            let window = cell.window(scope.id, now);
            match function.call(world, &mut cell.params, window) {
                Ok(output) => {
                    cell.ran(scope.id, now);
                    Func::defer(&mut cell.params, &mut *World::queue(world));
                    (*world).flush();
                    Ok(output)
                }
                Err(_) => Err(BridgeError::Invalid),
            }
        }
    }
    /// Resolves with the next `E` delivered to `entity` after this first
    /// polls (see [`World::deliver`]). Dropping it stops waiting. Waiting on
    /// an entity that is despawned never resolves. The world must store
    /// `Inbox<E>` (checked at compile time).
    pub fn on<E: Clone + Send + Sync + 'static>(&self, entity: Entity) -> Delivery<S, E> {
        Delivery { id: self.id, entity, ticket: None, schema: PhantomData }
    }
}

/// Waiters for event `E`, by entity: a resource. Store it in worlds whose
/// behaviors await `E`.
pub struct Inbox<E> {
    /// Waiters by entity index.
    waiting: Vec<Vec<Waiting<E>>>,
    tickets: u64,
}
impl<E> Default for Inbox<E> {
    fn default() -> Self {
        Self { waiting: Vec::new(), tickets: 0 }
    }
}
struct Waiting<E> {
    entity: Entity,
    ticket: u64,
    waker: Waker,
    value: Option<E>,
}
impl<E: Clone> Inbox<E> {
    fn list(&mut self, entity: Entity) -> &mut Vec<Waiting<E>> {
        let index = entity.index() as usize;
        if self.waiting.len() <= index {
            self.waiting.resize_with(index + 1, Vec::new);
        }
        &mut self.waiting[index]
    }
    /// Hand `event` to everyone waiting on `entity`; returns how many.
    pub fn deliver(&mut self, entity: Entity, event: E) -> usize {
        let Some(list) = self.waiting.get_mut(entity.index() as usize) else { return 0 };
        let mut woken = 0;
        for waiting in list.iter_mut().filter(|w| w.entity == entity && w.value.is_none()) {
            waiting.value = Some(event.clone());
            waiting.waker.wake_by_ref();
            woken += 1;
        }
        woken
    }
    /// Hand `event` to everyone waiting on any entity; returns how many.
    pub fn broadcast(&mut self, event: E) -> usize {
        let mut woken = 0;
        for waiting in self.waiting.iter_mut().flatten().filter(|w| w.value.is_none()) {
            waiting.value = Some(event.clone());
            waiting.waker.wake_by_ref();
            woken += 1;
        }
        woken
    }
    /// How many futures wait on `entity`.
    pub fn waiters(&self, entity: Entity) -> usize {
        self.waiting.get(entity.index() as usize).map_or(0, |list| list.iter().filter(|w| w.entity == entity).count())
    }
}

/// The future [`Cx::on`] returns.
pub struct Delivery<S: Storage, E: Clone + Send + Sync + 'static> {
    id: u64,
    entity: Entity,
    ticket: Option<u64>,
    schema: PhantomData<fn() -> (S, E)>,
}
impl<S: Storage, E: Clone + Send + Sync + 'static> Unpin for Delivery<S, E> {}
impl<S: Storage, E: Clone + Send + Sync + 'static> Delivery<S, E> {
    fn inbox<'a>(scope: &PollScope) -> &'a mut Inbox<E> {
        const {
            assert!(
                <S as ComponentLookup<Inbox<E>>>::COUNT == 1,
                "awaiting an event the world does not store an `Inbox` for: register `Inbox<E>`"
            )
        };
        // SAFETY: the poll step holds the world exclusively, and `with` is
        // not running (its closure cannot await).
        let world = unsafe { &mut *scope.world.cast::<World<S>>() };
        if world.resource::<Inbox<E>>().is_none() {
            world.init_resource::<Inbox<E>>();
        }
        world.resource_mut::<Inbox<E>>().unwrap().into_unmarked()
    }
}
impl<S: Storage, E: Clone + Send + Sync + 'static> Future for Delivery<S, E> {
    type Output = E;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<E> {
        let scope = PollScope::current(self.id).expect("`cx.on` was polled outside its poll step");
        let inbox = Self::inbox(scope);
        let entity = self.entity;
        match self.ticket {
            None => {
                inbox.tickets += 1;
                let ticket = inbox.tickets;
                let waker = cx.waker().clone();
                inbox.list(entity).push(Waiting { entity, ticket, waker, value: None });
                self.ticket = Some(ticket);
                Poll::Pending
            }
            Some(ticket) => {
                let list = inbox.list(entity);
                let index = list.iter().position(|w| w.ticket == ticket).expect("a waiter stays registered");
                if list[index].value.is_some() {
                    let waiting = list.swap_remove(index);
                    self.ticket = None;
                    return Poll::Ready(waiting.value.unwrap());
                }
                if !list[index].waker.will_wake(cx.waker()) {
                    list[index].waker = cx.waker().clone();
                }
                Poll::Pending
            }
        }
    }
}
impl<S: Storage, E: Clone + Send + Sync + 'static> Drop for Delivery<S, E> {
    fn drop(&mut self) {
        let Some(ticket) = self.ticket else { return };
        // Outside a poll step (the app is being dropped) nothing delivers.
        let Some(scope) = PollScope::current(self.id) else { return };
        let list = Self::inbox(scope).list(self.entity);
        if let Some(index) = list.iter().position(|w| w.ticket == ticket) {
            list.swap_remove(index);
        }
    }
}

impl<S: Storage> World<S> {
    /// Deliver `event` to everything awaiting `E` on `entity` (see
    /// [`Cx::on`]); returns how many waiters it woke. Nothing waiting, no
    /// effect: events are not queued.
    pub fn deliver<E: Clone + Send + Sync + 'static>(&mut self, entity: Entity, event: E) -> usize
    where
        S: ComponentLookup<Inbox<E>>,
    {
        self.resource_mut::<Inbox<E>>()
            .map_or(0, |inbox| inbox.into_unmarked().deliver(entity, event))
    }
}
impl<S: Storage> Commands<'_, S> {
    /// Deferred [`World::deliver`].
    pub fn deliver<E: Clone + Send + Sync + 'static>(&mut self, entity: Entity, event: E)
    where
        S: ComponentLookup<Inbox<E>>,
    {
        self.queue(move |world| {
            world.deliver(entity, event);
        });
    }
}
