//! Labels and access descriptions are compiled into constants. Execution follows
//! concrete Rust continuations; only potential cross-worker joins have state.
use crate::{
    access::{Access, Conflict, Same},
    executor::{self, Context, Continue, Fields, First, Flow, Inner, Join, Project, Second},
    executor::{Tail, Then, Within, run},
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
/// Nodes per `ready(...)` graph; nest graphs as nodes for larger schedules.
pub const MAX_NODES: usize = 128;
const MAX: usize = MAX_NODES;

/// Compiler-evaluated graph structure; nothing here exists at run time.
#[doc(hidden)]
pub struct Topology {
    /// Transitively reduced: `edges[node * MAX + parent]`.
    edges: [bool; MAX * MAX],
    rank: [usize; MAX],
    incoming: [usize; MAX],
    outgoing: [usize; MAX],
    /// Join index of nodes with several predecessors.
    join: [usize; MAX],
    /// The successor continued inline: the heaviest single-predecessor child.
    local: [usize; MAX],
    /// Static weight of a node plus its inline chain.
    work: [usize; MAX],
    /// Arrivals per join, including the final join when several nodes end.
    arrivals: [usize; MAX],
    joins: usize,
    root: usize,
    tails: usize,
    parallel: bool,
}
impl Topology {
    const fn compile(rows: &Rows, n: usize) -> Self {
        assert!(
            n <= MAX,
            "ready graph exceeds MAX_NODES; nest graphs instead"
        );
        let mut edges = [false; MAX * MAX];
        let mut remaining = [0; MAX];
        let mut order = [0; MAX];
        let mut used = 0;
        let mut node = 0;
        while node < n {
            let mut parent = 0;
            while parent < n {
                edges[node * MAX + parent] = rows.row(node).0.get(parent).0;
                remaining[node] += edges[node * MAX + parent] as usize;
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
            while node < n {
                if edges[node * MAX + order[cursor]] {
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
        assert!(used == n, "dependency graph contains a cycle");
        // Unordered conflicting nodes follow topological order.
        let mut rank = [0; MAX];
        cursor = 0;
        while cursor < n {
            node = order[cursor];
            rank[node] = cursor;
            let mut before = 0;
            while before < cursor {
                let parent = order[before];
                if rows.row(node).0.get(parent).1 || rows.row(parent).0.get(node).1 {
                    edges[node * MAX + parent] = true;
                }
                before += 1;
            }
            cursor += 1;
        }
        // Transitive reduction: keep only edges that are not implied.
        let mut ancestors = edges;
        cursor = 0;
        while cursor < n {
            node = order[cursor];
            let mut parent = 0;
            while parent < n {
                if edges[node * MAX + parent] {
                    let mut ancestor = 0;
                    while ancestor < n {
                        ancestors[node * MAX + ancestor] |= ancestors[parent * MAX + ancestor];
                        ancestor += 1;
                    }
                }
                parent += 1;
            }
            cursor += 1;
        }
        let original = edges;
        node = 0;
        while node < n {
            let mut parent = 0;
            while parent < n {
                let mut via = 0;
                while original[node * MAX + parent] && via < n {
                    if original[node * MAX + via] && ancestors[via * MAX + parent] {
                        edges[node * MAX + parent] = false;
                    }
                    via += 1;
                }
                parent += 1;
            }
            node += 1;
        }
        let mut incoming = [0; MAX];
        let mut outgoing = [0; MAX];
        let mut join = [usize::MAX; MAX];
        let mut arrivals = [0; MAX];
        let mut joins = 0;
        let mut roots = 0;
        node = 0;
        while node < n {
            let mut parent = 0;
            while parent < n {
                incoming[node] += edges[node * MAX + parent] as usize;
                outgoing[parent] += edges[node * MAX + parent] as usize;
                parent += 1;
            }
            roots += (incoming[node] == 0) as usize;
            if incoming[node] > 1 {
                join[node] = joins;
                arrivals[joins] = incoming[node];
                joins += 1;
            }
            node += 1;
        }
        let mut local = [usize::MAX; MAX];
        let mut work = [0; MAX];
        let mut tails = 0;
        let mut parallel = roots > 1;
        cursor = n;
        while cursor > 0 {
            cursor -= 1;
            node = order[cursor];
            parallel |= outgoing[node] > 1;
            tails += (outgoing[node] == 0) as usize;
            let mut child = 0;
            while child < n {
                if edges[child * MAX + node]
                    && incoming[child] == 1
                    && (local[node] == usize::MAX || work[child] > work[local[node]])
                {
                    local[node] = child;
                }
                child += 1;
            }
            work[node] = rows.row(node).1;
            if local[node] != usize::MAX {
                work[node] = work[node].saturating_add(work[local[node]]);
            }
        }
        if tails > 1 {
            arrivals[joins] = tails;
            joins += 1;
        }
        let mut root = order[0];
        node = 0;
        while node < n {
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
            local,
            work,
            arrivals,
            joins,
            root,
            tails,
            parallel,
        }
    }
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
pub trait Spec<S>: Sized + Inner<Inner = Self::Nodes> {
    type Nodes: Nodes<S>;
    const TOPOLOGY: Topology;
    /// # Safety
    /// The graph allocation is live and index is a compiled join index.
    unsafe fn join(this: *mut Self, index: usize) -> *const Join;
}
impl<L, F, D> Inner for Entry<L, F, D> {
    type Inner = F;
    unsafe fn inner(this: *mut Self) -> *mut F {
        unsafe { &raw mut (*this).system }
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
/// Node I has finished: release its successors, or arrive at the graph's end.
struct Finished<G, I, K>(PhantomData<(G, I, K)>);
/// Run node I at path P, then release its successors.
type Step<P, G, I, K> = Then<Within<P>, Finished<G, I, K>>;
impl<S: Send + Sync, R: Send, G: Project<R, Target: Spec<S>>, I: Index, K: Continue<S, R>>
    Continue<S, R> for Finished<G, I, K>
{
    #[inline(always)]
    unsafe fn resume(context: &Context<'_, S, R>, flow: Flow) -> Tail<S, R> {
        // Early-return constant guards: rustc skips instantiating dead arms.
        if const { G::Target::TOPOLOGY.outgoing[I::VALUE] > 0 } {
            // Publish or run side paths before continuing the inline successor.
            return unsafe {
                <G::Target as Spec<S>>::Nodes::release::<R, Within<G>, G, Zero, I, K, true>(
                    context, flow,
                );
                <G::Target as Spec<S>>::Nodes::release::<R, Within<G>, G, Zero, I, K, false>(
                    context, flow,
                )
            };
        }
        if const { G::Target::TOPOLOGY.tails == 1 } {
            return unsafe { K::resume(context, flow) };
        }
        // Inline constants are evaluated even in branches folded away.
        let last = const { G::Target::TOPOLOGY.joins.wrapping_sub(1) };
        let join = unsafe { &*G::Target::join(G::get(context.nodes), last) };
        match join.arrive(flow) {
            Some(flow) => Tail::to::<K>(flow),
            None => Tail::DONE,
        }
    }
}
mod sealed {
    pub trait Sealed {}
    impl<L, F, D> Sealed for super::Entry<L, F, D> {}
    impl<A, B> Sealed for super::Pair<A, B> {}
}
/// Each node's code is emitted once: inline at its unique local site, or in
/// one out-of-line entry shared by publication, fallback, and join arrival.
#[doc(hidden)]
pub trait Nodes<S>: sealed::Sealed + Sized {
    type Access: Access;
    const LEN: usize;
    const WEIGHT: usize;
    const HAS_PARALLEL: bool;
    /// # Safety
    /// The graph was just reset; P selects these entries within graph G.
    unsafe fn roots<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R, Target: Spec<S>>,
        I: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync;
    /// # Safety
    /// Node From has finished; each edge releases exactly once.
    unsafe fn release<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R, Target: Spec<S>>,
        I: Index,
        From: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync;
    /// # Safety
    /// The graph and world are exclusive; Stage is a compiled order position.
    unsafe fn serial<G: Spec<S>, I: Index, Stage: Index>(
        nodes: *mut Self,
        world: *mut S,
        clock: &mut u64,
    ) -> bool;
    /// # Safety
    /// The graph and world are exclusive; this tree supplies the stage list.
    unsafe fn ordered<G: Spec<S>, Stage: Index>(
        nodes: *mut G::Nodes,
        world: *mut S,
        clock: &mut u64,
    ) -> bool;
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
        G: Project<R, Target: Spec<S>>,
        I: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync,
    {
        if const {
            G::Target::TOPOLOGY.incoming[I::VALUE] != 0
                || (G::Target::TOPOLOGY.root != I::VALUE) != REMOTE
        } {
            return Tail::DONE;
        }
        if !REMOTE {
            return unsafe { F::enter::<R, Within<P>, Finished<G, I, K>>(context, flow) };
        }
        let work = const { G::Target::TOPOLOGY.work[I::VALUE] };
        if !context.offer::<Step<P, G, I, K>>(flow, work, P::KEY) {
            unsafe { run::<S, R, Step<P, G, I, K>>(context, flow) }
        }
        Tail::DONE
    }
    #[inline(always)]
    unsafe fn release<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R, Target: Spec<S>>,
        I: Index,
        From: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync,
    {
        if const {
            !G::Target::TOPOLOGY.edges[I::VALUE * MAX + From::VALUE]
                || (G::Target::TOPOLOGY.local[From::VALUE] != I::VALUE) != REMOTE
        } {
            return Tail::DONE;
        }
        let mut flow = flow;
        if const { G::Target::TOPOLOGY.incoming[I::VALUE] > 1 } {
            let index = const { G::Target::TOPOLOGY.join[I::VALUE] };
            let join = unsafe { &*G::Target::join(G::get(context.nodes), index) };
            match join.arrive(flow) {
                Some(all) => flow = all,
                None => return Tail::DONE,
            }
        }
        if !REMOTE {
            return unsafe { F::enter::<R, Within<P>, Finished<G, I, K>>(context, flow) };
        }
        let work = const { G::Target::TOPOLOGY.work[I::VALUE] };
        // Without an inline successor this thread continues here; otherwise
        // side paths run here only when no worker takes them. Graph size
        // bounds this nesting.
        if !(const { G::Target::TOPOLOGY.local[From::VALUE] != usize::MAX }
            && context.offer::<Step<P, G, I, K>>(flow, work, P::KEY))
        {
            unsafe { run::<S, R, Step<P, G, I, K>>(context, flow) }
        }
        Tail::DONE
    }
    #[inline(always)]
    unsafe fn serial<G: Spec<S>, I: Index, Stage: Index>(
        nodes: *mut Self,
        world: *mut S,
        clock: &mut u64,
    ) -> bool {
        if const { G::TOPOLOGY.rank[I::VALUE] == Stage::VALUE } {
            unsafe { (*nodes).system.run_unchecked(world, clock) }.is_some()
        } else {
            false
        }
    }
    #[inline(always)]
    unsafe fn ordered<G: Spec<S>, Stage: Index>(
        nodes: *mut G::Nodes,
        world: *mut S,
        clock: &mut u64,
    ) -> bool {
        unsafe { G::Nodes::serial::<G, Zero, Stage>(nodes, world, clock) }
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
        G: Project<R, Target: Spec<S>>,
        I: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync,
    {
        unsafe {
            A::roots::<R, First<P>, G, I, K, REMOTE>(context, flow).or(B::roots::<
                R,
                Second<P>,
                G,
                Offset<I, A, S>,
                K,
                REMOTE,
            >(context, flow))
        }
    }
    #[inline(always)]
    unsafe fn release<
        R: Send,
        P: Project<R, Target = Self>,
        G: Project<R, Target: Spec<S>>,
        I: Index,
        From: Index,
        K: Continue<S, R>,
        const REMOTE: bool,
    >(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R>
    where
        S: Send + Sync,
    {
        unsafe {
            A::release::<R, First<P>, G, I, From, K, REMOTE>(context, flow).or(B::release::<
                R,
                Second<P>,
                G,
                Offset<I, A, S>,
                From,
                K,
                REMOTE,
            >(
                context, flow
            ))
        }
    }
    #[inline(always)]
    unsafe fn serial<G: Spec<S>, I: Index, Stage: Index>(
        nodes: *mut Self,
        world: *mut S,
        clock: &mut u64,
    ) -> bool {
        unsafe {
            A::serial::<G, I, Stage>(&raw mut (*nodes).0, world, clock)
                | B::serial::<G, Offset<I, A, S>, Stage>(&raw mut (*nodes).1, world, clock)
        }
    }
    #[inline(always)]
    unsafe fn ordered<G: Spec<S>, Stage: Index>(
        nodes: *mut G::Nodes,
        world: *mut S,
        clock: &mut u64,
    ) -> bool {
        unsafe {
            A::ordered::<G, Stage>(nodes, world, clock)
                | B::ordered::<G, Offset<Stage, A, S>>(nodes, world, clock)
        }
    }
}

/// Concrete systems and cross-worker join storage. No runtime topology or order.
pub struct ReadyGraph<N> {
    nodes: N,
    joins: Box<[(Join, usize)]>,
}
impl<N> plan::sealed::Sealed for ReadyGraph<N> {}
impl<N> Inner for ReadyGraph<N> {
    type Inner = N;
    unsafe fn inner(this: *mut Self) -> *mut N {
        unsafe { &raw mut (*this).nodes }
    }
}
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
        let joins = const { <ReadyGraph<T::Nodes> as Spec<S>>::TOPOLOGY.joins };
        let arrivals = const { <ReadyGraph<T::Nodes> as Spec<S>>::TOPOLOGY.arrivals };
        ReadyGraph {
            nodes: self.0.into_nodes(),
            joins: arrivals[..joins]
                .iter()
                .map(|&arrivals| (Join::default(), arrivals))
                .collect(),
        }
    }
}
impl<S, N: Nodes<S> + BuildRows<S, N>> Spec<S> for ReadyGraph<N> {
    type Nodes = N;
    const TOPOLOGY: Topology = Topology::compile(&N::ROWS, N::LEN);
    unsafe fn join(this: *mut Self, index: usize) -> *const Join {
        unsafe { &raw const (*(*this).joins.as_ptr().add(index)).0 }
    }
}
impl<S: Send + Sync, N: Nodes<S> + BuildRows<S, N> + Send> System<S> for ReadyGraph<N> {
    type Access = N::Access;
    type Output = ();
    const VALID: bool = <N as BuildRows<S, N>>::VALID;
    const HAS_PARALLEL: bool = Self::TOPOLOGY.parallel || N::HAS_PARALLEL;
    const WEIGHT: usize = N::WEIGHT;
    #[inline(always)]
    unsafe fn enter<R: Send, P: Project<R, Target = Self>, K: Continue<S, R>>(
        context: &Context<'_, S, R>,
        flow: Flow,
    ) -> Tail<S, R> {
        if const { !Self::HAS_PARALLEL } {
            return unsafe { plan::direct::<S, R, Self, P, K>(context, flow) };
        }
        for (join, arrivals) in unsafe { &(*P::get(context.nodes)).joins }.iter() {
            join.reset(*arrivals);
        }
        unsafe {
            N::roots::<R, Within<P>, P, Zero, K, true>(context, flow);
            N::roots::<R, Within<P>, P, Zero, K, false>(context, flow)
        }
    }
    unsafe fn run_unchecked(&mut self, world: *mut S, clock: &mut u64) -> Option<()> {
        if const { !Self::HAS_PARALLEL } || !executor::parallel(Self::WEIGHT) {
            let ran = unsafe { N::ordered::<Self, Zero>(&raw mut self.nodes, world, clock) };
            return ran.then_some(());
        }
        unsafe { plan::invocation(self, world, clock) }
    }
}
