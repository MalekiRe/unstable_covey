use std::marker::PhantomData;

use crate::change::SystemWorld;
use crate::executor::{self, Context, Continue, Done, Fields, First, Flow, Inner, Project, Root};
use crate::executor::{Second, Tail, Within, serially};
use crate::hook::{Event, Hook, HookList};
use crate::lookup::ComponentLookup;
use crate::plan::{ExecutionMode, Input, NoInput, Ref, Shape, run_now};
use crate::plugin::{
    ComponentList, DeclarationList, InstallRelation, Instantiate, InstantiateHooks, Pending, Plugin, RelationList,
};
use crate::state::State;
use crate::vocab::{Declaration, Insert, Uses};
use crate::{Components, IntoSystems, Storage, System, World};

/// A schedule label: what `add_systems` registers under and `emit` runs.
/// `In` is what every system under it takes first: `NoInput` (nothing, the
/// default) or `Ref<Self>` (the emitted value). Lifecycle labels such as
/// `OnInsert<T>` hand their systems a `Trigger`.
/// ```
/// use static_ecs::prelude::*;
/// struct Update;
/// impl Schedule for Update {}
/// struct Damage(u32);
/// impl Schedule for Damage {
///     type In = Ref<Self>;
/// }
/// ```
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a schedule label",
    note = "implement `Schedule` for it: `impl Schedule for {Self} {{}}`, or `type In = Ref<Self>;` to hand systems the emitted value"
)]
pub trait Schedule: Sized + 'static {
    type In: Input = NoInput;
    /// Who fires the label: `emit`, or the world's structural operations.
    #[doc(hidden)]
    type Route: Route = Emitted;
}
/// Where a label's systems live.
#[doc(hidden)]
pub trait Route {
    type Place<E: Schedule, P: Shape<Output = (), In = E::In>>;
    #[cfg_attr(feature = "devtools", track_caller)]
    fn place<E: Schedule, P: Shape<Output = (), In = E::In>>(systems: P) -> Self::Place<E, P>;
}
/// Fired by `App::emit`: systems join the app's frame.
#[doc(hidden)]
pub struct Emitted;
impl Route for Emitted {
    type Place<E: Schedule, P: Shape<Output = (), In = E::In>> = Scheduled<E, P>;
    fn place<E: Schedule, P: Shape<Output = (), In = E::In>>(systems: P) -> Scheduled<E, P> {
        On {
            handler: Run(systems),
            marker: PhantomData,
            #[cfg(feature = "devtools")]
            location: std::panic::Location::caller(),
        }
    }
}
/// Fired by the world itself: systems join its hook table.
#[doc(hidden)]
pub struct Lifecycle;
impl Route for Lifecycle {
    type Place<E: Schedule, P: Shape<Output = (), In = E::In>> = Hook<E, P>;
    fn place<E: Schedule, P: Shape<Output = (), In = E::In>>(systems: P) -> Hook<E, P> {
        Hook::new(systems)
    }
}
/// Systems `P` placed under label `E`.
pub type Placed<E, P> = <<E as Schedule>::Route as Route>::Place<E, P>;
/// Builds a label's input from the emitted value.
#[doc(hidden)]
pub trait FromEvent<E>: Input {
    fn of(event: &E) -> Self::Item<'_>;
}
impl<E> FromEvent<E> for NoInput {
    fn of(_: &E) -> NoInput {
        NoInput
    }
}
impl<E: 'static> FromEvent<E> for Ref<E> {
    fn of(event: &E) -> &E {
        event
    }
}

/// The unit label, for apps with one schedule.
impl Schedule for () {}

/// Names a schedule label `E`: the label value itself (`Update`), or a tuple
/// struct's constructor (`Damage`, `OnInsert::<ChildOf>`), so payload and
/// lifecycle labels need no dummy value.
#[diagnostic::on_unimplemented(
    message = "`{Self}` is not a schedule label",
    note = "implement `Schedule` for the label type: `impl Schedule for Update {{}}`"
)]
pub trait Label<E, M> {}
impl<E: Schedule> Label<E, ()> for E {}
impl<E: Schedule, A, F: FnOnce(A) -> E> Label<E, fn(A)> for F {}
impl<E: Schedule, A, B, F: FnOnce(A, B) -> E> Label<E, fn(A, B)> for F {}
impl<E: Schedule, A, B, C, F: FnOnce(A, B, C) -> E> Label<E, fn(A, B, C)> for F {}

/// Names label `E` without a value of it, for payload labels whose fields
/// are named: `add_systems(label::<KeyInput>(), |key: &KeyInput| ...)`.
pub struct LabelOf<E>(PhantomData<fn() -> E>);
/// See [`LabelOf`].
pub fn label<E: Schedule>() -> LabelOf<E> {
    LabelOf(PhantomData)
}
impl<E: Schedule> Label<E, LabelOf<E>> for LabelOf<E> {}

/// An installable registration, e.g. returned by a plugin:
/// `.add_plugins(schedule(Click, count_clicks))` equals
/// `.add_systems(Click, count_clicks)`.
#[cfg_attr(feature = "devtools", track_caller)]
pub fn schedule<E, X, LM, P, M>(_: X, systems: P) -> Placed<E, P::Systems>
where
    E: Schedule,
    X: Label<E, LM>,
    P: IntoSystems<E::In, M>,
{
    <E::Route as Route>::place::<E, P::Systems>(systems.into_systems())
}

#[derive(Default)]
pub struct Nil;
pub struct Cons<H, T>(pub(crate) H, pub(crate) T);

/// Keep E visible across crate boundaries; only hide the systems.
pub struct On<E: 'static, H> {
    handler: H,
    marker: PhantomData<fn(E)>,
    /// Where it was registered.
    #[cfg(feature = "devtools")]
    location: &'static std::panic::Location<'static>,
}

pub trait Handle<E, S> {
    #[doc(hidden)]
    const HAS_PARALLEL: bool = false;
    #[doc(hidden)]
    const WEIGHT: usize = 1;
    /// Valid for world S: storage resolves and accesses do not alias.
    #[doc(hidden)]
    const VALID: bool = true;
    /// # Safety
    /// `input` points at a live item of `E`'s input family.
    #[doc(hidden)]
    unsafe fn handle(&mut self, input: *const (), state: &mut S);
    /// Move commands recorded by `handle`/`enter` to the world's queue.
    #[doc(hidden)]
    fn defer(&mut self, _queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
    }
    /// Handle within a frame invocation, then continue with K.
    /// # Safety
    /// Every earlier handler has finished; the context input belongs to `E`.
    #[doc(hidden)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld;
    /// Present the handler's systems to a devtools traversal.
    #[cfg(feature = "devtools")]
    #[doc(hidden)]
    fn inspect(&mut self, _visitor: &mut dyn crate::devtools::Visitor<S>)
    where
        S: SystemWorld,
    {
    }
}

pub struct Run<P>(P);
pub type Scheduled<E, P> = On<E, Run<P>>;

impl<P> Inner for Run<P> {
    type Inner = P;
    unsafe fn inner(this: *mut Self) -> *mut P {
        unsafe { &raw mut (*this).0 }
    }
}
impl<E: 'static, H> Inner for On<E, H> {
    type Inner = H;
    unsafe fn inner(this: *mut Self) -> *mut H {
        unsafe { &raw mut (*this).handler }
    }
}
impl<H, T> Fields for Cons<H, T> {
    type Left = H;
    type Right = T;
    unsafe fn left(this: *mut Self) -> *mut H {
        unsafe { &raw mut (*this).0 }
    }
    unsafe fn right(this: *mut Self) -> *mut T {
        unsafe { &raw mut (*this).1 }
    }
}

impl<E: Schedule, S: SystemWorld, P: System<S, Output = (), In = E::In>> Handle<E, S> for Run<P> {
    const HAS_PARALLEL: bool = P::HAS_PARALLEL;
    const WEIGHT: usize = P::WEIGHT;
    const VALID: bool = P::VALID;
    #[inline]
    unsafe fn handle(&mut self, input: *const (), state: &mut S) {
        let _ = unsafe { run_now(&mut self.0, input, state) };
    }
    fn defer(&mut self, queue: &mut S::Queue) {
        self.0.defer(queue);
    }
    #[inline(always)]
    unsafe fn enter<R: Send, Path: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld,
    {
        const {
            assert!(P::VALID, "invalid system access");
        }
        unsafe { P::enter::<R, Within<Path>, K>(context, flow) }
    }
    #[cfg(feature = "devtools")]
    fn inspect(&mut self, visitor: &mut dyn crate::devtools::Visitor<S>) {
        self.0.inspect(visitor);
    }
}

pub trait MaybeHandle<E: 'static, S> {
    #[doc(hidden)]
    const HAS_PARALLEL: bool;
    #[doc(hidden)]
    const WEIGHT: usize;
    /// # Safety
    /// As `Handle::handle`.
    #[doc(hidden)]
    unsafe fn maybe_handle(&mut self, input: *const (), state: &mut S);
    #[doc(hidden)]
    fn maybe_defer(&mut self, queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld;
    /// # Safety
    /// As `Handle::enter`.
    #[doc(hidden)]
    unsafe fn maybe_enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld;
}

// The essential specialization: unequal event types do nothing.
impl<E: 'static, S, H> MaybeHandle<E, S> for H {
    default const HAS_PARALLEL: bool = false;
    default const WEIGHT: usize = 0;
    #[inline]
    default unsafe fn maybe_handle(&mut self, _: *const (), _: &mut S) {}
    #[inline]
    default fn maybe_defer(&mut self, _: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
    }
    #[inline(always)]
    default unsafe fn maybe_enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld,
    {
        unsafe { K::resume(context, flow) }
    }
}

impl<E: 'static, S, H: Handle<E, S>> MaybeHandle<E, S> for On<E, H> {
    const HAS_PARALLEL: bool = H::HAS_PARALLEL;
    const WEIGHT: usize = H::WEIGHT;
    #[inline]
    unsafe fn maybe_handle(&mut self, input: *const (), state: &mut S) {
        unsafe { self.handler.handle(input, state) };
    }
    #[inline]
    fn maybe_defer(&mut self, queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
        self.handler.defer(queue);
    }
    #[inline(always)]
    unsafe fn maybe_enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld,
    {
        unsafe { H::enter::<R, Within<P>, K>(context, flow) }
    }
}

/// A nested list (a plugin's systems) handles like the list it is.
impl<E: 'static, S, H, T> MaybeHandle<E, S> for Cons<H, T>
where
    Cons<H, T>: Dispatch<E, S>,
{
    const HAS_PARALLEL: bool = <Self as Dispatch<E, S>>::HAS_PARALLEL;
    const WEIGHT: usize = <Self as Dispatch<E, S>>::WEIGHT;
    #[inline]
    unsafe fn maybe_handle(&mut self, input: *const (), state: &mut S) {
        unsafe { self.dispatch(input, state) };
    }
    #[inline]
    fn maybe_defer(&mut self, queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
        Dispatch::<E, S>::defer(self, queue);
    }
    #[inline(always)]
    unsafe fn maybe_enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld,
    {
        unsafe { <Self as Dispatch<E, S>>::enter::<R, P, K>(context, flow) }
    }
}

/// Registration-order dispatch. A frame runs every matching handler as one
/// sequence of continuations: no barrier between registrations.
pub trait Dispatch<E: 'static, S> {
    #[doc(hidden)]
    const HAS_PARALLEL: bool = false;
    #[doc(hidden)]
    const WEIGHT: usize = 0;
    /// # Safety
    /// `input` points at a live item of `E`'s input family.
    #[doc(hidden)]
    unsafe fn dispatch(&mut self, input: *const (), state: &mut S);
    /// Move the matching handlers' commands to the queue, in registration order.
    #[doc(hidden)]
    fn defer(&mut self, queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld;
    /// # Safety
    /// As `Handle::enter`.
    #[doc(hidden)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld;
}

impl<E: 'static, S> Dispatch<E, S> for Nil {
    #[inline]
    unsafe fn dispatch(&mut self, _: *const (), _: &mut S) {}
    #[inline]
    fn defer(&mut self, _: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
    }
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld,
    {
        unsafe { K::resume(context, flow) }
    }
}

/// After the earlier registrations, run the head handler at P.
struct Next<E, P, K>(PhantomData<fn(E)>, PhantomData<(P, K)>);
impl<E: 'static, S, R, P, K> Continue<S, R> for Next<E, P, K>
where
    S: Send + Sync + SystemWorld,
    R: Send,
    P: Project<R, Target: MaybeHandle<E, S>>,
    K: Continue<S, R>,
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
        unsafe { P::Target::maybe_enter::<R, P, K>(context, flow) }
    }
}

impl<E: 'static, S, H: MaybeHandle<E, S>, T: Dispatch<E, S>> Dispatch<E, S> for Cons<H, T> {
    const HAS_PARALLEL: bool = H::HAS_PARALLEL || T::HAS_PARALLEL;
    const WEIGHT: usize = H::WEIGHT + T::WEIGHT;
    #[inline]
    unsafe fn dispatch(&mut self, input: *const (), state: &mut S) {
        unsafe {
            self.1.dispatch(input, state);
            self.0.maybe_handle(input, state);
        }
    }
    #[inline]
    fn defer(&mut self, queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
        self.1.defer(queue);
        self.0.maybe_defer(queue);
    }
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld,
    {
        unsafe { T::enter::<R, Second<P>, Next<E, First<P>, K>>(context, flow) }
    }
}

/// The root continuation of a frame invocation.
struct Frame<E, K>(PhantomData<fn() -> (E, K)>);
impl<E: 'static, S, R, K> Continue<S, R> for Frame<E, K>
where
    S: Send + Sync + SystemWorld,
    R: Dispatch<E, S> + Send,
    K: Continue<S, R>,
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
        unsafe { R::enter::<R, Root, K>(context, flow) }
    }
}
/// Thread-safe listener lists run a frame as one invocation; others run each
/// parallel registration as its own invocation.
trait Emit<E: 'static, S>: Dispatch<E, S> {
    /// # Safety
    /// As `Dispatch::dispatch`.
    unsafe fn emit(&mut self, input: *const (), state: &mut S);
}
impl<E: 'static, S, L: Dispatch<E, S>> Emit<E, S> for L {
    #[inline]
    default unsafe fn emit(&mut self, input: *const (), state: &mut S) {
        unsafe { self.dispatch(input, state) };
    }
}
impl<E, S, L> Emit<E, S> for L
where
    E: 'static + Sync,
    S: Send + Sync + SystemWorld,
    L: Dispatch<E, S> + Send,
{
    #[inline]
    unsafe fn emit(&mut self, input: *const (), state: &mut S) {
        if const { !L::HAS_PARALLEL } || !executor::parallel(L::WEIGHT) {
            return unsafe { self.dispatch(input, state) };
        }
        unsafe {
            let start = Flow::new(S::clock(state), false);
            let end = executor::execute::<S, L, Frame<E, Done>>(self, state, input, start);
            S::set_clock(state, end.clock());
        }
    }
}

/// What a handler's systems access, independent of any world type.
#[doc(hidden)]
pub trait Handler {
    type Access;
}
impl<P: Shape> Handler for Run<P> {
    type Access = P::Access;
}

/// One registration from `add_systems`: listeners under an emitted label, or
/// hooks under a lifecycle label. `Components` is `C` extended with every
/// component its systems use.
#[doc(hidden)]
pub trait Register<C, L, K> {
    type Components: Storage;
    type Listeners;
    type Hooks;
    fn add_to(self, listeners: L, hooks: K) -> (Self::Listeners, Self::Hooks);
}
impl<E: 'static, H: Handler<Access: Uses<C>>, C, L, K> Register<C, L, K> for On<E, H> {
    type Components = <H::Access as Uses<C>>::Out;
    type Listeners = Cons<Self, L>;
    type Hooks = K;
    fn add_to(self, listeners: L, hooks: K) -> (Self::Listeners, K) {
        (Cons(self, listeners), hooks)
    }
}
impl<E, P, C, L, K> Register<C, L, K> for Hook<E, P>
where
    E: Event,
    P: Shape<Output = ()>,
    C: Insert<Components<E::Component>>,
    P::Access: Uses<<C as Insert<Components<E::Component>>>::Out>,
{
    type Components = <P::Access as Uses<<C as Insert<Components<E::Component>>>::Out>>::Out;
    type Listeners = L;
    type Hooks = Cons<Self, K>;
    fn add_to(self, listeners: L, hooks: K) -> (L, Self::Hooks) {
        (listeners, Cons(self, hooks))
    }
}

/// Anything `add_plugins` accepts: a `Plugin`, a `task(...)`, or a tuple of up
/// to 16 of them, nested freely, applied left to right. Each changes the
/// builder's type: `Components` is `C` extended with what it stores; systems
/// extend the listener list `L` (plugins as placeholders until `build`),
/// relations the hook list `K`.
pub trait Plugins<C, L, K> {
    type Components: Storage;
    type Listeners;
    type Hooks;
    #[doc(hidden)]
    fn add_to(self, listeners: L, hooks: K) -> (Self::Listeners, Self::Hooks);
}
/// The plugin's components and relations now; its systems at `build`.
impl<P: Plugin, C, L, K> Plugins<C, L, K> for P
where
    P::Storage: DeclarationList<C>,
    P::Components: ComponentList<<P::Storage as DeclarationList<C>>::Out>,
    P::Relations: RelationList<<P::Components as ComponentList<<P::Storage as DeclarationList<C>>::Out>>::Out, K>,
{
    type Components = <P::Relations as RelationList<
        <P::Components as ComponentList<<P::Storage as DeclarationList<C>>::Out>>::Out,
        K,
    >>::Components;
    type Listeners = Cons<Pending<P>, L>;
    type Hooks = <P::Relations as RelationList<
        <P::Components as ComponentList<<P::Storage as DeclarationList<C>>::Out>>::Out,
        K,
    >>::Hooks;
    fn add_to(self, listeners: L, hooks: K) -> (Self::Listeners, Self::Hooks) {
        let hooks = <P::Relations as RelationList<
            <P::Components as ComponentList<<P::Storage as DeclarationList<C>>::Out>>::Out,
            K,
        >>::install(hooks);
        (Cons(Pending(self), listeners), hooks)
    }
}
impl<C, L, K, A: Plugins<C, L, K>> Plugins<C, L, K> for (A,) {
    type Components = A::Components;
    type Listeners = A::Listeners;
    type Hooks = A::Hooks;
    fn add_to(self, listeners: L, hooks: K) -> (A::Listeners, A::Hooks) {
        self.0.add_to(listeners, hooks)
    }
}
impl<C, L, K, A, B> Plugins<C, L, K> for (A, B)
where
    A: Plugins<C, L, K>,
    B: Plugins<A::Components, A::Listeners, A::Hooks>,
{
    type Components = B::Components;
    type Listeners = B::Listeners;
    type Hooks = B::Hooks;
    fn add_to(self, listeners: L, hooks: K) -> (B::Listeners, B::Hooks) {
        let (listeners, hooks) = self.0.add_to(listeners, hooks);
        self.1.add_to(listeners, hooks)
    }
}
/// Longer tuples apply their head, then the rest as one tuple.
macro_rules! plugin_tuples {
    ($a:ident $($i:tt $t:ident)+) => {
        impl<C, L, K, $a, $($t),+> Plugins<C, L, K> for ($a, $($t),+)
        where
            ($a, ($($t,)+)): Plugins<C, L, K>,
        {
            type Components = <($a, ($($t,)+)) as Plugins<C, L, K>>::Components;
            type Listeners = <($a, ($($t,)+)) as Plugins<C, L, K>>::Listeners;
            type Hooks = <($a, ($($t,)+)) as Plugins<C, L, K>>::Hooks;
            fn add_to(self, listeners: L, hooks: K) -> (Self::Listeners, Self::Hooks) {
                (self.0, ($(self.$i,)+)).add_to(listeners, hooks)
            }
        }
    };
}
plugin_tuples!(P0 1 P1 2 P2);
plugin_tuples!(P0 1 P1 2 P2 3 P3);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11 12 P12);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11 12 P12 13 P13);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11 12 P12 13 P13 14 P14);
plugin_tuples!(P0 1 P1 2 P2 3 P3 4 P4 5 P5 6 P6 7 P7 8 P8 9 P9 10 P10 11 P11 12 P12 13 P13 14 P14 15 P15);

/// Every handler in a listener list runs against world `W`. Checked once at
/// `build`, which also fixes the world type of systems written as `World<_>`.
#[doc(hidden)]
pub trait Fits<W> {
    const VALID: bool;
    /// Present every registration, in dispatch order, to a devtools traversal.
    #[cfg(feature = "devtools")]
    fn inspect(&mut self, _visitor: &mut dyn crate::devtools::Visitor<W>)
    where
        W: SystemWorld,
    {
    }
}
impl<W> Fits<W> for Nil {
    const VALID: bool = true;
}
impl<W, H: Fits<W>, T: Fits<W>> Fits<W> for Cons<H, T> {
    const VALID: bool = H::VALID && T::VALID;
    /// The tail holds earlier registrations, which dispatch first.
    #[cfg(feature = "devtools")]
    fn inspect(&mut self, visitor: &mut dyn crate::devtools::Visitor<W>)
    where
        W: SystemWorld,
    {
        self.1.inspect(visitor);
        self.0.inspect(visitor);
    }
}
impl<W, E: 'static, H: Handle<E, W>> Fits<W> for On<E, H> {
    const VALID: bool = H::VALID;
    #[cfg(feature = "devtools")]
    fn inspect(&mut self, visitor: &mut dyn crate::devtools::Visitor<W>)
    where
        W: SystemWorld,
    {
        let registration = crate::devtools::Registration {
            label: std::any::TypeId::of::<E>(),
            name: std::any::type_name::<E>(),
            location: self.location,
            hook: false,
        };
        if visitor.registration(&registration) {
            self.handler.inspect(visitor);
        }
        visitor.end_registration();
    }
}

/// Composes plugins, systems, and hooks; `build` creates a world storing
/// exactly the components they use. `C` is that inferred component set.
pub struct AppBuilder<C = (), L = Nil, K = Nil, R = Nil> {
    listeners: L,
    hooks: K,
    /// Resources, states, and state links to apply at `build`, newest first.
    resources: R,
    execution_mode: ExecutionMode,
    components: PhantomData<fn() -> C>,
    /// Registration calls, for devtools' "why is this in my world".
    #[cfg(feature = "devtools")]
    registered: Vec<crate::devtools::Registered>,
}

impl<C: Storage, L, K, R: Setup<C>> AppBuilder<C, L, K, R> {
    pub fn with_execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }
    /// Add a plugin or a tuple of plugins (up to 16, nested freely), applied
    /// left to right: `.add_plugins((Physics, Net { stats: &mut stats }))`.
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn add_plugins<P: Plugins<C, L, K>>(
        self,
        plugins: P,
    ) -> AppBuilder<P::Components, P::Listeners, P::Hooks, R> {
        let (listeners, hooks) = plugins.add_to(self.listeners, self.hooks);
        AppBuilder {
            listeners,
            hooks,
            resources: self.resources,
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<P>(self.registered, "add_plugins"),
        }
    }
    /// Register systems under label `E`. Each takes `E`'s input first (none for
    /// plain labels, `&E` for payload labels, `Trigger` for lifecycle labels).
    /// Plain and payload labels run on `emit`; lifecycle labels run inside the
    /// world's structural operations, serially and without exclusive access:
    /// ```compile_fail
    /// use static_ecs::prelude::*;
    /// struct Tag;
    /// App::new()
    ///     .add_systems(OnAdd::<Tag>, exclusive(|_: Trigger, _: &mut World<_>| {}))
    ///     .build();
    /// ```
    /// A system that does not take the label's input fails to compile:
    /// ```compile_fail
    /// use static_ecs::prelude::*;
    /// struct Damage(u32);
    /// impl Schedule for Damage {
    ///     type In = Ref<Self>;
    /// }
    /// App::new().add_systems(Damage, |_: Query<&u32>| {});
    /// ```
    #[allow(clippy::type_complexity)]
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn add_systems<E: Schedule, X: Label<E, LM>, LM, P: IntoSystems<E::In, M>, M>(
        self,
        label: X,
        systems: P,
    ) -> AppBuilder<
        <Placed<E, P::Systems> as Register<C, L, K>>::Components,
        <Placed<E, P::Systems> as Register<C, L, K>>::Listeners,
        <Placed<E, P::Systems> as Register<C, L, K>>::Hooks,
        R,
    >
    where
        Placed<E, P::Systems>: Register<C, L, K>,
    {
        let (listeners, hooks) = schedule(label, systems).add_to(self.listeners, self.hooks);
        AppBuilder {
            listeners,
            hooks,
            resources: self.resources,
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: self.registered,
        }
    }
    /// Install relation `R`: its components `Relates<R>`/`Sources<R>` and the
    /// hooks keeping them consistent. Installing it again, here or from a
    /// plugin's `Relations`, changes nothing.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn relation<Rel: InstallRelation<C, K>>(
        self,
    ) -> AppBuilder<
        <Rel as InstallRelation<C, K>>::Components,
        L,
        <Rel as InstallRelation<C, K>>::Hooks,
        R,
    > {
        AppBuilder {
            listeners: self.listeners,
            hooks: Rel::install(self.hooks),
            resources: self.resources,
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<Rel>(self.registered, "relation"),
        }
    }
    /// Store `T` even though no system queries it, e.g. a tag that is only
    /// inserted, or data read through `World` methods outside systems. Keeps
    /// any storage already chosen for `T`.
    /// ```compile_fail
    /// use static_ecs::prelude::*;
    /// struct Tag;
    /// let mut app = App::new().build();
    /// let entity = app.world.spawn();
    /// app.world.insert(entity, Tag); // never used or registered
    /// ```
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn register<T: 'static>(self) -> AppBuilder<<C as Insert<Components<T>>>::Out, L, K, R>
    where
        C: Insert<Components<T>>,
    {
        AppBuilder {
            listeners: self.listeners,
            hooks: self.hooks,
            resources: self.resources,
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<T>(self.registered, "register"),
        }
    }
    /// Choose a component's storage: `Sparse<T>`, or `Components<T, true>` for
    /// column change summaries. Must precede systems that use `T`.
    /// ```compile_fail
    /// use static_ecs::prelude::*;
    /// struct Update;
    /// struct Tag;
    /// App::new()
    ///     .add_systems(Update, |_: Query<&Tag>| {})
    ///     .storage::<Sparse<Tag>>() // too late: Tag is already dense
    ///     .build();
    /// ```
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn storage<D: Declaration>(self) -> AppBuilder<<C as Insert<D>>::Out, L, K, R>
    where
        C: Insert<D>,
    {
        const {
            assert!(
                <C as ComponentLookup<D::Component>>::COUNT == 0
                    || (<C as ComponentLookup<D::Component>>::SPARSE == D::SPARSE
                        && <C as ComponentLookup<D::Component>>::SUMMARY == D::SUMMARY),
                "component already stored differently: choose storage before systems that use it"
            );
        }
        AppBuilder {
            listeners: self.listeners,
            hooks: self.hooks,
            resources: self.resources,
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<D::Component>(self.registered, "storage"),
        }
    }
    /// Insert resource `T` at `build`, after hooks are installed, so its
    /// `OnAdd`/`OnInsert` hooks run; resources apply in the order given.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn insert_resource<T: Send + 'static>(
        self,
        value: T,
    ) -> AppBuilder<<C as Insert<Components<T>>>::Out, L, K, Cons<InsertResource<T>, R>>
    where
        C: Insert<Components<T>>,
    {
        AppBuilder {
            listeners: self.listeners,
            hooks: self.hooks,
            resources: Cons(InsertResource(value), self.resources),
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<T>(self.registered, "insert_resource"),
        }
    }
    /// Insert `T::default()` at `build` unless an earlier insert provided `T`.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn init_resource<T: Default + Send + 'static>(
        self,
    ) -> AppBuilder<<C as Insert<Components<T>>>::Out, L, K, Cons<InitResource<T>, R>>
    where
        C: Insert<Components<T>>,
    {
        AppBuilder {
            listeners: self.listeners,
            hooks: self.hooks,
            resources: Cons(InitResource(PhantomData), self.resources),
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<T>(self.registered, "init_resource"),
        }
    }
    /// Enter `state` at `build`, after hooks are installed, so its `OnEnter`
    /// hooks and substates run.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn init_state<T: State>(
        self,
        state: T,
    ) -> AppBuilder<<C as Insert<Components<T>>>::Out, L, K, Cons<InitState<T>, R>>
    where
        C: Insert<Components<T>>,
    {
        AppBuilder {
            listeners: self.listeners,
            hooks: self.hooks,
            resources: Cons(InitState(state), self.resources),
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<T>(self.registered, "init_state"),
        }
    }
    /// Substate group of `Sub`: entered as `Sub::default()` after parent state
    /// `P` is entered, exited before `P` exits. Setting a state of the group
    /// while `P` is inactive is ignored (a debug panic).
    #[allow(clippy::type_complexity)]
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn add_sub_state<P: State, Sub: State + Default>(
        self,
    ) -> AppBuilder<
        <<C as Insert<Components<P>>>::Out as Insert<Components<Sub>>>::Out,
        L,
        K,
        Cons<SubState<P, Sub>, R>,
    >
    where
        C: Insert<Components<P>>,
        <C as Insert<Components<P>>>::Out: Insert<Components<Sub>>,
    {
        AppBuilder {
            listeners: self.listeners,
            hooks: self.hooks,
            resources: Cons(SubState(PhantomData), self.resources),
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<Sub>(self.registered, "add_sub_state"),
        }
    }
    /// Computed state group of `X`: whenever parent state `P` is entered,
    /// `compute(&p)` enters its result (if any); exited before `P` exits.
    #[allow(clippy::type_complexity)]
    #[cfg_attr(feature = "devtools", track_caller)]
    pub fn add_computed_state<P: State, X: State>(
        self,
        compute: fn(&P) -> Option<X>,
    ) -> AppBuilder<
        <<C as Insert<Components<P>>>::Out as Insert<Components<X>>>::Out,
        L,
        K,
        Cons<ComputedState<P, X>, R>,
    >
    where
        C: Insert<Components<P>>,
        <C as Insert<Components<P>>>::Out: Insert<Components<X>>,
    {
        AppBuilder {
            listeners: self.listeners,
            hooks: self.hooks,
            resources: Cons(ComputedState(compute), self.resources),
            execution_mode: self.execution_mode,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: crate::devtools::noted::<X>(self.registered, "add_computed_state"),
        }
    }
    /// Create the world, which owns the hooks. Plugins build their systems
    /// for it, then every handler and hook is checked against it. A plugin's
    /// hooks run after the app's own.
    pub fn build(self) -> App<C, <L as Instantiate<C>>::Listeners>
    where
        L: Instantiate<C>,
        K: InstantiateHooks<C>,
    {
        fn check<S: Storage, L: Fits<World<S>>, K: HookList<S>>(_: &L, _: &K) {
            const {
                assert!(
                    L::VALID && K::VALID,
                    "invalid system: missing/ambiguous storage or conflicting access"
                );
            }
        }
        let (listeners, plugin_hooks) = self.listeners.instantiate();
        let hooks = Cons(plugin_hooks, self.hooks.instantiate());
        check::<C, _, _>(&listeners, &hooks);
        let mut world = World::default();
        if !is_empty::<C, _>(&hooks) {
            world.set_hooks(hooks);
        }
        self.resources.link(&mut world);
        self.resources.apply(&mut world);
        #[cfg_attr(not(feature = "devtools"), expect(unused_mut))]
        let mut app = App {
            world,
            listeners,
            execution_mode: self.execution_mode,
            #[cfg(feature = "devtools")]
            stepping: Default::default(),
            #[cfg(feature = "devtools")]
            snapshots: Default::default(),
            #[cfg(feature = "devtools")]
            registered: self.registered,
        };
        #[cfg(feature = "devtools")]
        crate::devtools::step::register(&mut app);
        app
    }
}
/// A builder's setup for the built world, oldest first: every state link,
/// then every resource and initial state.
#[doc(hidden)]
pub trait Setup<S: Storage> {
    fn link(&self, _: &mut World<S>) {}
    fn apply(self, world: &mut World<S>);
}
impl<S: Storage> Setup<S> for Nil {
    fn apply(self, _: &mut World<S>) {}
}
impl<S: Storage, H: Setup<S>, T: Setup<S>> Setup<S> for Cons<H, T> {
    fn link(&self, world: &mut World<S>) {
        self.1.link(world);
        self.0.link(world);
    }
    fn apply(self, world: &mut World<S>) {
        self.1.apply(world);
        self.0.apply(world);
    }
}
#[doc(hidden)]
pub struct InsertResource<T>(T);
impl<S: Storage, T: 'static> Setup<S> for InsertResource<T> {
    fn apply(self, world: &mut World<S>) {
        world.insert_resource(self.0);
    }
}
#[doc(hidden)]
pub struct InitResource<T>(PhantomData<fn() -> T>);
impl<S: Storage, T: Default + 'static> Setup<S> for InitResource<T> {
    fn apply(self, world: &mut World<S>) {
        world.init_resource::<T>();
    }
}
#[doc(hidden)]
pub struct InitState<T>(T);
impl<S: Storage, T: State> Setup<S> for InitState<T> {
    fn apply(self, world: &mut World<S>) {
        world.set_state(self.0);
    }
}
#[doc(hidden)]
pub struct SubState<P, C>(PhantomData<fn() -> (P, C)>);
impl<S: Storage, P: State, C: State + Default> Setup<S> for SubState<P, C> {
    fn link(&self, world: &mut World<S>) {
        world.link_sub_state::<P, C>();
    }
    fn apply(self, _: &mut World<S>) {}
}
#[doc(hidden)]
pub struct ComputedState<P, X>(fn(&P) -> Option<X>);
impl<S: Storage, P: State, X: State> Setup<S> for ComputedState<P, X> {
    fn link(&self, world: &mut World<S>) {
        world.link_computed_state(self.0);
    }
    fn apply(self, _: &mut World<S>) {}
}
/// Whether a hook list holds no hooks at all.
fn is_empty<S: Storage, K: HookList<S>>(_: &K) -> bool {
    K::EMPTY
}

/// A built app: a world with its inferred component set `S`, and every handler.
pub struct App<S: Storage, L = Nil> {
    pub world: World<S>,
    pub(crate) listeners: L,
    pub(crate) execution_mode: ExecutionMode,
    /// Position of a devtools step pass.
    #[cfg(feature = "devtools")]
    pub(crate) stepping: crate::devtools::step::Stepping,
    /// Automatic devtools snapshots.
    #[cfg(feature = "devtools")]
    pub(crate) snapshots: crate::devtools::snapshot::Snapshots<S>,
    /// The builder's registration calls.
    #[cfg(feature = "devtools")]
    pub(crate) registered: Vec<crate::devtools::Registered>,
}

impl App<()> {
    /// Start composing an app. Components are inferred from its systems.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> AppBuilder {
        AppBuilder {
            listeners: Nil,
            hooks: Nil,
            resources: Nil,
            execution_mode: ExecutionMode::Parallel,
            components: PhantomData,
            #[cfg(feature = "devtools")]
            registered: Vec::new(),
        }
    }
}

impl<S: Storage, L> App<S, L> {
    pub fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.execution_mode = mode;
    }
    /// Runs every system registered under `E`, in registration order, handing
    /// each `E`'s input, then applies their commands in the same order.
    #[inline]
    pub fn emit<E: Schedule>(&mut self, event: &E)
    where
        E::In: FromEvent<E>,
        L: Dispatch<E, World<S>>,
    {
        #[cfg(feature = "devtools")]
        crate::devtools::emitted::<E>();
        let input = <E::In as FromEvent<E>>::of(event);
        let input = (&raw const input).cast();
        // SAFETY: `input` is `E`'s input and outlives the frame.
        match self.execution_mode {
            ExecutionMode::Parallel => unsafe { self.listeners.emit(input, &mut self.world) },
            ExecutionMode::Serial => {
                serially(|| unsafe { self.listeners.dispatch(input, &mut self.world) })
            }
        }
        self.listeners
            .defer(unsafe { &mut *<World<S> as SystemWorld>::queue(&mut self.world) });
        self.world.flush();
        #[cfg(feature = "devtools")]
        self.world.record_frame(std::any::type_name::<E>());
        #[cfg(feature = "devtools")]
        self.snapshots.after_frame(&self.world);
    }
}
