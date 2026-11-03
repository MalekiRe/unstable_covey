use crate::{
    Entity, Storage, World,
    access::{Access, Read, ValidAccess, Write},
    change::{ChangeWindow, Mut, MutSlice, SystemWorld},
    filter::{FilterFor, QueryFilter, TableMatch},
    lookup::ComponentLookup,
    storage::{ColumnPtr, Locator, Table},
};
use std::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
    slice,
};
mod sealed {
    pub trait Sealed {}
    impl<T: 'static> Sealed for &T {}
    impl<T: 'static> Sealed for &mut T {}
    impl Sealed for super::Entity {}
    impl<A: Sealed, B: Sealed> Sealed for (A, B) {}
    impl<A: Sealed, B: Sealed, C: Sealed> Sealed for (A, B, C) {}
}
/// Sealed component, entity, or nested tuple query data.
pub trait QueryData: sealed::Sealed {
    type Access: Access;
    type Indices: Copy;
    type Fetch: Copy + Default;
    type Item<'w>;
    type Slice<'w>;
    #[doc(hidden)]
    fn matches(indices: &Self::Indices, present: &[bool]) -> bool;
    #[doc(hidden)]
    fn requires(indices: &Self::Indices, component: usize) -> bool;
    /// # Safety
    /// Indices identify the correct types and the table matches. Writable
    /// columns have their change summaries raised to `tick`.
    unsafe fn fetch(table: &Table, indices: &Self::Indices, tick: u64) -> Self::Fetch;
    /// Whether every column indexes rows directly, with no archetype row map.
    fn direct(fetch: &Self::Fetch) -> bool;
    /// # Safety
    /// Row is live, access granted for 'a, and mutable rows are yielded once.
    unsafe fn get<'a>(fetch: &Self::Fetch, row: usize, tick: u64) -> Self::Item<'a>;
    /// # Safety
    /// Fetch and all logical rows are valid.
    unsafe fn contiguous(fetch: &Self::Fetch, len: usize) -> bool;
    /// # Safety
    /// All rows are contiguous, live, uniquely yielded, and access granted for 'a.
    unsafe fn slice<'a>(fetch: &Self::Fetch, len: usize, tick: u64) -> Self::Slice<'a>;
}
/// # Safety
/// VALID guarantees indices identify the unique requested component declarations.
pub unsafe trait FetchQuery<S: Storage>: QueryData {
    const VALID: bool;
    const INDICES: Self::Indices;
}
#[doc(hidden)]
#[derive(Default)]
pub struct QueryState(Option<Box<MatchingTables>>);
struct MatchingTables {
    world: u64,
    scanned: usize,
    candidate: Option<usize>,
    matching: Vec<usize>,
}
fn candidate<D: QueryData, F: QueryFilter>(
    indices: &D::Indices,
    filter: &F::State,
    tables: &[Vec<usize>],
) -> Option<usize> {
    tables
        .iter()
        .enumerate()
        .filter(|(i, _)| D::requires(indices, *i) || F::requires(filter, *i))
        .min_by_key(|(_, entries)| entries.len())
        .map(|(i, _)| i)
}
#[derive(Clone)]
enum Tables<'a> {
    All(slice::Iter<'a, Table>),
    Indexed(&'a [Table], slice::Iter<'a, usize>, bool),
}
impl<'a> Tables<'a> {
    #[inline]
    fn next<D: QueryData, F: QueryFilter>(
        &mut self,
        indices: &D::Indices,
        filter: &F::State,
    ) -> Option<&'a Table> {
        let matches = |table: &&Table| {
            D::matches(indices, &table.present) && F::matches(filter, &table.present)
        };
        match self {
            Self::All(tables) => tables.find(matches),
            Self::Indexed(tables, entries, cached) => {
                if *cached {
                    entries.next().map(|&i| &tables[i])
                } else {
                    entries.find_map(|&i| matches(&&tables[i]).then_some(&tables[i]))
                }
            }
        }
    }
}
/// Borrowed query; static component indices resolve columns once per archetype.
pub struct Query<'w, D: QueryData, F: QueryFilter = ()> {
    tables: &'w [Table],
    locator: Locator<'w>,
    matching: Option<&'w [usize]>,
    cached: bool,
    indices: D::Indices,
    filter: F::State,
    window: ChangeWindow,
}
impl<'w, D: QueryData, F: QueryFilter> Query<'w, D, F> {
    pub(crate) unsafe fn from_world<S: Storage>(world: *mut World<S>, window: ChangeWindow) -> Self
    where
        D: FetchQuery<S>,
        D::Access: ValidAccess,
        F: FilterFor<S>,
    {
        const {
            assert!(
                D::VALID && F::VALID && D::Access::VALID,
                "invalid query: missing/ambiguous component or conflicting access"
            );
        }
        Self {
            tables: unsafe { &(*world).tables },
            locator: unsafe { World::locate(world) },
            matching: None,
            cached: false,
            indices: D::INDICES,
            filter: F::STATE,
            window,
        }
    }
    pub(crate) unsafe fn from_cached<S: Storage>(
        world: *mut World<S>,
        state: &'w mut QueryState,
        window: ChangeWindow,
    ) -> Self
    where
        D: FetchQuery<S>,
        D::Access: ValidAccess,
        F: FilterFor<S>,
    {
        let mut query = unsafe { Self::from_world(world, window) };
        let id = unsafe { World::<S>::world_id(world) };
        let component_tables = unsafe { &(*world).component_tables };
        let state = state.0.get_or_insert_with(|| {
            Box::new(MatchingTables {
                world: u64::MAX,
                scanned: 0,
                candidate: None,
                matching: Vec::new(),
            })
        });
        if state.world != id {
            state.matching.clear();
            state.scanned = 0;
            state.world = id;
            state.candidate = candidate::<D, F>(&query.indices, &query.filter, component_tables);
        }
        if state.scanned < query.tables.len() {
            if let Some(component) = state.candidate {
                let candidates = &component_tables[component];
                let start = candidates.partition_point(|&i| i < state.scanned);
                for &i in &candidates[start..] {
                    if D::matches(&query.indices, &query.tables[i].present)
                        && F::matches(&query.filter, &query.tables[i].present)
                    {
                        state.matching.push(i);
                    }
                    state.scanned = i + 1;
                }
            } else {
                for (i, table) in query.tables.iter().enumerate().skip(state.scanned) {
                    if D::matches(&query.indices, &table.present)
                        && F::matches(&query.filter, &table.present)
                    {
                        state.matching.push(i);
                    }
                    state.scanned = i + 1;
                }
            }
            state.scanned = query.tables.len();
        }
        query.matching = Some(&state.matching);
        query.cached = true;
        query
    }
    fn tables(&self) -> Tables<'_> {
        match self.matching {
            Some(matching) => Tables::Indexed(self.tables, matching.iter(), self.cached),
            None => Tables::All(self.tables.iter()),
        }
    }
    /// The item for `entity`, if it is live and matches this query and its
    /// filters (including row filters such as `Changed`).
    pub fn get_mut(&mut self, entity: Entity) -> Option<D::Item<'_>> {
        let (table, row) = self.locator.find(entity)?;
        let table = &self.tables[table];
        if !D::matches(&self.indices, &table.present) || !F::matches(&self.filter, &table.present) {
            return None;
        }
        // SAFETY: the table matches, the row is live, and `&mut self` keeps
        // every other item of this query from being alive meanwhile.
        unsafe {
            match F::classify(&self.filter, table, self.window) {
                TableMatch::None => return None,
                TableMatch::Maybe if !F::row(&self.filter, table, row, self.window) => return None,
                _ => {}
            }
            let fetch = D::fetch(table, &self.indices, self.window.now);
            Some(D::get(&fetch, row, self.window.now))
        }
    }
    pub fn iter_mut(&mut self) -> QueryIter<'_, D, F> {
        QueryIter {
            tables: self.tables(),
            indices: self.indices,
            filter: self.filter,
            window: self.window,
            table: None,
            fetch: Default::default(),
            row: 0,
            end: 0,
            class: TableMatch::None,
            lifetime: PhantomData,
        }
    }
    /// Each item is one contiguous archetype slice (or tuple of slices).
    /// Row-filtered or fragmented queries fail before yielding any references.
    pub fn contiguous_mut(&mut self) -> Result<QueryChunks<'_, D, F>, ContiguousError> {
        if !F::ARCHETYPAL {
            return Err(ContiguousError::RowFilter);
        }
        let mut tables = self.tables();
        while let Some(table) = tables.next::<D, F>(&self.indices, &self.filter) {
            if !table.entities.is_empty()
                && !unsafe {
                    D::contiguous(
                        &D::fetch(table, &self.indices, self.window.now),
                        table.entities.len(),
                    )
                }
            {
                return Err(ContiguousError::Fragmented);
            }
        }
        Ok(QueryChunks {
            tables: self.tables(),
            indices: self.indices,
            filter: self.filter,
            tick: self.window.now,
            lifetime: PhantomData,
        })
    }
    /// Exactly one fully matching entity across every matching archetype.
    pub fn single(self) -> Option<Single<'w, D, F>> {
        let mut tables = match self.matching {
            Some(entries) => Tables::Indexed(self.tables, entries.iter(), self.cached),
            None => Tables::All(self.tables.iter()),
        };
        let mut matched = None;
        while let Some(table) = tables.next::<D, F>(&self.indices, &self.filter) {
            if table.entities.is_empty() {
                continue;
            }
            match unsafe { F::classify(&self.filter, table, self.window) } {
                TableMatch::None => {}
                TableMatch::All => {
                    if table.entities.len() != 1 || matched.is_some() {
                        return None;
                    }
                    matched = Some((table, 0));
                }
                TableMatch::Maybe => {
                    for row in 0..table.entities.len() {
                        if unsafe { F::row(&self.filter, table, row, self.window) } {
                            if matched.is_some() {
                                return None;
                            }
                            matched = Some((table, row));
                        }
                    }
                }
            }
        }
        let (table, row) = matched?;
        Some(Single {
            item: unsafe {
                let fetch = D::fetch(table, &self.indices, self.window.now);
                D::get(&fetch, row, self.window.now)
            },
            filter: PhantomData,
        })
    }
}
pub struct QueryIter<'a, D: QueryData, F: QueryFilter = ()> {
    tables: Tables<'a>,
    indices: D::Indices,
    filter: F::State,
    fetch: D::Fetch,
    window: ChangeWindow,
    table: Option<&'a Table>,
    row: usize,
    end: usize,
    class: TableMatch,
    lifetime: PhantomData<&'a mut ()>,
}
impl<'a, D: QueryData, F: QueryFilter> QueryIter<'a, D, F> {
    /// The next nonempty matching table. Outlined, and the cursor travels by
    /// value: the iterator's address never escapes, so an inlined per-row
    /// loop keeps its state in registers.
    #[inline(never)]
    fn next_table(
        mut tables: Tables<'a>,
        indices: D::Indices,
        filter: F::State,
        window: ChangeWindow,
    ) -> (Tables<'a>, Option<(&'a Table, TableMatch)>) {
        while let Some(table) = tables.next::<D, F>(&indices, &filter) {
            if table.entities.is_empty() {
                continue;
            }
            let class = unsafe { F::classify(&filter, table, window) };
            if class != TableMatch::None {
                return (tables, Some((table, class)));
            }
        }
        (tables, None)
    }
    #[inline]
    fn advance(&mut self) -> bool {
        let tables = self.tables.clone();
        let (tables, next) = Self::next_table(tables, self.indices, self.filter, self.window);
        self.tables = tables;
        let Some((table, class)) = next else {
            return false;
        };
        self.fetch = unsafe { D::fetch(table, &self.indices, self.window.now) };
        self.table = Some(table);
        self.row = 0;
        self.end = table.entities.len();
        self.class = class;
        true
    }
}
impl<'a, D: QueryData, F: QueryFilter> Iterator for QueryIter<'a, D, F> {
    type Item = D::Item<'a>;
    #[inline]
    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.row == self.end && !self.advance() {
                return None;
            }
            let row = self.row;
            self.row += 1;
            if const { F::ARCHETYPAL }
                || self.class == TableMatch::All
                || unsafe { F::row(&self.filter, self.table.unwrap(), row, self.window) }
            {
                return Some(unsafe { D::get(&self.fetch, row, self.window.now) });
            }
        }
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        let mut lower = if self.class == TableMatch::All {
            self.end - self.row
        } else {
            0
        };
        let mut upper = self.end - self.row;
        let mut tables = self.tables.clone();
        while let Some(table) = tables.next::<D, F>(&self.indices, &self.filter) {
            match unsafe { F::classify(&self.filter, table, self.window) } {
                TableMatch::All => {
                    lower += table.entities.len();
                    upper += table.entities.len();
                }
                TableMatch::Maybe => upper += table.entities.len(),
                TableMatch::None => {}
            }
        }
        (lower, Some(upper))
    }
    fn count(mut self) -> usize {
        let mut count = 0;
        loop {
            if self.class == TableMatch::All {
                count += self.end - self.row;
            } else if let Some(table) = self.table {
                for row in self.row..self.end {
                    count += unsafe { F::row(&self.filter, table, row, self.window) } as usize;
                }
            }
            if !self.advance() {
                return count;
            }
        }
    }
    #[inline]
    fn fold<B, G: FnMut(B, Self::Item) -> B>(mut self, mut result: B, mut f: G) -> B {
        loop {
            let (fetch, rows, now) = (self.fetch, self.row..self.end, self.window.now);
            if const { F::ARCHETYPAL } || self.class == TableMatch::All {
                // Unswitched per table: direct rows fold every row-map branch away.
                #[expect(clippy::if_same_then_else, reason = "per-table loop unswitching")]
                if D::direct(&fetch) {
                    for row in rows {
                        result = f(result, unsafe { D::get(&fetch, row, now) });
                    }
                } else {
                    for row in rows {
                        result = f(result, unsafe { D::get(&fetch, row, now) });
                    }
                }
            } else if let Some(table) = self.table {
                for row in rows {
                    if unsafe { F::row(&self.filter, table, row, self.window) } {
                        result = f(result, unsafe { D::get(&fetch, row, now) });
                    }
                }
            }
            if !self.advance() {
                return result;
            }
        }
    }
}
impl<D: QueryData, F: QueryFilter> std::iter::FusedIterator for QueryIter<'_, D, F> {}
impl<'a, 'w: 'a, D: QueryData, F: QueryFilter> IntoIterator for &'a mut Query<'w, D, F> {
    type Item = D::Item<'a>;
    type IntoIter = QueryIter<'a, D, F>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContiguousError {
    RowFilter,
    Fragmented,
}
impl std::fmt::Display for ContiguousError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::RowFilter => "row filters cannot yield contiguous slices",
            Self::Fragmented => "query rows are not physically contiguous",
        })
    }
}
impl std::error::Error for ContiguousError {}
pub struct QueryChunks<'a, D: QueryData, F: QueryFilter = ()> {
    tables: Tables<'a>,
    indices: D::Indices,
    filter: F::State,
    tick: u64,
    lifetime: PhantomData<&'a mut ()>,
}
impl<'a, D: QueryData, F: QueryFilter> Iterator for QueryChunks<'a, D, F> {
    type Item = D::Slice<'a>;
    fn next(&mut self) -> Option<Self::Item> {
        while let Some(table) = self.tables.next::<D, F>(&self.indices, &self.filter) {
            if !table.entities.is_empty() {
                return Some(unsafe {
                    D::slice(
                        &D::fetch(table, &self.indices, self.tick),
                        table.entities.len(),
                        self.tick,
                    )
                });
            }
        }
        None
    }
}
impl<D: QueryData, F: QueryFilter> std::iter::FusedIterator for QueryChunks<'_, D, F> {}
pub struct Single<'w, D: QueryData, F: QueryFilter = ()> {
    item: D::Item<'w>,
    filter: PhantomData<fn() -> F>,
}
impl<'w, D: QueryData, F: QueryFilter> Single<'w, D, F> {
    pub fn into_inner(self) -> D::Item<'w> {
        self.item
    }
}
impl<'w, D: QueryData, F: QueryFilter> Deref for Single<'w, D, F> {
    type Target = D::Item<'w>;
    fn deref(&self) -> &Self::Target {
        &self.item
    }
}
impl<D: QueryData, F: QueryFilter> DerefMut for Single<'_, D, F> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.item
    }
}
impl<T: 'static> QueryData for &T {
    type Access = Read<T>;
    type Indices = usize;
    type Fetch = ColumnPtr;
    type Item<'w> = &'w T;
    type Slice<'w> = &'w [T];
    fn matches(index: &usize, present: &[bool]) -> bool {
        present[*index]
    }
    fn requires(index: &usize, component: usize) -> bool {
        *index == component
    }
    unsafe fn fetch(table: &Table, index: &usize, _: u64) -> ColumnPtr {
        table.columns[*index]
    }
    fn direct(fetch: &ColumnPtr) -> bool {
        fetch.direct()
    }
    unsafe fn get<'a>(fetch: &ColumnPtr, row: usize, _: u64) -> &'a T {
        unsafe { &*fetch.value::<T>(row) }
    }
    unsafe fn contiguous(fetch: &ColumnPtr, len: usize) -> bool {
        unsafe { fetch.contiguous(0, len) }
    }
    unsafe fn slice<'a>(fetch: &ColumnPtr, len: usize, _: u64) -> &'a [T] {
        unsafe { slice::from_raw_parts(fetch.value::<T>(0), len) }
    }
}
unsafe impl<T: 'static, S: ComponentLookup<T>> FetchQuery<S> for &T {
    const VALID: bool = S::COUNT == 1;
    const INDICES: usize = S::INDEX;
}
impl<T: 'static> QueryData for &mut T {
    type Access = Write<T>;
    type Indices = usize;
    type Fetch = ColumnPtr;
    type Item<'w> = Mut<'w, T>;
    type Slice<'w> = MutSlice<'w, T>;
    fn matches(index: &usize, present: &[bool]) -> bool {
        present[*index]
    }
    fn requires(index: &usize, component: usize) -> bool {
        *index == component
    }
    unsafe fn fetch(table: &Table, index: &usize, tick: u64) -> ColumnPtr {
        let column = table.columns[*index];
        unsafe { column.touch(tick) };
        column
    }
    fn direct(fetch: &ColumnPtr) -> bool {
        fetch.direct()
    }
    unsafe fn get<'a>(fetch: &ColumnPtr, row: usize, tick: u64) -> Mut<'a, T> {
        unsafe { Mut::new(&mut *fetch.value::<T>(row), &mut *fetch.changed(row), tick) }
    }
    unsafe fn contiguous(fetch: &ColumnPtr, len: usize) -> bool {
        unsafe { fetch.contiguous(0, len) }
    }
    unsafe fn slice<'a>(fetch: &ColumnPtr, len: usize, tick: u64) -> MutSlice<'a, T> {
        unsafe {
            MutSlice::new(
                slice::from_raw_parts_mut(fetch.value::<T>(0), len),
                slice::from_raw_parts_mut(fetch.changed(0), len),
                tick,
            )
        }
    }
}
unsafe impl<T: 'static, S: ComponentLookup<T>> FetchQuery<S> for &mut T {
    const VALID: bool = S::COUNT == 1;
    const INDICES: usize = S::INDEX;
}
impl QueryData for Entity {
    type Access = ();
    type Indices = ();
    type Fetch = *const Entity;
    type Item<'w> = Entity;
    type Slice<'w> = &'w [Entity];
    fn matches(_: &(), _: &[bool]) -> bool {
        true
    }
    fn requires(_: &(), _: usize) -> bool {
        false
    }
    unsafe fn fetch(table: &Table, _: &(), _: u64) -> Self::Fetch {
        table.entities.as_ptr()
    }
    fn direct(_: &Self::Fetch) -> bool {
        true
    }
    unsafe fn get<'a>(fetch: &Self::Fetch, row: usize, _: u64) -> Self::Item<'a> {
        unsafe { *fetch.add(row) }
    }
    unsafe fn contiguous(_: &Self::Fetch, _: usize) -> bool {
        true
    }
    unsafe fn slice<'a>(fetch: &Self::Fetch, len: usize, _: u64) -> &'a [Entity] {
        unsafe { slice::from_raw_parts(*fetch, len) }
    }
}
unsafe impl<S: Storage> FetchQuery<S> for Entity {
    const VALID: bool = true;
    const INDICES: () = ();
}
macro_rules! tuple_query {
    ($access:ty; $($data:ident:$index:tt),+) => {
        impl<$($data: QueryData),+> QueryData for ($($data,)+) {
            type Access = $access; type Indices = ($($data::Indices,)+); type Fetch = ($($data::Fetch,)+);
            type Item<'w> = ($($data::Item<'w>,)+); type Slice<'w> = ($($data::Slice<'w>,)+);
            fn matches(indices: &Self::Indices, present: &[bool]) -> bool { true $(&& $data::matches(&indices.$index, present))+ }
            fn requires(indices: &Self::Indices, component: usize) -> bool { false $(|| $data::requires(&indices.$index, component))+ }
            unsafe fn fetch(table: &Table, indices: &Self::Indices, tick: u64) -> Self::Fetch { unsafe { ($($data::fetch(table, &indices.$index, tick),)+) } }
            fn direct(fetch: &Self::Fetch) -> bool { true $(&& $data::direct(&fetch.$index))+ }
            unsafe fn get<'a>(fetch: &Self::Fetch, row: usize, tick: u64) -> Self::Item<'a> { unsafe { ($($data::get(&fetch.$index, row, tick),)+) } }
            unsafe fn contiguous(fetch: &Self::Fetch, len: usize) -> bool { true $(&& unsafe { $data::contiguous(&fetch.$index, len) })+ }
            unsafe fn slice<'a>(fetch: &Self::Fetch, len: usize, tick: u64) -> Self::Slice<'a> { unsafe { ($($data::slice(&fetch.$index, len, tick),)+) } }
        }
        unsafe impl<S: Storage, $($data: FetchQuery<S>),+> FetchQuery<S> for ($($data,)+) { const VALID: bool = true $(&& $data::VALID)+; const INDICES: Self::Indices = ($($data::INDICES,)+); }
    };
}
tuple_query!((A::Access, B::Access); A:0, B:1);
tuple_query!((A::Access, (B::Access, C::Access)); A:0, B:1, C:2);
impl<S: Storage> World<S> {
    pub fn query<D: FetchQuery<S>>(&mut self) -> Query<'_, D>
    where
        D::Access: ValidAccess,
    {
        self.query_filtered::<D, ()>()
    }
    pub fn query_filtered<D: FetchQuery<S>, F: FilterFor<S>>(&mut self) -> Query<'_, D, F>
    where
        D::Access: ValidAccess,
    {
        let now = if D::Access::WRITES {
            self.change_tick()
        } else {
            self.latest_tick()
        };
        let mut query = unsafe { Query::from_world(self, ChangeWindow { last: 0, now }) };
        query.matching = candidate::<D, F>(&query.indices, &query.filter, &self.component_tables)
            .map(|i| self.component_tables[i].as_slice());
        query
    }
    pub fn single<D: FetchQuery<S>>(&mut self) -> Option<Single<'_, D>>
    where
        D::Access: ValidAccess,
    {
        self.query::<D>().single()
    }
    pub fn single_filtered<D: FetchQuery<S>, F: FilterFor<S>>(&mut self) -> Option<Single<'_, D, F>>
    where
        D::Access: ValidAccess,
    {
        self.query_filtered::<D, F>().single()
    }
}
