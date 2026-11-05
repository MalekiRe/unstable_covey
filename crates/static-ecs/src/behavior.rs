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

/// Entities with a running behavior instance: a resource the poll steps
/// keep. Events for such an entity wait in its mailboxes even for types its
/// behaviors have not awaited yet, so one that awaits `DragStart` and then
/// `Drag` sees a `Drag` that arrived with the `DragStart`.
#[derive(Default)]
pub struct Behaving {
    /// (entity, instances) by entity index.
    counts: Vec<(Option<Entity>, u32)>,
}
impl Behaving {
    /// Whether `entity` runs a behavior.
    pub fn has(&self, entity: Entity) -> bool {
        self.counts.get(entity.index() as usize).is_some_and(|&(e, n)| e == Some(entity) && n > 0)
    }
    fn add(&mut self, entity: Entity) {
        let index = entity.index() as usize;
        if self.counts.len() <= index {
            self.counts.resize(index + 1, (None, 0));
        }
        let slot = &mut self.counts[index];
        if slot.0 != Some(entity) {
            *slot = (Some(entity), 0);
        }
        slot.1 += 1;
    }
    fn remove(&mut self, entity: Entity) {
        if let Some(slot) = self.counts.get_mut(entity.index() as usize)
            && slot.0 == Some(entity)
        {
            slot.1 = slot.1.saturating_sub(1);
        }
    }
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
    type Components = (B, Wakeup, Behaving);
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
    /// The stamp when it last yielded: events stamped since are new to it.
    suspended_at: u64,
    /// Mailboxes it subscribed to, released with it.
    subscriptions: Vec<Subscription>,
}
/// A mailbox an instance listens on: `(Inbox<E>, entity)`, by its remover.
#[derive(Clone, Copy, PartialEq)]
struct Subscription {
    unsubscribe: unsafe fn(*mut (), Entity),
    entity: Entity,
}
/// Orders deliveries against instances' suspensions, process-wide.
static STAMP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
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
                    suspended_at: 0,
                    subscriptions: Vec::new(),
                })
                .collect();
            self.chunks.push(chunk);
            self.free = base;
        }
        let slot = self.free;
        let entry = self.entry(slot);
        let free = entry.next_free;
        entry.entity = entity;
        entry.suspended_at = STAMP.load(Ordering::Relaxed);
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
    /// Drop the slot's future in place, end its subscriptions, and free
    /// the slot.
    fn release(&mut self, slot: u32, world: *mut World<S>) {
        let free = self.free;
        let entry = self.entry(slot);
        entry.future = None;
        // SAFETY: the poll step holds the world exclusively.
        if let Some(behaving) = unsafe { (*world).resource_mut::<Behaving>() } {
            behaving.into_unmarked().remove(entry.entity);
        }
        for subscription in entry.subscriptions.drain(..) {
            // SAFETY: the poll step holds the world exclusively.
            unsafe { (subscription.unsubscribe)(world.cast(), subscription.entity) };
        }
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
            since: Cell::new(0),
            subscriptions: Cell::new(ptr::null_mut()),
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
                    scope.since.set(entry.suspended_at);
                    scope.subscriptions.set(&raw mut entry.subscriptions);
                    // SAFETY: chunk entries never move, and futures are
                    // dropped in place.
                    let future = unsafe { Pin::new_unchecked(future) };
                    let ready = future.poll(&mut Context::from_waker(waker)).is_ready();
                    scope.subscriptions.set(ptr::null_mut());
                    let entry = self.entry(slot);
                    entry.suspended_at = STAMP.fetch_add(1, Ordering::Relaxed) + 1;
                    if ready {
                        entry.future = None;
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
                // SAFETY: as above.
                unsafe {
                    if (*world).resource::<Behaving>().is_none() {
                        (*world).init_resource::<Behaving>();
                    }
                    (*world).resource_mut::<Behaving>().unwrap().into_unmarked().add(entity);
                }
                let future = (self.start)(entity, Cx { id, schema: PhantomData });
                self.entry(slot).future = Some(future);
                self.runnable.push(slot);
            } else if let Some(slot) = self.slot_of(entity) {
                self.frozen.retain(|&s| s != slot);
                self.runnable.retain(|&s| s != slot);
                self.release(slot, world);
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
    /// The polled instance's last suspension stamp.
    since: Cell<u64>,
    /// The polled instance's subscriptions; null between polls.
    subscriptions: Cell<*mut Vec<Subscription>>,
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
    /// Whether this thread is in its app's poll step (so `with` works):
    /// false while the app itself is being dropped.
    pub fn is_polling(&self) -> bool {
        PollScope::current(self.id).is_some()
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
    /// Resolves with the next `E` delivered to `entity` (see
    /// [`World::deliver`]) since this behavior last yielded: events that
    /// arrive while it is suspended wait in `entity`'s mailbox, in order,
    /// so a burst (say `DragStart` then `Drag` in one input batch) is not
    /// lost while it handles the first. Awaiting subscribes this behavior
    /// to the mailbox until it ends. Waiting on an entity that is despawned
    /// never resolves. The world must store `Inbox<E>` (checked at compile
    /// time).
    pub fn on<E: Clone + Send + Sync + 'static>(&self, entity: Entity) -> Delivery<S, E> {
        Delivery { id: self.id, entity, ticket: None, schema: PhantomData }
    }
}

/// Mailboxes for event `E`, by entity: a resource. Store it in worlds
/// whose behaviors await `E`.
pub struct Inbox<E> {
    mailboxes: Vec<Mailbox<E>>,
    tickets: u64,
}
impl<E> Default for Inbox<E> {
    fn default() -> Self {
        Self { mailboxes: Vec::new(), tickets: 0 }
    }
}
/// One entity's `E`s: queued while someone subscribes, and who waits.
struct Mailbox<E> {
    entity: Option<Entity>,
    /// Live behavior instances listening here.
    subscribers: u32,
    /// Delivered events with their stamps, oldest first (at most `QUEUE`).
    queue: std::collections::VecDeque<(u64, E)>,
    waiters: Vec<(u64, Waker)>,
}
/// Events kept per mailbox; older ones are dropped first.
const QUEUE: usize = 32;
impl<E> Default for Mailbox<E> {
    fn default() -> Self {
        Self { entity: None, subscribers: 0, queue: std::collections::VecDeque::new(), waiters: Vec::new() }
    }
}
impl<E> Mailbox<E> {
    /// Accept `entity` here, forgetting a previous entity's state.
    fn claim(&mut self, entity: Entity) {
        if self.entity != Some(entity) {
            self.entity = Some(entity);
            self.subscribers = 0;
            self.queue.clear();
            self.waiters.clear();
        }
    }
}
impl<E: Clone> Inbox<E> {
    fn mailbox(&mut self, entity: Entity) -> &mut Mailbox<E> {
        let index = entity.index() as usize;
        if self.mailboxes.len() <= index {
            self.mailboxes.resize_with(index + 1, Mailbox::default);
        }
        let mailbox = &mut self.mailboxes[index];
        mailbox.claim(entity);
        mailbox
    }
    fn get(&self, entity: Entity) -> Option<&Mailbox<E>> {
        self.mailboxes.get(entity.index() as usize).filter(|m| m.entity == Some(entity))
    }
    /// Queue `event` for `entity` if a behavior listens there, waking its
    /// waiters; returns whether one did (an event nobody listens for is
    /// dropped, and bubbling moves on).
    pub fn deliver(&mut self, entity: Entity, event: E) -> bool {
        self.deliver_or_keep(entity, event, false)
    }
    /// As `deliver`; with `keep`, an entity running behaviors that do not
    /// listen for `E` yet still gets it queued (but it is not "taken").
    fn deliver_or_keep(&mut self, entity: Entity, event: E, keep: bool) -> bool {
        let subscribed = self.get(entity).is_some_and(|m| m.subscribers > 0);
        if !subscribed && !keep {
            return false;
        }
        let mailbox = self.mailbox(entity);
        if mailbox.queue.len() == QUEUE {
            mailbox.queue.pop_front();
        }
        mailbox.queue.push_back((STAMP.load(Ordering::Relaxed), event));
        for (_, waker) in &mailbox.waiters {
            waker.wake_by_ref();
        }
        subscribed
    }
    /// Deliver `event` to every listening entity; returns how many.
    pub fn broadcast(&mut self, event: E) -> usize {
        let stamp = STAMP.load(Ordering::Relaxed);
        let mut reached = 0;
        for mailbox in self.mailboxes.iter_mut().filter(|m| m.subscribers > 0) {
            if mailbox.queue.len() == QUEUE {
                mailbox.queue.pop_front();
            }
            mailbox.queue.push_back((stamp, event.clone()));
            for (_, waker) in &mailbox.waiters {
                waker.wake_by_ref();
            }
            reached += 1;
        }
        reached
    }
    /// Deliver `event` only to entities with a future waiting right now: for
    /// "it is now" events (frame ticks) that must not queue up.
    pub fn deliver_to_waiting(&mut self, event: E) -> usize {
        let stamp = STAMP.load(Ordering::Relaxed);
        let mut reached = 0;
        for mailbox in self.mailboxes.iter_mut().filter(|m| m.subscribers > 0 && !m.waiters.is_empty()) {
            if mailbox.queue.len() == QUEUE {
                mailbox.queue.pop_front();
            }
            mailbox.queue.push_back((stamp, event.clone()));
            for (_, waker) in &mailbox.waiters {
                waker.wake_by_ref();
            }
            reached += 1;
        }
        reached
    }
    /// Whether any future waits for `E` anywhere.
    pub fn any_waiting(&self) -> bool {
        self.mailboxes.iter().any(|m| m.subscribers > 0 && !m.waiters.is_empty())
    }
    /// How many futures wait on `entity` right now.
    pub fn waiters(&self, entity: Entity) -> usize {
        self.get(entity).map_or(0, |m| m.waiters.len())
    }
    /// Whether a behavior listens on `entity`.
    pub fn is_listening(&self, entity: Entity) -> bool {
        self.get(entity).is_some_and(|m| m.subscribers > 0)
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
    fn inbox<'a>(world: *mut ()) -> &'a mut Inbox<E> {
        const {
            assert!(
                <S as ComponentLookup<Inbox<E>>>::COUNT == 1,
                "awaiting an event the world does not store an `Inbox` for: register `Inbox<E>`"
            )
        };
        // SAFETY: the poll step holds the world exclusively, and `with` is
        // not running (its closure cannot await).
        let world = unsafe { &mut *world.cast::<World<S>>() };
        if world.resource::<Inbox<E>>().is_none() {
            world.init_resource::<Inbox<E>>();
        }
        world.resource_mut::<Inbox<E>>().unwrap().into_unmarked()
    }
    /// Drop one subscriber of `entity`'s mailbox: the instance ended.
    unsafe fn unsubscribe(world: *mut (), entity: Entity) {
        let inbox = Self::inbox(world);
        if let Some(mailbox) = inbox.mailboxes.get_mut(entity.index() as usize).filter(|m| m.entity == Some(entity)) {
            mailbox.subscribers = mailbox.subscribers.saturating_sub(1);
            if mailbox.subscribers == 0 {
                mailbox.queue.clear();
            }
        }
    }
}
impl<S: Storage, E: Clone + Send + Sync + 'static> Future for Delivery<S, E> {
    type Output = E;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<E> {
        let scope = PollScope::current(self.id).expect("`cx.on` was polled outside its poll step");
        let entity = self.entity;
        let inbox = Self::inbox(scope.world);
        let mailbox = inbox.mailbox(entity);
        // Subscribe this instance, once.
        let subscriptions = scope.subscriptions.get();
        let subscription = Subscription { unsubscribe: Self::unsubscribe, entity };
        // SAFETY: set by the poll step for the instance being polled.
        if let Some(subscriptions) = unsafe { subscriptions.as_mut() }
            && !subscriptions.contains(&subscription)
        {
            subscriptions.push(subscription);
            mailbox.subscribers += 1;
        }
        // The oldest event new to this instance.
        let since = scope.since.get();
        if let Some(index) = mailbox.queue.iter().position(|&(stamp, _)| stamp >= since) {
            let (_, event) = mailbox.queue.remove(index).unwrap();
            if let Some(ticket) = self.ticket.take() {
                mailbox.waiters.retain(|&(t, _)| t != ticket);
            }
            return Poll::Ready(event);
        }
        match self.ticket {
            None => {
                inbox.tickets += 1;
                let ticket = inbox.tickets;
                inbox.mailbox(entity).waiters.push((ticket, cx.waker().clone()));
                self.ticket = Some(ticket);
            }
            Some(ticket) => {
                if let Some((_, waker)) = mailbox.waiters.iter_mut().find(|(t, _)| *t == ticket)
                    && !waker.will_wake(cx.waker())
                {
                    *waker = cx.waker().clone();
                }
            }
        }
        Poll::Pending
    }
}
impl<S: Storage, E: Clone + Send + Sync + 'static> Drop for Delivery<S, E> {
    fn drop(&mut self) {
        let Some(ticket) = self.ticket else { return };
        // Outside a poll step (the app is being dropped) nothing delivers.
        let Some(scope) = PollScope::current(self.id) else { return };
        let inbox = Self::inbox(scope.world);
        if let Some(mailbox) = inbox.mailboxes.get_mut(self.entity.index() as usize).filter(|m| m.entity == Some(self.entity)) {
            mailbox.waiters.retain(|&(t, _)| t != ticket);
        }
    }
}

impl<S: Storage> World<S> {
    /// Deliver `event` to `entity`'s behaviors listening for `E` (see
    /// [`Cx::on`]); returns whether any listens. If `entity` runs behaviors
    /// that have not awaited `E` yet, it waits in their mailbox (but counts
    /// as not taken); otherwise it is dropped.
    pub fn deliver<E: Clone + Send + Sync + 'static>(&mut self, entity: Entity, event: E) -> bool
    where
        S: ComponentLookup<Inbox<E>>,
    {
        let keep = self.optional_resource::<Behaving>().is_some_and(|b| b.has(entity));
        if keep && self.resource::<Inbox<E>>().is_none() {
            self.init_resource::<Inbox<E>>();
        }
        self.resource_mut::<Inbox<E>>()
            .is_some_and(|inbox| inbox.into_unmarked().deliver_or_keep(entity, event, keep))
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
