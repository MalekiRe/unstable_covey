//! Relations. Non-fragmenting: a source's `Relates<R>` lists its targets, a
//! target's `Sources<R>` lists its sources, and every entry records its index
//! in the other side's list, so unlinking is O(1); hooks unlink, or despawn,
//! when either side goes. Fragmenting: the `(relation, target)` pair is part of
//! the source's table key and nothing else stores it; relating is one table
//! move, sources come from the world's pair index, and despawning a target
//! cascades through that index.
use std::marker::PhantomData;

use crate::access::Write;
use crate::change::{Mut, MutSlice};
use crate::command::CommandQueue;
use crate::dispatch::schedule;
use crate::hook::{Batch, Hook, HookList, OnInsert, OnReplace};
use crate::join::JoinKind;
use crate::lookup::ComponentLookup;
use crate::plan::FunctionSystem;
use crate::query::{FetchQuery, QueryData, QueryState};
use crate::storage::{ColumnPtr, Reach, Table};
use crate::{Commands, Entity, Query, Storage, Trigger, World};

/// A relation kind. Declare one per relation; its constants pick the
/// bookkeeping at compile time.
pub trait Relation: Send + Sync + 'static {
    type Kind: JoinKind;
    /// Despawn every source with its target (e.g. a scene hierarchy).
    /// Otherwise a despawned target is just unlinked from its sources.
    const LINKED: bool = false;
    /// Give each target's sources their own tables: `From<R, Q>` walks whole
    /// tables of sources and `To<R, Q>` resolves a table's target once, at
    /// the cost of a table move per relate/unrelate and a table per distinct
    /// target set. One-to-one relations cannot fragment:
    /// ```compile_fail
    /// use static_ecs::prelude::*;
    /// struct Spouse;
    /// impl Relation for Spouse {
    ///     type Kind = OneToOne;
    ///     const FRAGMENTING: bool = true;
    /// }
    /// App::new().relation::<Spouse>().build();
    /// ```
    const FRAGMENTING: bool = false;
    /// Refuse edges that would close a cycle (relating panics), so walks
    /// along it end: `Query::cascade` needs it.
    const ACYCLIC: bool = false;
    /// This relation from the entity it is inserted on to `target`, as a
    /// bundle part: `commands.spawn((Name("Raider"), Targets.to(station)))`.
    /// Inserting replaces the entity's previous link (every edge, for
    /// many-to-many); `relate` adds one.
    fn to(self, target: Entity) -> Relates<Self>
    where
        Self: Sized,
    {
        let mut relates = Relates::new(self);
        relates.side.push((target, PENDING));
        relates
    }
}
/// Present in exactly the tables holding a pair of fragmenting relation `R`;
/// its component index is the relation's id in pair keys.
#[doc(hidden)]
pub struct PairOf<R>(PhantomData<fn() -> R>);
/// The pair id of fragmenting relation `R` in world `S`, with the
/// compile-time cardinality check.
pub(crate) fn pair_index<R: Relation, S: ComponentLookup<PairOf<R>>>() -> usize {
    const {
        assert!(
            !(R::FRAGMENTING && SourceSide::<R>::ONE && TargetSide::<R>::ONE),
            "a one-to-one relation cannot fragment: every table would hold one entity (did you mean OneToMany?)"
        );
        assert!(
            !R::FRAGMENTING || size_of::<R>() == 0,
            "a fragmenting relation carries no data: its target lives in the table key"
        );
    }
    <S as ComponentLookup<PairOf<R>>>::INDEX
}
/// Registers fragmenting relation `R` with the world, for despawn cascades.
#[doc(hidden)]
pub struct Fragment<R>(PhantomData<fn() -> R>);
impl<R: Relation, S: Storage + ComponentLookup<PairOf<R>>> HookList<S> for Fragment<R> {
    const VALID: bool = <S as ComponentLookup<PairOf<R>>>::COUNT == 1;
    unsafe fn fire<E: crate::Event>(_: *mut Self, _: *mut World<S>, _: &[Entity]) {}
    fn register<Root: HookList<S>>(_: &mut [[Option<crate::hook::Entry<S>>; crate::hook::KINDS]]) {}
    fn relations(relations: &mut Vec<(usize, bool)>) {
        if R::FRAGMENTING {
            relations.push((pair_index::<R, S>(), R::LINKED));
        }
    }
}
/// How many targets a source has, and sources a target has.
pub trait Cardinality: 'static {
    #[doc(hidden)]
    type Source: Side;
    #[doc(hidden)]
    type Target: Side;
}
/// One target per source, one source per target.
pub struct OneToOne;
/// One target per source (e.g. a parent), any sources per target.
pub struct OneToMany;
/// Any targets per source and sources per target.
pub struct ManyToMany;
impl Cardinality for OneToOne {
    type Source = One;
    type Target = One;
}
impl Cardinality for OneToMany {
    type Source = One;
    type Target = Many;
}
impl Cardinality for ManyToMany {
    type Source = Many;
    type Target = Many;
}

/// One relation side: (other entity, index of the matching entry there).
#[doc(hidden)]
pub trait Side: Default + Send + Sync + 'static {
    const ONE: bool;
    fn entries(&self) -> &[(Entity, u32)];
    fn entries_mut(&mut self) -> &mut [(Entity, u32)];
    /// Append; returns the new entry's index. A `One` side must be empty.
    fn push(&mut self, entry: (Entity, u32)) -> u32;
    /// Remove entry `index`; returns the entry moved into its place, if any.
    fn swap_remove(&mut self, index: u32) -> Option<(Entity, u32)>;
}
#[doc(hidden)]
#[derive(Default)]
pub struct One(Option<(Entity, u32)>);
#[doc(hidden)]
#[derive(Default)]
pub struct Many(Vec<(Entity, u32)>);
impl Side for One {
    const ONE: bool = true;
    fn entries(&self) -> &[(Entity, u32)] {
        self.0.as_slice()
    }
    fn entries_mut(&mut self) -> &mut [(Entity, u32)] {
        self.0.as_mut_slice()
    }
    fn push(&mut self, entry: (Entity, u32)) -> u32 {
        debug_assert!(self.0.is_none());
        self.0 = Some(entry);
        0
    }
    fn swap_remove(&mut self, _: u32) -> Option<(Entity, u32)> {
        self.0 = None;
        None
    }
}
impl Side for Many {
    const ONE: bool = false;
    fn entries(&self) -> &[(Entity, u32)] {
        &self.0
    }
    fn entries_mut(&mut self) -> &mut [(Entity, u32)] {
        &mut self.0
    }
    fn push(&mut self, entry: (Entity, u32)) -> u32 {
        self.0.push(entry);
        u32::try_from(self.0.len() - 1).expect("relation list exceeds u32 entries")
    }
    fn swap_remove(&mut self, index: u32) -> Option<(Entity, u32)> {
        self.0.swap_remove(index as usize);
        self.0.get(index as usize).copied()
    }
}

type SourceSide<R> = <<R as Relation>::Kind as Cardinality>::Source;
type TargetSide<R> = <<R as Relation>::Kind as Cardinality>::Target;

/// The back index of a link set by `.to()` and not yet linked.
pub(crate) const PENDING: u32 = u32::MAX;

/// A source's targets and the relation's value (its edge data). Set it with
/// `R.to(target)` in a bundle, or `relate`; remove it to unrelate. Reads as
/// the value: `Query<&Relates<DockedAt>>` gives `.berth` and `.target()`.
pub struct Relates<R: Relation> {
    side: SourceSide<R>,
    value: R,
}
/// A target's sources. Created and changed only through relating.
pub struct Sources<R: Relation> {
    side: TargetSide<R>,
    relation: PhantomData<fn() -> R>,
}
impl<R: Relation> Relates<R> {
    fn new(value: R) -> Self {
        Self {
            side: Default::default(),
            value,
        }
    }
    pub fn targets(&self) -> impl ExactSizeIterator<Item = Entity> + '_ {
        self.side.entries().iter().map(|&(entity, _)| entity)
    }
    pub fn len(&self) -> usize {
        self.side.entries().len()
    }
    pub fn is_empty(&self) -> bool {
        self.side.entries().is_empty()
    }
    pub(crate) fn entries(&self) -> &[(Entity, u32)] {
        self.side.entries()
    }
}
impl<R: Relation> Relates<R>
where
    R::Kind: Cardinality<Source = One>,
{
    /// The single target.
    pub fn target(&self) -> Entity {
        self.side.entries()[0].0
    }
}
impl<R: Relation> std::ops::Deref for Relates<R> {
    type Target = R;
    fn deref(&self) -> &R {
        &self.value
    }
}
impl<R: Relation> Sources<R> {
    fn new() -> Self {
        Self {
            side: Default::default(),
            relation: PhantomData,
        }
    }
    pub fn sources(&self) -> impl ExactSizeIterator<Item = Entity> + '_ {
        self.side.entries().iter().map(|&(entity, _)| entity)
    }
    pub fn len(&self) -> usize {
        self.side.entries().len()
    }
    pub fn is_empty(&self) -> bool {
        self.side.entries().is_empty()
    }
    pub(crate) fn entries(&self) -> &[(Entity, u32)] {
        self.side.entries()
    }
}
impl<R: Relation> Sources<R>
where
    R::Kind: Cardinality<Target = One>,
{
    /// The single source.
    pub fn source(&self) -> Entity {
        self.side.entries()[0].0
    }
}

/// Link components: their lists index each other across entities, so only
/// relating writes them; `&mut` access to one does not compile.
#[doc(hidden)]
pub trait LinkComponent {
    const IS: bool;
}
impl<T: ?Sized> LinkComponent for T {
    default const IS: bool = false;
}
impl<R: Relation> LinkComponent for Relates<R> {
    const IS: bool = true;
}
impl<R: Relation> LinkComponent for Sources<R> {
    const IS: bool = true;
}

/// `&mut L` for the relation's own hooks, past the `LinkComponent` gate.
#[doc(hidden)]
pub struct LinksMut<L>(PhantomData<fn() -> L>);
impl<L: 'static> QueryData for LinksMut<L> {
    type Access = Write<L>;
    type Direct = ();
    type Far = ();
    type Indices = usize;
    type Fetch = ColumnPtr;
    type Item<'w> = Mut<'w, L>;
    type Slice<'w> = MutSlice<'w, L>;
    fn matches(index: &usize, present: &[bool]) -> bool {
        <&mut L as QueryData>::matches(index, present)
    }
    fn requires(index: &usize, component: usize) -> bool {
        <&mut L as QueryData>::requires(index, component)
    }
    unsafe fn fetch(reach: Reach, table: &Table, index: &usize, tick: u64) -> ColumnPtr {
        unsafe { <&mut L as QueryData>::fetch(reach, table, index, tick) }
    }
    fn direct(fetch: &ColumnPtr) -> bool {
        <&mut L as QueryData>::direct(fetch)
    }
    unsafe fn get<'a>(fetch: &ColumnPtr, row: usize, tick: u64) -> Mut<'a, L> {
        unsafe { <&mut L as QueryData>::get(fetch, row, tick) }
    }
    unsafe fn contiguous(fetch: &ColumnPtr, len: usize) -> bool {
        unsafe { <&mut L as QueryData>::contiguous(fetch, len) }
    }
    unsafe fn slice<'a>(fetch: &ColumnPtr, len: usize, tick: u64) -> MutSlice<'a, L> {
        unsafe { <&mut L as QueryData>::slice(fetch, len, tick) }
    }
}
// SAFETY: exactly `&mut L`'s access and indices.
unsafe impl<L: 'static, S: ComponentLookup<L>> FetchQuery<S> for LinksMut<L> {
    const VALID: bool = S::COUNT == 1;
    const INDICES: usize = S::INDEX;
}

/// Both sides of relation `R`, by entity: the world, or a hook's queries.
/// `source`/`target` mark the side changed (`Relinked<R>`); back-index
/// fix-ups go through `repoint`, which does not: the entity's links stay.
trait Lists<R: Relation> {
    fn source(&mut self, entity: Entity) -> &mut SourceSide<R>;
    fn target(&mut self, entity: Entity) -> &mut TargetSide<R>;
    fn source_unmarked(&mut self, entity: Entity) -> &mut SourceSide<R>;
    fn target_unmarked(&mut self, entity: Entity) -> &mut TargetSide<R>;
}
/// An entry moved within one side: repoint its counterpart's back index.
enum Moved {
    /// Within a source's list: `(target, slot in target)` now sits at `index`.
    InSource {
        target: Entity,
        slot: u32,
        index: u32,
    },
    /// Within a target's list: `(source, index in source)` now sits at `slot`.
    InTarget {
        source: Entity,
        index: u32,
        slot: u32,
    },
}
fn repoint<R: Relation>(lists: &mut impl Lists<R>, moved: Moved) {
    match moved {
        Moved::InSource {
            target,
            slot,
            index,
        } => {
            lists.target_unmarked(target).entries_mut()[slot as usize].1 = index;
        }
        Moved::InTarget {
            source,
            index,
            slot,
        } => {
            lists.source_unmarked(source).entries_mut()[index as usize].1 = slot;
        }
    }
}
/// Unlink `source`'s entry `index` from both sides; returns its target.
fn unlink<R: Relation>(lists: &mut impl Lists<R>, source: Entity, index: u32) -> Entity {
    let (target, slot) = lists.source(source).entries()[index as usize];
    if let Some((moved, back)) = lists.source(source).swap_remove(index) {
        repoint(
            lists,
            Moved::InSource {
                target: moved,
                slot: back,
                index,
            },
        );
    }
    if let Some((moved, back)) = lists.target(target).swap_remove(slot) {
        repoint(
            lists,
            Moved::InTarget {
                source: moved,
                index: back,
                slot,
            },
        );
    }
    target
}
fn link<R: Relation>(lists: &mut impl Lists<R>, source: Entity, target: Entity) {
    let index = lists.source(source).entries().len() as u32;
    let slot = lists.target(target).push((source, index));
    lists.source(source).push((target, slot));
}
/// Link `source`'s existing entry `index` (pending) to its target.
fn link_at<R: Relation>(lists: &mut impl Lists<R>, source: Entity, index: u32) {
    let target = lists.source(source).entries()[index as usize].0;
    let slot = lists.target(target).push((source, index));
    lists.source(source).entries_mut()[index as usize].1 = slot;
}

impl<S: Storage + ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>>, R: Relation> Lists<R>
    for World<S>
{
    fn source(&mut self, entity: Entity) -> &mut SourceSide<R> {
        &mut self
            .links_mut::<Relates<R>>(entity)
            .unwrap()
            .into_inner()
            .side
    }
    fn target(&mut self, entity: Entity) -> &mut TargetSide<R> {
        &mut self
            .links_mut::<Sources<R>>(entity)
            .unwrap()
            .into_inner()
            .side
    }
    fn source_unmarked(&mut self, entity: Entity) -> &mut SourceSide<R> {
        &mut self
            .links_mut::<Relates<R>>(entity)
            .unwrap()
            .into_unmarked()
            .side
    }
    fn target_unmarked(&mut self, entity: Entity) -> &mut TargetSide<R> {
        &mut self
            .links_mut::<Sources<R>>(entity)
            .unwrap()
            .into_unmarked()
            .side
    }
}
struct HookLists<'q, 'a, 'b, R: Relation> {
    sources: &'q mut Query<'a, LinksMut<Relates<R>>>,
    targets: &'q mut Query<'b, LinksMut<Sources<R>>>,
}
impl<R: Relation> Lists<R> for HookLists<'_, '_, '_, R> {
    fn source(&mut self, entity: Entity) -> &mut SourceSide<R> {
        &mut self.sources.get_mut(entity).unwrap().into_inner().side
    }
    fn target(&mut self, entity: Entity) -> &mut TargetSide<R> {
        &mut self.targets.get_mut(entity).unwrap().into_inner().side
    }
    fn source_unmarked(&mut self, entity: Entity) -> &mut SourceSide<R> {
        &mut self.sources.get_mut(entity).unwrap().into_unmarked().side
    }
    fn target_unmarked(&mut self, entity: Entity) -> &mut TargetSide<R> {
        &mut self.targets.get_mut(entity).unwrap().into_unmarked().side
    }
}

impl<S: Storage> World<S> {
    /// Relate `source` to `target` with `value` as the edge's data. A
    /// one-target source moves to `target`; a one-source target drops its
    /// previous source; relating an existing pair only replaces the value.
    /// An entity cannot relate to itself through a relation with a one-side.
    /// Install the relation first. Relating a fragmenting relation that was
    /// never installed does not compile; a non-fragmenting one panics.
    /// ```compile_fail
    /// use static_ecs::prelude::*;
    /// struct InSquad;
    /// impl Relation for InSquad {
    ///     type Kind = OneToMany;
    ///     const FRAGMENTING: bool = true;
    /// }
    /// let mut app = App::new().build();
    /// let (a, b) = (app.world.spawn(), app.world.spawn());
    /// app.world.relate(a, InSquad, b);
    /// ```
    pub fn relate<R: Relation>(&mut self, source: Entity, value: R, target: Entity)
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        assert!(
            self.contains(source) && self.contains(target),
            "relate: both entities must be alive"
        );
        self.check_edge::<R>(source, target);
        if const { R::FRAGMENTING } {
            self.relate_pair::<R>(source, target);
            return;
        }
        assert!(
            self.has_hook::<OnReplace<Relates<R>>>() && self.has_hook::<OnReplace<Sources<R>>>(),
            "relate: install the relation with `.relation::<R>()`"
        );
        if let Some(mut current) = self.links_mut::<Relates<R>>(source) {
            current.value = value;
            let current = self.get::<Relates<R>>(source).unwrap();
            if current.targets().any(|t| t == target) {
                return;
            }
            if SourceSide::<R>::ONE && !current.is_empty() {
                let previous = unlink::<R>(self, source, 0);
                self.drop_if_empty_target::<R>(previous);
            }
        } else {
            self.insert(source, Relates::new(value));
        }
        self.displace_source::<R>(target);
        if self.get::<Sources<R>>(target).is_none() {
            self.insert(target, Sources::<R>::new());
        }
        link::<R>(self, source, target);
    }
    /// Fragmenting: one table move into the `(R, target)` pair.
    fn relate_pair<R: Relation>(&mut self, source: Entity, target: Entity)
    where
        S: ComponentLookup<PairOf<R>>,
    {
        let relation = pair_index::<R, S>();
        assert!(
            self.fragmenting(relation).is_some(),
            "relate: install the relation with `.relation::<R>()`"
        );
        let previous = self.pair_targets(source, relation).next();
        if previous == Some(target)
            || !SourceSide::<R>::ONE && self.has_pair(source, relation, target)
        {
            return;
        }
        let replaced = if SourceSide::<R>::ONE { previous } else { None };
        self.move_pair(source, relation, replaced, Some(target));
    }
    /// A one-source target drops its current source before gaining another.
    fn displace_source<R: Relation>(&mut self, target: Entity)
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>>,
    {
        if TargetSide::<R>::ONE
            && let Some(&(previous, index)) = self
                .get::<Sources<R>>(target)
                .and_then(|sources| sources.side.entries().first())
        {
            unlink::<R>(self, previous, index);
            self.drop_if_empty_source::<R>(previous);
        }
    }
    /// Link the targets `R.to(...)` left pending on `source`; dead targets
    /// are dropped. Fragmenting relations become pairs and lose `Relates<R>`.
    fn link_pending<R: Relation>(&mut self, source: Entity)
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        if const { R::FRAGMENTING } {
            let Some(mut relates) = self.links_mut::<Relates<R>>(source) else {
                return;
            };
            // Taken out first, so removing `Relates<R>` unlinks nothing.
            let pending = std::mem::take(&mut relates.side);
            self.remove::<Relates<R>>(source);
            for &(target, _) in pending.entries() {
                if self.contains(target) {
                    self.check_edge::<R>(source, target);
                    self.relate_pair::<R>(source, target);
                }
            }
            return;
        }
        let mut index = 0;
        while let Some(&(target, slot)) = self
            .get::<Relates<R>>(source)
            .and_then(|relates| relates.side.entries().get(index))
        {
            if slot != PENDING {
                index += 1;
                continue;
            }
            if !self.contains(target) {
                // Never linked: only this side moves an entry into `index`.
                let mut relates = self.links_mut::<Relates<R>>(source).unwrap();
                if let Some((moved, back)) = relates.side.swap_remove(index as u32)
                    && back != PENDING
                {
                    let moved = Moved::InSource {
                        target: moved,
                        slot: back,
                        index: index as u32,
                    };
                    repoint::<R>(self, moved);
                }
                continue;
            }
            self.check_edge::<R>(source, target);
            self.displace_source::<R>(target);
            if self.get::<Sources<R>>(target).is_none() {
                self.insert(target, Sources::<R>::new());
            }
            link_at::<R>(self, source, index as u32);
            index += 1;
        }
        self.drop_if_empty_source::<R>(source);
    }
    /// Remove the `source` → `target` link; false if it did not exist.
    pub fn unrelate<R: Relation>(&mut self, source: Entity, target: Entity) -> bool
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        if const { R::FRAGMENTING } {
            let relation = pair_index::<R, S>();
            if !self.has_pair(source, relation, target) {
                return false;
            }
            self.move_pair(source, relation, Some(target), None);
            return true;
        }
        let Some(index) = self
            .get::<Relates<R>>(source)
            .and_then(|current| current.targets().position(|t| t == target))
        else {
            return false;
        };
        unlink::<R>(self, source, index as u32);
        self.drop_if_empty_source::<R>(source);
        self.drop_if_empty_target::<R>(target);
        true
    }
    /// `source`'s targets along `R`, for either storage.
    pub fn targets<R: Relation>(&self, source: Entity) -> impl Iterator<Item = Entity> + '_
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<PairOf<R>>,
    {
        let pairs = R::FRAGMENTING.then(|| self.pair_targets(source, pair_index::<R, S>()));
        let links = if R::FRAGMENTING {
            None
        } else {
            self.get::<Relates<R>>(source)
        };
        pairs
            .into_iter()
            .flatten()
            .chain(links.into_iter().flat_map(Relates::targets))
    }
    /// `target`'s sources along `R`, for either storage.
    pub fn sources<R: Relation>(&self, target: Entity) -> impl Iterator<Item = Entity> + '_
    where
        S: ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        let pairs = R::FRAGMENTING.then(|| self.pair_sources(pair_index::<R, S>(), target));
        let links = if R::FRAGMENTING {
            None
        } else {
            self.get::<Sources<R>>(target)
        };
        pairs
            .into_iter()
            .flatten()
            .chain(links.into_iter().flat_map(Sources::sources))
    }
    /// Every entity below `root` along `R`, parents before children (breadth
    /// first), for either storage. With `R` as a hierarchy, propagate
    /// transforms in this order.
    pub fn descendants<R: Relation>(&self, root: Entity) -> impl Iterator<Item = Entity> + '_
    where
        S: ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        let mut queue: Vec<Entity> = self.sources::<R>(root).collect();
        let mut next = 0;
        std::iter::from_fn(move || {
            let entity = *queue.get(next)?;
            next += 1;
            queue.extend(self.sources::<R>(entity));
            Some(entity)
        })
    }
    fn drop_if_empty_source<R: Relation>(&mut self, source: Entity)
    where
        S: ComponentLookup<Relates<R>>,
    {
        if self
            .get::<Relates<R>>(source)
            .is_some_and(Relates::is_empty)
        {
            self.remove::<Relates<R>>(source);
        }
    }
    fn drop_if_empty_target<R: Relation>(&mut self, target: Entity)
    where
        S: ComponentLookup<Sources<R>>,
    {
        if self
            .get::<Sources<R>>(target)
            .is_some_and(Sources::is_empty)
        {
            self.remove::<Sources<R>>(target);
        }
    }
}
impl<S: Storage> Commands<'_, S> {
    /// Deferred `World::relate`; skipped if either entity died meanwhile.
    pub fn relate<R: Relation>(&mut self, source: Entity, value: R, target: Entity)
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        self.queue(move |world| {
            if world.contains(source) && world.contains(target) {
                world.relate(source, value, target);
            }
        });
    }
    /// Deferred `World::unrelate`.
    pub fn unrelate<R: Relation>(&mut self, source: Entity, target: Entity)
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>>,
    {
        self.queue(move |world| {
            world.unrelate::<R>(source, target);
        });
    }
}

/// A source is losing `Relates<R>` (removed or despawned): unlink it from
/// every target, dropping `Sources<R>` from targets left without sources.
fn source_gone<
    R: Relation,
    S: Storage + ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>>,
>(
    trigger: Trigger,
    mut sources: Query<LinksMut<Relates<R>>>,
    mut targets: Query<LinksMut<Sources<R>>>,
    mut commands: Commands<S>,
) {
    let mut lists = HookLists {
        sources: &mut sources,
        targets: &mut targets,
    };
    for &source in trigger.iter() {
        // From the end: no entry moves.
        while let Some(index) = lists.source(source).entries().len().checked_sub(1) {
            if lists.source(source).entries()[index].1 == PENDING {
                lists.source(source).swap_remove(index as u32);
                continue;
            }
            let target = unlink::<R>(&mut lists, source, index as u32);
            if lists.target(target).entries().is_empty() {
                commands.remove::<(Sources<R>,)>(target);
            }
        }
    }
}
/// A target is losing `Sources<R>`: despawn its sources if linked, otherwise
/// unlink them, dropping `Relates<R>` from sources left without targets.
fn target_gone<
    R: Relation,
    S: Storage + ComponentLookup<Relates<R>> + ComponentLookup<Sources<R>>,
>(
    trigger: Trigger,
    mut sources: Query<LinksMut<Relates<R>>>,
    mut targets: Query<LinksMut<Sources<R>>>,
    mut commands: Commands<S>,
) {
    let mut lists = HookLists {
        sources: &mut sources,
        targets: &mut targets,
    };
    for &target in trigger.iter() {
        while let Some(&(source, index)) = lists.target(target).entries().last() {
            unlink::<R>(&mut lists, source, index);
            if R::LINKED {
                commands.despawn(source);
                continue;
            }
            if lists.source(source).entries().is_empty() {
                commands.remove::<(Relates<R>,)>(source);
            }
        }
    }
}

/// `R.to(...)` was inserted: link its pending targets before the insertion
/// returns.
fn pending_inserted<
    R: Relation,
    S: Storage
        + ComponentLookup<Relates<R>>
        + ComponentLookup<Sources<R>>
        + ComponentLookup<PairOf<R>>,
>(
    trigger: Trigger,
    mut sources: Query<&'static Relates<R>>,
    mut commands: Commands<S>,
) {
    for &source in trigger.iter() {
        let pending = sources
            .get_mut(source)
            .is_some_and(|relates| relates.entries().iter().any(|&(_, slot)| slot == PENDING));
        if pending {
            commands.queue(move |world| world.link_pending::<R>(source));
        }
    }
}

/// Both cleanup hooks share this signature.
type Cleanup<R, S> = for<'a, 'b, 'c, 'd> fn(
    Trigger<'a>,
    Query<'b, LinksMut<Relates<R>>>,
    Query<'c, LinksMut<Sources<R>>>,
    Commands<'d, S>,
);
type CleanupSystem<R, S> = FunctionSystem<
    Cleanup<R, S>,
    (
        Query<'static, LinksMut<Relates<R>>>,
        Query<'static, LinksMut<Sources<R>>>,
        Commands<'static, S>,
    ),
    (QueryState, QueryState, CommandQueue<S>),
    (),
    Batch,
>;
type Pending<R, S> =
    for<'a, 'b, 'c> fn(Trigger<'a>, Query<'b, &'static Relates<R>>, Commands<'c, S>);
type PendingSystem<R, S> = FunctionSystem<
    Pending<R, S>,
    (Query<'static, &'static Relates<R>>, Commands<'static, S>),
    (QueryState, CommandQueue<S>),
    (),
    Batch,
>;
/// The hooks and registration keeping relation `R` consistent in world `S`.
pub type RelationHooks<R, S> = (
    Hook<OnReplace<Relates<R>>, CleanupSystem<R, S>>,
    (
        Hook<OnReplace<Sources<R>>, CleanupSystem<R, S>>,
        (Hook<OnInsert<Relates<R>>, PendingSystem<R, S>>, Fragment<R>),
    ),
);

/// Install relation `R`: its components, and either the hooks keeping the
/// lists consistent or, if fragmenting, its registration for despawn
/// cascades. Installed by `.relation::<R>()` or a plugin's `Relations`.
pub(crate) fn relation<R, S>() -> RelationHooks<R, S>
where
    R: Relation,
    S: Storage
        + ComponentLookup<Relates<R>>
        + ComponentLookup<Sources<R>>
        + ComponentLookup<PairOf<R>>,
{
    pair_index::<R, S>();
    (
        schedule(
            OnReplace::<Relates<R>>::new(),
            source_gone::<R, S> as Cleanup<R, S>,
        ),
        (
            schedule(
                OnReplace::<Sources<R>>::new(),
                target_gone::<R, S> as Cleanup<R, S>,
            ),
            (
                schedule(
                    OnInsert::<Relates<R>>::new(),
                    pending_inserted::<R, S> as Pending<R, S>,
                ),
                Fragment(PhantomData),
            ),
        ),
    )
}

impl<S: Storage> World<S> {
    /// Refuse edges that break `R`'s rules. Self-edges through a one-side
    /// would alias a row's own `&mut T` with a join's; an `ACYCLIC` edge must
    /// not close a cycle, i.e. `target` must not already reach `source`.
    fn check_edge<R: Relation>(&self, source: Entity, target: Entity)
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<PairOf<R>>,
    {
        if SourceSide::<R>::ONE || TargetSide::<R>::ONE {
            assert!(
                source != target,
                "relate: an entity cannot relate to itself through a relation with a one-side"
            );
        }
        if R::ACYCLIC {
            assert!(
                !self.reaches::<R>(target, source),
                "relate: the edge would close a cycle in an ACYCLIC relation"
            );
        }
    }
    /// Whether `to` is `from` or above it along `R` (`R` acyclic, so the walk ends).
    fn reaches<R: Relation>(&self, from: Entity, to: Entity) -> bool
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<PairOf<R>>,
    {
        if SourceSide::<R>::ONE {
            let mut at = Some(from);
            while let Some(entity) = at {
                if entity == to {
                    return true;
                }
                at = self.targets::<R>(entity).next();
            }
            return false;
        }
        let (mut stack, mut seen) = (vec![from], Vec::new());
        while let Some(entity) = stack.pop() {
            if entity == to {
                return true;
            }
            if !seen.contains(&entity) {
                seen.push(entity);
                stack.extend(self.targets::<R>(entity));
            }
        }
        false
    }
}
