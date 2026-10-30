//! Labels and access descriptions are compiled into constants. Execution follows
//! concrete Rust continuations; only potential cross-worker joins have state.
use crate::{
    access::{Access, Conflict, Same},
    executor::{Context, Continue, Fields, First, Join, Project, Second},
    plan::{self, IntoSystems, System},
};
use std::marker::PhantomData;

pub struct Dependency<L>(PhantomData<fn() -> L>);
pub struct Node<L, F, D = ()> {
    system: F,
    marker: PhantomData<fn() -> (L, D)>,
}
pub fn node<L: 'static, F>(_: L, system: F) -> Node<L, F> {
    Node {
        system,
        marker: PhantomData,
    }
}
impl<L, F, D> Node<L, F, D> {
    /// Order after a uniquely labelled prerequisite. Missing/duplicate labels
    /// and cycles are compile-time errors; repeated edges are harmless.
    pub fn after<P: 'static>(self, _: P) -> Node<L, F, (D, Dependency<P>)> {
        Node {
            system: self.system,
            marker: PhantomData,
        }
    }
}
pub struct GraphConfig<T>(T);
pub fn ready<T>(nodes: T) -> GraphConfig<T> {
    GraphConfig(nodes)
}
#[doc(hidden)]
pub struct Entry<L, F, D> {
    system: F,
    marker: PhantomData<fn() -> (L, D)>,
}
#[doc(hidden)]
pub struct Pair<A, B>(A, B);
#[doc(hidden)]
pub trait IntoNodes<S, M> {
    type Nodes;
    fn into_nodes(self) -> Self::Nodes;
}
impl<S, L, F: IntoSystems<S, M>, M, D> IntoNodes<S, M> for Node<L, F, D> {
    type Nodes = Entry<L, F::Systems, D>;
    fn into_nodes(self) -> Self::Nodes {
        Entry {
            system: self.system.into_systems(),
            marker: PhantomData,
        }
    }
}
impl<S, A: IntoNodes<S, AM>, B: IntoNodes<S, BM>, AM, BM> IntoNodes<S, (AM, BM)> for (A, B) {
    type Nodes = Pair<A::Nodes, B::Nodes>;
    fn into_nodes(self) -> Self::Nodes {
        Pair(self.0.into_nodes(), self.1.into_nodes())
    }
}
impl<S, A: IntoNodes<S, AM>, B: IntoNodes<S, BM>, C: IntoNodes<S, CM>, AM, BM, CM>
    IntoNodes<S, (AM, BM, CM)> for (A, B, C)
{
    type Nodes = Pair<A::Nodes, Pair<B::Nodes, C::Nodes>>;
    fn into_nodes(self) -> Self::Nodes {
        Pair(
            self.0.into_nodes(),
            Pair(self.1.into_nodes(), self.2.into_nodes()),
        )
    }
}
impl<
    S,
    A: IntoNodes<S, AM>,
    B: IntoNodes<S, BM>,
    C: IntoNodes<S, CM>,
    D: IntoNodes<S, DM>,
    AM,
    BM,
    CM,
    DM,
> IntoNodes<S, (AM, BM, CM, DM)> for (A, B, C, D)
{
    type Nodes = Pair<A::Nodes, Pair<B::Nodes, Pair<C::Nodes, D::Nodes>>>;
    fn into_nodes(self) -> Self::Nodes {
        Pair(
            self.0.into_nodes(),
            Pair(
                self.1.into_nodes(),
                Pair(self.2.into_nodes(), self.3.into_nodes()),
            ),
        )
    }
}
trait Labels<L: 'static> {
    const COUNT: usize;
}
impl<L: 'static, O: Same<L>, F, D> Labels<L> for Entry<O, F, D> {
    const COUNT: usize = O::VALUE as usize;
}
impl<L: 'static, A: Labels<L>, B: Labels<L>> Labels<L> for Pair<A, B> {
    const COUNT: usize = A::COUNT + B::COUNT;
}
trait Depends<L: 'static> {
    const VALUE: bool;
}
impl<L: 'static> Depends<L> for () {
    const VALUE: bool = false;
}
impl<L: 'static, D: Depends<L>, P: Same<L>> Depends<L> for (D, Dependency<P>) {
    const VALUE: bool = D::VALUE || P::VALUE;
}
trait ValidDependencies<All> {
    const VALID: bool;
}
impl<All> ValidDependencies<All> for () {
    const VALID: bool = true;
}
impl<All: Labels<L>, L: 'static, D: ValidDependencies<All>> ValidDependencies<All>
    for (D, Dependency<L>)
{
    const VALID: bool = D::VALID && All::COUNT == 1;
}

#[doc(hidden)]
pub enum Bits {
    Leaf(bool, bool),
    Pair(&'static Bits, &'static Bits, usize),
}
impl Bits {
    const fn get(&self, index: usize) -> (bool, bool) {
        match self {
            Self::Leaf(dependency, conflict) => {
                assert!(index == 0);
                (*dependency, *conflict)
            }
            Self::Pair(left, right, split) => {
                if index < *split {
                    left.get(index)
                } else {
                    right.get(index - *split)
                }
            }
        }
    }
}
#[doc(hidden)]
pub enum Rows {
    Leaf(&'static Bits, usize),
    Pair(&'static Rows, &'static Rows, usize),
}
impl Rows {
    const fn row(&self, index: usize) -> (&Bits, usize) {
        match self {
            Self::Leaf(bits, weight) => {
                assert!(index == 0);
                (bits, *weight)
            }
            Self::Pair(left, right, split) => {
                if index < *split {
                    left.row(index)
                } else {
                    right.row(index - *split)
                }
            }
        }
    }
}
trait Compare<S, D, A> {
    const LEN: usize;
    const BITS: Bits;
}
impl<S, L: 'static, F: System<S>, OD, D: Depends<L>, A: Conflict<F::Access>> Compare<S, D, A>
    for Entry<L, F, OD>
{
    const LEN: usize = 1;
    const BITS: Bits = Bits::Leaf(D::VALUE, A::VALUE);
}
impl<S, D, A, L: Compare<S, D, A>, R: Compare<S, D, A>> Compare<S, D, A> for Pair<L, R> {
    const LEN: usize = L::LEN + R::LEN;
    const BITS: Bits = Bits::Pair(&L::BITS, &R::BITS, L::LEN);
}
#[doc(hidden)]
pub trait BuildRows<S, All> {
    const VALID: bool;
    const COUNT: usize;
    const ROWS: Rows;
}
impl<
    S,
    L: 'static,
    F: System<S>,
    D: ValidDependencies<All>,
    All: Labels<L> + Compare<S, D, F::Access>,
> BuildRows<S, All> for Entry<L, F, D>
{
    const VALID: bool = F::VALID && D::VALID && All::COUNT == 1;
    const COUNT: usize = 1;
    const ROWS: Rows = Rows::Leaf(&All::BITS, F::WEIGHT);
}
impl<S, All, A: BuildRows<S, All>, B: BuildRows<S, All>> BuildRows<S, All> for Pair<A, B> {
    const VALID: bool = A::VALID && B::VALID;
    const COUNT: usize = A::COUNT + B::COUNT;
    const ROWS: Rows = Rows::Pair(&A::ROWS, &B::ROWS, A::COUNT);
}

// All allocations below are compiler constants, not initialization/frame work.
struct Topology<const N: usize, const M: usize> {
    edges: [bool; M],
    rank: [usize; N],
    incoming: [usize; N],
    outgoing: [usize; N],
    join: [usize; N],
    first: [usize; N],
    work: [usize; N],
    root: usize,
    tails: usize,
    joins: usize,
    parallel: bool,
}
impl<const N: usize, const M: usize> Topology<N, M> {
    const fn compile(rows: &Rows) -> Self {
        let mut edges = [false; M];
        let mut remaining = [0; N];
        let mut order = [0; N];
        let mut used = 0;
        let mut node = 0;
        while node < N {
            let mut parent = 0;
            while parent < N {
                edges[node * N + parent] = rows.row(node).0.get(parent).0;
                remaining[node] += edges[node * N + parent] as usize;
                parent += 1;
            }
            if remaining[node] == 0 {
                order[used] = node;
                used += 1;
            }
            node += 1;
        }
        let mut cursor = 0;
        while cursor < used {
            node = 0;
            while node < N {
                if edges[node * N + order[cursor]] {
                    remaining[node] -= 1;
                    if remaining[node] == 0 {
                        order[used] = node;
                        used += 1;
                    }
                }
                node += 1;
            }
            cursor += 1;
        }
        assert!(used == N, "dependency graph contains a cycle");
        let mut rank = [0; N];
        cursor = 0;
        while cursor < N {
            node = order[cursor];
            rank[node] = cursor;
            let mut before = 0;
            while before < cursor {
                let parent = order[before];
                if rows.row(node).0.get(parent).1 || rows.row(parent).0.get(node).1 {
                    edges[node * N + parent] = true;
                }
                before += 1;
            }
            cursor += 1;
        }
        // Transitive reduction happens once in const evaluation, never at run time.
        let mut ancestors = edges;
        cursor = 0;
        while cursor < N {
            node = order[cursor];
            let mut parent = 0;
            while parent < N {
                if edges[node * N + parent] {
                    let mut ancestor = 0;
                    while ancestor < N {
                        ancestors[node * N + ancestor] |= ancestors[parent * N + ancestor];
                        ancestor += 1;
                    }
                }
                parent += 1;
            }
            cursor += 1;
        }
        let original = edges;
        node = 0;
        while node < N {
            let mut parent = 0;
            while parent < N {
                if original[node * N + parent] {
                    let mut via = 0;
                    while via < N {
                        if original[node * N + via] && ancestors[via * N + parent] {
                            edges[node * N + parent] = false;
                            break;
                        }
                        via += 1;
                    }
                }
                parent += 1;
            }
            node += 1;
        }
        let mut incoming = [0; N];
        let mut outgoing = [0; N];
        let mut roots = 0;
        let mut parallel = false;
        let mut join = [usize::MAX; N];
        let mut joins = 0;
        node = 0;
        while node < N {
            let mut parent = 0;
            while parent < N {
                incoming[node] += edges[node * N + parent] as usize;
                outgoing[parent] += edges[node * N + parent] as usize;
                parent += 1;
            }
            if incoming[node] == 0 {
                roots += 1;
            }
            if incoming[node] > 1 {
                join[node] = joins;
                joins += 1;
            }
            node += 1;
        }
        let mut work = [0; N];
        let mut first = [usize::MAX; N];
        let mut tails = 0;
        cursor = N;
        while cursor > 0 {
            cursor -= 1;
            node = order[cursor];
            parallel |= outgoing[node] > 1;
            let mut child = 0;
            let mut longest = 0;
            while child < N {
                if edges[child * N + node] {
                    if first[node] == usize::MAX || work[child] > work[first[node]] {
                        first[node] = child;
                    }
                    // A published branch owns its chain only until the next join.
                    if incoming[child] == 1 && work[child] > longest {
                        longest = work[child];
                    }
                }
                child += 1;
            }
            if first[node] == usize::MAX {
                tails += 1;
            }
            work[node] = rows.row(node).1.saturating_add(longest);
        }
        let mut root = order[0];
        node = 0;
        while node < N {
            if incoming[node] == 0 && work[node] > work[root] {
                root = node;
            }
            node += 1;
        }
        Self {
            edges,
            rank,
            incoming,
            outgoing,
            join,
            first,
            work,
            root,
            tails,
            joins,
            parallel: parallel || roots > 1,
        }
    }
}
#[doc(hidden)]
pub struct View {
    edges: &'static [bool],
    rank: &'static [usize],
    incoming: &'static [usize],
    outgoing: &'static [usize],
    join: &'static [usize],
    first: &'static [usize],
    work: &'static [usize],
    root: usize,
    tails: usize,
    joins: usize,
    parallel: bool,
}
struct Compiled<S, N>(PhantomData<fn() -> (S, N)>);
impl<S, N: Nodes<S> + BuildRows<S, N>> Compiled<S, N> {
    const TABLE: Topology<{ core::gca!(N::LEN) }, { core::gca!(N::CELLS) }> =
        Topology::compile(&N::ROWS);
    const VIEW: View = View {
        edges: &Self::TABLE.edges,
        rank: &Self::TABLE.rank,
        incoming: &Self::TABLE.incoming,
        outgoing: &Self::TABLE.outgoing,
        join: &Self::TABLE.join,
        first: &Self::TABLE.first,
        work: &Self::TABLE.work,
        root: Self::TABLE.root,
        tails: Self::TABLE.tails,
        joins: Self::TABLE.joins,
        parallel: Self::TABLE.parallel,
    };
}

#[doc(hidden)]
pub trait Index {
    const VALUE: usize;
}
#[doc(hidden)]
pub struct Zero;
impl Index for Zero {
    const VALUE: usize = 0;
}
struct Offset<I, A, S>(PhantomData<fn() -> (I, A)>, PhantomData<fn() -> S>);
impl<I: Index, A: Nodes<S>, S> Index for Offset<I, A, S> {
    const VALUE: usize = I::VALUE + A::LEN;
}

#[doc(hidden)]
pub trait Spec<S>: Sized {
    type Nodes: Nodes<S>;
    const VIEW: View;
    /// # Safety
    /// The graph allocation is live. Only raw child fields may be projected.
    unsafe fn nodes(this: *mut Self) -> *mut Self::Nodes;
    /// # Safety
    /// Index is a compiled join index in this live graph.
    unsafe fn join(this: *mut Self, index: usize) -> *const Join;
}
struct Contents<P, S>(PhantomData<fn() -> (P, S)>);
impl<R, P: Project<R>, S> Project<R> for Contents<P, S>
where
    P::Target: Spec<S>,
{
    type Target = <P::Target as Spec<S>>::Nodes;
    const KEY: usize = P::KEY;
    unsafe fn get(root: *mut R) -> *mut Self::Target {
        unsafe { P::Target::nodes(P::get(root)) }
    }
}
struct Value<P>(PhantomData<P>);
impl<R, P: Project<R, Target = Entry<L, F, D>>, L, F, D> Project<R> for Value<P> {
    type Target = F;
    const KEY: usize = P::KEY;
    unsafe fn get(root: *mut R) -> *mut F {
        unsafe { &raw mut (*P::get(root)).system }
    }
}
impl<A, B> Fields for Pair<A, B> {
    type Left = A;
    type Right = B;
    unsafe fn left(this: *mut Self) -> *mut A {
        unsafe { &raw mut (*this).0 }
    }
    unsafe fn right(this: *mut Self) -> *mut B {
        unsafe { &raw mut (*this).1 }
    }
}
struct Finished<G, I, K>(PhantomData<(G, I, K)>);
impl<S: Send + Sync, R: Send, G: Project<R>, I: Index, K: Continue<S, R>> Continue<S, R>
    for Finished<G, I, K>
where
    G::Target: Spec<S>,
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, executed: bool) {
        if const {
            G::Target::VIEW.outgoing[I::VALUE] > 1
                || (G::Target::VIEW.first[I::VALUE] != usize::MAX
                    && G::Target::VIEW.incoming[G::Target::VIEW.first[I::VALUE]] > 1)
        } && context.cancelled()
        {
            return;
        }
        if const { <G::Target as Spec<S>>::VIEW.first[I::VALUE] == usize::MAX } {
            if const { <G::Target as Spec<S>>::VIEW.tails == 1 } {
                unsafe {
                    K::resume(context, executed);
                }
            } else {
                let join =
                    unsafe { &*G::Target::join(G::get(context.nodes), G::Target::VIEW.joins) };
                if let Some(executed) = join.arrive(executed) {
                    unsafe {
                        K::resume(context, executed);
                    }
                }
            }
        } else {
            // Publish side paths before entering the retained concrete successor.
            unsafe {
                <G::Target as Spec<S>>::Nodes::release::<R, Contents<G, S>, G, Zero, I, K, true>(
                    context, executed,
                );
                <G::Target as Spec<S>>::Nodes::release::<R, Contents<G, S>, G, Zero, I, K, false>(
                    context, executed,
                );
            }
        }
    }
}
mod sealed {
    pub trait Sealed {}
    impl<L, F, D> Sealed for super::Entry<L, F, D> {}
    impl<A, B> Sealed for super::Pair<A, B> {}
}
#[doc(hidden)]
pub trait Nodes<S>: sealed::Sealed + Sized {
    type Access: Access;
    const LEN: usize;
    const CELLS: usize = Self::LEN * Self::LEN;
    const WEIGHT: usize;
    const HAS_PARALLEL: bool;
    /// # Safety
    /// Paths select disjoint graph entries and compiled dependencies grant access.
    unsafe fn roots<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R>,
        I: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
        G::Target: Spec<S>;
    /// # Safety
    /// The predecessor has finished; each edge releases exactly once.
    unsafe fn release<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R>,
        I: Index,
        From: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
        G::Target: Spec<S>;
    /// # Safety
    /// The graph owns the world exclusively and Stage is a compiled order position.
    unsafe fn serial<G: Spec<S>, I: Index, Stage: Index>(nodes: *mut Self, world: *mut S) -> bool;
    /// # Safety
    /// The graph and world are exclusive; this tree supplies the finite stage list.
    unsafe fn ordered<G: Spec<S>, Stage: Index>(nodes: *mut G::Nodes, world: *mut S) -> bool;
}
impl<S, L, F: System<S, Output = ()>, D> Nodes<S> for Entry<L, F, D> {
    type Access = F::Access;
    const LEN: usize = 1;
    const WEIGHT: usize = F::WEIGHT;
    const HAS_PARALLEL: bool = F::HAS_PARALLEL;
    #[inline(always)]
    unsafe fn roots<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R>,
        I: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
        G::Target: Spec<S>,
    {
        if const {
            G::Target::VIEW.incoming[I::VALUE] == 0 && (G::Target::VIEW.root != I::VALUE) == REMOTE
        } {
            if context.cancelled() {
                return;
            }
            if !REMOTE
                || !context
                    .offer::<Value<P>, Finished<G, I, K>>(executed, G::Target::VIEW.work[I::VALUE])
            {
                unsafe {
                    F::enter::<R, Value<P>, Finished<G, I, K>>(context, executed);
                }
            }
        }
    }
    #[inline(always)]
    unsafe fn release<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R>,
        I: Index,
        From: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
        G::Target: Spec<S>,
    {
        if const {
            G::Target::VIEW.edges[I::VALUE * G::Target::VIEW.incoming.len() + From::VALUE]
                && (G::Target::VIEW.first[From::VALUE] != I::VALUE) == REMOTE
        } {
            let executed = if const { G::Target::VIEW.incoming[I::VALUE] > 1 } {
                let join = unsafe {
                    &*G::Target::join(G::get(context.nodes), G::Target::VIEW.join[I::VALUE])
                };
                match join.arrive(executed) {
                    Some(executed) => executed,
                    None => return,
                }
            } else {
                executed
            };
            if !REMOTE
                || !context
                    .offer::<Value<P>, Finished<G, I, K>>(executed, G::Target::VIEW.work[I::VALUE])
            {
                unsafe {
                    F::enter::<R, Value<P>, Finished<G, I, K>>(context, executed);
                }
            }
        }
    }
    #[inline(always)]
    unsafe fn serial<G: Spec<S>, I: Index, Stage: Index>(nodes: *mut Self, world: *mut S) -> bool {
        if const { G::VIEW.rank[I::VALUE] == Stage::VALUE } {
            unsafe { (*nodes).system.run_serial_unchecked(world) }.is_some()
        } else {
            false
        }
    }
    #[inline(always)]
    unsafe fn ordered<G: Spec<S>, Stage: Index>(nodes: *mut G::Nodes, world: *mut S) -> bool {
        unsafe { G::Nodes::serial::<G, Zero, Stage>(nodes, world) }
    }
}
impl<S, A: Nodes<S>, B: Nodes<S>> Nodes<S> for Pair<A, B> {
    type Access = (A::Access, B::Access);
    const LEN: usize = A::LEN + B::LEN;
    const WEIGHT: usize = A::WEIGHT + B::WEIGHT;
    const HAS_PARALLEL: bool = A::HAS_PARALLEL || B::HAS_PARALLEL;
    #[inline(always)]
    unsafe fn roots<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R>,
        I: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
        G::Target: Spec<S>,
    {
        unsafe {
            A::roots::<R, First<P>, G, I, K, REMOTE>(context, executed);
            B::roots::<R, Second<P>, G, Offset<I, A, S>, K, REMOTE>(context, executed);
        }
    }
    #[inline(always)]
    unsafe fn release<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R>,
        I: Index,
        From: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        executed: bool,
    ) where
        S: Send + Sync,
        G::Target: Spec<S>,
    {
        unsafe {
            A::release::<R, First<P>, G, I, From, K, REMOTE>(context, executed);
            B::release::<R, Second<P>, G, Offset<I, A, S>, From, K, REMOTE>(context, executed);
        }
    }
    #[inline(always)]
    unsafe fn serial<G: Spec<S>, I: Index, Stage: Index>(nodes: *mut Self, world: *mut S) -> bool {
        unsafe {
            A::serial::<G, I, Stage>(&raw mut (*nodes).0, world)
                | B::serial::<G, Offset<I, A, S>, Stage>(&raw mut (*nodes).1, world)
        }
    }
    #[inline(always)]
    unsafe fn ordered<G: Spec<S>, Stage: Index>(nodes: *mut G::Nodes, world: *mut S) -> bool {
        unsafe {
            A::ordered::<G, Stage>(nodes, world)
                | B::ordered::<G, Offset<Stage, A, S>>(nodes, world)
        }
    }
}

/// Concrete systems and cross-worker join storage. No runtime topology or order.
pub struct ReadyGraph<N> {
    nodes: N,
    joins: Box<[(Join, usize)]>,
}
impl<N> plan::sealed::Sealed for ReadyGraph<N> {}
#[doc(hidden)]
pub struct ReadyMarker<M>(PhantomData<M>);
impl<S: Send + Sync, T: IntoNodes<S, M>, M> IntoSystems<S, ReadyMarker<M>> for GraphConfig<T>
where
    T::Nodes: Nodes<S> + BuildRows<S, T::Nodes> + Send,
{
    type Systems = ReadyGraph<T::Nodes>;
    fn into_systems(self) -> Self::Systems {
        const {
            assert!(
                <T::Nodes as BuildRows<S, T::Nodes>>::VALID,
                "invalid graph: duplicate/missing labels or invalid system access"
            );
        }
        let view = &Compiled::<S, T::Nodes>::VIEW;
        let mut joins = Vec::with_capacity(view.joins + usize::from(view.tails > 1));
        for &incoming in view.incoming {
            if incoming > 1 {
                joins.push((Join::default(), incoming));
            }
        }
        if view.tails > 1 {
            joins.push((Join::default(), view.tails));
        }
        ReadyGraph {
            nodes: self.0.into_nodes(),
            joins: joins.into_boxed_slice(),
        }
    }
}
impl<S, N: Nodes<S> + BuildRows<S, N>> Spec<S> for ReadyGraph<N> {
    type Nodes = N;
    const VIEW: View = Compiled::<S, N>::VIEW;
    unsafe fn nodes(this: *mut Self) -> *mut N {
        unsafe { &raw mut (*this).nodes }
    }
    unsafe fn join(this: *mut Self, index: usize) -> *const Join {
        unsafe { &raw const (*(*this).joins.as_ptr().add(index)).0 }
    }
}
impl<S: Send + Sync, N: Nodes<S> + BuildRows<S, N> + Send> System<S> for ReadyGraph<N> {
    type Access = N::Access;
    type Output = ();
    const VALID: bool = <N as BuildRows<S, N>>::VALID;
    const HAS_PARALLEL: bool = Compiled::<S, N>::VIEW.parallel || N::HAS_PARALLEL;
    const WEIGHT: usize = N::WEIGHT;
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        executed: bool,
    ) {
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
            return;
        }
        for (join, arrivals) in unsafe { &(*P::get(context.nodes)).joins }.iter() {
            join.reset(*arrivals);
        }
        unsafe {
            N::roots::<R, Contents<P, S>, P, Zero, K, true>(context, executed);
            N::roots::<R, Contents<P, S>, P, Zero, K, false>(context, executed);
        }
    }
    unsafe fn run_unchecked(&mut self, world: *mut S) -> Option<()> {
        unsafe { crate::executor::execute(self, world) }
    }
    #[inline]
    unsafe fn run_serial_unchecked(&mut self, world: *mut S) -> Option<()> {
        unsafe { N::ordered::<Self, Zero>(&raw mut self.nodes, world) }.then_some(())
    }
}
