//! Joins: query items that read other entities along a relation. `To<R, Q>`
//! follows `R` from the row to its targets, `From<R, Q>` back to its
//! sources, and fetches `Q` there. The item is an `Option` where at most one
//! entity is reached and an iterator otherwise, by the relation's
//! cardinality. Non-fragmenting relations read the row's link list;
//! fragmenting ones read the table key (`To`, resolved once per table) or
//! the pair index (`From`, whole tables of sources).
use std::marker::PhantomData;
use std::slice;

use crate::Entity;
use crate::access::{Access, Read};
use crate::lookup::ComponentLookup;
use crate::query::{FetchQuery, JoinCheck, QueryData};
use crate::relation::{
    Cardinality, ManyToMany, OneToMany, OneToOne, PENDING, PairOf, Relates, Relation, Sources,
};
use crate::storage::{ColumnPtr, Reach, Storage, Table};

/// Relation `R`'s targets of the row, fetched as `Q`: `Option` for relations
/// with one target per source, an iterator for many-to-many. Targets that do
/// not match `Q` are skipped.
pub struct To<R, Q>(PhantomData<fn() -> (R, Q)>);
/// Relation `R`'s sources of the row, fetched as `Q`: `Option` for
/// one-to-one, an iterator otherwise. Sources that do not match `Q` are
/// skipped.
pub struct From<R, Q>(PhantomData<fn() -> (R, Q)>);

/// What a cardinality means for joins: item shapes, and whether a join may
/// reach the row itself or share entities between rows.
pub trait JoinKind: Cardinality {
    /// `To<R, Q>`'s item.
    type To<'w, Q: QueryData>;
    /// `From<R, Q>`'s item.
    type From<'w, Q: QueryData>;
    /// Access one hop away; relations with a one-side never reach the row
    /// (they refuse self-edges).
    #[doc(hidden)]
    type Direct<A: Access>: Access;
    /// Access that may reach the row: many-to-many self-edges are allowed.
    #[doc(hidden)]
    type Far<A: Access, B: Access>: Access;
    /// Several sources may share one target (`To` may repeat an entity).
    #[doc(hidden)]
    const SHARED_TARGETS: bool;
    #[doc(hidden)]
    const MANY_TO_MANY: bool;
    /// `To` / `From` yield iterators: several items alive at once.
    #[doc(hidden)]
    const TO_MANY: bool;
    #[doc(hidden)]
    const FROM_MANY: bool;
    #[doc(hidden)]
    fn to<'w, Q: QueryData>(joined: Joined<'w, Q>) -> Self::To<'w, Q>;
    #[doc(hidden)]
    fn from<'w, Q: QueryData>(joined: Joined<'w, Q>) -> Self::From<'w, Q>;
}
impl JoinKind for OneToOne {
    type To<'w, Q: QueryData> = Option<Q::Item<'w>>;
    type From<'w, Q: QueryData> = Option<Q::Item<'w>>;
    type Direct<A: Access> = A;
    type Far<A: Access, B: Access> = B;
    const SHARED_TARGETS: bool = false;
    const MANY_TO_MANY: bool = false;
    const TO_MANY: bool = false;
    const FROM_MANY: bool = false;
    fn to<'w, Q: QueryData>(mut joined: Joined<'w, Q>) -> Option<Q::Item<'w>> {
        joined.next()
    }
    fn from<'w, Q: QueryData>(mut joined: Joined<'w, Q>) -> Option<Q::Item<'w>> {
        joined.next()
    }
}
impl JoinKind for OneToMany {
    type To<'w, Q: QueryData> = Option<Q::Item<'w>>;
    type From<'w, Q: QueryData> = Joined<'w, Q>;
    type Direct<A: Access> = A;
    type Far<A: Access, B: Access> = B;
    const SHARED_TARGETS: bool = true;
    const MANY_TO_MANY: bool = false;
    const TO_MANY: bool = false;
    const FROM_MANY: bool = true;
    fn to<'w, Q: QueryData>(mut joined: Joined<'w, Q>) -> Option<Q::Item<'w>> {
        joined.next()
    }
    fn from<'w, Q: QueryData>(joined: Joined<'w, Q>) -> Joined<'w, Q> {
        joined
    }
}
impl JoinKind for ManyToMany {
    type To<'w, Q: QueryData> = Joined<'w, Q>;
    type From<'w, Q: QueryData> = Joined<'w, Q>;
    type Direct<A: Access> = ();
    type Far<A: Access, B: Access> = (A, B);
    const SHARED_TARGETS: bool = true;
    const MANY_TO_MANY: bool = true;
    const TO_MANY: bool = true;
    const FROM_MANY: bool = true;
    fn to<'w, Q: QueryData>(joined: Joined<'w, Q>) -> Joined<'w, Q> {
        joined
    }
    fn from<'w, Q: QueryData>(joined: Joined<'w, Q>) -> Joined<'w, Q> {
        joined
    }
}

/// Where a join's entities come from.
enum Reached<'w, Q: QueryData> {
    /// A non-fragmenting link list.
    Links(slice::Iter<'w, (Entity, u32)>),
    /// A fragmenting table key, filtered to one relation.
    Pairs(slice::Iter<'w, (usize, Entity)>, usize),
    /// A fragmenting target, already resolved for the whole table.
    Resolved(Option<(Q::Fetch, usize)>),
    /// A fragmenting target's source tables, and the table being walked.
    Tables {
        tables: slice::Iter<'w, usize>,
        table: Option<(Q::Fetch, usize, usize)>,
    },
}
/// The entities a join reaches, fetched as `Q`.
pub struct Joined<'w, Q: QueryData> {
    reached: Reached<'w, Q>,
    reach: Reach,
    indices: Q::Indices,
    tick: u64,
}
/// `Q` for `entity`, if it is live and matches.
/// # Safety
/// The reach is live and grants `Q` on `entity` for 'w.
unsafe fn reach_into<'w, Q: QueryData>(
    reach: Reach,
    indices: &Q::Indices,
    tick: u64,
    entity: Entity,
) -> Option<Q::Item<'w>> {
    let (table, row) = unsafe { reach.find(entity) }?;
    if !Q::matches(indices, &table.present) {
        return None;
    }
    unsafe {
        let fetch = Q::fetch(reach, table, indices, tick);
        Some(Q::get(&fetch, row, tick))
    }
}
impl<'w, Q: QueryData> Iterator for Joined<'w, Q> {
    type Item = Q::Item<'w>;
    fn next(&mut self) -> Option<Q::Item<'w>> {
        // SAFETY: the join's checks grant `Q` on every entity reached, and
        // each is reached at most once per join.
        unsafe {
            match &mut self.reached {
                Reached::Links(links) => {
                    for &(entity, slot) in links.by_ref() {
                        if slot != PENDING
                            && let Some(item) =
                                reach_into::<Q>(self.reach, &self.indices, self.tick, entity)
                        {
                            return Some(item);
                        }
                    }
                    None
                }
                Reached::Pairs(pairs, relation) => {
                    let relation = *relation;
                    for &(r, entity) in pairs.by_ref() {
                        if r == relation
                            && let Some(item) =
                                reach_into::<Q>(self.reach, &self.indices, self.tick, entity)
                        {
                            return Some(item);
                        }
                    }
                    None
                }
                Reached::Resolved(target) => target
                    .take()
                    .map(|(fetch, row)| Q::get(&fetch, row, self.tick)),
                Reached::Tables { tables, table } => loop {
                    if let Some((fetch, row, end)) = table
                        && *row < *end
                    {
                        let item = Q::get(fetch, *row, self.tick);
                        *row += 1;
                        return Some(item);
                    }
                    let index = *tables.next()?;
                    let next = self.reach.table(index);
                    *table = Q::matches(&self.indices, &next.present).then(|| {
                        let fetch = Q::fetch(self.reach, next, &self.indices, self.tick);
                        (fetch, 0, next.entities.len())
                    });
                },
            }
        }
    }
}

/// A join's per-table state.
#[doc(hidden)]
pub struct JoinFetch<Q: QueryData> {
    reach: Reach,
    /// The row's link column (`Relates<R>` or `Sources<R>`), if present.
    links: Option<ColumnPtr>,
    /// The table's fragmenting pairs and entities.
    pairs: *const [(usize, Entity)],
    entities: *const Entity,
    /// A fragmenting one-target `To`'s target, resolved for the whole table.
    resolved: Option<(Q::Fetch, usize)>,
    relation: usize,
    indices: Option<Q::Indices>,
}
impl<Q: QueryData> Clone for JoinFetch<Q> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<Q: QueryData> Copy for JoinFetch<Q> {}
impl<Q: QueryData> Default for JoinFetch<Q> {
    fn default() -> Self {
        Self {
            reach: Reach::default(),
            links: None,
            pairs: &[],
            entities: std::ptr::null(),
            resolved: None,
            relation: 0,
            indices: None,
        }
    }
}
impl<Q: QueryData> JoinFetch<Q> {
    /// # Safety
    /// As `QueryData::fetch`.
    unsafe fn new(
        reach: Reach,
        table: &Table,
        links: usize,
        relation: usize,
        indices: Q::Indices,
    ) -> Self {
        Self {
            reach,
            links: table
                .present
                .get(links)
                .copied()
                .unwrap_or(false)
                .then(|| table.columns[links]),
            pairs: &*table.targets,
            entities: table.entities.as_ptr(),
            resolved: None,
            relation,
            indices: Some(indices),
        }
    }
    fn joined<'w>(&self, reached: Reached<'w, Q>, tick: u64) -> Joined<'w, Q> {
        Joined {
            reached,
            reach: self.reach,
            indices: self.indices.expect("a fetched join"),
            tick,
        }
    }
    /// The non-fragmenting link list at `row`.
    /// # Safety
    /// The column holds `L` and the row is live.
    unsafe fn links<'w, L: 'w>(
        &self,
        row: usize,
        entries: fn(&L) -> &[(Entity, u32)],
    ) -> slice::Iter<'w, (Entity, u32)> {
        match self.links {
            Some(column) => entries(unsafe { &*column.value::<L>(row) }).iter(),
            None => [].iter(),
        }
    }
}

/// A join is sound on its own: its inner query is; many-to-many joins never
/// write (sources share targets and may relate to themselves); and a join
/// yielding several items holds an inner query whose items never alias.
const fn join_valid<R: Relation, Q: JoinCheck>(many: bool) -> bool {
    let writes = <Q::Access as Access>::WRITES
        || <Q::Direct as Access>::WRITES
        || <Q::Far as Access>::WRITES;
    Q::ROW_VALID && (!many || Q::UNIQUE) && !(<R::Kind as JoinKind>::MANY_TO_MANY && writes)
}

impl<R: Relation, Q: JoinCheck> QueryData for To<R, Q> {
    type Access = Read<Relates<R>>;
    type Direct = <R::Kind as JoinKind>::Direct<Q::Access>;
    type Far = <R::Kind as JoinKind>::Far<Q::Access, (Q::Direct, Q::Far)>;
    const SHARED_WRITE: bool =
        <R::Kind as JoinKind>::SHARED_TARGETS && <Q::Access as Access>::WRITES;
    const JOINS: bool = true;
    const JOINS_VALID: bool = join_valid::<R, Q>(<R::Kind as JoinKind>::TO_MANY);
    const WRITES_LINKS: bool = Q::WRITES_LINKS;
    type Indices = (usize, usize, Q::Indices);
    type Fetch = JoinFetch<Q>;
    type Item<'w> = <R::Kind as JoinKind>::To<'w, Q>;
    type Slice<'w> = ();
    fn matches(_: &Self::Indices, _: &[bool]) -> bool {
        true
    }
    fn requires(_: &Self::Indices, _: usize) -> bool {
        false
    }
    unsafe fn fetch(
        reach: Reach,
        table: &Table,
        indices: &Self::Indices,
        tick: u64,
    ) -> Self::Fetch {
        let mut fetch = unsafe { JoinFetch::new(reach, table, indices.0, indices.1, indices.2) };
        if R::FRAGMENTING && !<R::Kind as JoinKind>::MANY_TO_MANY {
            // Every row of this table has the same target: resolve it once.
            let target = table.targets.iter().find(|&&(r, _)| r == indices.1);
            fetch.resolved = target.and_then(|&(_, target)| {
                let (table, row) = unsafe { reach.find(target) }?;
                Q::matches(&indices.2, &table.present)
                    .then(|| (unsafe { Q::fetch(reach, table, &indices.2, tick) }, row))
            });
        }
        fetch
    }
    fn direct(_: &Self::Fetch) -> bool {
        true
    }
    unsafe fn get<'a>(fetch: &Self::Fetch, row: usize, tick: u64) -> Self::Item<'a> {
        let reached = if !R::FRAGMENTING {
            Reached::Links(unsafe { fetch.links::<Relates<R>>(row, Relates::entries) })
        } else if !<R::Kind as JoinKind>::MANY_TO_MANY {
            Reached::Resolved(fetch.resolved)
        } else {
            Reached::Pairs(unsafe { (*fetch.pairs).iter() }, fetch.relation)
        };
        <R::Kind as JoinKind>::to(fetch.joined(reached, tick))
    }
    unsafe fn contiguous(_: &Self::Fetch, _: usize) -> bool {
        false
    }
    unsafe fn slice<'a>(_: &Self::Fetch, _: usize, _: u64) -> Self::Slice<'a> {}
}
impl<R: Relation, Q: JoinCheck> QueryData for From<R, Q> {
    type Access = Read<Sources<R>>;
    type Direct = <R::Kind as JoinKind>::Direct<Q::Access>;
    type Far = <R::Kind as JoinKind>::Far<Q::Access, (Q::Direct, Q::Far)>;
    const JOINS: bool = true;
    const JOINS_VALID: bool = join_valid::<R, Q>(<R::Kind as JoinKind>::FROM_MANY);
    const WRITES_LINKS: bool = Q::WRITES_LINKS;
    type Indices = (usize, usize, Q::Indices);
    type Fetch = JoinFetch<Q>;
    type Item<'w> = <R::Kind as JoinKind>::From<'w, Q>;
    type Slice<'w> = ();
    fn matches(_: &Self::Indices, _: &[bool]) -> bool {
        true
    }
    fn requires(_: &Self::Indices, _: usize) -> bool {
        false
    }
    unsafe fn fetch(reach: Reach, table: &Table, indices: &Self::Indices, _: u64) -> Self::Fetch {
        unsafe { JoinFetch::new(reach, table, indices.0, indices.1, indices.2) }
    }
    fn direct(_: &Self::Fetch) -> bool {
        true
    }
    unsafe fn get<'a>(fetch: &Self::Fetch, row: usize, tick: u64) -> Self::Item<'a> {
        let reached = if !R::FRAGMENTING {
            Reached::Links(unsafe { fetch.links::<Sources<R>>(row, Sources::entries) })
        } else {
            let entity = unsafe { *fetch.entities.add(row) };
            let tables = unsafe { fetch.reach.pair_tables(fetch.relation, entity) };
            Reached::Tables {
                tables: tables.iter(),
                table: None,
            }
        };
        <R::Kind as JoinKind>::from(fetch.joined(reached, tick))
    }
    unsafe fn contiguous(_: &Self::Fetch, _: usize) -> bool {
        false
    }
    unsafe fn slice<'a>(_: &Self::Fetch, _: usize, _: u64) -> Self::Slice<'a> {}
}

macro_rules! fetch_join {
    ($join:ident, $links:ident) => {
        // SAFETY: indices name the relation's own link column and pair id;
        // `Q`'s are its own.
        unsafe impl<S, R, Q> FetchQuery<S> for $join<R, Q>
        where
            S: Storage + ComponentLookup<$links<R>> + ComponentLookup<PairOf<R>>,
            R: Relation,
            Q: FetchQuery<S> + JoinCheck,
        {
            const VALID: bool = Q::VALID
                && if R::FRAGMENTING {
                    <S as ComponentLookup<PairOf<R>>>::COUNT == 1
                } else {
                    <S as ComponentLookup<$links<R>>>::COUNT == 1
                };
            const INDICES: Self::Indices = (
                <S as ComponentLookup<$links<R>>>::INDEX,
                <S as ComponentLookup<PairOf<R>>>::INDEX,
                Q::INDICES,
            );
        }
    };
}
fetch_join!(To, Relates);
fetch_join!(From, Sources);
