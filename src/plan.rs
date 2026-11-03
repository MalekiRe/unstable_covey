use crate::access::{Access, Conflict, Exclusive, ValidAccess};
use crate::change::{ChangeWindow, SystemWorld};
use crate::executor::{
    self, Context, Continue, Done, Fields, First, Flow, Join, Outlined, Project, Root, Second,
    Tail, Then,
};
use crate::param::{Param, ParamShape};
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// `Serial` runs every nested group on the calling thread without workers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExecutionMode {
    #[default]
    Parallel,
    Serial,
}

/// What a system accesses and returns, independent of any world type. Apps
/// infer their component vocabulary from these access types.
pub trait Shape: sealed::Sealed + Sized {
    type Access: Access;
    type Output;
    #[doc(hidden)]
    const HAS_PARALLEL: bool = false;
    #[doc(hidden)]
    const WEIGHT: usize = 1;
}

/// A system runnable against world type `S`. Implementations are sealed.
pub trait System<S>: Shape {
    const VALID: bool;
    /// Run this system inside an invocation, then its continuation.
    /// # Safety
    /// The path uniquely owns this system and all its declared world accesses.
    #[doc(hidden)]
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync,
    {
        unsafe { direct::<S, R, Self, P, K>(context, flow) }
    }
    /// Returns None if parameters or a condition skip execution. Groups attempt
    /// every child and return Some(()) if any child executes. `clock` is the
    /// latest change tick on this path; writers advance it.
    /// # Safety
    /// VALID must hold; world must be live and its declared access must not
    /// conflict with any other active borrow. State and captures are exclusive.
    unsafe fn run_unchecked(&mut self, world: *mut S, clock: &mut u64) -> Option<Self::Output>;
    /// Move deferred work (commands) recorded by the last runs to the world's
    /// queue, in system order.
    #[doc(hidden)]
    fn defer(&mut self, _queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
    }

    /// Run once against an exclusive world, then apply its commands.
    #[inline]
    fn run(&mut self, world: &mut S) -> Option<Self::Output>
    where
        S: SystemWorld,
    {
        let output = run_now(self, world);
        self.defer(unsafe { &mut *S::queue(world) });
        world.flush();
        output
    }
    /// Run every nested group directly on this thread.
    #[inline]
    fn run_serial(&mut self, world: &mut S) -> Option<Self::Output>
    where
        S: SystemWorld,
    {
        executor::serially(|| self.run(world))
    }
}
/// Run without applying deferred work; dispatch applies it when a frame ends.
#[inline]
pub(crate) fn run_now<S: SystemWorld, T: System<S>>(
    system: &mut T,
    world: &mut S,
) -> Option<T::Output> {
    const {
        assert!(
            T::VALID,
            "invalid system: missing/ambiguous storage or conflicting access"
        );
    }
    // SAFETY: compile-time validation plus exclusive ownership of the world.
    unsafe {
        let mut clock = S::clock(world);
        let output = Entry::entry(system, world, &mut clock);
        S::set_clock(world, clock);
        output
    }
}
#[inline(always)]
pub(crate) unsafe fn direct<S, R, T: System<S>, P: Project<R, Target = T>, K: Continue<S, R>>(
    context: &Context<'_, S, R>,
    flow: Flow,
) -> Tail<S, R> {
    let mut clock = flow.clock();
    let ran = unsafe { (*P::get(context.nodes)).run_unchecked(context.world, &mut clock) };
    unsafe { K::resume(context, Flow::new(clock, flow.ran() | ran.is_some())) }
}
/// Run a group root as one invocation, advancing the caller's clock.
#[inline]
pub(crate) unsafe fn invocation<S: Send + Sync, G: System<S> + Send>(
    group: &mut G,
    world: *mut S,
    clock: &mut u64,
) -> Option<()> {
    let root = std::ptr::from_mut(group);
    let start = Flow::new(*clock, false);
    let end = unsafe {
        executor::execute::<S, G, Then<Root, Done>>(root, world, std::ptr::null(), start)
    };
    *clock = end.clock();
    end.ran().then_some(())
}

/// Lowers a whole thread-safe tree into one invocation. Trees with non-Send
/// systems still run, with each parallel group as its own invocation.
trait Entry<S>: System<S> {
    unsafe fn entry(&mut self, world: *mut S, clock: &mut u64) -> Option<Self::Output>;
}
impl<S, T: System<S>> Entry<S> for T {
    #[inline]
    default unsafe fn entry(&mut self, world: *mut S, clock: &mut u64) -> Option<Self::Output> {
        unsafe { self.run_unchecked(world, clock) }
    }
}
impl<S: Send + Sync, T: System<S, Output = ()> + Send> Entry<S> for T {
    #[inline]
    unsafe fn entry(&mut self, world: *mut S, clock: &mut u64) -> Option<()> {
        if const { !Self::HAS_PARALLEL } || !executor::parallel(Self::WEIGHT) {
            return unsafe { self.run_unchecked(world, clock) };
        }
        unsafe { invocation(self, world, clock) }
    }
}

pub struct FunctionSystem<F, P, L, R> {
    function: F,
    local: L,
    last_run: u64,
    last_world: u64,
    marker: PhantomData<fn() -> (P, R)>,
}
impl<F, P, L, R> sealed::Sealed for FunctionSystem<F, P, L, R> {}

/// Converts a function or closure into a system without naming a world.
pub trait IntoFunction<M, R> {
    type Function: Shape<Output = R>;
    fn into_function(self) -> Self::Function;
}

macro_rules! accesses {
    () => { () };
    ($p:ident $(, $rest:ident)*) => { ($p::Access, accesses!($($rest),*)) };
}
macro_rules! function {
    ($($p:ident $i:tt),*) => {
        impl<F, R, $($p: ParamShape),*> IntoFunction<fn($($p),*) -> R, R> for F
        where F: FnMut($($p),*) -> R + for<'a> FnMut($($p::Item<'a>),*) -> R {
            type Function = FunctionSystem<F, ($($p,)*), ($($p::State,)*), R>;
            fn into_function(self) -> Self::Function {
                FunctionSystem { function: self, local: Default::default(), last_run: 0, last_world: u64::MAX, marker: PhantomData }
            }
        }
        impl<F, R, $($p: ParamShape),*> Shape for FunctionSystem<F, ($($p,)*), ($($p::State,)*), R> {
            type Access = accesses!($($p),*);
            type Output = R;
        }
        impl<S: SystemWorld, F, R, $($p: Param<S>),*> System<S>
            for FunctionSystem<F, ($($p,)*), ($($p::State,)*), R>
        where F: for<'a> FnMut($($p::Item<'a>),*) -> R,
              accesses!($($p),*): ValidAccess {
            const VALID: bool = <Self::Access as ValidAccess>::VALID $(&& $p::VALID)*;
            #[inline]
            unsafe fn run_unchecked(&mut self, _world: *mut S, clock: &mut u64) -> Option<R> {
                let world_id = unsafe { S::world_id(_world) };
                // Lamport clock: writers issue the next tick on their own path.
                let now = if Self::Access::WRITES { crate::change::next(*clock) } else { *clock };
                let window = ChangeWindow { last: if world_id == self.last_world { self.last_run } else { 0 }, now };
                // Fetch every parameter before committing the cursor. Invalid
                // Single parameters therefore preserve the previous window.
                let _args = ($(unsafe { $p::fetch(_world, &mut self.local.$i, window) }?,)*);
                let result = (self.function)($(_args.$i),*);
                self.last_run = window.now;
                self.last_world = world_id;
                *clock = window.now;
                Some(result)
            }
            fn defer(&mut self, _queue: &mut S::Queue) {
                $($p::defer(&mut self.local.$i, _queue);)*
            }
        }
    };
}
function!();
function!(A 0);
function!(A 0, B 1);
function!(A 0, B 1, C 2);
function!(A 0, B 1, C 2, D 3);

pub struct Sequential<A, B>(A, B);
/// The flag records whether B was published for the current invocation.
pub struct Concurrent<A, B>(A, B, Join, AtomicBool);
pub struct Conditional<P, C>(P, C);
impl<A, B> sealed::Sealed for Sequential<A, B> {}
impl<A, B> sealed::Sealed for Concurrent<A, B> {}
impl<A, B> sealed::Sealed for Conditional<A, B> {}
impl<A, B> Sequential<A, B> {
    fn new(a: A, b: B) -> Self {
        Self(a, b)
    }
}
impl<A, B> Concurrent<A, B> {
    fn new(a: A, b: B) -> Self {
        Self(a, b, Join::default(), AtomicBool::new(false))
    }
}

macro_rules! fields {
    ($name:ident) => {
        impl<A, B> Fields for $name<A, B> {
            type Left = A;
            type Right = B;
            unsafe fn left(this: *mut Self) -> *mut A {
                unsafe { &raw mut (*this).0 }
            }
            unsafe fn right(this: *mut Self) -> *mut B {
                unsafe { &raw mut (*this).1 }
            }
        }
    };
}
fields!(Sequential);
fields!(Concurrent);
fields!(Conditional);

impl<A: Shape<Output = ()>, B: Shape<Output = ()>> Shape for Sequential<A, B> {
    type Access = (A::Access, B::Access);
    type Output = ();
    const HAS_PARALLEL: bool = A::HAS_PARALLEL || B::HAS_PARALLEL;
    const WEIGHT: usize = A::WEIGHT + B::WEIGHT;
}
impl<S, A: System<S, Output = ()>, B: System<S, Output = ()>> System<S> for Sequential<A, B> {
    const VALID: bool = A::VALID && B::VALID;
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync,
    {
        // A constant guard keeps rustc from instantiating the parallel arm for
        // plain sequences, whose chains would otherwise hit the recursion limit.
        if const { !Self::HAS_PARALLEL } {
            return unsafe { direct::<S, R, Self, P, K>(context, flow) };
        }
        unsafe { A::enter::<R, First<P>, Then<Second<P>, K>>(context, flow) }
    }
    #[inline]
    unsafe fn run_unchecked(&mut self, world: *mut S, clock: &mut u64) -> Option<()> {
        // SAFETY: the first child finishes before the second begins.
        let left = unsafe { self.0.run_unchecked(world, clock) };
        let right = unsafe { self.1.run_unchecked(world, clock) };
        left.or(right)
    }
    fn defer(&mut self, queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
        self.0.defer(queue);
        self.1.defer(queue);
    }
}

/// A's continuation: arrive at the join if B was published, else run B here.
struct Fork<P, K>(PhantomData<(P, K)>);
/// B's continuation, shared by the published and local paths.
struct Joined<P, K>(PhantomData<(P, K)>);
impl<S, R, A, B, P, K> Continue<S, R> for Fork<P, K>
where
    S: Send + Sync,
    R: Send,
    A: System<S, Output = ()> + Send,
    B: System<S, Output = ()> + Send,
    A::Access: Conflict<B::Access>,
    P: Project<R, Target = Concurrent<A, B>>,
    K: Continue<S, R>,
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
        // Borrow only the coordination fields; children may be borrowed elsewhere.
        if unsafe { &(*P::get(context.nodes)).3 }.load(Ordering::Relaxed) {
            unsafe { Joined::<P, K>::resume(context, flow) }
        } else {
            // B runs after A here, so it continues A's clock.
            Tail::to::<Then<Second<P>, Joined<P, K>>>(flow)
        }
    }
}
impl<S, R, A, B, P, K> Continue<S, R> for Joined<P, K>
where
    P: Project<R, Target = Concurrent<A, B>>,
    K: Continue<S, R>,
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
        let node = unsafe { P::get(context.nodes) };
        if !unsafe { &(*node).3 }.load(Ordering::Relaxed) {
            Tail::to::<K>(flow)
        } else if let Some(flow) = unsafe { &(*node).2 }.arrive(flow) {
            Tail::to::<K>(flow)
        } else {
            Tail::DONE
        }
    }
}

impl<A: Shape<Output = ()>, B: Shape<Output = ()>> Shape for Concurrent<A, B> {
    type Access = (A::Access, B::Access);
    type Output = ();
    const HAS_PARALLEL: bool = true;
    const WEIGHT: usize = A::WEIGHT + B::WEIGHT;
}
impl<S: Send + Sync, A, B> System<S> for Concurrent<A, B>
where
    A: System<S, Output = ()> + Send,
    B: System<S, Output = ()> + Send,
    A::Access: Conflict<B::Access>,
{
    const VALID: bool = A::VALID && B::VALID && !<A::Access as Conflict<B::Access>>::VALUE;
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R> {
        let node = unsafe { P::get(context.nodes) };
        let published = unsafe { &(*node).3 };
        unsafe { &(*node).2 }.reset(2);
        // Set before publication: the thief's continuation reads it.
        published.store(true, Ordering::Relaxed);
        let branch = Flow::new(flow.clock(), false);
        if !context.offer::<Then<Second<P>, Joined<P, K>>>(branch, B::WEIGHT, Second::<P>::KEY) {
            published.store(false, Ordering::Relaxed);
        }
        unsafe { A::enter::<R, First<P>, Fork<P, K>>(context, flow) }
    }
    unsafe fn run_unchecked(&mut self, world: *mut S, clock: &mut u64) -> Option<()> {
        if executor::parallel(Self::WEIGHT) {
            return unsafe { invocation(self, world, clock) };
        }
        let left = unsafe { self.0.run_unchecked(world, clock) };
        let right = unsafe { self.1.run_unchecked(world, clock) };
        left.or(right)
    }
    fn defer(&mut self, queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
        self.0.defer(queue);
        self.1.defer(queue);
    }
}

impl<P: Shape<Output = ()>, C: Shape<Output = bool>> Shape for Conditional<P, C> {
    type Access = (P::Access, C::Access);
    type Output = ();
    const HAS_PARALLEL: bool = P::HAS_PARALLEL;
    const WEIGHT: usize = P::WEIGHT + C::WEIGHT;
}
impl<S, P: System<S, Output = ()>, C: System<S, Output = bool>> System<S> for Conditional<P, C> {
    const VALID: bool = P::VALID && C::VALID && !C::Access::WRITES;
    #[inline(always)]
    unsafe fn enter<R: Send, Path: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync,
    {
        if const { !Self::HAS_PARALLEL } {
            return unsafe { direct::<S, R, Self, Path, K>(context, flow) };
        }
        let condition = unsafe { &mut *Second::<Path>::get(context.nodes) };
        let mut clock = flow.clock();
        let run = unsafe { condition.run_unchecked(context.world, &mut clock) } == Some(true);
        let flow = Flow::new(clock, flow.ran());
        // Both outcomes share one out-of-line K.
        if run {
            unsafe { P::enter::<R, First<Path>, Outlined<K>>(context, flow) }
        } else {
            Tail::to::<K>(flow)
        }
    }
    #[inline]
    unsafe fn run_unchecked(&mut self, world: *mut S, clock: &mut u64) -> Option<()> {
        // SAFETY: condition finishes before body; the combined access is exposed
        // to enclosing parallel groups. Skipping does not reset either state.
        if unsafe { self.1.run_unchecked(world, clock) } == Some(true) {
            unsafe { self.0.run_unchecked(world, clock) }
        } else {
            None
        }
    }
    fn defer(&mut self, queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
        self.1.defer(queue);
        self.0.defer(queue);
    }
}

/// Exclusive world access, for structural changes. Name the world as `World<_>`
/// in the app body; a plugin can be generic over it (`World<S>` with `S: Has<T>`).
pub struct ExclusiveSystem<F>(F);
pub fn exclusive<F>(function: F) -> ExclusiveSystem<F> {
    ExclusiveSystem(function)
}
impl<F> sealed::Sealed for ExclusiveSystem<F> {}
impl<F> Shape for ExclusiveSystem<F> {
    type Access = Exclusive;
    type Output = ();
}
impl<S: SystemWorld, F: FnMut(&mut S)> System<S> for ExclusiveSystem<F> {
    const VALID: bool = true;
    unsafe fn run_unchecked(&mut self, world: *mut S, clock: &mut u64) -> Option<()> {
        // SAFETY: Exclusive access conflicts with all other world accesses, so
        // the world's own clock may follow this path's clock meanwhile.
        unsafe {
            S::set_clock(world, *clock);
            (self.0)(&mut *world);
            *clock = S::clock(world);
        }
        Some(())
    }
}

pub struct FunctionMarker<M>(PhantomData<M>);
pub struct Ready;
pub struct Chained<T>(T);
pub struct Parallel<T>(T);
pub struct RunIf<T, C>(T, C);

/// Converts system configurations without naming a world; the app checks them
/// against its inferred world when built.
pub trait IntoSystems<M>: Sized {
    type Systems: Shape<Output = ()>;
    fn into_systems(self) -> Self::Systems;
}
impl<F, M> IntoSystems<FunctionMarker<M>> for F
where
    F: IntoFunction<M, ()>,
{
    type Systems = F::Function;
    fn into_systems(self) -> Self::Systems {
        self.into_function()
    }
}
impl<F> IntoSystems<Ready> for ExclusiveSystem<F> {
    type Systems = Self;
    fn into_systems(self) -> Self {
        self
    }
}

/// Composition stays concrete; tuples default to registration-order execution.
pub trait IntoSystemConfigs: Sized {
    fn chain(self) -> Chained<Self> {
        Chained(self)
    }
    fn parallel(self) -> Parallel<Self> {
        Parallel(self)
    }
    fn run_if<C>(self, condition: C) -> RunIf<Self, C> {
        RunIf(self, condition)
    }
}
impl<T> IntoSystemConfigs for T {}

impl<T: IntoSystems<M>, M> IntoSystems<Chained<M>> for Chained<T> {
    type Systems = T::Systems;
    fn into_systems(self) -> Self::Systems {
        self.0.into_systems()
    }
}
impl<T, C, M, CM> IntoSystems<RunIf<M, CM>> for RunIf<T, C>
where
    T: IntoSystems<M>,
    C: IntoFunction<CM, bool>,
{
    type Systems = Conditional<T::Systems, C::Function>;
    fn into_systems(self) -> Self::Systems {
        Conditional(self.0.into_systems(), self.1.into_function())
    }
}

macro_rules! fold_type {
    ($node:ident; $a:ident) => { $a::Systems };
    ($node:ident; $a:ident, $($rest:ident),+) => { $node<$a::Systems, fold_type!($node; $($rest),+)> };
}
macro_rules! fold_value {
    ($node:ident; $value:ident; $i:tt) => { $value.$i.into_systems() };
    ($node:ident; $value:ident; $i:tt, $($rest:tt),+) => {
        $node::new($value.$i.into_systems(), fold_value!($node; $value; $($rest),+))
    };
}
macro_rules! tuple {
    ($($t:ident $m:ident $i:tt),+) => {
        impl<$($t: IntoSystems<$m>, $m),+> IntoSystems<($($m,)+)> for ($($t,)+) {
            type Systems = fold_type!(Sequential; $($t),+);
            fn into_systems(self) -> Self::Systems { fold_value!(Sequential; self; $($i),+) }
        }
        impl<$($t: IntoSystems<$m>, $m),+> IntoSystems<Parallel<($($m,)+)>> for Parallel<($($t,)+)> {
            type Systems = fold_type!(Concurrent; $($t),+);
            fn into_systems(self) -> Self::Systems {
                let values = self.0;
                fold_value!(Concurrent; values; $($i),+)
            }
        }
    };
}
tuple!(A MA 0, B MB 1);
tuple!(A MA 0, B MB 1, C MC 2);
tuple!(A MA 0, B MB 1, C MC 2, D MD 3);
