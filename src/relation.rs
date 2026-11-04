//! Relations. Non-fragmenting: a source's `Relates<R>` lists its targets, a
//! target's `Sources<R>` lists its sources, and every entry records its index
//! in the other side's list, so unlinking is O(1); hooks unlink, or despawn,
//! when either side goes. Fragmenting: the `(relation, target)` pair is part of
//! the source's table key and nothing else stores it; relating is one table
//! move, sources come from the world's pair index, and despawning a target
//! cascades through that index.
use std::marker::PhantomData;

use crate::command::CommandQueue;
use crate::dispatch::schedule;
use crate::filter::QueryFilter;
use crate::hook::{Batch, Hook, HookList, OnInsert, OnReplace};
use crate::join::JoinKind;
use crate::lookup::ComponentLookup;
use crate::plan::FunctionSystem;
use crate::query::QueryState;
use crate::{Commands, Entity, Query, Storage, Trigger, World};

/// A relation kind. Declare one per relation; its constants pick the
/// bookkeeping at compile time.
pub trait Relation: Send + Sync + 'static {
    type Kind: JoinKind;
    /// Despawn every source with its target (e.g. a scene hierarchy).
    /// Otherwise a despawned target is just unlinked from its sources.
    const LINKED: bool = false;
    /// Give each target's sources their own tables: "every source of X" is a
    /// table-level query (`Query::iter_related`), at the cost of a table move
    /// per relate/unrelate and a table per distinct target set. One-to-one
    /// relations cannot fragment:
    /// ```compile_fail
    /// use static_ecs::prelude::*;
    /// struct Spouse;
    /// impl Relation for Spouse {
    ///     type Kind = OneToOne;
    ///     const FRAGMENTING: bool = true;
    /// }
    /// App::new().relation::<Spouse, _>().build();
    /// ```
    const FRAGMENTING: bool = false;
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
impl<R: Relation> std::ops::DerefMut for Relates<R> {
    fn deref_mut(&mut self) -> &mut R {
        &mut self.value
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

/// Both sides of relation `R`, by entity: the world, or a hook's queries.
trait Lists<R: Relation> {
    fn source(&mut self, entity: Entity) -> &mut SourceSide<R>;
    fn target(&mut self, entity: Entity) -> &mut TargetSide<R>;
}
/// Unlink `source`'s entry `index` from both sides; returns its target.
fn unlink<R: Relation>(lists: &mut impl Lists<R>, source: Entity, index: u32) -> Entity {
    let (target, slot) = lists.source(source).entries()[index as usize];
    if let Some((moved, back)) = lists.source(source).swap_remove(index) {
        lists.target(moved).entries_mut()[back as usize].1 = index;
    }
    if let Some((moved, back)) = lists.target(target).swap_remove(slot) {
        lists.source(moved).entries_mut()[back as usize].1 = slot;
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
            .get_mut::<Relates<R>>(entity)
            .unwrap()
            .into_inner()
            .side
    }
    fn target(&mut self, entity: Entity) -> &mut TargetSide<R> {
        &mut self
            .get_mut::<Sources<R>>(entity)
            .unwrap()
            .into_inner()
            .side
    }
}
struct HookLists<'q, 'a, 'b, R: Relation> {
    sources: &'q mut Query<'a, &'static mut Relates<R>>,
    targets: &'q mut Query<'b, &'static mut Sources<R>>,
}
impl<R: Relation> Lists<R> for HookLists<'_, '_, '_, R> {
    fn source(&mut self, entity: Entity) -> &mut SourceSide<R> {
        &mut self.sources.get_mut(entity).unwrap().into_inner().side
    }
    fn target(&mut self, entity: Entity) -> &mut TargetSide<R> {
        &mut self.targets.get_mut(entity).unwrap().into_inner().side
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
        check_self_edge::<R>(source, target);
        if const { R::FRAGMENTING } {
            self.relate_pair::<R>(source, target);
            return;
        }
        assert!(
            self.has_hook::<OnReplace<Relates<R>>>() && self.has_hook::<OnReplace<Sources<R>>>(),
            "relate: install the relation with `.relation::<R, _>()`"
        );
        if let Some(mut current) = self.get_mut::<Relates<R>>(source) {
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
            "relate: install the relation with `.relation::<R, _>()`"
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
            let Some(mut relates) = self.get_mut::<Relates<R>>(source) else {
                return;
            };
            // Taken out first, so removing `Relates<R>` unlinks nothing.
            let pending = std::mem::take(&mut relates.side);
            self.remove::<Relates<R>>(source);
            for &(target, _) in pending.entries() {
                if self.contains(target) {
                    check_self_edge::<R>(source, target);
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
                let mut relates = self.get_mut::<Relates<R>>(source).unwrap();
                if let Some((moved, back)) = relates.side.swap_remove(index as u32)
                    && back != PENDING
                {
                    Lists::<R>::target(self, moved).entries_mut()[back as usize].1 = index as u32;
                }
                continue;
            }
            check_self_edge::<R>(source, target);
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
    pub fn targets<R: Relation>(&self, source: Entity) -> Vec<Entity>
    where
        S: ComponentLookup<Relates<R>> + ComponentLookup<PairOf<R>>,
    {
        if const { R::FRAGMENTING } {
            return self.pair_targets(source, pair_index::<R, S>()).collect();
        }
        self.get::<Relates<R>>(source)
            .map_or_else(Vec::new, |r| r.targets().collect())
    }
    /// `target`'s sources along `R`, for either storage.
    pub fn sources<R: Relation>(&self, target: Entity) -> Vec<Entity>
    where
        S: ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        let mut sources = Vec::new();
        self.append_sources::<R>(target, &mut sources);
        sources
    }
    fn append_sources<R: Relation>(&self, target: Entity, out: &mut Vec<Entity>)
    where
        S: ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        if const { R::FRAGMENTING } {
            self.pair_sources(pair_index::<R, S>(), target, out);
            return;
        }
        if let Some(sources) = self.get::<Sources<R>>(target) {
            out.extend(sources.sources());
        }
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
    mut sources: Query<&'static mut Relates<R>>,
    mut targets: Query<&'static mut Sources<R>>,
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
    mut sources: Query<&'static mut Relates<R>>,
    mut targets: Query<&'static mut Sources<R>>,
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
    Query<'b, &'static mut Relates<R>>,
    Query<'c, &'static mut Sources<R>>,
    Commands<'d, S>,
);
type CleanupSystem<R, S> = FunctionSystem<
    Cleanup<R, S>,
    (
        Query<'static, &'static mut Relates<R>>,
        Query<'static, &'static mut Sources<R>>,
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
/// cascades. Use `.relation::<R, _>()` or install from a plugin.
pub fn relation<R, S>() -> RelationHooks<R, S>
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

/// Self-edges would alias a row's own `&mut T` with a join's.
fn check_self_edge<R: Relation>(source: Entity, target: Entity) {
    if SourceSide::<R>::ONE || TargetSide::<R>::ONE {
        assert!(
            source != target,
            "relate: an entity cannot relate to itself through a relation with a one-side"
        );
    }
}

/// Breadth first from `root` (excluded): every parent before its children.
/// `sources` appends an entity's sources to the order.
fn breadth_first(root: Entity, mut sources: impl FnMut(Entity, &mut Vec<Entity>)) -> Vec<Entity> {
    let mut order = Vec::new();
    sources(root, &mut order);
    let mut next = 0;
    while let Some(&parent) = order.get(next) {
        sources(parent, &mut order);
        next += 1;
    }
    order
}
impl<S: Storage> World<S> {
    /// Every entity below `root` along `R`, parents before children (breadth
    /// first), for either storage. With `R` as a hierarchy, propagate
    /// transforms in this order.
    pub fn descendants<R: Relation>(&self, root: Entity) -> Vec<Entity>
    where
        S: ComponentLookup<Sources<R>> + ComponentLookup<PairOf<R>>,
    {
        breadth_first(root, |e, order| self.append_sources::<R>(e, order))
    }
}
/// Non-fragmenting relations from inside a system; fragmenting ones use
/// `Query<D, Related<R>>::related_descendants`.
impl<R: Relation, F: QueryFilter> Query<'_, &Sources<R>, F> {
    /// `World::descendants` from inside a system.
    pub fn descendants(&mut self, root: Entity) -> Vec<Entity> {
        breadth_first(root, |e, order| {
            if let Some(s) = self.get_mut(e) {
                order.extend(s.sources());
            }
        })
    }
}
