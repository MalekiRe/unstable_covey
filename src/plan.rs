use crate::access::{Access, Conflict, Exclusive, ValidAccess};
use crate::change::{ChangeWindow, SystemWorld};
use crate::executor::{Context, Continue, Fields, First, Join, Project, Second, Then};
use crate::param::Param;
use std::marker::PhantomData;

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Parallel remains the default; serial execution never initializes workers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExecutionMode {
    #[default]
    Parallel,
    Serial,
}
impl ExecutionMode {
    #[inline]
    pub(crate) fn serial(self, parallel: bool) -> bool {
        self == Self::Serial || !parallel || crate::executor::serial()
    }
}

/// A concrete, access-described system. Implementations are sealed.
pub trait System<S>: sealed::Sealed + Sized {
    type Access: Access;
    type Output;
    const VALID: bool;
    const HAS_PARALLEL: bool = false;
    #[doc(hidden)]
    const WEIGHT: usize = 1;
    /// Enter a concrete path and resume its concrete successor.
    /// # Safety
    /// The path uniquely owns this system and all its declared world accesses.
    #[doc(hidden)]
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
    {
        let result =
            unsafe { (&mut *P::get(context.nodes)).run_serial_unchecked(context.world) }.is_some();
        unsafe {
            K::resume(context, executed | result);
        }
    }
    /// Returns None if parameters or a condition skip execution. Groups attempt
    /// every child and return Some(()) if any child executes.
    /// # Safety
    /// VALID must hold; world must be live and its declared access must not
    /// conflict with any other active borrow. State and captures are exclusive.
    unsafe fn run_unchecked(&mut self, world: *mut S) -> Option<Self::Output>;

    /// Direct execution without worker dispatch or worker-count inspection.
    /// # Safety
    /// The same validity, access, and lifetime requirements as run_unchecked apply.
    #[inline]
    unsafe fn run_serial_unchecked(&mut self, world: *mut S) -> Option<Self::Output> {
        unsafe { self.run_unchecked(world) }
    }

    #[inline]
    fn run(&mut self, world: &mut S) -> Option<Self::Output> {
        self.run_with_mode(world, ExecutionMode::Parallel)
    }
    #[inline]
    fn run_serial(&mut self, world: &mut S) -> Option<Self::Output> {
        self.run_with_mode(world, ExecutionMode::Serial)
    }
    #[inline]
    fn run_with_mode(&mut self, world: &mut S, mode: ExecutionMode) -> Option<Self::Output> {
        const {
            assert!(
                Self::VALID,
                "invalid system: missing/ambiguous storage or conflicting access"
            );
        }
        // SAFETY: compile-time validation plus exclusive ownership of the world.
        unsafe {
            if mode.serial(Self::HAS_PARALLEL) {
                self.run_serial_unchecked(world)
            } else {
                self.run_unchecked(world)
            }
        }
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

pub trait IntoFunction<S, M, R> {
    type Function: System<S, Output = R>;
    fn into_function(self) -> Self::Function;
}

macro_rules! accesses {
    () => { () };
    ($p:ident $(, $rest:ident)*) => { ($p::Access, accesses!($($rest),*)) };
}
macro_rules! function {
    ($($p:ident $i:tt),*) => {
        impl<S: SystemWorld, F, R, $($p: Param<S>),*> IntoFunction<S, fn($($p),*) -> R, R> for F
        where F: FnMut($($p),*) -> R + for<'a> FnMut($($p::Item<'a>),*) -> R,
              accesses!($($p),*): ValidAccess {
            type Function = FunctionSystem<F, ($($p,)*), ($($p::State,)*), R>;
            fn into_function(self) -> Self::Function {
                FunctionSystem { function: self, local: Default::default(), last_run: 0, last_world: u64::MAX, marker: PhantomData }
            }
        }
        impl<S: SystemWorld, F, R, $($p: Param<S>),*> System<S>
            for FunctionSystem<F, ($($p,)*), ($($p::State,)*), R>
        where F: for<'a> FnMut($($p::Item<'a>),*) -> R,
              accesses!($($p),*): ValidAccess {
            type Access = accesses!($($p),*);
            type Output = R;
            const VALID: bool = <Self::Access as ValidAccess>::VALID $(&& $p::VALID)*;
            #[inline]
            unsafe fn run_unchecked(&mut self, _world: *mut S) -> Option<R> {
                let world_id = unsafe { S::world_id(_world) };
                let now = unsafe { if Self::Access::WRITES { S::next_tick(_world) } else { S::read_tick(_world) } };
                let window = ChangeWindow { last: if world_id == self.last_world { self.last_run } else { 0 }, now };
                // Fetch every parameter before committing the cursor. Invalid
                // Single parameters therefore preserve the previous window.
                let _args = ($(unsafe { $p::fetch(_world, &mut self.local.$i, window) }?,)*);
                let result = (self.function)($(_args.$i),*);
                self.last_run = window.now;
                self.last_world = world_id;
                Some(result)
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
pub struct Concurrent<A, B>(A, B, Join);
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
        Self(a, b, Join::default())
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

struct Joined<P, K>(PhantomData<(P, K)>);
impl<S, R, A, B, P, K> Continue<S, R> for Joined<P, K>
where
    P: Project<R, Target = Concurrent<A, B>>,
    K: Continue<S, R>,
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, executed: bool) {
        let join = unsafe { &(*P::get(context.nodes)).2 };
        if let Some(executed) = join.arrive(executed) {
            unsafe {
                K::resume(context, executed);
            }
        }
    }
}

impl<S, A: System<S, Output = ()>, B: System<S, Output = ()>> System<S> for Sequential<A, B> {
    type Access = (A::Access, B::Access);
    type Output = ();
    const VALID: bool = A::VALID && B::VALID;
    const HAS_PARALLEL: bool = A::HAS_PARALLEL || B::HAS_PARALLEL;
    const WEIGHT: usize = A::WEIGHT + B::WEIGHT;
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
    {
        if context.cancelled() {
            return;
        }
        if !Self::HAS_PARALLEL {
            let result =
                unsafe { (&mut *P::get(context.nodes)).run_serial_unchecked(context.world) }
                    .is_some();
            unsafe {
                K::resume(context, executed | result);
            }
        } else {
            unsafe {
                A::enter::<R, First<P>, Then<Second<P>, K>>(context, executed);
            }
        }
    }
    #[inline]
    unsafe fn run_unchecked(&mut self, world: *mut S) -> Option<()> {
        // SAFETY: the first invocation ends before the second begins.
        let (left, right) = unsafe { (self.0.run_unchecked(world), self.1.run_unchecked(world)) };
        left.or(right)
    }
    #[inline]
    unsafe fn run_serial_unchecked(&mut self, world: *mut S) -> Option<()> {
        let left = unsafe { self.0.run_serial_unchecked(world) };
        let right = unsafe { self.1.run_serial_unchecked(world) };
        left.or(right)
    }
}

impl<S: Send + Sync, A, B> System<S> for Concurrent<A, B>
where
    A: System<S, Output = ()> + Send,
    B: System<S, Output = ()> + Send,
    A::Access: Conflict<B::Access>,
{
    type Access = (A::Access, B::Access);
    type Output = ();
    const VALID: bool = A::VALID && B::VALID && !<A::Access as Conflict<B::Access>>::VALUE;
    const HAS_PARALLEL: bool = true;
    const WEIGHT: usize = A::WEIGHT + B::WEIGHT;
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        executed: bool,
    ) {
        if context.cancelled() {
            return;
        }
        if let Some(reservation) = context.reserve::<Second<P>>(B::WEIGHT) {
            unsafe { &(*P::get(context.nodes)).2 }.reset(2);
            reservation.publish::<Second<P>, Joined<P, K>>(false);
            unsafe {
                A::enter::<R, First<P>, Joined<P, K>>(context, executed);
            }
        } else {
            // No job, join counter, or shared completion operation on this path.
            unsafe {
                A::enter::<R, First<P>, Then<Second<P>, K>>(context, executed);
            }
        }
    }
    unsafe fn run_unchecked(&mut self, world: *mut S) -> Option<()> {
        unsafe { crate::executor::execute(self, world) }
    }
    #[inline]
    unsafe fn run_serial_unchecked(&mut self, world: *mut S) -> Option<()> {
        let left = unsafe { self.0.run_serial_unchecked(world) };
        let right = unsafe { self.1.run_serial_unchecked(world) };
        left.or(right)
    }
}

impl<S, P: System<S, Output = ()>, C: System<S, Output = bool>> System<S> for Conditional<P, C> {
    type Access = (P::Access, C::Access);
    type Output = ();
    const VALID: bool = P::VALID && C::VALID && !C::Access::WRITES;
    const HAS_PARALLEL: bool = P::HAS_PARALLEL || C::HAS_PARALLEL;
    const WEIGHT: usize = P::WEIGHT + C::WEIGHT;
    #[inline(always)]
    unsafe fn enter<R: Send, Path: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
    {
        if context.cancelled() {
            return;
        }
        let condition = unsafe { &mut *Second::<Path>::get(context.nodes) };
        if unsafe { condition.run_serial_unchecked(context.world) } == Some(true) {
            unsafe {
                P::enter::<R, First<Path>, K>(context, executed);
            }
        } else {
            unsafe {
                K::resume(context, executed);
            }
        }
    }
    unsafe fn run_unchecked(&mut self, world: *mut S) -> Option<()> {
        // SAFETY: condition finishes before body; the combined access is exposed
        // to enclosing parallel groups. Skipping does not reset either state.
        if unsafe { self.1.run_unchecked(world) } == Some(true) {
            unsafe { self.0.run_unchecked(world) }
        } else {
            None
        }
    }
    #[inline]
    unsafe fn run_serial_unchecked(&mut self, world: *mut S) -> Option<()> {
        if unsafe { self.1.run_serial_unchecked(world) } == Some(true) {
            unsafe { self.0.run_serial_unchecked(world) }
        } else {
            None
        }
    }
}

/// Explicit escape hatch for code needing the complete storage layout.
pub struct ExclusiveSystem<F>(F);
pub fn exclusive<F>(function: F) -> ExclusiveSystem<F> {
    ExclusiveSystem(function)
}
impl<F> sealed::Sealed for ExclusiveSystem<F> {}
impl<S, F: FnMut(&mut S)> System<S> for ExclusiveSystem<F> {
    type Access = Exclusive;
    type Output = ();
    const VALID: bool = true;
    unsafe fn run_unchecked(&mut self, world: *mut S) -> Option<()> {
        // SAFETY: Exclusive access conflicts with all other world accesses.
        (self.0)(unsafe { &mut *world });
        Some(())
    }
}

pub struct FunctionMarker<M>(PhantomData<M>);
pub struct Ready;
pub struct Chained<T>(T);
pub struct Parallel<T>(T);
pub struct RunIf<T, C>(T, C);

pub trait IntoSystems<S, M>: Sized {
    type Systems: System<S, Output = ()>;
    fn into_systems(self) -> Self::Systems;
}
impl<S, F, M> IntoSystems<S, FunctionMarker<M>> for F
where
    F: IntoFunction<S, M, ()>,
{
    type Systems = F::Function;
    fn into_systems(self) -> Self::Systems {
        self.into_function()
    }
}
impl<S, F: FnMut(&mut S)> IntoSystems<S, Ready> for ExclusiveSystem<F> {
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

impl<S, T: IntoSystems<S, M>, M> IntoSystems<S, Chained<M>> for Chained<T> {
    type Systems = T::Systems;
    fn into_systems(self) -> Self::Systems {
        self.0.into_systems()
    }
}
impl<S, T, C, M, CM> IntoSystems<S, RunIf<M, CM>> for RunIf<T, C>
where
    T: IntoSystems<S, M>,
    C: IntoFunction<S, CM, bool>,
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
        impl<S, $($t: IntoSystems<S, $m>, $m),+> IntoSystems<S, ($($m,)+)> for ($($t,)+) {
            type Systems = fold_type!(Sequential; $($t),+);
            fn into_systems(self) -> Self::Systems { fold_value!(Sequential; self; $($i),+) }
        }
        impl<S, $($t: IntoSystems<S, $m>, $m),+> IntoSystems<S, Parallel<($($m,)+)>> for Parallel<($($t,)+)>
        where fold_type!(Concurrent; $($t),+): System<S, Output = ()> {
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
