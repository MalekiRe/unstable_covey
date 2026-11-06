//! Reusable plugins. A plugin names the components and relations it uses, so
//! the app's world is known before any of its systems exist; `build` then asks
//! it for its systems, typed against that world.
use std::marker::PhantomData;

use crate::dispatch::{Cons, Fits, Handle, Nil, On};
use crate::hook::{Hook, HookList};
use crate::relation::{self, Fragment, PairOf, Relates, Relation, RelationHooks, Sources};
use crate::task::{Task, TaskFn};
use crate::vocab::{Choose, If, Insert, Present};
use crate::{Components, Storage, World};

/// A reusable bundle of systems. It declares what the world must store, then
/// builds its systems for whatever world the app ends up with:
/// ```
/// use static_ecs::prelude::*;
/// struct Update;
/// impl Schedule for Update {}
/// struct Pos(i32);
/// struct Vel(i32);
///
/// struct Physics;
/// impl Plugin for Physics {
///     type Components = (Pos, Vel);
///     fn systems<S: Storage>(self) -> impl Systems<S> {
///         (
///             schedule(Update, |mut q: Query<(&mut Pos, &Vel)>| {
///                 for (mut pos, vel) in &mut q {
///                     pos.0 += vel.0;
///                 }
///             }),
///             schedule(Update, |mut commands: Commands<S>, mut q: Query<&Pos>| {
///                 if q.iter_mut().next().is_none() {
///                     commands.spawn((Pos(0), Vel(1)));
///                 }
///             }),
///         )
///     }
/// }
/// let mut app = App::new().add_plugins(Physics).build();
/// app.emit(&Update);
/// app.emit(&Update);
/// let mut q = app.world.query::<&Pos>();
/// assert_eq!(q.iter_mut().map(|p| p.0).collect::<Vec<_>>(), [1]);
/// ```
/// `S` is the app's world: write it where a system names the world
/// (`Commands<S>`, `&mut World<S>`, `AsyncWorld<S>`), or `_`.
pub trait Plugin: Sized {
    /// Every component the plugin's systems use, as a tuple (up to 32):
    /// `(Pos, Vel)`, or `()`. Components the app already stores, under any
    /// storage, are kept as they are.
    type Components;
    /// Relations the plugin uses (a tuple of up to 16), installed once
    /// however many plugins and apps name them: `(ChildOf,)`.
    type Relations = ();
    /// Storage choices for components the plugin names, applied before
    /// `Components`: a tuple (up to 16) of declarations such as
    /// `Components<Node, true>` (change summaries, so an unchanged column
    /// is skipped with one load) or `Sparse<Hovered>`. A component the app
    /// already stores keeps its storage.
    type Storage = ();
    /// The plugin's systems, for the app's final world `S`: anything
    /// `schedule(...)` or `task(...)` returns, or a tuple of those (up to 16,
    /// nested freely), registered in order where the plugin was added. They
    /// must be `Send` (see `Systems`).
    fn systems<S: Storage>(self) -> impl Systems<S>;
}

/// Systems a plugin returns for world `S`: `schedule(label, systems)`,
/// `task(body)`, another plugin's `systems::<S>()`, or a tuple of them. They
/// are `Send`, so an app built from plugins can move to another thread:
/// capture `Mutex`, atomics, or owned data rather than `RefCell` or `Rc`.
pub trait Systems<S: Storage> {
    #[doc(hidden)]
    type Listeners: Fits<World<S>> + Send;
    #[doc(hidden)]
    type Hooks: HookList<S> + Send + Sync + 'static;
    #[doc(hidden)]
    fn split(self) -> (Self::Listeners, Self::Hooks);
}
impl<S: Storage, E: 'static, H: Handle<E, World<S>> + Send> Systems<S> for On<E, H> {
    type Listeners = Self;
    type Hooks = Nil;
    fn split(self) -> (Self, Nil) {
        (self, Nil)
    }
}
impl<S: Storage, E, P> Systems<S> for Hook<E, P>
where
    Self: HookList<S> + Send + Sync + 'static,
{
    type Listeners = Nil;
    type Hooks = Self;
    fn split(self) -> (Nil, Self) {
        (Nil, self)
    }
}
impl<S: Storage, R: Relation> Systems<S> for Fragment<R>
where
    Self: HookList<S>,
{
    type Listeners = Nil;
    type Hooks = Self;
    fn split(self) -> (Nil, Self) {
        (Nil, self)
    }
}
impl<S: Storage, F: TaskFn<S>> Systems<S> for Task<S, F>
where
    Self: Send,
{
    type Listeners = Self;
    type Hooks = Nil;
    fn split(self) -> (Self, Nil) {
        (self, Nil)
    }
}
/// No systems: a plugin that only declares components and storage.
impl<S: Storage> Systems<S> for () {
    type Listeners = Nil;
    type Hooks = Nil;
    fn split(self) -> (Nil, Nil) {
        (Nil, Nil)
    }
}
impl<S: Storage, A: Systems<S>> Systems<S> for (A,) {
    type Listeners = A::Listeners;
    type Hooks = A::Hooks;
    fn split(self) -> (A::Listeners, A::Hooks) {
        self.0.split()
    }
}
// Lists keep registration order with the newest at the head.
impl<S: Storage, A: Systems<S>, B: Systems<S>> Systems<S> for (A, B) {
    type Listeners = Cons<B::Listeners, Cons<A::Listeners, Nil>>;
    type Hooks = Cons<B::Hooks, A::Hooks>;
    fn split(self) -> (Self::Listeners, Self::Hooks) {
        let ((a, ah), (b, bh)) = (self.0.split(), self.1.split());
        (Cons(b, Cons(a, Nil)), Cons(bh, ah))
    }
}
/// Longer tuples: their head, then the rest as one tuple.
macro_rules! systems_tuples {
    ($a:ident $($i:tt $t:ident)+) => {
        impl<S: Storage, $a: Systems<S>, $($t: Systems<S>),+> Systems<S> for ($a, $($t),+) {
            type Listeners = <($a, ($($t,)+)) as Systems<S>>::Listeners;
            type Hooks = <($a, ($($t,)+)) as Systems<S>>::Hooks;
            fn split(self) -> (Self::Listeners, Self::Hooks) {
                (self.0, ($(self.$i,)+)).split()
            }
        }
    };
}
systems_tuples!(P0 1 P1 2 P2);
systems_tuples!(P0 1 P1 2 P2 3 P3);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11 12 P12);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11 12 P12 13 P13);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11 12 P12 13 P13 14 P14);
systems_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11 12 P12 13 P13 14 P14 15 P15);

/// `C` extended with a tuple of component types.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a tuple of component types",
    note = "a plugin's `Components` lists its component types: `type Components = (Pos, Vel);`"
)]
pub trait ComponentList<C> {
    type Out: Storage;
}
impl<C: Storage> ComponentList<C> for () {
    type Out = C;
}
macro_rules! component_list {
    ($a:ident $(, $t:ident)*) => {
        impl<C: Insert<Components<$a>>, $a: 'static, $($t: 'static),*> ComponentList<C> for ($a, $($t,)*)
        where
            ($($t,)*): ComponentList<<C as Insert<Components<$a>>>::Out>,
        {
            type Out = <($($t,)*) as ComponentList<<C as Insert<Components<$a>>>::Out>>::Out;
        }
        component_list!($($t),*);
    };
    () => {};
}
component_list!(
    A, B, D, E, F, G, H, I, J, K, L, M, N, O, P, Q, T0, T1, T2, T3, T4, T5, T6, T7, T8, T9, T10,
    T11, T12, T13, T14, T15
);

/// `C` extended with a tuple of storage declarations.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a tuple of storage declarations",
    note = "a plugin's `Storage` lists declarations: `type Storage = (Components<Pos, true>, Sparse<Hovered>);`"
)]
pub trait DeclarationList<C> {
    type Out: Storage;
}
impl<C: Storage> DeclarationList<C> for () {
    type Out = C;
}
macro_rules! declaration_list {
    ($a:ident $(, $t:ident)*) => {
        impl<C: Insert<$a>, $a: crate::vocab::Declaration, $($t: crate::vocab::Declaration),*> DeclarationList<C> for ($a, $($t,)*)
        where
            ($($t,)*): DeclarationList<<C as Insert<$a>>::Out>,
        {
            type Out = <($($t,)*) as DeclarationList<<C as Insert<$a>>::Out>>::Out;
        }
        declaration_list!($($t),*);
    };
    () => {};
}
declaration_list!(A, B, D, E, F, G, H, I, J, K, L, M, N, O, P, Q);

/// Relation `R` installed in a builder: its components now, its hooks at
/// `build`, for the final world.
#[doc(hidden)]
pub struct RelationSlot<R>(PhantomData<fn() -> R>);

/// `C` and hook list `K` with a tuple of relations installed, each once: a
/// relation whose pair marker `C` already stores is left as it is.
#[doc(hidden)]
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a tuple of relations",
    note = "a plugin's `Relations` lists relation types: `type Relations = (ChildOf,);`"
)]
pub trait RelationList<C, K> {
    type Components: Storage;
    type Hooks;
    fn install(hooks: K) -> Self::Hooks;
}
impl<C: Storage, K> RelationList<C, K> for () {
    type Components = C;
    type Hooks = K;
    fn install(hooks: K) -> K {
        hooks
    }
}
/// The relation's components, added to `C`.
#[doc(hidden)]
pub type WithRelation<R, C> = <<<C as Insert<Components<Relates<R>>>>::Out as Insert<
    Components<Sources<R>>,
>>::Out as Insert<Components<PairOf<R>>>>::Out;
/// Installs one relation unless already present.
#[doc(hidden)]
pub trait Once<R, K> {
    type Hooks;
    fn install(hooks: K) -> Self::Hooks;
}
impl<R, K> Once<R, K> for If<true> {
    type Hooks = K;
    fn install(hooks: K) -> K {
        hooks
    }
}
impl<R, K> Once<R, K> for If<false> {
    type Hooks = Cons<RelationSlot<R>, K>;
    fn install(hooks: K) -> Self::Hooks {
        Cons(RelationSlot(PhantomData), hooks)
    }
}
/// Relation `R` installed into components `C` and hooks `K`, unless `C`
/// already stores its pair marker.
#[doc(hidden)]
pub trait InstallRelation<C, K> {
    type Components: Storage;
    type Hooks;
    fn install(hooks: K) -> Self::Hooks;
}
impl<R: Relation, C, K> InstallRelation<C, K> for R
where
    C: Storage + Present<PairOf<R>> + Insert<Components<Relates<R>>>,
    <C as Insert<Components<Relates<R>>>>::Out: Insert<Components<Sources<R>>>,
    <<C as Insert<Components<Relates<R>>>>::Out as Insert<Components<Sources<R>>>>::Out:
        Insert<Components<PairOf<R>>>,
    If<{ core::gca!(<C as Present<PairOf<R>>>::VALUE) }>:
        Choose<C, WithRelation<R, C>, Out: Storage> + Once<R, K>,
{
    type Components = <If<{ core::gca!(<C as Present<PairOf<R>>>::VALUE) }> as Choose<
        C,
        WithRelation<R, C>,
    >>::Out;
    type Hooks = <If<{ core::gca!(<C as Present<PairOf<R>>>::VALUE) }> as Once<R, K>>::Hooks;
    fn install(hooks: K) -> Self::Hooks {
        <If<{ core::gca!(<C as Present<PairOf<R>>>::VALUE) }> as Once<R, K>>::install(hooks)
    }
}
macro_rules! relation_list {
    ($a:ident $(, $t:ident)*) => {
        impl<C, K, $a: InstallRelation<C, K>, $($t),*> RelationList<C, K> for ($a, $($t,)*)
        where
            ($($t,)*): RelationList<$a::Components, $a::Hooks>,
        {
            type Components = <($($t,)*) as RelationList<$a::Components, $a::Hooks>>::Components;
            type Hooks = <($($t,)*) as RelationList<$a::Components, $a::Hooks>>::Hooks;
            fn install(hooks: K) -> Self::Hooks {
                <($($t,)*) as RelationList<$a::Components, $a::Hooks>>::install($a::install(hooks))
            }
        }
        relation_list!($($t),*);
    };
    () => {};
}
relation_list!(
    R0, R1, R2, R3, R4, R5, R6, R7, R8, R9, R10, R11, R12, R13, R14, R15
);

/// A plugin added to a builder, waiting at its registration position for the
/// final world.
#[doc(hidden)]
pub struct Pending<P>(pub(crate) P);

/// A builder's listener list for world `S`: plugins become their systems,
/// whose hooks are returned beside it. Only a plugin's systems are opaque, and
/// they promise `Send`, so the app is `Send` exactly when its systems are.
#[doc(hidden)]
pub trait Instantiate<S: Storage> {
    type Listeners: Fits<World<S>>;
    type Hooks: HookList<S> + Send + Sync + 'static;
    fn instantiate(self) -> (Self::Listeners, Self::Hooks);
}
impl<S: Storage> Instantiate<S> for Nil {
    type Listeners = Nil;
    type Hooks = Nil;
    fn instantiate(self) -> (Nil, Nil) {
        (Nil, Nil)
    }
}
impl<S: Storage, H: Instantiate<S>, T: Instantiate<S>> Instantiate<S> for Cons<H, T> {
    type Listeners = Cons<H::Listeners, T::Listeners>;
    type Hooks = Cons<H::Hooks, T::Hooks>;
    fn instantiate(self) -> (Self::Listeners, Self::Hooks) {
        let (tail, tail_hooks) = self.1.instantiate();
        let (head, head_hooks) = self.0.instantiate();
        (Cons(head, tail), Cons(head_hooks, tail_hooks))
    }
}
impl<S: Storage, E: 'static, H: Handle<E, World<S>>> Instantiate<S> for On<E, H> {
    type Listeners = Self;
    type Hooks = Nil;
    fn instantiate(self) -> (Self, Nil) {
        (self, Nil)
    }
}
impl<S: Storage, F: TaskFn<S>> Instantiate<S> for Task<S, F> {
    type Listeners = Self;
    type Hooks = Nil;
    fn instantiate(self) -> (Self, Nil) {
        (self, Nil)
    }
}
impl<S: Storage, P: Plugin> Instantiate<S> for Pending<P> {
    type Listeners = impl Fits<World<S>> + Send;
    type Hooks = impl HookList<S> + Send + Sync + 'static;
    fn instantiate(self) -> (Self::Listeners, Self::Hooks) {
        self.0.systems::<S>().split()
    }
}

/// A builder's hook list for world `S`: relation slots become their hooks.
#[doc(hidden)]
pub trait InstantiateHooks<S: Storage> {
    type Out: HookList<S> + Send + Sync + 'static;
    fn instantiate(self) -> Self::Out;
}
impl<S: Storage> InstantiateHooks<S> for Nil {
    type Out = Nil;
    fn instantiate(self) -> Nil {
        Nil
    }
}
impl<S: Storage, A: InstantiateHooks<S>, B: InstantiateHooks<S>> InstantiateHooks<S>
    for Cons<A, B>
{
    type Out = Cons<A::Out, B::Out>;
    fn instantiate(self) -> Self::Out {
        Cons(self.0.instantiate(), self.1.instantiate())
    }
}
impl<S: Storage, A: InstantiateHooks<S>, B: InstantiateHooks<S>> InstantiateHooks<S> for (A, B) {
    type Out = Cons<A::Out, B::Out>;
    fn instantiate(self) -> Self::Out {
        Cons(self.0.instantiate(), self.1.instantiate())
    }
}
impl<S: Storage, E, P> InstantiateHooks<S> for Hook<E, P>
where
    Self: HookList<S> + Send + Sync + 'static,
{
    type Out = Self;
    fn instantiate(self) -> Self {
        self
    }
}
impl<S: Storage, R: Relation> InstantiateHooks<S> for Fragment<R>
where
    Self: HookList<S>,
{
    type Out = Self;
    fn instantiate(self) -> Self {
        self
    }
}
impl<S: Storage, R: Relation> InstantiateHooks<S> for RelationSlot<R>
where
    RelationHooks<R, S>: InstantiateHooks<S>,
{
    type Out = <RelationHooks<R, S> as InstantiateHooks<S>>::Out;
    fn instantiate(self) -> Self::Out {
        relation::relation::<R, S>().instantiate()
    }
}
