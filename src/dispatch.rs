use std::marker::PhantomData;

use crate::change::SystemWorld;
use crate::executor::{self, Context, Continue, Done, Fields, First, Flow, Inner, Project, Root};
use crate::executor::{Second, Tail, Within, serially};
use crate::plan::ExecutionMode;
use crate::{IntoSystems, Storage, System, World};

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

impl<E, S, F: FnMut(&E, &mut S)> Handle<E, S> for F {
    #[inline]
    fn handle(&mut self, event: &E, state: &mut S) {
        self(event, state);
    }
}

pub struct Listener<F>(F);
pub type Subscribe<E, F> = On<E, Listener<F>>;

impl<E, S, F: FnMut(&E)> Handle<E, S> for Listener<F> {
    #[inline]
    fn handle(&mut self, event: &E, _: &mut S) {
        (self.0)(event);
    }
}

pub fn subscribe<E: 'static, F: FnMut(&E)>(callback: F) -> Subscribe<E, F> {
    On {
        handler: Listener(callback),
        marker: PhantomData,
    }
}

pub fn system<E: 'static, S, F: FnMut(&E, &mut S)>(callback: F) -> On<E, F> {
    On {
        handler: callback,
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

/// Type-transforming installation. Tuples install their left fragment first.
pub trait Install<S, L> {
    type Output;
    fn install(self, listeners: L) -> Self::Output;
}

impl<E: 'static, H: Handle<E, S>, S, L> Install<S, L> for On<E, H> {
    type Output = Cons<Self, L>;
    fn install(self, listeners: L) -> Self::Output {
        Cons(self, listeners)
    }
}

impl<S, L, A: Install<S, L>, B: Install<S, A::Output>> Install<S, L> for (A, B) {
    type Output = B::Output;
    fn install(self, listeners: L) -> Self::Output {
        self.1.install(self.0.install(listeners))
    }
}

/// Event-only dispatcher; listeners and their captures live inline.
#[derive(Default)]
pub struct Bus<L = Nil>(L);

impl Bus {
    pub fn new() -> Self {
        Self(Nil)
    }
}

impl<L> Bus<L> {
    pub fn install<P: Install<(), L>>(self, plugin: P) -> Bus<P::Output> {
        Bus(plugin.install(self.0))
    }

    pub fn on<E: 'static, F: FnMut(&E)>(self, callback: F) -> Bus<Cons<Subscribe<E, F>, L>> {
        self.install(subscribe(callback))
    }

    /// E must not contain non-static borrowed data. The event itself is borrowed
    /// only for this call; closure captures may borrow local state.
    /// ```compile_fail
    /// use static_ecs::Bus;
    /// struct Borrowed<'a>(&'a str);
    /// let text = String::from("local");
    /// Bus::new().emit(&Borrowed(&text));
    /// ```
    #[inline]
    pub fn emit<E: 'static>(&mut self, event: &E)
    where
        L: Dispatch<E, ()>,
    {
        self.0.dispatch(event, &mut ());
    }
}

/// The same specialized dispatcher, with typed ECS storage passed to systems.
pub struct App<S: Storage, L = Nil> {
    pub world: World<S>,
    listeners: L,
    execution_mode: ExecutionMode,
}

impl<S: Storage> App<S> {
    pub fn new(world: World<S>) -> Self {
        Self {
            world,
            listeners: Nil,
            execution_mode: ExecutionMode::Parallel,
        }
    }
}

impl<S: Storage, L> App<S, L> {
    pub fn with_execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.execution_mode = mode;
        self
    }
    pub fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.execution_mode = mode;
    }
    /// Installation checks the handler's storage type, rather than silently
    /// accepting an incompatible system that would take the no-op fallback.
    /// ```compile_fail
    /// use static_ecs::{App, Components, World, system};
    /// struct Update;
    /// App::new(World::<Components<u32>>::default())
    ///     .install(system(|_: &Update, _: &mut World<Components<String>>| {}));
    /// ```
    pub fn install<P: Install<World<S>, L>>(self, plugin: P) -> App<S, P::Output> {
        App {
            world: self.world,
            listeners: plugin.install(self.listeners),
            execution_mode: self.execution_mode,
        }
    }

    pub fn on<E: 'static, F: FnMut(&E, &mut World<S>)>(
        self,
        callback: F,
    ) -> App<S, Cons<On<E, F>, L>> {
        self.install(system(callback))
    }

    /// A zero-sized event type can serve as an open-ended schedule label.
    /// Plans ignore the payload; use `on` when a system needs event data.
    pub fn add_systems<E: 'static, P: IntoSystems<World<S>, M>, M>(
        self,
        _: E,
        systems: P,
    ) -> App<S, Cons<Scheduled<E, P::Systems>, L>> {
        const {
            assert!(
                P::Systems::VALID,
                "invalid system: missing/ambiguous storage or conflicting access"
            );
        }
        self.install(On {
            handler: Run(systems.into_systems()),
            marker: PhantomData,
        })
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
