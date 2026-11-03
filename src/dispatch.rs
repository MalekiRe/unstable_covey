use std::marker::PhantomData;

use crate::change::SystemWorld;
use crate::executor::{self, Context, Continue, Done, Fields, First, Flow, Inner, Project, Root};
use crate::executor::{Second, Tail, Within, serially};
use crate::lookup::ComponentLookup;
use crate::plan::{ExecutionMode, Shape};
use crate::vocab::{Declaration, Insert, Uses};
use crate::{Components, IntoSystems, Storage, System, World};

#[derive(Default)]
pub struct Nil;
pub struct Cons<H, T>(H, T);

/// Keep E visible across crate boundaries; only hide the callback implementation.
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
    fn handle(&mut self, event: &E, state: &mut S);
    /// Handle within a frame invocation, then continue with K. The handler has
    /// exclusive world access, so the world clock follows this path meanwhile.
    /// # Safety
    /// Every earlier handler has finished; the context event has type E.
    #[doc(hidden)]
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync + SystemWorld,
    {
        unsafe {
            S::set_clock(context.world, flow.clock());
            (*P::get(context.nodes)).handle(&*context.event.cast::<E>(), &mut *context.world);
            K::resume(context, Flow::new(S::clock(context.world), flow.ran()))
        }
    }
}

/// A payload handler with exclusive world access.
pub struct Callback<F>(F);
impl<E, S, F: FnMut(&E, &mut S)> Handle<E, S> for Callback<F> {
    #[inline]
    fn handle(&mut self, event: &E, state: &mut S) {
        (self.0)(event, state);
    }
}

/// An installable payload handler. Annotate the world as `&mut World<_>`, or
/// make a plugin generic over it.
pub fn system<E: 'static, F>(callback: F) -> On<E, Callback<F>> {
    On {
        handler: Callback(callback),
        marker: PhantomData,
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

impl<E, S: SystemWorld, P: System<S, Output = ()>> Handle<E, S> for Run<P> {
    const HAS_PARALLEL: bool = P::HAS_PARALLEL;
    const WEIGHT: usize = P::WEIGHT;
    const VALID: bool = P::VALID;
    #[inline]
    fn handle(&mut self, _: &E, state: &mut S) {
        let _ = self.0.run(state);
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
    fn maybe_handle(&mut self, event: &E, state: &mut S);
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
    default fn maybe_handle(&mut self, _: &E, _: &mut S) {}
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
    fn maybe_handle(&mut self, event: &E, state: &mut S) {
        self.handler.handle(event, state);
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

/// Registration-order dispatch. A frame runs every matching handler as one
/// sequence of continuations: no barrier between registrations.
pub trait Dispatch<E: 'static, S> {
    #[doc(hidden)]
    const HAS_PARALLEL: bool = false;
    #[doc(hidden)]
    const WEIGHT: usize = 0;
    fn dispatch(&mut self, event: &E, state: &mut S);
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
    fn dispatch(&mut self, _: &E, _: &mut S) {}
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
    fn dispatch(&mut self, event: &E, state: &mut S) {
        self.1.dispatch(event, state);
        self.0.maybe_handle(event, state);
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
    fn emit(&mut self, event: &E, state: &mut S);
}
impl<E: 'static, S, L: Dispatch<E, S>> Emit<E, S> for L {
    #[inline]
    default fn emit(&mut self, event: &E, state: &mut S) {
        self.dispatch(event, state);
    }
}
impl<E, S, L> Emit<E, S> for L
where
    E: 'static + Sync,
    S: Send + Sync + SystemWorld,
    L: Dispatch<E, S> + Send,
{
    #[inline]
    fn emit(&mut self, event: &E, state: &mut S) {
        if const { !L::HAS_PARALLEL } || !executor::parallel(L::WEIGHT) {
            return self.dispatch(event, state);
        }
        let event = std::ptr::from_ref(event).cast();
        unsafe {
            let start = Flow::new(S::clock(state), false);
            let end = executor::execute::<S, L, Frame<E, Done>>(self, state, event, start);
            S::set_clock(state, end.clock());
        }
    }
}

/// What a handler's systems access, independent of any world type.
#[doc(hidden)]
pub trait Handler {
    type Access;
}
impl<F> Handler for Callback<F> {
    type Access = ();
}
impl<P: Shape> Handler for Run<P> {
    type Access = P::Access;
}

/// Type-transforming installation. Tuples install their left fragment first.
/// `Components` is `C` extended with every component the fragment's systems use.
pub trait Install<C, L> {
    type Components: Storage;
    type Output;
    fn install(self, listeners: L) -> Self::Output;
}
impl<E: 'static, H: Handler<Access: Uses<C>>, C, L> Install<C, L> for On<E, H> {
    type Components = <H::Access as Uses<C>>::Out;
    type Output = Cons<Self, L>;
    fn install(self, listeners: L) -> Self::Output {
        Cons(self, listeners)
    }
}
impl<C, L, A: Install<C, L>, B: Install<A::Components, A::Output>> Install<C, L> for (A, B) {
    type Components = B::Components;
    type Output = B::Output;
    fn install(self, listeners: L) -> Self::Output {
        self.1.install(self.0.install(listeners))
    }
}

/// Every handler in a listener list runs against world `W`. Checked once at
/// `build`, which also fixes the world type of callbacks written as `World<_>`.
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

/// Composes plugins and systems; `build` creates a world storing exactly the
/// components they use. `C` is that inferred component set.
pub struct AppBuilder<C = (), L = Nil> {
    listeners: L,
    execution_mode: ExecutionMode,
    components: PhantomData<fn() -> C>,
}

impl<C: Storage, L> AppBuilder<C, L> {
    pub fn with_execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }
    pub fn install<P: Install<C, L>>(self, plugin: P) -> AppBuilder<P::Components, P::Output> {
        AppBuilder {
            listeners: plugin.install(self.listeners),
            execution_mode: self.execution_mode,
            components: PhantomData,
        }
    }
    /// A payload handler with exclusive world access. Annotate the world as
    /// `world: &mut World<_>`; it is inferred at `build`.
    pub fn on<E: 'static, F>(self, callback: F) -> AppBuilder<C, Cons<On<E, Callback<F>>, L>> {
        self.install(system(callback))
    }
    /// A zero-sized event type can serve as an open-ended schedule label.
    /// Plans ignore the payload; use `on` when a system needs event data.
    #[allow(clippy::type_complexity)]
    pub fn add_systems<E: 'static, P: IntoSystems<M>, M>(
        self,
        _: E,
        systems: P,
    ) -> AppBuilder<
        <<P::Systems as Shape>::Access as Uses<C>>::Out,
        Cons<Scheduled<E, P::Systems>, L>,
    >
    where
        <P::Systems as Shape>::Access: Uses<C>,
    {
        self.install(On {
            handler: Run(systems.into_systems()),
            marker: PhantomData,
        })
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
    pub fn register<T: 'static>(self) -> AppBuilder<<C as Insert<Components<T>>>::Out, L>
    where
        C: Insert<Components<T>>,
    {
        AppBuilder {
            listeners: self.listeners,
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
    pub fn storage<D: Declaration>(self) -> AppBuilder<<C as Insert<D>>::Out, L>
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
            execution_mode: self.execution_mode,
            components: PhantomData,
        }
    }
    /// Create the world. Every handler is checked against it here.
    pub fn build(self) -> App<C, L>
    where
        L: Fits<World<C>>,
    {
        const {
            assert!(
                L::VALID,
                "invalid system: missing/ambiguous storage or conflicting access"
            );
        }
        App {
            world: World::default(),
            listeners: self.listeners,
            execution_mode: self.execution_mode,
        }
    }
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
            execution_mode: ExecutionMode::Parallel,
            components: PhantomData,
        }
    }
}

impl<S: Storage, L> App<S, L> {
    pub fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.execution_mode = mode;
    }
    /// Runs every handler registered for E, in registration order.
    #[inline]
    pub fn emit<E: 'static>(&mut self, event: &E)
    where
        L: Dispatch<E, World<S>>,
    {
        match self.execution_mode {
            ExecutionMode::Parallel => self.listeners.emit(event, &mut self.world),
            ExecutionMode::Serial => serially(|| self.listeners.dispatch(event, &mut self.world)),
        }
    }
}
