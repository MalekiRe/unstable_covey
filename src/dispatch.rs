use std::marker::PhantomData;

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
    const HAS_PARALLEL: bool = false;
    fn handle(&mut self, event: &E, state: &mut S);
    #[inline]
    fn handle_in<const SERIAL: bool>(&mut self, event: &E, state: &mut S) {
        self.handle(event, state);
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

impl<E, S, P: System<S, Output = ()>> Handle<E, S> for Run<P> {
    const HAS_PARALLEL: bool = P::HAS_PARALLEL;
    #[inline]
    fn handle(&mut self, _: &E, state: &mut S) {
        let _ = self.0.run(state);
    }
    #[inline]
    fn handle_in<const SERIAL: bool>(&mut self, _: &E, state: &mut S) {
        const {
            assert!(P::VALID, "invalid system access");
        }
        unsafe {
            if SERIAL {
                let _ = self.0.run_serial_unchecked(state);
            } else {
                let _ = self.0.run_unchecked(state);
            }
        }
    }
}

pub trait MaybeHandle<E: 'static, S> {
    const HAS_PARALLEL: bool;
    fn maybe_handle(&mut self, event: &E, state: &mut S);
    fn maybe_handle_in<const SERIAL: bool>(&mut self, event: &E, state: &mut S);
}

// The essential specialization: unequal event types do nothing.
impl<E: 'static, S, H> MaybeHandle<E, S> for H {
    default const HAS_PARALLEL: bool = false;
    #[inline]
    default fn maybe_handle(&mut self, _: &E, _: &mut S) {}
    #[inline]
    default fn maybe_handle_in<const SERIAL: bool>(&mut self, _: &E, _: &mut S) {}
}

impl<E: 'static, S, H: Handle<E, S>> MaybeHandle<E, S> for On<E, H> {
    const HAS_PARALLEL: bool = H::HAS_PARALLEL;
    #[inline]
    fn maybe_handle(&mut self, event: &E, state: &mut S) {
        self.handler.handle(event, state);
    }
    #[inline]
    fn maybe_handle_in<const SERIAL: bool>(&mut self, event: &E, state: &mut S) {
        self.handler.handle_in::<SERIAL>(event, state);
    }
}

pub trait Dispatch<E: 'static, S> {
    const HAS_PARALLEL: bool = false;
    fn dispatch(&mut self, event: &E, state: &mut S);
    fn dispatch_in<const SERIAL: bool>(&mut self, event: &E, state: &mut S);
}

impl<E: 'static, S> Dispatch<E, S> for Nil {
    #[inline]
    fn dispatch(&mut self, _: &E, _: &mut S) {}
    #[inline]
    fn dispatch_in<const SERIAL: bool>(&mut self, _: &E, _: &mut S) {}
}

impl<E: 'static, S, H: MaybeHandle<E, S>, T: Dispatch<E, S>> Dispatch<E, S> for Cons<H, T> {
    const HAS_PARALLEL: bool = H::HAS_PARALLEL || T::HAS_PARALLEL;
    #[inline]
    fn dispatch(&mut self, event: &E, state: &mut S) {
        self.1.dispatch(event, state);
        self.0.maybe_handle(event, state);
    }
    #[inline]
    fn dispatch_in<const SERIAL: bool>(&mut self, event: &E, state: &mut S) {
        self.1.dispatch_in::<SERIAL>(event, state);
        self.0.maybe_handle_in::<SERIAL>(event, state);
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
pub struct Bus<L = Nil>(L, ExecutionMode);

impl Bus {
    pub fn new() -> Self {
        Self(Nil, ExecutionMode::Parallel)
    }
}

impl<L> Bus<L> {
    pub fn with_execution_mode(mut self, mode: ExecutionMode) -> Self {
        self.1 = mode;
        self
    }
    pub fn set_execution_mode(&mut self, mode: ExecutionMode) {
        self.1 = mode;
    }
    pub fn install<P: Install<(), L>>(self, plugin: P) -> Bus<P::Output> {
        Bus(plugin.install(self.0), self.1)
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
        if self.1.serial(L::HAS_PARALLEL) {
            self.0.dispatch_in::<true>(event, &mut ());
        } else {
            self.0.dispatch_in::<false>(event, &mut ());
        }
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

    #[inline]
    pub fn emit<E: 'static>(&mut self, event: &E)
    where
        L: Dispatch<E, World<S>>,
    {
        if self.execution_mode.serial(L::HAS_PARALLEL) {
            self.listeners.dispatch_in::<true>(event, &mut self.world);
        } else {
            self.listeners.dispatch_in::<false>(event, &mut self.world);
        }
    }
}
