//! Lifecycle hooks: ordinary systems registered for a component event. A
//! built world owns its hook list and runs it from structural operations,
//! serially and with system parameters only, never `&mut World`: structural
//! changes go through `Commands`, applied before the outermost structural
//! operation returns.
//!
//! The world's type does not mention its hooks (a hook's `Commands<_>` names
//! the world, which would make that type infinite). Instead the world keeps a
//! table with one function per (component, event kind), each monomorphized
//! over the whole hook list: every hook for that event runs as one static,
//! inlined sequence behind a single indirect call.
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;

use crate::access::{Access, Same};
use crate::change::SystemWorld;
use crate::dispatch::{Cons, Lifecycle, Nil, Schedule};
use crate::executor::Fields;
use crate::lookup::ComponentLookup;
use crate::plan::{Input, Shape, System, Takes};
use crate::{Entity, Storage, World, executor};

/// Lifecycle label: after `T` is added to entities that lacked it.
pub struct OnAdd<T>(pub PhantomData<fn(T)>);
/// Lifecycle label: after `T` is written, whether added or replaced.
pub struct OnInsert<T>(pub PhantomData<fn(T)>);
/// Lifecycle label: before an existing `T` is overwritten, removed, or
/// despawned, while the old value is still readable.
pub struct OnReplace<T>(pub PhantomData<fn(T)>);
/// Lifecycle label: before `T` is removed or its entity despawned.
pub struct OnRemove<T>(pub PhantomData<fn(T)>);

/// Lifecycle labels' input family: the affected entities, as a `Trigger`.
pub struct Batch;
impl Input for Batch {
    type Item<'a> = Trigger<'a>;
}
impl Takes for Batch {}

#[doc(hidden)]
pub struct Add;
#[doc(hidden)]
pub struct Insert;
#[doc(hidden)]
pub struct Replace;
#[doc(hidden)]
pub struct Remove;

/// A lifecycle event of one component. Labels of one component and kind
/// (`OnInsert<T>`, `OnEnter<T>`) share one table entry; `admits` narrows when
/// a label's hooks run.
pub trait Event: 'static {
    type Component: 'static;
    #[doc(hidden)]
    type Kind: Kind;
    /// Whether this label's hooks run for the current firing.
    /// # Safety
    /// `world` is live and no structural change runs.
    #[doc(hidden)]
    #[inline(always)]
    unsafe fn admits<S: Storage>(_: *mut World<S>) -> bool {
        true
    }
}
/// The structural event a label listens to.
pub(crate) type Canonical<E> = <<E as Event>::Kind as Kind>::Of<<E as Event>::Component>;
/// Maps a component to this kind's event.
#[doc(hidden)]
pub trait Kind: 'static {
    const INDEX: usize;
    type Of<T: 'static>: Event;
}
macro_rules! events {
    ($($event:ident $kind:ident $index:literal),*) => {$(
        impl<T: 'static> Event for $event<T> {
            type Component = T;
            type Kind = $kind;
        }
        impl<T: 'static> Schedule for $event<T> {
            type In = Batch;
            type Route = Lifecycle;
        }
        impl<T> $event<T> {
            /// The label, for `add_systems`.
            pub const fn new() -> Self {
                Self(PhantomData)
            }
        }
        impl<T> Default for $event<T> {
            fn default() -> Self {
                Self::new()
            }
        }
        impl Kind for $kind {
            const INDEX: usize = $index;
            type Of<T: 'static> = $event<T>;
        }
    )*};
}
events!(OnAdd Add 0, OnInsert Insert 1, OnReplace Replace 2, OnRemove Remove 3);
pub(crate) const KINDS: usize = 4;

/// The entities a hook fired for: one per structural call, all of a batch.
#[derive(Clone, Copy)]
pub struct Trigger<'a>(pub(crate) &'a [Entity]);
impl Deref for Trigger<'_> {
    type Target = [Entity];
    fn deref(&self) -> &[Entity] {
        self.0
    }
}

/// Systems `P` registered for event `E`.
pub struct Hook<E, P> {
    systems: P,
    event: PhantomData<fn(E)>,
    /// Where it was registered.
    #[cfg(feature = "devtools")]
    location: &'static std::panic::Location<'static>,
}
impl<E, P: Shape<Output = ()>> Hook<E, P> {
    #[cfg_attr(feature = "devtools", track_caller)]
    pub(crate) fn new(systems: P) -> Self {
        const {
            assert!(
                !P::Access::EXCLUSIVE,
                "hooks run inside structural operations: use Commands, not exclusive access"
            );
        }
        Self {
            systems,
            event: PhantomData,
            #[cfg(feature = "devtools")]
            location: std::panic::Location::caller(),
        }
    }
}
/// Runs every hook of one event in an erased hook list.
pub(crate) type Entry<S> = unsafe fn(*mut u8, *mut World<S>, &[Entity]);

/// Every registered hook, statically dispatched by event type.
pub trait HookList<S: Storage> {
    const VALID: bool;
    /// No hooks at all: the world skips installing a table.
    const EMPTY: bool = false;
    /// # Safety
    /// `this` is the world's hook list; the world is live and its other parts
    /// are accessed only through `world`; no structural change happens.
    unsafe fn fire<E: Event>(this: *mut Self, world: *mut World<S>, entities: &[Entity]);
    /// Point the table entry of every registered event at `Root`'s runner.
    fn register<Root: HookList<S>>(table: &mut [[Option<Entry<S>>; KINDS]]);
    /// Fragmenting relations as (pair id, linked).
    fn relations(_: &mut Vec<(usize, bool)>) {}
    /// Present every hook, in firing order, to a devtools traversal.
    /// # Safety
    /// `this` is a live hook list, exclusively borrowed.
    #[cfg(feature = "devtools")]
    unsafe fn inspect(_this: *mut Self, _visitor: &mut dyn crate::devtools::Visitor<World<S>>) {}
}
impl<S: Storage> HookList<S> for Nil {
    const VALID: bool = true;
    const EMPTY: bool = true;
    #[inline(always)]
    unsafe fn fire<E: Event>(_: *mut Self, _: *mut World<S>, _: &[Entity]) {}
    fn register<Root: HookList<S>>(_: &mut [[Option<Entry<S>>; KINDS]]) {}
}
impl<S: Storage, A: HookList<S>, B: HookList<S>> HookList<S> for Cons<A, B> {
    const VALID: bool = A::VALID && B::VALID;
    const EMPTY: bool = A::EMPTY && B::EMPTY;
    #[inline(always)]
    unsafe fn fire<E: Event>(this: *mut Self, world: *mut World<S>, entities: &[Entity]) {
        // Registration order: the tail holds earlier hooks.
        unsafe {
            B::fire::<E>(Cons::right(this), world, entities);
            A::fire::<E>(Cons::left(this), world, entities);
        }
    }
    fn register<Root: HookList<S>>(table: &mut [[Option<Entry<S>>; KINDS]]) {
        B::register::<Root>(table);
        A::register::<Root>(table);
    }
    fn relations(relations: &mut Vec<(usize, bool)>) {
        B::relations(relations);
        A::relations(relations);
    }
    #[cfg(feature = "devtools")]
    unsafe fn inspect(this: *mut Self, visitor: &mut dyn crate::devtools::Visitor<World<S>>) {
        unsafe {
            B::inspect(Cons::right(this), visitor);
            A::inspect(Cons::left(this), visitor);
        }
    }
}
impl<S, E, P> HookList<S> for Hook<E, P>
where
    S: Storage + ComponentLookup<E::Component>,
    E: Event + Schedule,
    P: System<World<S>, Output = (), In = E::In>,
{
    const VALID: bool = P::VALID && <S as ComponentLookup<E::Component>>::COUNT == 1;
    #[inline(always)]
    unsafe fn fire<F: Event>(this: *mut Self, world: *mut World<S>, entities: &[Entity]) {
        if const { <F as Same<Canonical<E>>>::VALUE } && unsafe { E::admits(world) } {
            unsafe { run(this, world, entities) };
        }
    }
    fn register<Root: HookList<S>>(table: &mut [[Option<Entry<S>>; KINDS]]) {
        unsafe fn every<S: Storage, Root: HookList<S>, E: Event>(
            hooks: *mut u8,
            world: *mut World<S>,
            entities: &[Entity],
        ) {
            unsafe { Root::fire::<E>(hooks.cast(), world, entities) };
        }
        table[<S as ComponentLookup<E::Component>>::INDEX][E::Kind::INDEX] =
            Some(every::<S, Root, Canonical<E>>);
    }
    #[cfg(feature = "devtools")]
    unsafe fn inspect(this: *mut Self, visitor: &mut dyn crate::devtools::Visitor<World<S>>) {
        let this = unsafe { &mut *this };
        let registration = crate::devtools::Registration {
            label: std::any::TypeId::of::<E>(),
            name: std::any::type_name::<E>(),
            location: this.location,
            hook: true,
        };
        if visitor.registration(&registration) {
            this.systems.inspect(visitor);
        }
        visitor.end_registration();
    }
}
/// Run a hook serially on the world's clock, then move its commands to the
/// world's queue. Systems without input ignore the trigger.
#[inline(never)]
unsafe fn run<S: Storage, E, P: System<World<S>, Output = ()>>(
    this: *mut Hook<E, P>,
    world: *mut World<S>,
    entities: &[Entity],
) {
    unsafe {
        let systems = &mut (*this).systems;
        let trigger = Trigger(entities);
        let input = (&raw const trigger).cast();
        let mut clock = World::clock(world);
        executor::serially(|| systems.run_unchecked(input, world, &mut clock));
        World::set_clock(world, clock);
        systems.defer(&mut *World::queue(world));
    }
}

/// A world's erased hook list and its per-(component, kind) runners.
pub(crate) struct HookTable<S: Storage> {
    hooks: NonNull<u8>,
    drop: unsafe fn(NonNull<u8>),
    entries: Box<[[Option<Entry<S>>; KINDS]]>,
    /// Components with any hook, for despawns.
    hooked: Box<[usize]>,
    /// Fragmenting relations as (pair id, linked), for despawn cascades.
    relations: Box<[(usize, bool)]>,
    /// The hook list's devtools traversal.
    #[cfg(feature = "devtools")]
    inspect: unsafe fn(NonNull<u8>, &mut dyn crate::devtools::Visitor<World<S>>),
}
// The hook list is Send + Sync (checked where it is installed).
unsafe impl<S: Storage> Send for HookTable<S> {}
unsafe impl<S: Storage> Sync for HookTable<S> {}
impl<S: Storage> HookTable<S> {
    pub(crate) fn new<H: HookList<S> + Send + Sync + 'static>(hooks: H) -> Self {
        unsafe fn drop_hooks<H>(hooks: NonNull<u8>) {
            drop(unsafe { Box::from_raw(hooks.cast::<H>().as_ptr()) });
        }
        #[cfg(feature = "devtools")]
        unsafe fn inspect_hooks<S: Storage, H: HookList<S>>(
            hooks: NonNull<u8>,
            visitor: &mut dyn crate::devtools::Visitor<World<S>>,
        ) {
            unsafe { H::inspect(hooks.cast::<H>().as_ptr(), visitor) };
        }
        let mut entries = vec![[None; KINDS]; S::LEN].into_boxed_slice();
        H::register::<H>(&mut entries);
        let hooked = (0..S::LEN)
            .filter(|&i| entries[i].iter().any(Option::is_some))
            .collect();
        let mut relations = Vec::new();
        H::relations(&mut relations);
        Self {
            hooks: Box::into_non_null(Box::new(hooks)).cast(),
            drop: drop_hooks::<H>,
            entries,
            hooked,
            relations: relations.into(),
            #[cfg(feature = "devtools")]
            inspect: inspect_hooks::<S, H>,
        }
    }
    /// The runner for kind `K` of component `index`, if any hook listens.
    #[inline]
    pub(crate) fn entry(&self, index: usize, kind: usize) -> Option<(Entry<S>, *mut u8)> {
        self.entries[index][kind].map(|entry| (entry, self.hooks.as_ptr()))
    }
    pub(crate) fn hooked(&self) -> &[usize] {
        &self.hooked
    }
    pub(crate) fn relations(&self) -> &[(usize, bool)] {
        &self.relations
    }
    /// Present every hook to a devtools traversal.
    /// # Safety
    /// No hook is running.
    #[cfg(feature = "devtools")]
    pub(crate) unsafe fn inspect(&mut self, visitor: &mut dyn crate::devtools::Visitor<World<S>>) {
        unsafe { (self.inspect)(self.hooks, visitor) };
    }
}
impl<S: Storage> Drop for HookTable<S> {
    fn drop(&mut self) {
        unsafe { (self.drop)(self.hooks) };
    }
}
