use crate::{
    change::{Mut, SystemWorld},
    column::{Column, Info, Store, Summary},
    lookup::ComponentLookup,
};
use std::{
    marker::PhantomData,
    num::NonZeroUsize,
    ops::{Deref, Index},
    sync::atomic::{AtomicU64, Ordering},
};

/// World-specific generational handle. Exhausted generations retire their slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Entity {
    world: u64,
    slot: u32,
    generation: u32,
}
/// Declares a dense component; SUMMARY opts into conservative column summaries.
pub struct Components<T, const SUMMARY: bool = false>(PhantomData<T>);
/// Declares archetype-local sparse storage. Sparse-only changes preserve the
/// physical dense row; sparse columns remain aligned with archetype membership.
pub struct Sparse<T, const SUMMARY: bool = false>(PhantomData<T>);
/// Declares an exact bundle type with a compiler-selected destination cache.
pub struct BundleSlot<B>(PhantomData<fn() -> B>);
impl<T, const B: bool> Default for Components<T, B> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<T, const B: bool> Default for Sparse<T, B> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
impl<B> Default for BundleSlot<B> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
mod sealed {
    pub trait Sealed {}
    impl Sealed for () {}
    impl<T, const B: bool> Sealed for super::Components<T, B> {}
    impl<T, const B: bool> Sealed for super::Sparse<T, B> {}
    impl<B> Sealed for super::BundleSlot<B> {}
    impl<A: super::Storage, B: super::Storage> Sealed for (A, B) {}
}
/// Sealed, statically composed component and bundle vocabulary.
#[allow(private_interfaces)]
pub trait Storage: sealed::Sealed + Default {
    const LEN: usize;
    const BUNDLES: usize;
    const HAS_SPARSE: bool;
    #[doc(hidden)]
    type BundleCache: Default + Send + Sync;
    #[doc(hidden)]
    fn describe(output: &mut Vec<Info>);
}
impl Storage for () {
    const LEN: usize = 0;
    const BUNDLES: usize = 0;
    const HAS_SPARSE: bool = false;
    type BundleCache = ();
    fn describe(_: &mut Vec<Info>) {}
}
macro_rules! schema {
    ($name:ident, $sparse:expr) => {
        impl<T, const SUMMARY: bool> Storage for $name<T, SUMMARY> {
            const LEN: usize = 1;
            const BUNDLES: usize = 0;
            const HAS_SPARSE: bool = $sparse;
            type BundleCache = ();
            fn describe(output: &mut Vec<Info>) {
                output.push(Info::of::<T>($sparse, SUMMARY));
            }
        }
    };
}
schema!(Components, false);
schema!(Sparse, true);
impl<B: 'static> Storage for BundleSlot<B> {
    const LEN: usize = 0;
    const BUNDLES: usize = 1;
    const HAS_SPARSE: bool = false;
    type BundleCache = Option<NonZeroUsize>;
    fn describe(_: &mut Vec<Info>) {}
}
impl<A: Storage, B: Storage> Storage for (A, B) {
    const LEN: usize = A::LEN + B::LEN;
    const BUNDLES: usize = A::BUNDLES + B::BUNDLES;
    const HAS_SPARSE: bool = A::HAS_SPARSE || B::HAS_SPARSE;
    type BundleCache = (A::BundleCache, B::BundleCache);
    fn describe(output: &mut Vec<Info>) {
        A::describe(output);
        B::describe(output);
    }
}
trait BundleLookup<B: 'static>: Storage {
    const COUNT: usize;
    const INDEX: usize;
    fn slot(cache: &mut Self::BundleCache) -> &mut Option<NonZeroUsize>;
}
impl<B: 'static, S: Storage> BundleLookup<B> for S {
    default const COUNT: usize = 0;
    default const INDEX: usize = 0;
    default fn slot(_: &mut Self::BundleCache) -> &mut Option<NonZeroUsize> {
        panic!("missing bundle slot")
    }
}
impl<B: 'static> BundleLookup<B> for BundleSlot<B> {
    const COUNT: usize = 1;
    const INDEX: usize = 0;
    fn slot(cache: &mut Self::BundleCache) -> &mut Option<NonZeroUsize> {
        cache
    }
}
impl<B: 'static, L: BundleLookup<B>, R: BundleLookup<B>> BundleLookup<B> for (L, R) {
    const COUNT: usize = L::COUNT + R::COUNT;
    const INDEX: usize = if L::COUNT != 0 {
        L::INDEX
    } else {
        L::BUNDLES + R::INDEX
    };
    fn slot(cache: &mut Self::BundleCache) -> &mut Option<NonZeroUsize> {
        if L::COUNT != 0 {
            L::slot(&mut cache.0)
        } else {
            R::slot(&mut cache.1)
        }
    }
}

/// Immutable access metadata, separate from all owned component buffers.
#[doc(hidden)]
#[derive(Clone, Copy, Default)]
pub struct ColumnPtr {
    data: *mut u8,
    added: *mut u64,
    changed: *mut u64,
    summary: *const Summary,
    rows: *const usize,
}
unsafe impl Send for ColumnPtr {}
unsafe impl Sync for ColumnPtr {}
impl ColumnPtr {
    fn new(column: &Column, rows: *const usize) -> Self {
        Self {
            data: column.data,
            added: column.added,
            changed: column.changed,
            summary: column.summary.as_deref().map_or(std::ptr::null(), |s| s),
            rows,
        }
    }
    unsafe fn row(self, row: usize) -> usize {
        if self.rows.is_null() {
            row
        } else {
            unsafe { *self.rows.add(row) }
        }
    }
    pub(crate) unsafe fn value<T>(self, row: usize) -> *mut T {
        unsafe { self.data.cast::<T>().add(self.row(row)) }
    }
    pub(crate) unsafe fn added(self, row: usize) -> *mut u64 {
        unsafe { self.added.add(self.row(row)) }
    }
    pub(crate) unsafe fn changed(self, row: usize) -> *mut u64 {
        unsafe { self.changed.add(self.row(row)) }
    }
    pub(crate) unsafe fn added_summary<'a>(self) -> Option<&'a AtomicU64> {
        unsafe { self.summary.as_ref().map(|s| &s.added) }
    }
    pub(crate) unsafe fn changed_summary<'a>(self) -> Option<&'a AtomicU64> {
        unsafe { self.summary.as_ref().map(|s| &s.changed) }
    }
    pub(crate) unsafe fn contiguous(self, start: usize, len: usize) -> bool {
        if self.rows.is_null() || len < 2 {
            return true;
        }
        let first = unsafe { *self.rows.add(start) };
        (1..len).all(|i| unsafe { *self.rows.add(start + i) == first + i })
    }
    unsafe fn mark(self, row: usize, tick: u64, added: bool) {
        unsafe {
            self.changed(row).write(tick);
            if let Some(summary) = self.changed_summary() {
                summary.fetch_max(tick, Ordering::Relaxed);
            }
            if added {
                self.added(row).write(tick);
                if let Some(summary) = self.added_summary() {
                    summary.fetch_max(tick, Ordering::Relaxed);
                }
            }
        }
    }
}
/// Static schema indices select compact present-only pointer descriptors.
pub(crate) struct Columns {
    map: Vec<usize>,
    values: Vec<ColumnPtr>,
}
impl Index<usize> for Columns {
    type Output = ColumnPtr;
    fn index(&self, index: usize) -> &ColumnPtr {
        &self.values[self.map[index]]
    }
}
/// Immutable entity metadata references a store's entity allocation.
#[derive(Clone, Copy)]
pub(crate) struct Entities {
    pointer: *const Entity,
    len: usize,
}
impl Deref for Entities {
    type Target = [Entity];
    fn deref(&self) -> &[Entity] {
        unsafe { std::slice::from_raw_parts(self.pointer, self.len) }
    }
}
unsafe impl Send for Entities {}
unsafe impl Sync for Entities {}
#[doc(hidden)]
pub struct Table {
    pub(crate) present: Vec<bool>,
    pub(crate) entities: Entities,
    pub(crate) columns: Columns,
}
#[derive(Clone, Copy)]
struct Location {
    table: usize,
    row: usize,
}
struct Slot {
    generation: u32,
    location: Option<Location>,
    next: Option<u32>,
}
struct Archetype {
    dense: usize,
    sparse: Store,
    rows: Vec<usize>,
    components: Vec<usize>,
    transitions: Vec<[Option<NonZeroUsize>; 2]>,
    bundles: Vec<[Option<NonZeroUsize>; 2]>,
}
/// Dynamic archetypes over a compile-time component vocabulary.
pub struct World<S: Storage> {
    id: u64,
    locations: Vec<Slot>,
    free: Option<u32>,
    free_count: usize,
    pub(crate) tables: Vec<Table>,
    pub(crate) component_tables: Vec<Vec<usize>>,
    info: Vec<Info>,
    dense: Vec<Store>,
    archetypes: Vec<Archetype>,
    bundles: S::BundleCache,
    /// The latest issued change tick; systems carry their own copy during a frame.
    clock: u64,
    schema: PhantomData<S>,
}
impl<S: Storage> Default for World<S> {
    fn default() -> Self {
        Self::new(S::default())
    }
}
impl<S: Storage> World<S> {
    pub fn new(_: S) -> Self {
        static NEXT_WORLD: AtomicU64 = AtomicU64::new(0);
        let id = NEXT_WORLD
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |id| id.checked_add(1))
            .expect("world identifiers exhausted");
        let mut info = Vec::with_capacity(S::LEN);
        S::describe(&mut info);
        let mut world = Self {
            id,
            locations: Vec::new(),
            free: None,
            free_count: 0,
            tables: Vec::new(),
            component_tables: (0..S::LEN).map(|_| Vec::new()).collect(),
            info,
            dense: Vec::new(),
            archetypes: Vec::new(),
            bundles: Default::default(),
            clock: 0,
            schema: PhantomData,
        };
        world.add_table(vec![false; S::LEN]);
        world
    }
    fn add_table(&mut self, present: Vec<bool>) -> usize {
        let components: Vec<_> = present
            .iter()
            .enumerate()
            .filter_map(|(i, &p)| p.then_some(i))
            .collect();
        let dense_indices: Vec<_> = components
            .iter()
            .copied()
            .filter(|&i| !self.info[i].sparse)
            .collect();
        let sparse_indices: Vec<_> = components
            .iter()
            .copied()
            .filter(|&i| self.info[i].sparse)
            .collect();
        let dense = self
            .dense
            .iter()
            .position(|s| s.indices == dense_indices)
            .unwrap_or_else(|| {
                self.dense.push(Store::new(dense_indices, &self.info));
                self.dense.len() - 1
            });
        let mut map = vec![usize::MAX; S::LEN];
        for (position, &component) in components.iter().enumerate() {
            map[component] = position;
            self.component_tables[component].reserve(1);
        }
        let id = self.tables.len();
        self.tables.reserve(1);
        self.archetypes.reserve(1);
        self.dense[dense].archetypes.reserve(1);
        let columns = Columns {
            values: vec![ColumnPtr::default(); components.len()],
            map,
        };
        let archetype = Archetype {
            dense,
            sparse: Store::new(sparse_indices, &self.info),
            rows: Vec::new(),
            components,
            transitions: vec![[None; 2]; S::LEN],
            bundles: vec![[None; 2]; S::BUNDLES],
        };
        self.tables.push(Table {
            present,
            entities: Entities {
                pointer: std::ptr::NonNull::dangling().as_ptr(),
                len: 0,
            },
            columns,
        });
        self.archetypes.push(archetype);
        self.dense[dense].archetypes.push(id);
        for &component in &self.archetypes[id].components {
            self.component_tables[component].push(id);
        }
        self.refresh(id);
        id
    }
    fn refresh(&mut self, table: usize) {
        let archetype = &self.archetypes[table];
        let dense = &self.dense[archetype.dense];
        let entities = if S::HAS_SPARSE {
            &archetype.sparse.entities
        } else {
            &dense.entities
        };
        self.tables[table].entities = Entities {
            pointer: entities.as_ptr(),
            len: entities.len(),
        };
        let rows = if S::HAS_SPARSE {
            archetype.rows.as_ptr()
        } else {
            std::ptr::null()
        };
        for (i, &component) in dense.indices.iter().enumerate() {
            let index = self.tables[table].columns.map[component];
            self.tables[table].columns.values[index] = ColumnPtr::new(&dense.columns[i], rows);
        }
        for (i, &component) in archetype.sparse.indices.iter().enumerate() {
            let index = self.tables[table].columns.map[component];
            self.tables[table].columns.values[index] =
                ColumnPtr::new(&archetype.sparse.columns[i], std::ptr::null());
        }
    }
    fn refresh_len(&mut self, table: usize) {
        self.tables[table].entities.len = if S::HAS_SPARSE {
            self.archetypes[table].sparse.entities.len()
        } else {
            self.dense[self.archetypes[table].dense].entities.len()
        };
    }
    fn reserve_rows(&mut self, table: usize, additional: usize, dense_rows: bool) {
        let dense = self.archetypes[table].dense;
        if dense_rows && self.dense[dense].reserve(additional) {
            for i in 0..self.dense[dense].archetypes.len() {
                self.refresh(self.dense[dense].archetypes[i]);
            }
        }
        if S::HAS_SPARSE {
            let archetype = &mut self.archetypes[table];
            // Reserve mapping first; refresh even if a later capacity check unwinds.
            let before = archetype.rows.capacity();
            archetype.rows.reserve(additional);
            if before != archetype.rows.capacity() {
                self.refresh(table);
            }
            if self.archetypes[table].sparse.reserve(additional) {
                self.refresh(table);
            }
        }
    }
    fn reserve_slots(&mut self, additional: usize) {
        let fresh = additional.saturating_sub(self.free_count);
        assert!(
            self.locations
                .len()
                .checked_add(fresh)
                .is_some_and(|n| n <= u32::MAX as usize),
            "entity slots exhausted"
        );
        self.locations.reserve(fresh);
    }
    fn allocate(&mut self) -> Entity {
        if let Some(slot) = self.free {
            let entry = &mut self.locations[slot as usize];
            self.free = entry.next;
            self.free_count -= 1;
            Entity {
                world: self.id,
                slot,
                generation: entry.generation,
            }
        } else {
            let slot = u32::try_from(self.locations.len()).expect("entity slots exhausted");
            self.locations.push(Slot {
                generation: 0,
                location: None,
                next: None,
            });
            Entity {
                world: self.id,
                slot,
                generation: 0,
            }
        }
    }
    fn location(&self, entity: Entity) -> Option<Location> {
        if entity.world != self.id {
            return None;
        }
        let slot = self.locations.get(entity.slot as usize)?;
        if slot.generation != entity.generation {
            return None;
        }
        slot.location
    }
    pub fn contains(&self, entity: Entity) -> bool {
        self.location(entity).is_some()
    }
    fn check_bundle<B: Bundle<S>>() {
        const {
            assert!(
                B::VALID,
                "bundle components must exist uniquely and must not repeat"
            );
        }
        const {
            assert!(
                <S as BundleLookup<B>>::COUNT == 1,
                "bundle must have exactly one BundleSlot in the world schema"
            );
        }
    }
    #[inline]
    fn bundle_table<B: Bundle<S>>(&mut self) -> usize {
        Self::check_bundle::<B>();
        if let Some(table) = *<S as BundleLookup<B>>::slot(&mut self.bundles) {
            return table.get() - 1;
        }
        self.resolve_bundle::<B>()
    }
    #[cold]
    fn resolve_bundle<B: Bundle<S>>(&mut self) -> usize {
        let table = self
            .tables
            .iter()
            .position(|table| {
                table
                    .present
                    .iter()
                    .enumerate()
                    .all(|(i, &p)| p == B::INDICES.contains(&i))
            })
            .unwrap_or_else(|| {
                self.add_table((0..S::LEN).map(|i| B::INDICES.contains(&i)).collect())
            });
        *<S as BundleLookup<B>>::slot(&mut self.bundles) = NonZeroUsize::new(table + 1);
        table
    }
    fn destination(
        &mut self,
        source: usize,
        indices: &[usize],
        adding: bool,
        bundle_slot: Option<usize>,
    ) -> usize {
        let cached = if let Some(slot) = bundle_slot {
            self.archetypes[source].bundles[slot][adding as usize]
        } else {
            self.archetypes[source].transitions[indices[0]][adding as usize]
        };
        if let Some(id) = cached {
            return id.get() - 1;
        }
        let mut signature = self.tables[source].present.clone();
        for &i in indices {
            signature[i] = adding;
        }
        let destination = self
            .tables
            .iter()
            .position(|t| t.present == signature)
            .unwrap_or_else(|| self.add_table(signature));
        let id = NonZeroUsize::new(destination + 1);
        if let Some(slot) = bundle_slot {
            self.archetypes[source].bundles[slot][adding as usize] = id;
        } else {
            self.archetypes[source].transitions[indices[0]][adding as usize] = id;
        }
        destination
    }
    /// Spawn directly into the final archetype using a compiler-selected bundle slot.
    pub fn spawn_bundle<B: Bundle<S>>(&mut self, bundle: B) -> Entity {
        let table = self.bundle_table::<B>();
        let tick = self.change_tick();
        self.reserve_slots(1);
        self.spawn_at(table, bundle, tick)
    }
    /// One destination, reservation, and timestamp per batch. A panicking iterator
    /// leaves its completed prefix live; unknown-length iterators grow amortized.
    pub fn spawn_batch<I: IntoIterator>(&mut self, bundles: I) -> Vec<Entity>
    where
        I::Item: Bundle<S>,
    {
        Self::check_bundle::<I::Item>();
        let mut bundles = bundles.into_iter();
        let Some(first) = bundles.next() else {
            return Vec::new();
        };
        let table = self.bundle_table::<I::Item>();
        let tick = self.change_tick();
        let count = bundles.size_hint().0.saturating_add(1);
        self.reserve_slots(count);
        self.reserve_rows(table, count, true);
        let mut entities = Vec::with_capacity(count);
        entities.push(self.spawn_at(table, first, tick));
        for bundle in bundles {
            entities.push(self.spawn_at(table, bundle, tick));
        }
        entities
    }
    fn spawn_at<B: Bundle<S>>(&mut self, table: usize, bundle: B, tick: u64) -> Entity {
        if self.free.is_none() && self.locations.len() == self.locations.capacity() {
            self.reserve_slots(1);
        }
        self.reserve_rows(table, 1, true);
        let entity = self.allocate();
        let dense = self.archetypes[table].dense;
        let row = self.tables[table].entities.len();
        if S::HAS_SPARSE {
            self.archetypes[table]
                .rows
                .push(self.dense[dense].entities.len());
        }
        // Sealed typed writes cannot allocate, drop user values, or unwind.
        unsafe { bundle.write(&self.tables[table], row, tick) };
        self.dense[dense].entities.push(entity);
        if S::HAS_SPARSE {
            self.archetypes[table].sparse.entities.push(entity);
        }
        self.locations[entity.slot as usize].location = Some(Location { table, row });
        self.refresh_len(table);
        entity
    }
    pub fn spawn(&mut self) -> Entity {
        self.reserve_slots(1);
        self.reserve_rows(0, 1, true);
        let entity = self.allocate();
        let dense = self.archetypes[0].dense;
        let row = self.tables[0].entities.len();
        if S::HAS_SPARSE {
            self.archetypes[0]
                .rows
                .push(self.dense[dense].entities.len());
            self.archetypes[0].sparse.entities.push(entity);
        }
        self.dense[dense].entities.push(entity);
        self.locations[entity.slot as usize].location = Some(Location { table: 0, row });
        self.refresh_len(0);
        entity
    }
    fn physical_row(&self, location: Location) -> usize {
        if S::HAS_SPARSE {
            self.archetypes[location.table].rows[location.row]
        } else {
            location.row
        }
    }
    fn prepare_move(&mut self, location: Location, destination: usize) -> usize {
        if location.table == destination {
            return location.row;
        }
        let moving_dense =
            self.archetypes[location.table].dense != self.archetypes[destination].dense;
        self.reserve_rows(destination, 1, moving_dense);
        let row = self.tables[destination].entities.len();
        if S::HAS_SPARSE {
            let dense_row = if moving_dense {
                self.dense[self.archetypes[destination].dense]
                    .entities
                    .len()
            } else {
                self.physical_row(location)
            };
            self.archetypes[destination].rows.push(dense_row);
        }
        row
    }
    fn detach_dense(&mut self, dense: usize, row: usize) -> Entity {
        let entity = self.dense[dense].entities.swap_remove(row);
        if let Some(&moved) = self.dense[dense].entities.get(row) {
            let location = self.locations[moved.slot as usize]
                .location
                .as_mut()
                .unwrap();
            if S::HAS_SPARSE {
                self.archetypes[location.table].rows[location.row] = row;
            } else {
                location.row = row;
            }
        }
        entity
    }
    fn detach_sparse(&mut self, location: Location) {
        let archetype = &mut self.archetypes[location.table];
        archetype.sparse.entities.swap_remove(location.row);
        archetype.rows.swap_remove(location.row);
        if let Some(&moved) = archetype.sparse.entities.get(location.row) {
            self.locations[moved.slot as usize]
                .location
                .as_mut()
                .unwrap()
                .row = location.row;
        }
    }
    fn transfer(&mut self, location: Location, destination: usize) {
        if location.table == destination {
            return;
        }
        let entity = self.tables[location.table].entities[location.row];
        let row = self.tables[destination].entities.len();
        let source_dense = self.archetypes[location.table].dense;
        let target_dense = self.archetypes[destination].dense;
        if source_dense != target_dense {
            let physical = self.physical_row(location);
            unsafe { self.dense[source_dense].transfer(physical, &self.dense[target_dense]) };
            self.detach_dense(source_dense, physical);
            self.dense[target_dense].entities.push(entity);
        }
        if S::HAS_SPARSE {
            unsafe {
                self.archetypes[location.table]
                    .sparse
                    .transfer(location.row, &self.archetypes[destination].sparse)
            };
            self.detach_sparse(location);
            self.archetypes[destination].sparse.entities.push(entity);
        }
        self.locations[entity.slot as usize].location = Some(Location {
            table: destination,
            row,
        });
        self.refresh_len(location.table);
        self.refresh_len(destination);
    }
    /// Replace/insert every bundle component with at most one dense migration.
    /// Returns the previous values, one Option per tuple member.
    pub fn insert_bundle<B: Bundle<S>>(&mut self, entity: Entity, bundle: B) -> B::Replaced {
        Self::check_bundle::<B>();
        self.insert_inner(entity, bundle, Some(<S as BundleLookup<B>>::INDEX))
    }
    fn insert_inner<B: Bundle<S>>(
        &mut self,
        entity: Entity,
        bundle: B,
        slot: Option<usize>,
    ) -> B::Replaced {
        const {
            assert!(B::VALID, "invalid bundle components");
        }
        let location = self
            .location(entity)
            .expect("entity is dead or belongs to another world");
        let destination = self.destination(location.table, B::INDICES, true, slot);
        let tick = self.change_tick();
        let row = self.prepare_move(location, destination);
        let replaced = unsafe {
            bundle.put(
                &self.tables[location.table],
                location.row,
                &self.tables[destination],
                row,
                tick,
            )
        };
        self.transfer(location, destination);
        replaced
    }
    /// Remove all present members with one transition; absent members return None.
    pub fn remove_bundle<B: Bundle<S>>(&mut self, entity: Entity) -> B::Replaced {
        Self::check_bundle::<B>();
        self.remove_inner::<B>(entity, Some(<S as BundleLookup<B>>::INDEX))
    }
    fn remove_inner<B: Bundle<S>>(&mut self, entity: Entity, slot: Option<usize>) -> B::Replaced {
        const {
            assert!(B::VALID, "invalid bundle components");
        }
        let Some(location) = self.location(entity) else {
            return B::Replaced::default();
        };
        let destination = self.destination(location.table, B::INDICES, false, slot);
        self.prepare_move(location, destination);
        let removed = unsafe { B::take(&self.tables[location.table], location.row) };
        self.transfer(location, destination);
        removed
    }
    pub fn insert<T: 'static>(&mut self, entity: Entity, value: T) -> Option<T>
    where
        S: ComponentLookup<T>,
    {
        self.insert_inner(entity, (value,), None).0
    }
    pub fn remove<T: 'static>(&mut self, entity: Entity) -> Option<T>
    where
        S: ComponentLookup<T>,
    {
        self.remove_inner::<(T,)>(entity, None).0
    }
    pub fn get<T: 'static>(&self, entity: Entity) -> Option<&T>
    where
        S: ComponentLookup<T>,
    {
        const {
            assert!(
                S::COUNT == 1,
                "component must exist exactly once in the vocabulary"
            );
        }
        let location = self.location(entity)?;
        let table = &self.tables[location.table];
        if !table.present[S::INDEX] {
            return None;
        }
        Some(unsafe { &*table.columns[S::INDEX].value::<T>(location.row) })
    }
    pub fn get_mut<T: 'static>(&mut self, entity: Entity) -> Option<Mut<'_, T>>
    where
        S: ComponentLookup<T>,
    {
        const {
            assert!(
                S::COUNT == 1,
                "component must exist exactly once in the vocabulary"
            );
        }
        let location = self.location(entity)?;
        if !self.tables[location.table].present[S::INDEX] {
            return None;
        }
        let tick = self.change_tick();
        let column = self.tables[location.table].columns[S::INDEX];
        Some(unsafe {
            Mut::new(
                &mut *column.value::<T>(location.row),
                &mut *column.changed(location.row),
                column.changed_summary(),
                tick,
            )
        })
    }
    pub fn despawn(&mut self, entity: Entity) -> bool {
        let Some(location) = self.location(entity) else {
            return false;
        };
        let dense = self.archetypes[location.table].dense;
        let physical = self.physical_row(location);
        unsafe { self.dense[dense].prepare_drop(physical) };
        if S::HAS_SPARSE {
            unsafe {
                self.archetypes[location.table]
                    .sparse
                    .prepare_drop(location.row)
            };
        }
        self.detach_dense(dense, physical);
        if S::HAS_SPARSE {
            self.detach_sparse(location);
        }
        let slot = &mut self.locations[entity.slot as usize];
        slot.location = None;
        if let Some(generation) = slot.generation.checked_add(1) {
            slot.generation = generation;
            slot.next = self.free;
            self.free = Some(entity.slot);
            self.free_count += 1;
        }
        self.refresh_len(location.table);
        // Both stores and the entity index are coherent before any destructor.
        struct Finish<'a>(&'a Store, usize);
        impl Drop for Finish<'_> {
            fn drop(&mut self) {
                unsafe { self.0.drop_row(self.1) };
            }
        }
        let sparse = &self.archetypes[location.table].sparse;
        let finish = S::HAS_SPARSE.then(|| Finish(sparse, sparse.entities.len()));
        unsafe { self.dense[dense].drop_row(self.dense[dense].entities.len()) };
        drop(finish);
        true
    }
    /// Issue the next change tick under exclusive access.
    pub(crate) fn change_tick(&mut self) -> u64 {
        self.clock = crate::change::next(self.clock);
        self.clock
    }
    pub(crate) fn latest_tick(&self) -> u64 {
        self.clock
    }
}
impl<S: Storage> SystemWorld for World<S> {
    unsafe fn world_id(world: *mut Self) -> u64 {
        unsafe { (*world).id }
    }
    unsafe fn clock(world: *mut Self) -> u64 {
        unsafe { (*world).clock }
    }
    unsafe fn set_clock(world: *mut Self, latest: u64) {
        unsafe { (*world).clock = latest }
    }
}
mod bundle_sealed {
    pub trait Sealed {}
}
/// Tuples of one through eight unique components.
pub trait Bundle<S: Storage>: bundle_sealed::Sealed + 'static {
    const INDICES: &'static [usize];
    const VALID: bool;
    type Replaced: Default;
    /// # Safety
    /// Unique correctly typed columns and reserved destination row are required.
    unsafe fn write(self, table: &Table, row: usize, tick: u64);
    /// # Safety
    /// Source rows are live; destination rows and any new columns are reserved.
    unsafe fn put(
        self,
        source: &Table,
        row: usize,
        target: &Table,
        target_row: usize,
        tick: u64,
    ) -> Self::Replaced;
    /// # Safety
    /// Present columns are live and removed values will not be dropped again.
    unsafe fn take(table: &Table, row: usize) -> Self::Replaced;
}
macro_rules! bundle {
    ($($t:ident:$i:tt),+) => {
        impl<$($t),+> bundle_sealed::Sealed for ($($t,)+) {}
        impl<S: Storage, $($t: 'static),+> Bundle<S> for ($($t,)+) where $(S: ComponentLookup<$t>),+ {
            const INDICES: &'static [usize] = &[$(<S as ComponentLookup<$t>>::INDEX),+];
            const VALID: bool = {
                let indices = <Self as Bundle<S>>::INDICES; let mut valid = true $(&& <S as ComponentLookup<$t>>::COUNT == 1)+;
                let mut i = 0; while i < indices.len() { let mut j = i + 1; while j < indices.len() { valid = valid && indices[i] != indices[j]; j += 1; } i += 1; } valid
            };
            type Replaced = ($(Option<$t>,)+);
            unsafe fn write(self, table: &Table, row: usize, tick: u64) { $(unsafe {
                let column = table.columns[<S as ComponentLookup<$t>>::INDEX]; column.value::<$t>(row).write(self.$i); column.mark(row, tick, true);
            })+ }
            unsafe fn put(self, source: &Table, row: usize, target: &Table, target_row: usize, tick: u64) -> Self::Replaced {
                ($(unsafe {
                    let index = <S as ComponentLookup<$t>>::INDEX;
                    if source.present[index] {
                        let column = source.columns[index]; let old = column.value::<$t>(row).replace(self.$i); column.mark(row, tick, false); Some(old)
                    } else {
                        let column = target.columns[index]; column.value::<$t>(target_row).write(self.$i); column.mark(target_row, tick, true); None
                    }
                },)+)
            }
            unsafe fn take(table: &Table, row: usize) -> Self::Replaced {
                ($(if table.present[<S as ComponentLookup<$t>>::INDEX] { Some(unsafe { table.columns[<S as ComponentLookup<$t>>::INDEX].value::<$t>(row).read() }) } else { None },)+)
            }
        }
    };
}
bundle!(A:0);
bundle!(A:0, B:1);
bundle!(A:0, B:1, C:2);
bundle!(A:0, B:1, C:2, D:3);
bundle!(A:0, B:1, C:2, D:3, E:4);
bundle!(A:0, B:1, C:2, D:3, E:4, F:5);
bundle!(A:0, B:1, C:2, D:3, E:4, F:5, G:6);
bundle!(A:0, B:1, C:2, D:3, E:4, F:5, G:6, H:7);
