use std::marker::PhantomData;

use crate::change::SystemWorld;
use crate::executor::{self, Context, Continue, Done, Fields, First, Flow, Inner, Project, Root};
use crate::executor::{Second, Tail, Within, serially};
use crate::hook::{Event, Hook, HookList};
use crate::lookup::ComponentLookup;
use crate::plan::{ExecutionMode, Input, NoInput, Ref, Shape, run_now};
use crate::plugin::{
    ComponentList, InstallRelation, Instantiate, InstantiateHooks, Pending, Plugin, RelationList,
};
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

/// An installable registration, e.g. returned by a plugin:
/// `.add_plugins(schedule(Click, count_clicks))` equals
/// `.add_systems(Click, count_clicks)`.
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
/// to 12 of them, nested freely, applied left to right. Each changes the
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
    P::Components: ComponentList<C>,
    P::Relations: RelationList<<P::Components as ComponentList<C>>::Out, K>,
{
    type Components =
        <P::Relations as RelationList<<P::Components as ComponentList<C>>::Out, K>>::Components;
    type Listeners = Cons<Pending<P>, L>;
    type Hooks = <P::Relations as RelationList<<P::Components as ComponentList<C>>::Out, K>>::Hooks;
    fn add_to(self, listeners: L, hooks: K) -> (Self::Listeners, Self::Hooks) {
        let hooks =
            <P::Relations as RelationList<<P::Components as ComponentList<C>>::Out, K>>::install(
                hooks,
            );
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
plugin_tuples!(A 1 B 2 C2);
plugin_tuples!(A 1 B 2 C2 3 D);
plugin_tuples!(A 1 B 2 C2 3 D 4 E);
plugin_tuples!(A 1 B 2 C2 3 D 4 E 5 F);
plugin_tuples!(A 1 B 2 C2 3 D 4 E 5 F 6 G);
plugin_tuples!(A 1 B 2 C2 3 D 4 E 5 F 6 G 7 H);
plugin_tuples!(A 1 B 2 C2 3 D 4 E 5 F 6 G 7 H 8 I);
plugin_tuples!(A 1 B 2 C2 3 D 4 E 5 F 6 G 7 H 8 I 9 J);
plugin_tuples!(A 1 B 2 C2 3 D 4 E 5 F 6 G 7 H 8 I 9 J 10 K2);
plugin_tuples!(A 1 B 2 C2 3 D 4 E 5 F 6 G 7 H 8 I 9 J 10 K2 11 L2);

/// Every handler in a listener list runs against world `W`. Checked once at
/// `build`, which also fixes the world type of systems written as `World<_>`.
#[doc(hidden)]
pub trait Fits<W> {
    const VALID: bool;
}
impl<W> Fits<W> for Nil {
    const VALID: bool = true;
}
impl<W, H: Fits<W>, T: Fits<W>> Fits<W> for Cons<H, T> {
    const VALID: bool = H::VALID && T::VALID;
}
impl<W, E: 'static, H: Handle<E, W>> Fits<W> for On<E, H> {
    const VALID: bool = H::VALID;
}

/// Composes plugins, systems, and hooks; `build` creates a world storing
/// exactly the components they use. `C` is that inferred component set.
pub struct AppBuilder<C = (), L = Nil, K = Nil> {
    listeners: L,
    hooks: K,
    execution_mode: ExecutionMode,
    components: PhantomData<fn() -> C>,
}

impl<C: Storage, L, K> AppBuilder<C, L, K> {
    pub fn with_execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }
    /// Add a plugin or a tuple of plugins (up to 12, nested freely), applied
    /// left to right: `.add_plugins((Physics, Net { stats: &mut stats }))`.
    pub fn add_plugins<P: Plugins<C, L, K>>(
        self,
        plugins: P,
    ) -> AppBuilder<P::Components, P::Listeners, P::Hooks> {
        let (listeners, hooks) = plugins.add_to(self.listeners, self.hooks);
        AppBuilder {
            listeners,
            hooks,
            execution_mode: self.execution_mode,
            components: PhantomData,
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
    pub fn add_systems<E: Schedule, X: Label<E, LM>, LM, P: IntoSystems<E::In, M>, M>(
        self,
        label: X,
        systems: P,
    ) -> AppBuilder<
        <Placed<E, P::Systems> as Register<C, L, K>>::Components,
        <Placed<E, P::Systems> as Register<C, L, K>>::Listeners,
        <Placed<E, P::Systems> as Register<C, L, K>>::Hooks,
    >
    where
        Placed<E, P::Systems>: Register<C, L, K>,
    {
        let (listeners, hooks) = schedule(label, systems).add_to(self.listeners, self.hooks);
        AppBuilder {
            listeners,
            hooks,
            execution_mode: self.execution_mode,
            components: PhantomData,
        }
    }
    /// Install relation `R`: its components `Relates<R>`/`Sources<R>` and the
    /// hooks keeping them consistent. Installing it again, here or from a
    /// plugin's `Relations`, changes nothing.
    #[allow(clippy::type_complexity)]
    pub fn relation<R: InstallRelation<C, K>>(
        self,
    ) -> AppBuilder<<R as InstallRelation<C, K>>::Components, L, <R as InstallRelation<C, K>>::Hooks>
    {
        AppBuilder {
            listeners: self.listeners,
            hooks: R::install(self.hooks),
            execution_mode: self.execution_mode,
            components: PhantomData,
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
    pub fn register<T: 'static>(self) -> AppBuilder<<C as Insert<Components<T>>>::Out, L, K>
    where
        C: Insert<Components<T>>,
    {
        AppBuilder {
            listeners: self.listeners,
            hooks: self.hooks,
            execution_mode: self.execution_mode,
            components: PhantomData,
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
    pub fn storage<D: Declaration>(self) -> AppBuilder<<C as Insert<D>>::Out, L, K>
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
            execution_mode: self.execution_mode,
            components: PhantomData,
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
        App {
            world,
            listeners,
            execution_mode: self.execution_mode,
        }
    }
}
/// Whether a hook list holds no hooks at all.
fn is_empty<S: Storage, K: HookList<S>>(_: &K) -> bool {
    K::EMPTY
}

/// A built app: a world with its inferred component set `S`, and every handler.
pub struct App<S: Storage, L = Nil> {
    pub world: World<S>,
    listeners: L,
    execution_mode: ExecutionMode,
}

impl App<()> {
    /// Start composing an app. Components are inferred from its systems.
    #[allow(clippy::new_ret_no_self)]
    pub fn new() -> AppBuilder {
        AppBuilder {
            listeners: Nil,
            hooks: Nil,
            execution_mode: ExecutionMode::Parallel,
            components: PhantomData,
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
    }
}
