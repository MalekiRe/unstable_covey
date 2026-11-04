use crate::access::{Access, Conflict, Exclusive, ValidAccess};
use crate::change::{ChangeWindow, SystemWorld};
use crate::executor::{
    self, Context, Continue, Done, Fields, First, Flow, Join, Outlined, Project, Root, Second,
    Tail, Then,
};
use crate::init::Init;
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

/// What a schedule hands every system registered under it, as the system's
/// first parameter. Implemented by input families, not by the inputs.
pub trait Input: 'static {
    type Item<'a>: Copy;
}
/// Families whose systems take the input as their first parameter.
#[doc(hidden)]
pub trait Takes: Input {}
/// No input: systems take parameters only.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoInput;
impl Input for NoInput {
    type Item<'a> = NoInput;
}
/// The emitted event by reference: systems start with `&E`.
pub struct Ref<E>(PhantomData<fn() -> E>);
impl<E: 'static> Input for Ref<E> {
    type Item<'a> = &'a E;
}
impl<E: 'static> Takes for Ref<E> {}
/// Read a system's input from the erased pointer frames carry.
/// # Safety
/// `input` points at a live `I::Item`.
#[inline(always)]
pub(crate) unsafe fn read<'a, I: Input>(input: *const ()) -> I::Item<'a> {
    unsafe { *input.cast::<I::Item<'a>>() }
}

/// What a system accesses, returns, and receives, independent of any world
/// type. Apps infer their component vocabulary from these access types.
pub trait Shape: sealed::Sealed + Sized {
    type Access: Access;
    type Output;
    /// The input family every system in this tree takes first.
    type In: Input;
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
    /// The path uniquely owns this system and all its declared world accesses;
    /// the context's input points at a live `<Self::In as Input>::Item`.
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
    /// `input` points at a live `<Self::In as Input>::Item`.
    unsafe fn run_unchecked(
        &mut self,
        input: *const (),
        world: *mut S,
        clock: &mut u64,
    ) -> Option<Self::Output>;
    /// Move deferred work (commands) recorded by the last runs to the world's
    /// queue, in system order.
    #[doc(hidden)]
    fn defer(&mut self, _queue: &mut <S as SystemWorld>::Queue)
    where
        S: SystemWorld,
    {
    }

    /// Run once with `input` against an exclusive world, then apply its
    /// commands. Systems without input take `NoInput`.
    #[inline]
    fn run(&mut self, input: <Self::In as Input>::Item<'_>, world: &mut S) -> Option<Self::Output>
    where
        S: SystemWorld,
    {
        // SAFETY: `input` is a live item of this system's input family.
        let output = unsafe { run_now(self, (&raw const input).cast(), world) };
        self.defer(unsafe { &mut *S::queue(world) });
        world.flush();
        output
    }
    /// `run`, with every nested group directly on this thread.
    #[inline]
    fn run_serial(
        &mut self,
        input: <Self::In as Input>::Item<'_>,
        world: &mut S,
    ) -> Option<Self::Output>
    where
        S: SystemWorld,
    {
        executor::serially(|| self.run(input, world))
    }
}
/// Run without applying deferred work; dispatch applies it when a frame ends.
/// # Safety
/// As `System::run_unchecked` for `input`.
#[inline]
pub(crate) unsafe fn run_now<S: SystemWorld, T: System<S>>(
    system: &mut T,
    input: *const (),
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
        let output = Entry::entry(system, input, world, &mut clock);
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
    let ran =
        unsafe { (*P::get(context.nodes)).run_unchecked(context.event, context.world, &mut clock) };
    unsafe { K::resume(context, Flow::new(clock, flow.ran() | ran.is_some())) }
}
/// Run a group root as one invocation, advancing the caller's clock.
#[inline]
pub(crate) unsafe fn invocation<S: Send + Sync, G: System<S> + Send>(
    group: &mut G,
    input: *const (),
    world: *mut S,
    clock: &mut u64,
) -> Option<()> {
    let root = std::ptr::from_mut(group);
    let start = Flow::new(*clock, false);
    let end = unsafe { executor::execute::<S, G, Then<Root, Done>>(root, world, input, start) };
    *clock = end.clock();
    end.ran().then_some(())
}

/// Lowers a whole thread-safe tree into one invocation. Trees with non-Send
/// systems still run, with each parallel group as its own invocation.
trait Entry<S>: System<S> {
    unsafe fn entry(
        &mut self,
        input: *const (),
        world: *mut S,
        clock: &mut u64,
    ) -> Option<Self::Output>;
}
impl<S, T: System<S>> Entry<S> for T {
    #[inline]
    default unsafe fn entry(
        &mut self,
        input: *const (),
        world: *mut S,
        clock: &mut u64,
    ) -> Option<Self::Output> {
        unsafe { self.run_unchecked(input, world, clock) }
    }
}
impl<S: Send + Sync, T: System<S, Output = ()> + Send> Entry<S> for T {
    #[inline]
    unsafe fn entry(&mut self, input: *const (), world: *mut S, clock: &mut u64) -> Option<()> {
        if const { !Self::HAS_PARALLEL } || !executor::parallel(Self::WEIGHT) {
            return unsafe { self.run_unchecked(input, world, clock) };
        }
        unsafe { invocation(self, input, world, clock) }
    }
}

/// A function or closure with typed parameters `P`, their states `L`, and the
/// input family `K` its first parameter receives (`NoInput`: no such parameter).
pub struct FunctionSystem<F, P, L, R, K = NoInput> {
    function: F,
    local: L,
    last_run: u64,
    last_world: u64,
    marker: Marker<P, R, K>,
}
/// Parameter, output, and input types, without owning any of them.
type Marker<P, R, K> = PhantomData<fn() -> (P, R, K)>;
impl<F, P, L, R, K> sealed::Sealed for FunctionSystem<F, P, L, R, K> {}
impl<F, P, L: Init, R, K> FunctionSystem<F, P, L, R, K> {
    fn new(function: F) -> Self {
        Self {
            function,
            local: L::init(),
            last_run: 0,
            last_world: u64::MAX,
            marker: PhantomData,
        }
    }
}

/// Converts a function or closure into a system without naming a world.
/// `K` is the schedule's input family; for anything but `NoInput` it must be
/// the function's first parameter.
#[diagnostic::on_unimplemented(
    message = "this function does not fit a schedule with input `{K}`",
    note = "every system under a label takes the label's input first: nothing for plain labels, `&E` for `Ref<E>`, `Trigger` for lifecycle labels; then up to 16 parameters"
)]
pub trait IntoFunction<K, M, R> {
    type Function: Shape<Output = R, In = K>;
    fn into_function(self) -> Self::Function;
}

macro_rules! accesses {
    () => { () };
    ($p:ident $(, $rest:ident)*) => { ($p::Access, accesses!($($rest),*)) };
}
pub(crate) use accesses;
/// Fetch every parameter, then call; shared by both function forms.
macro_rules! invoke {
    ($this:ident, $world:ident, $clock:ident, |$args:ident| $call:expr; $($p:ident $i:tt),*) => {{
        let world_id = unsafe { S::world_id($world) };
        // Lamport clock: writers issue the next tick on their own path.
        let now = if Self::Access::WRITES { crate::change::next(*$clock) } else { *$clock };
        let window = ChangeWindow { last: if world_id == $this.last_world { $this.last_run } else { 0 }, now };
        // Fetch every parameter before committing the cursor. Invalid
        // Single parameters therefore preserve the previous window.
        let $args = ($(unsafe { $p::fetch($world, &mut $this.local.$i, window) }?,)*);
        let result = $call;
        $this.last_run = window.now;
        $this.last_world = world_id;
        *$clock = window.now;
        Some(result)
    }};
}
macro_rules! function {
    ($($p:ident $i:tt),*) => {
        impl<F, R, $($p: ParamShape),*> IntoFunction<NoInput, fn($($p),*) -> R, R> for F
        where F: FnMut($($p),*) -> R + for<'a> FnMut($($p::Item<'a>),*) -> R {
            type Function = FunctionSystem<F, ($($p,)*), ($($p::State,)*), R, NoInput>;
            fn into_function(self) -> Self::Function {
                FunctionSystem::new(self)
            }
        }
        impl<K: Takes, F, R, I, $($p: ParamShape),*> IntoFunction<K, fn(I, $($p),*) -> R, R> for F
        where F: FnMut(I, $($p),*) -> R + for<'a> FnMut(K::Item<'a>, $($p::Item<'a>),*) -> R {
            type Function = FunctionSystem<F, ($($p,)*), ($($p::State,)*), R, K>;
            fn into_function(self) -> Self::Function {
                FunctionSystem::new(self)
            }
        }
        impl<F, R, K: Input, $($p: ParamShape),*> Shape for FunctionSystem<F, ($($p,)*), ($($p::State,)*), R, K> {
            type Access = accesses!($($p),*);
            type Output = R;
            type In = K;
        }
        impl<S: SystemWorld, F, R, $($p: Param<S>),*> System<S>
            for FunctionSystem<F, ($($p,)*), ($($p::State,)*), R, NoInput>
        where F: for<'a> FnMut($($p::Item<'a>),*) -> R,
              accesses!($($p),*): ValidAccess {
            const VALID: bool = <Self::Access as ValidAccess>::VALID $(&& $p::VALID)*;
            #[inline]
            unsafe fn run_unchecked(&mut self, _: *const (), _world: *mut S, clock: &mut u64) -> Option<R> {
                invoke!(self, _world, clock, |_args| (self.function)($(_args.$i),*); $($p $i),*)
            }
            fn defer(&mut self, _queue: &mut S::Queue) {
                $($p::defer(&mut self.local.$i, _queue);)*
            }
        }
        impl<S: SystemWorld, K: Takes, F, R, $($p: Param<S>),*> System<S>
            for FunctionSystem<F, ($($p,)*), ($($p::State,)*), R, K>
        where F: for<'a> FnMut(K::Item<'a>, $($p::Item<'a>),*) -> R,
              accesses!($($p),*): ValidAccess {
            const VALID: bool = <Self::Access as ValidAccess>::VALID $(&& $p::VALID)*;
            #[inline]
            unsafe fn run_unchecked(&mut self, input: *const (), _world: *mut S, clock: &mut u64) -> Option<R> {
                let input = unsafe { read::<K>(input) };
                invoke!(self, _world, clock, |_args| (self.function)(input, $(_args.$i),*); $($p $i),*)
            }
            fn defer(&mut self, _queue: &mut S::Queue) {
                $($p::defer(&mut self.local.$i, _queue);)*
            }
        }
    };
}
function!();
function!(P0 0);
function!(P0 0, P1 1);
function!(P0 0, P1 1, P2 2);
function!(P0 0, P1 1, P2 2, P3 3);
function!(P0 0, P1 1, P2 2, P3 3, P4 4);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11, P12 12);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11, P12 12, P13 13);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11, P12 12, P13 13, P14 14);
function!(P0 0, P1 1, P2 2, P3 3, P4 4, P5 5, P6 6, P7 7, P8 8, P9 9, P10 10, P11 11, P12 12, P13 13, P14 14, P15 15);

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

impl<A: Shape<Output = ()>, B: Shape<Output = (), In = A::In>> Shape for Sequential<A, B> {
    type Access = (A::Access, B::Access);
    type Output = ();
    type In = A::In;
    const HAS_PARALLEL: bool = A::HAS_PARALLEL || B::HAS_PARALLEL;
    const WEIGHT: usize = A::WEIGHT + B::WEIGHT;
}
impl<S, A: System<S, Output = ()>, B: System<S, Output = (), In = A::In>> System<S>
    for Sequential<A, B>
{
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
    unsafe fn run_unchecked(
        &mut self,
        input: *const (),
        world: *mut S,
        clock: &mut u64,
    ) -> Option<()> {
        // SAFETY: the first child finishes before the second begins.
        let left = unsafe { self.0.run_unchecked(input, world, clock) };
        let right = unsafe { self.1.run_unchecked(input, world, clock) };
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
    B: System<S, Output = (), In = A::In> + Send,
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

impl<A: Shape<Output = ()>, B: Shape<Output = (), In = A::In>> Shape for Concurrent<A, B> {
    type Access = (A::Access, B::Access);
    type Output = ();
    type In = A::In;
    const HAS_PARALLEL: bool = true;
    const WEIGHT: usize = A::WEIGHT + B::WEIGHT;
}
impl<S: Send + Sync, A, B> System<S> for Concurrent<A, B>
where
    A: System<S, Output = ()> + Send,
    B: System<S, Output = (), In = A::In> + Send,
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
    unsafe fn run_unchecked(
        &mut self,
        input: *const (),
        world: *mut S,
        clock: &mut u64,
    ) -> Option<()> {
        if executor::parallel(Self::WEIGHT) {
            return unsafe { invocation(self, input, world, clock) };
        }
        let left = unsafe { self.0.run_unchecked(input, world, clock) };
        let right = unsafe { self.1.run_unchecked(input, world, clock) };
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

impl<P: Shape<Output = ()>, C: Shape<Output = bool, In = P::In>> Shape for Conditional<P, C> {
    type Access = (P::Access, C::Access);
    type Output = ();
    type In = P::In;
    const HAS_PARALLEL: bool = P::HAS_PARALLEL;
    const WEIGHT: usize = P::WEIGHT + C::WEIGHT;
}
impl<S, P: System<S, Output = ()>, C: System<S, Output = bool, In = P::In>> System<S>
    for Conditional<P, C>
{
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
        let run = unsafe { condition.run_unchecked(context.event, context.world, &mut clock) }
            == Some(true);
        let flow = Flow::new(clock, flow.ran());
        // Both outcomes share one out-of-line K.
        if run {
            unsafe { P::enter::<R, First<Path>, Outlined<K>>(context, flow) }
        } else {
            Tail::to::<K>(flow)
        }
    }
    #[inline]
    unsafe fn run_unchecked(
        &mut self,
        input: *const (),
        world: *mut S,
        clock: &mut u64,
    ) -> Option<()> {
        // SAFETY: condition finishes before body; the combined access is exposed
        // to enclosing parallel groups. Skipping does not reset either state.
        if unsafe { self.1.run_unchecked(input, world, clock) } == Some(true) {
            unsafe { self.0.run_unchecked(input, world, clock) }
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
/// in the app body; a plugin can be generic over it (`World<S>` with `S: Stores<T>`).
/// Under a schedule with input, the input comes first: `|hit: &Damage, world|`.
pub struct ExclusiveSystem<F, K = NoInput>(F, PhantomData<fn() -> K>);
pub fn exclusive<F>(function: F) -> ExclusiveSystem<F> {
    ExclusiveSystem(function, PhantomData)
}
impl<F, K> sealed::Sealed for ExclusiveSystem<F, K> {}
impl<F, K: Input> Shape for ExclusiveSystem<F, K> {
    type Access = Exclusive;
    type Output = ();
    type In = K;
}
/// Run `body` with exclusive access on this path's clock.
/// # Safety
/// As `System::run_unchecked`; exclusive access conflicts with everything.
unsafe fn exclusively<S: SystemWorld>(world: *mut S, clock: &mut u64, body: impl FnOnce(&mut S)) {
    // SAFETY: the world's own clock may follow this path's clock meanwhile.
    unsafe {
        S::set_clock(world, *clock);
        body(&mut *world);
        *clock = S::clock(world);
    }
}
impl<S: SystemWorld, F: FnMut(&mut S)> System<S> for ExclusiveSystem<F, NoInput> {
    const VALID: bool = true;
    unsafe fn run_unchecked(&mut self, _: *const (), world: *mut S, clock: &mut u64) -> Option<()> {
        unsafe { exclusively(world, clock, &mut self.0) };
        Some(())
    }
}
impl<S: SystemWorld, K: Takes, F: for<'a> FnMut(K::Item<'a>, &mut S)> System<S>
    for ExclusiveSystem<F, K>
{
    const VALID: bool = true;
    unsafe fn run_unchecked(
        &mut self,
        input: *const (),
        world: *mut S,
        clock: &mut u64,
    ) -> Option<()> {
        let input = unsafe { read::<K>(input) };
        unsafe { exclusively(world, clock, |world| (self.0)(input, world)) };
        Some(())
    }
}

pub struct FunctionMarker<M>(PhantomData<M>);
pub struct Ready;
pub struct Chained<T>(T);
pub struct Parallel<T>(T);
pub struct RunIf<T, C>(T, C);

/// Converts system configurations for input family `K` without naming a
/// world; the app checks them against its inferred world when built.
#[diagnostic::on_unimplemented(
    message = "these systems do not fit a schedule with input `{K}`",
    note = "every system under a label takes the label's input first: nothing for plain labels, `&E` for `Ref<E>`, `Trigger` for lifecycle labels"
)]
pub trait IntoSystems<K: Input, M>: Sized {
    type Systems: Shape<Output = (), In = K>;
    fn into_systems(self) -> Self::Systems;
}
impl<K: Input, F, M> IntoSystems<K, FunctionMarker<M>> for F
where
    F: IntoFunction<K, M, ()>,
{
    type Systems = F::Function;
    fn into_systems(self) -> Self::Systems {
        self.into_function()
    }
}
impl<K: Input, F> IntoSystems<K, Ready> for ExclusiveSystem<F> {
    type Systems = ExclusiveSystem<F, K>;
    fn into_systems(self) -> Self::Systems {
        ExclusiveSystem(self.0, PhantomData)
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

impl<K: Input, T: IntoSystems<K, M>, M> IntoSystems<K, Chained<M>> for Chained<T> {
    type Systems = T::Systems;
    fn into_systems(self) -> Self::Systems {
        self.0.into_systems()
    }
}
impl<K: Input, T, C, M, CM> IntoSystems<K, RunIf<M, CM>> for RunIf<T, C>
where
    T: IntoSystems<K, M>,
    C: IntoFunction<K, CM, bool>,
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
        impl<K: Input, $($t: IntoSystems<K, $m>, $m),+> IntoSystems<K, ($($m,)+)> for ($($t,)+) {
            type Systems = fold_type!(Sequential; $($t),+);
            fn into_systems(self) -> Self::Systems { fold_value!(Sequential; self; $($i),+) }
        }
        impl<K: Input, $($t: IntoSystems<K, $m>, $m),+> IntoSystems<K, Parallel<($($m,)+)>> for Parallel<($($t,)+)> {
            type Systems = fold_type!(Concurrent; $($t),+);
            fn into_systems(self) -> Self::Systems {
                let values = self.0;
                fold_value!(Concurrent; values; $($i),+)
            }
        }
    };
}
tuple!(T0 M0 0, T1 M1 1);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7, T8 M8 8);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7, T8 M8 8, T9 M9 9);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7, T8 M8 8, T9 M9 9, T10 M10 10);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7, T8 M8 8, T9 M9 9, T10 M10 10, T11 M11 11);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7, T8 M8 8, T9 M9 9, T10 M10 10, T11 M11 11, T12 M12 12);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7, T8 M8 8, T9 M9 9, T10 M10 10, T11 M11 11, T12 M12 12, T13 M13 13);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7, T8 M8 8, T9 M9 9, T10 M10 10, T11 M11 11, T12 M12 12, T13 M13 13, T14 M14 14);
tuple!(T0 M0 0, T1 M1 1, T2 M2 2, T3 M3 3, T4 M4 4, T5 M5 5, T6 M6 6, T7 M7 7, T8 M8 8, T9 M9 9, T10 M10 10, T11 M11 11, T12 M12 12, T13 M13 13, T14 M14 14, T15 M15 15);
