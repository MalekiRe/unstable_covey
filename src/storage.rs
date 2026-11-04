use crate::init::Init;
use crate::require::{HasRequired, MaybeRequires, RequiredInit};
use crate::{
    change::{Mut, SystemWorld},
    column::{Column, Info, Store, Summary},
    command::CommandQueue,
    hook::{Add, Event, HookList, HookTable, Insert, Kind, Remove, Replace},
    lookup::ComponentLookup,
    remote::{Points, Remote},
};
use std::{
    collections::HashMap,
    marker::PhantomData,
    num::NonZeroUsize,
    ops::{Deref, Index},
    ptr::NonNull,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

/// World-specific generational handle. Exhausted generations retire their slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Entity {
    world: u64,
    slot: u32,
    generation: u32,
}
/// Reserves entities from a frame without structural access.
pub(crate) struct Reserve<'a> {
    world: u64,
    base: usize,
    next: &'a AtomicU32,
}
impl Reserve<'_> {
    /// A fresh slot beyond every live one; it becomes an entity when a spawn
    /// command places it. No free-list reuse, so reservations never race.
    pub(crate) fn entity(&self) -> Entity {
        let next = self.next.fetch_add(1, Ordering::Relaxed);
        let slot = u32::try_from(self.base)
            .ok()
            .and_then(|base| base.checked_add(next))
            .expect("entity slots exhausted");
        Entity {
            world: self.world,
            slot,
            generation: 0,
        }
    }
}
/// Declares a dense component; SUMMARY opts into conservative column summaries.
pub struct Components<T, const SUMMARY: bool = false>(PhantomData<fn() -> T>);
/// Declares archetype-local sparse storage. Sparse-only changes preserve the
/// physical dense row; sparse columns remain aligned with archetype membership.
pub struct Sparse<T, const SUMMARY: bool = false>(PhantomData<fn() -> T>);
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
/// Declares a sync point `P`'s request queue: a schema entry that is not a
/// component. Added by placing `sync_point(P)` in the app.
pub struct Point<P>(PhantomData<fn() -> P>);
impl<P> Default for Point<P> {
    fn default() -> Self {
        Self(PhantomData)
    }
}
mod sealed {
    pub trait Sealed {}
    impl Sealed for () {}
    impl<T, const B: bool> Sealed for super::Components<T, B> {}
    impl<T, const B: bool> Sealed for super::Sparse<T, B> {}
    impl<P> Sealed for super::Point<P> {}
    impl<A: super::Storage, B: super::Storage> Sealed for (A, B) {}
}
/// Sealed, statically composed component vocabulary: zero-sized markers,
/// so every world type can cross threads.
pub trait Storage: sealed::Sealed + Default + Send + Sync + 'static {
    const LEN: usize;
    const HAS_SPARSE: bool;
    /// Sync points declared, numbered apart from components.
    #[doc(hidden)]
    const POINTS: usize = 0;
    #[doc(hidden)]
    fn describe(output: &mut Vec<Info>);
}
impl Storage for () {
    const LEN: usize = 0;
    const HAS_SPARSE: bool = false;
    fn describe(_: &mut Vec<Info>) {}
}
macro_rules! schema {
    ($name:ident, $sparse:expr) => {
        impl<T: 'static, const SUMMARY: bool> Storage for $name<T, SUMMARY> {
            const LEN: usize = 1;
            const HAS_SPARSE: bool = $sparse;
            fn describe(output: &mut Vec<Info>) {
                output.push(Info::of::<T>($sparse, SUMMARY));
            }
        }
    };
}
schema!(Components, false);
schema!(Sparse, true);
impl<P: 'static> Storage for Point<P> {
    const LEN: usize = 0;
    const HAS_SPARSE: bool = false;
    const POINTS: usize = 1;
    fn describe(_: &mut Vec<Info>) {}
}
impl<A: Storage, B: Storage> Storage for (A, B) {
    const LEN: usize = A::LEN + B::LEN;
    const HAS_SPARSE: bool = A::HAS_SPARSE || B::HAS_SPARSE;
    const POINTS: usize = A::POINTS + B::POINTS;
    fn describe(output: &mut Vec<Info>) {
        A::describe(output);
        B::describe(output);
    }
}

/// Multi-component transitions, keyed by source table and a bundle's
/// compile-time key. Open addressing; entries verify their exact index list.
#[derive(Default)]
struct Transitions {
    slots: Box<[Option<Transition>]>,
    len: usize,
}
#[derive(Clone, Copy)]
struct Transition {
    source: usize,
    adding: bool,
    indices: &'static [usize],
    destination: usize,
}
impl Transitions {
    fn start(&self, key: u64, source: usize, adding: bool) -> usize {
        let hash = (key ^ (source as u64) << 1 ^ adding as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        (hash >> 32) as usize & (self.slots.len() - 1)
    }
    #[inline]
    fn get(&self, key: u64, source: usize, indices: &[usize], adding: bool) -> Option<usize> {
        if self.slots.is_empty() {
            return None;
        }
        let mut i = self.start(key, source, adding);
        loop {
            let entry = self.slots[i]?;
            if entry.source == source && entry.adding == adding && entry.indices == indices {
                return Some(entry.destination);
            }
            i = (i + 1) & (self.slots.len() - 1);
        }
    }
    #[cold]
    fn insert(&mut self, key: u64, entry: Transition) {
        if (self.len + 1) * 2 > self.slots.len() {
            let capacity = (self.slots.len() * 2).max(16);
            let old = std::mem::replace(&mut self.slots, vec![None; capacity].into_boxed_slice());
            self.len = 0;
            for entry in old.into_iter().flatten() {
                self.insert(bundle_key(entry.indices), entry);
            }
        }
        let mut i = self.start(key, entry.source, entry.adding);
        while self.slots[i].is_some() {
            i = (i + 1) & (self.slots.len() - 1);
        }
        self.slots[i] = Some(entry);
        self.len += 1;
    }
}
/// FNV-1a over a bundle's component indices; evaluated at compile time.
const fn bundle_key(indices: &[usize]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut i = 0;
    while i < indices.len() {
        hash = (hash ^ indices[i] as u64).wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// Immutable access metadata, separate from all owned component buffers.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct ColumnPtr {
    // NonNull lets optimized `Option<item>` checks fold away.
    data: NonNull<u8>,
    added: NonNull<u64>,
    changed: NonNull<u64>,
    summary: *const Summary,
    rows: *const usize,
}
impl crate::init::Init for ColumnPtr {
    #[inline]
    fn init() -> Self {
        Self::default()
    }
}
impl Default for ColumnPtr {
    fn default() -> Self {
        Self {
            data: NonNull::dangling(),
            added: NonNull::dangling(),
            changed: NonNull::dangling(),
            summary: std::ptr::null(),
            rows: std::ptr::null(),
        }
    }
}
unsafe impl Send for ColumnPtr {}
unsafe impl Sync for ColumnPtr {}
impl ColumnPtr {
    fn new(column: &Column, rows: *const usize) -> Self {
        // Column buffers are allocated or dangling, never null.
        unsafe {
            Self {
                data: NonNull::new_unchecked(column.data),
                added: NonNull::new_unchecked(column.added),
                changed: NonNull::new_unchecked(column.changed),
                summary: column.summary.as_deref().map_or(std::ptr::null(), |s| s),
                rows,
            }
        }
    }
    /// Whether rows index the column directly, with no archetype row map.
    pub(crate) fn direct(self) -> bool {
        self.rows.is_null()
    }
    unsafe fn row(self, row: usize) -> usize {
        if self.rows.is_null() {
            row
        } else {
            unsafe { *self.rows.add(row) }
        }
    }
    pub(crate) unsafe fn value<T>(self, row: usize) -> *mut T {
        unsafe { self.data.cast::<T>().add(self.row(row)).as_ptr() }
    }
    pub(crate) unsafe fn added(self, row: usize) -> *mut u64 {
        unsafe { self.added.add(self.row(row)).as_ptr() }
    }
    pub(crate) unsafe fn changed(self, row: usize) -> *mut u64 {
        unsafe { self.changed.add(self.row(row)).as_ptr() }
    }
    pub(crate) unsafe fn added_summary<'a>(self) -> Option<&'a AtomicU64> {
        unsafe { self.summary.as_ref().map(|s| &s.added) }
    }
    pub(crate) unsafe fn changed_summary<'a>(self) -> Option<&'a AtomicU64> {
        unsafe { self.summary.as_ref().map(|s| &s.changed) }
    }
    /// Raise the changed summary before handing out mutable rows. Summaries
    /// only bound row ticks, so raising one for rows left unchanged is exact.
    pub(crate) unsafe fn touch(self, tick: u64) {
        if let Some(summary) = unsafe { self.changed_summary() } {
            summary.fetch_max(tick, Ordering::Relaxed);
        }
    }
    pub(crate) unsafe fn contiguous(self, start: usize, len: usize) -> bool {
        if self.rows.is_null() || len < 2 {
            return true;
        }
        let first = unsafe { *self.rows.add(start) };
        (1..len).all(|i| unsafe { *self.rows.add(start + i) == first + i })
    }
    pub(crate) unsafe fn mark(self, row: usize, tick: u64, added: bool) {
        unsafe {
            self.changed(row).write(tick);
            self.touch(tick);
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
    /// Fragmenting relation pairs `(index of Relates<R>, target)`, sorted.
    pub(crate) targets: Pairs,
    /// Holds the resource entity: no query matches it.
    pub(crate) resource: bool,
    pub(crate) entities: Entities,
    pub(crate) columns: Columns,
}
/// A table's fragmenting relation pairs, sorted.
pub(crate) type Pairs = Box<[(usize, Entity)]>;
/// Every table holding a pair.
pub(crate) type PairIndex = HashMap<(usize, Entity), Vec<usize>>;
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
/// Entity lookups that borrow only the slot index: queries hold one while
/// other parts of the world (hooks, columns) are in use.
#[derive(Clone, Copy)]
pub(crate) struct Locator<'a> {
    world: u64,
    slots: &'a [Slot],
}
impl Locator<'_> {
    /// The live entity's table and logical row.
    pub(crate) fn find(self, entity: Entity) -> Option<(usize, usize)> {
        if entity.world != self.world {
            return None;
        }
        let slot = self.slots.get(entity.slot as usize)?;
        if slot.generation != entity.generation {
            return None;
        }
        slot.location.map(|location| (location.table, location.row))
    }
}
/// What a query item may reach beyond its own row: entity locations, every
/// table, and the pair index. Raw, so fetches stay `Copy`; valid while the
/// query that made it borrows the world.
#[doc(hidden)]
#[derive(Clone, Copy)]
pub struct Reach {
    world: u64,
    slots: *const [Slot],
    tables: *const [Table],
    pairs: *const PairIndex,
}
impl Default for Reach {
    fn default() -> Self {
        Self {
            world: u64::MAX,
            slots: &[],
            tables: &[],
            pairs: std::ptr::null(),
        }
    }
}
impl Reach {
    pub(crate) fn new(locator: Locator<'_>, tables: &[Table], pairs: &PairIndex) -> Self {
        Self {
            world: locator.world,
            slots: locator.slots,
            tables,
            pairs,
        }
    }
    /// The live entity's table and logical row.
    /// # Safety
    /// The world this reach was made from is still borrowed by its query.
    pub(crate) unsafe fn find<'a>(self, entity: Entity) -> Option<(&'a Table, usize)> {
        let locator = Locator {
            world: self.world,
            slots: unsafe { &*self.slots },
        };
        let (table, row) = locator.find(entity)?;
        Some((unsafe { &(&*self.tables)[table] }, row))
    }
    /// # Safety
    /// As `find`.
    pub(crate) unsafe fn table<'a>(self, table: usize) -> &'a Table {
        unsafe { &(&*self.tables)[table] }
    }
    /// The tables holding pair `(relation, target)`.
    /// # Safety
    /// As `find`.
    pub(crate) unsafe fn pair_tables<'a>(self, relation: usize, target: Entity) -> &'a [usize] {
        unsafe { self.pairs.as_ref() }
            .and_then(|pairs| pairs.get(&(relation, target)))
            .map_or(&[], Vec::as_slice)
    }
}
struct Archetype {
    dense: usize,
    sparse: Store,
    rows: Vec<usize>,
    components: Vec<usize>,
    transitions: Vec<[Option<NonZeroUsize>; 2]>,
    /// Inserts of bundles whose members require components, resolved once.
    required: Vec<RequiredMove>,
}
/// Inserting a requiring bundle from one archetype: its destination, and the
/// required components it lacks there, written as `Default`.
struct RequiredMove {
    key: u64,
    indices: &'static [usize],
    destination: usize,
    inits: Box<[RequiredInit]>,
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
    /// Each dense store's pairs: entities with different targets never share rows.
    dense_targets: Vec<Pairs>,
    archetypes: Vec<Archetype>,
    bundles: Transitions,
    pub(crate) pairs: PairIndex,
    /// Cached pair moves: (source table, relation, removed, added) -> table.
    pair_moves: HashMap<(usize, usize, Option<Entity>, Option<Entity>), usize>,
    /// Entities whose fragmenting cascade is running, so cycles terminate.
    dying: Vec<Entity>,
    /// The latest issued change tick; systems carry their own copy during a frame.
    clock: u64,
    /// Fresh slots handed out by `Commands` during a frame, beyond `locations`.
    reserved: AtomicU32,
    /// Commands from hooks and finished systems, applied by `flush`.
    queue: CommandQueue<S>,
    pub(crate) flushing: bool,
    hooks: Option<HookTable<S>>,
    /// Request queues of the schema's sync points; dropping closes them.
    pub(crate) points: Points<S>,
    /// The entity holding every resource, made by the first insert.
    pub(crate) resources: Option<Entity>,
    /// Substates and computed states, in registration order.
    pub(crate) state_links: Vec<crate::state::Link<S>>,
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
            dense_targets: Vec::new(),
            archetypes: Vec::new(),
            bundles: Transitions::default(),
            pairs: HashMap::new(),
            pair_moves: HashMap::new(),
            dying: Vec::new(),
            clock: 0,
            reserved: AtomicU32::new(0),
            queue: CommandQueue::default(),
            flushing: false,
            hooks: None,
            points: Points::new(),
            resources: None,
            state_links: Vec::new(),
            schema: PhantomData,
        };
        world.add_table(vec![false; S::LEN], Box::new([]), false);
        world
    }
    /// A handle for bridging into this world from any thread, through its
    /// sync points; systems can also take it as a `&Remote<_>` parameter.
    pub fn remote(&self) -> Remote<S> {
        self.points.remote.clone()
    }
    /// Tables (archetypes) created so far, including empty ones.
    pub fn archetype_count(&self) -> usize {
        self.tables.len()
    }
    /// Install an app's hooks; a world with hooks owns and runs them.
    pub(crate) fn set_hooks<H: HookList<S> + Send + Sync + 'static>(&mut self, hooks: H) {
        self.hooks = Some(HookTable::new(hooks));
    }
    /// Whether any hook listens for event `E`.
    pub(crate) fn has_hook<E: Event>(&self) -> bool
    where
        S: ComponentLookup<E::Component>,
    {
        self.hooks.as_ref().is_some_and(|hooks| {
            let index = <S as ComponentLookup<E::Component>>::INDEX;
            hooks.entry(index, <E::Kind as Kind>::INDEX).is_some()
        })
    }
    /// Run the hooks for event `E` on `entities`: one indirect call when any
    /// hook listens, nothing otherwise.
    #[inline]
    fn fire<E: Event>(&mut self, entities: &[Entity])
    where
        S: ComponentLookup<E::Component>,
    {
        let Some(hooks) = &self.hooks else {
            return;
        };
        let index = <S as ComponentLookup<E::Component>>::INDEX;
        if let Some((entry, list)) = hooks.entry(index, <E::Kind as Kind>::INDEX) {
            let world: *mut Self = self;
            // SAFETY: hooks reach the world only through `world`.
            unsafe { entry(list, world, entities) };
        }
    }
    /// Run kind `K` hooks for every hooked component the entity's table has,
    /// in component order.
    fn fire_present<K: Kind>(&mut self, entity: Entity, table: usize) {
        let Some(hooks) = &self.hooks else {
            return;
        };
        // Hooks never change structure, tables, or the hook table itself.
        let hooked: *const [usize] = hooks.hooked();
        let world: *mut Self = self;
        unsafe {
            for &index in &*hooked {
                let tables = &(*world).tables;
                if !tables[table].present[index] {
                    continue;
                }
                let hooks = (*world).hooks.as_ref().unwrap_unchecked();
                if let Some((entry, list)) = hooks.entry(index, K::INDEX) {
                    entry(list, world, &[entity]);
                }
            }
        }
    }
    /// Apply queued commands until none remain, breadth first: commands
    /// queued while applying (by hooks) run after the current batch. Nested
    /// calls from those commands return at once.
    pub fn flush(&mut self) {
        if self.flushing {
            return;
        }
        struct Flushing<'a, S: Storage>(&'a mut World<S>);
        impl<S: Storage> Drop for Flushing<'_, S> {
            fn drop(&mut self) {
                self.0.flushing = false;
            }
        }
        self.flushing = true;
        let world = Flushing(self);
        while !world.0.queue.is_empty() {
            let mut queue = std::mem::take(&mut world.0.queue);
            queue.apply(world.0);
            if world.0.queue.is_empty() {
                // Keep the allocation.
                world.0.queue = queue;
            }
        }
    }
    /// After a structural operation: apply what its hooks queued.
    #[inline]
    fn settle(&mut self) {
        if self.hooks.is_some() {
            self.flush();
        }
    }
    /// Resource tables get their own dense stores and stay out of
    /// `component_tables`, so queries never see the resource entity.
    fn add_table(&mut self, present: Vec<bool>, targets: Pairs, resource: bool) -> usize {
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
        let dense = (0..self.dense.len())
            .find(|&d| {
                self.dense[d].indices == dense_indices
                    && self.dense_targets[d] == targets
                    // A store's first table was created with it.
                    && self.tables[self.dense[d].archetypes[0]].resource == resource
            })
            .unwrap_or_else(|| {
                self.dense.push(Store::new(dense_indices, &self.info));
                self.dense_targets.push(targets.clone());
                self.dense.len() - 1
            });
        let mut map = vec![usize::MAX; S::LEN];
        for (position, &component) in components.iter().enumerate() {
            map[component] = position;
            if !resource {
                self.component_tables[component].reserve(1);
            }
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
            required: Vec::new(),
        };
        for &pair in &targets {
            self.pairs.entry(pair).or_default().push(id);
        }
        self.tables.push(Table {
            present,
            targets,
            resource,
            entities: Entities {
                pointer: std::ptr::NonNull::dangling().as_ptr(),
                len: 0,
            },
            columns,
        });
        self.archetypes.push(archetype);
        self.dense[dense].archetypes.push(id);
        if !resource {
            for &component in &self.archetypes[id].components {
                self.component_tables[component].push(id);
            }
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
    /// A handle reserving entities without structural access, reading only the
    /// fields it needs: running hooks own other parts of the world meanwhile.
    /// # Safety
    /// The world is live for 'a and no structural change happens meanwhile.
    pub(crate) unsafe fn reserve<'a>(world: *mut Self) -> Reserve<'a> {
        unsafe {
            Reserve {
                world: (*world).id,
                base: (*world).locations.len(),
                next: &(*world).reserved,
            }
        }
    }
    /// Materialize reserved slots, unplaced, before any other allocation.
    /// `reserve_entity` already bounded every reserved slot index.
    fn flush_reserved(&mut self) {
        let reserved = std::mem::take(self.reserved.get_mut()) as usize;
        self.locations.extend((0..reserved).map(|_| Slot {
            generation: 0,
            location: None,
            next: None,
        }));
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
        let (table, row) = self.locator().find(entity)?;
        Some(Location { table, row })
    }
    fn locator(&self) -> Locator<'_> {
        Locator {
            world: self.id,
            slots: &self.locations,
        }
    }
    /// # Safety
    /// The world is live for 'a with no structural change meanwhile.
    pub(crate) unsafe fn locate<'a>(world: *mut Self) -> Locator<'a> {
        unsafe {
            Locator {
                world: (*world).id,
                slots: &(*world).locations,
            }
        }
    }
    pub fn contains(&self, entity: Entity) -> bool {
        self.location(entity).is_some()
    }
    fn check_bundle<B: Bundle<S>>() {
        const {
            assert!(
                B::VALID,
                "component not stored by this world (no system uses it: register it), or repeated in the bundle"
            );
        }
    }
    /// Where adding bundle B from `source` leads, and for requiring bundles
    /// the cached entry listing the required components to write.
    #[inline]
    fn adding<B: Bundle<S>>(&mut self, source: usize) -> (usize, usize) {
        if !B::REQUIRES {
            return (self.destination::<B>(source, true), 0);
        }
        let entry = match self.archetypes[source]
            .required
            .iter()
            .position(|m| m.key == B::KEY && m.indices == B::INDICES)
        {
            Some(entry) => entry,
            None => self.resolve_required::<B>(source),
        };
        (self.archetypes[source].required[entry].destination, entry)
    }
    #[cold]
    fn resolve_required<B: Bundle<S>>(&mut self, source: usize) -> usize {
        let mut all = Vec::new();
        B::required(&mut all);
        let mut signature = self.tables[source].present.clone();
        let inits: Box<[RequiredInit]> = all
            .into_iter()
            .filter(|r| !signature[r.index] && !B::INDICES.contains(&r.index))
            .collect();
        for &i in B::INDICES {
            signature[i] = true;
        }
        for r in &inits {
            signature[r.index] = true;
        }
        let destination = self.table_for(
            signature,
            self.tables[source].targets.clone(),
            self.tables[source].resource,
        );
        let list = &mut self.archetypes[source].required;
        list.push(RequiredMove {
            key: B::KEY,
            indices: B::INDICES,
            destination,
            inits,
        });
        list.len() - 1
    }
    /// Write the defaults of entry `entry` (from `source`) at `row` of `table`.
    #[inline]
    fn init_required(&self, source: usize, entry: usize, table: usize, row: usize, tick: u64) {
        for r in &self.archetypes[source].required[entry].inits {
            // SAFETY: the destination has the column and reserved `row` for
            // this entity; the source lacked it, so it is uninitialized.
            unsafe { (r.init)(&self.tables[table], r.index, row, tick) };
        }
    }
    /// Kind `K` hooks for the required components entry `entry` added.
    #[inline]
    fn fire_required<K: Kind>(&mut self, source: usize, entry: usize, entities: &[Entity]) {
        if self.hooks.is_none() {
            return;
        }
        for i in 0..self.archetypes[source].required[entry].inits.len() {
            let index = self.archetypes[source].required[entry].inits[i].index;
            let Some((run, list)) = self.hooks.as_ref().unwrap().entry(index, K::INDEX) else {
                continue;
            };
            let world: *mut Self = self;
            // SAFETY: hooks reach the world only through `world`, and never
            // move entities or change archetype caches.
            unsafe { run(list, world, entities) };
        }
    }
    /// The table reached from `source` by adding or removing B's components.
    /// Single components use a per-archetype array; bundles a keyed cache.
    #[inline]
    fn destination<B: Bundle<S>>(&mut self, source: usize, adding: bool) -> usize {
        let cached = if B::INDICES.len() == 1 {
            self.archetypes[source].transitions[B::INDICES[0]][adding as usize]
                .map(|id| id.get() - 1)
        } else {
            self.bundles.get(B::KEY, source, B::INDICES, adding)
        };
        match cached {
            Some(destination) => destination,
            None => self.resolve::<B>(source, adding),
        }
    }
    #[cold]
    fn resolve<B: Bundle<S>>(&mut self, source: usize, adding: bool) -> usize {
        let mut signature = self.tables[source].present.clone();
        for &i in B::INDICES {
            signature[i] = adding;
        }
        let destination = self.table_for(
            signature,
            self.tables[source].targets.clone(),
            self.tables[source].resource,
        );
        if B::INDICES.len() == 1 {
            self.archetypes[source].transitions[B::INDICES[0]][adding as usize] =
                NonZeroUsize::new(destination + 1);
        } else {
            let entry = Transition {
                source,
                adding,
                indices: B::INDICES,
                destination,
            };
            self.bundles.insert(B::KEY, entry);
        }
        destination
    }
    /// The table with exactly these components and pairs, reusing an empty
    /// table whose targets died before creating one.
    #[cold]
    fn table_for(&mut self, present: Vec<bool>, targets: Pairs, resource: bool) -> usize {
        let find = |tables: &[Table]| {
            tables.iter().position(|t| {
                t.present == present && t.targets == targets && t.resource == resource
            })
        };
        if let Some(table) = find(&self.tables) {
            return table;
        }
        if !targets.is_empty()
            && self.reuse(&present, &targets)
            && let Some(table) = find(&self.tables)
        {
            return table;
        }
        self.add_table(present, targets, resource)
    }
    /// Retarget an empty dense store whose pairs name a dead entity, so
    /// churning parents do not accumulate tables. Query caches stay valid:
    /// they match on components, which do not change.
    fn reuse(&mut self, present: &[bool], targets: &Pairs) -> bool {
        let dense_indices: Vec<_> = (0..S::LEN)
            .filter(|&i| present[i] && !self.info[i].sparse)
            .collect();
        let Some(dense) = (0..self.dense.len()).find(|&d| {
            self.dense[d].indices == dense_indices
                && self.dense[d].entities.is_empty()
                && self.dense_targets[d]
                    .iter()
                    .any(|&(_, e)| !self.contains(e))
        }) else {
            return false;
        };
        for i in 0..self.dense[dense].archetypes.len() {
            let table = self.dense[dense].archetypes[i];
            for pair in &self.tables[table].targets {
                if let Some(tables) = self.pairs.get_mut(pair) {
                    tables.retain(|&t| t != table);
                    if tables.is_empty() {
                        self.pairs.remove(pair);
                    }
                }
            }
            for &pair in targets {
                self.pairs.entry(pair).or_default().push(table);
            }
            self.tables[table].targets = targets.clone();
        }
        self.dense_targets[dense] = targets.clone();
        // Cached transitions may point into the retargeted tables.
        for archetype in &mut self.archetypes {
            archetype.transitions.fill([None; 2]);
            archetype.required.clear();
        }
        self.bundles = Transitions::default();
        self.pair_moves.clear();
        true
    }
    /// Replace pair `(relation, remove)` with `(relation, add)` in one table
    /// move; either side may be absent. No component changes, no hooks. The
    /// relation's marker is present exactly while a pair of it remains.
    pub(crate) fn move_pair(
        &mut self,
        entity: Entity,
        relation: usize,
        remove: Option<Entity>,
        add: Option<Entity>,
    ) {
        let Some(location) = self.location(entity) else {
            return;
        };
        let key = (location.table, relation, remove, add);
        let destination = match self.pair_moves.get(&key) {
            Some(&destination) => destination,
            None => {
                let mut targets = self.tables[location.table].targets.to_vec();
                if let Some(target) = remove
                    && let Ok(i) = targets.binary_search(&(relation, target))
                {
                    targets.remove(i);
                }
                if let Some(target) = add
                    && let Err(i) = targets.binary_search(&(relation, target))
                {
                    targets.insert(i, (relation, target));
                }
                let mut present = self.tables[location.table].present.clone();
                present[relation] = targets.iter().any(|&(r, _)| r == relation);
                let destination = self.table_for(
                    present,
                    targets.into(),
                    self.tables[location.table].resource,
                );
                self.pair_moves.insert(key, destination);
                destination
            }
        };
        let row = self.prepare_move(location, destination);
        let appears = !self.tables[location.table].present[relation];
        self.transfer(location, destination);
        if self.tables[destination].present[relation] {
            // The zero-sized marker has no value, but its tick slots do: every
            // relink marks it changed (`Relinked<R>`), its first pair added.
            let tick = self.change_tick();
            unsafe { self.tables[destination].columns[relation].mark(row, tick, appears) };
        }
    }
    /// Whether `relation` is installed as fragmenting, and if so, linked.
    pub(crate) fn fragmenting(&self, relation: usize) -> Option<bool> {
        let hooks = self.hooks.as_ref()?;
        let &(_, linked) = hooks.relations().iter().find(|&&(r, _)| r == relation)?;
        Some(linked)
    }
    /// `entity`'s targets along fragmenting `relation`, from its table key.
    pub(crate) fn pair_targets(
        &self,
        entity: Entity,
        relation: usize,
    ) -> impl Iterator<Item = Entity> + '_ {
        let pairs = self
            .location(entity)
            .map_or(&[][..], |location| &self.tables[location.table].targets);
        pairs
            .iter()
            .filter(move |&&(r, _)| r == relation)
            .map(|&(_, target)| target)
    }
    pub(crate) fn has_pair(&self, entity: Entity, relation: usize, target: Entity) -> bool {
        self.location(entity).is_some_and(|location| {
            self.tables[location.table]
                .targets
                .binary_search(&(relation, target))
                .is_ok()
        })
    }
    /// The entities of every table holding `(relation, target)`.
    pub(crate) fn pair_sources(
        &self,
        relation: usize,
        target: Entity,
    ) -> impl Iterator<Item = Entity> + '_ {
        let tables = self.pairs.get(&(relation, target)).into_iter().flatten();
        tables.flat_map(|&table| self.tables[table].entities.iter().copied())
    }
    /// Before `entity` is despawned: despawn the sources of every linked
    /// fragmenting relation targeting it, and move the others out of its pair.
    fn cascade(&mut self, entity: Entity) {
        let Some(hooks) = &self.hooks else {
            return;
        };
        if hooks.relations().is_empty() {
            return;
        }
        self.dying.push(entity);
        let mut sources = Vec::new();
        let mut i = 0;
        while let Some(&(relation, linked)) = self
            .hooks
            .as_ref()
            .and_then(|hooks| hooks.relations().get(i))
        {
            i += 1;
            sources.clear();
            sources.extend(self.pair_sources(relation, entity));
            for &source in &sources {
                if !linked {
                    self.move_pair(source, relation, Some(entity), None);
                } else if !self.dying.contains(&source) {
                    self.despawn(source);
                }
            }
        }
        self.dying.pop();
    }
    /// Spawn directly into the bundle's archetype; no declaration needed.
    pub fn spawn_bundle<B: Bundle<S>>(&mut self, bundle: B) -> Entity {
        Self::check_bundle::<B>();
        self.flush_reserved();
        let (table, entry) = self.adding::<B>(0);
        let tick = self.change_tick();
        self.reserve_slots(1);
        let entity = self.spawn_at(table, entry, bundle, tick);
        self.added::<B>(entry, &[entity]);
        entity
    }
    /// Place a bundle at an entity reserved by `Commands`.
    pub(crate) fn spawn_reserved<B: Bundle<S>>(&mut self, entity: Entity, bundle: B) {
        Self::check_bundle::<B>();
        self.flush_reserved();
        let slot = &self.locations[entity.slot as usize];
        debug_assert!(entity.world == self.id && slot.generation == 0 && slot.location.is_none());
        let (table, entry) = self.adding::<B>(0);
        let tick = self.change_tick();
        self.place(entity, table, entry, bundle, tick);
        self.added::<B>(entry, &[entity]);
    }
    /// Hooks for freshly spawned entities, once per batch.
    #[inline]
    fn added<B: Bundle<S>>(&mut self, entry: usize, entities: &[Entity]) {
        B::fire::<Add>(self, entities, B::ALL);
        if B::REQUIRES {
            self.fire_required::<Add>(0, entry, entities);
        }
        B::fire::<Insert>(self, entities, B::ALL);
        if B::REQUIRES {
            self.fire_required::<Insert>(0, entry, entities);
        }
        self.settle();
    }
    /// One destination, reservation, and timestamp per batch. A panicking iterator
    /// leaves its completed prefix live; unknown-length iterators grow amortized.
    pub fn spawn_batch<I: IntoIterator>(&mut self, bundles: I) -> Vec<Entity>
    where
        I::Item: Bundle<S>,
    {
        Self::check_bundle::<I::Item>();
        self.flush_reserved();
        let mut bundles = bundles.into_iter();
        let Some(first) = bundles.next() else {
            return Vec::new();
        };
        let (table, entry) = self.adding::<I::Item>(0);
        let tick = self.change_tick();
        let count = bundles.size_hint().0.saturating_add(1);
        self.reserve_slots(count);
        self.reserve_rows(table, count, true);
        let mut entities = Vec::with_capacity(count);
        entities.push(self.spawn_at(table, entry, first, tick));
        for bundle in bundles {
            entities.push(self.spawn_at(table, entry, bundle, tick));
        }
        self.added::<I::Item>(entry, &entities);
        entities
    }
    fn spawn_at<B: Bundle<S>>(
        &mut self,
        table: usize,
        entry: usize,
        bundle: B,
        tick: u64,
    ) -> Entity {
        if self.free.is_none() && self.locations.len() == self.locations.capacity() {
            self.reserve_slots(1);
        }
        let entity = self.allocate();
        self.place(entity, table, entry, bundle, tick);
        entity
    }
    /// Write a fresh entity's bundle, and its required defaults (entry
    /// `entry` from the empty archetype), into a new row of `table`.
    fn place<B: Bundle<S>>(
        &mut self,
        entity: Entity,
        table: usize,
        entry: usize,
        bundle: B,
        tick: u64,
    ) {
        self.reserve_rows(table, 1, true);
        let dense = self.archetypes[table].dense;
        let row = self.tables[table].entities.len();
        if S::HAS_SPARSE {
            self.archetypes[table]
                .rows
                .push(self.dense[dense].entities.len());
        }
        // Sealed typed writes cannot allocate, drop user values, or unwind.
        unsafe { bundle.write(&self.tables[table], row, tick) };
        if B::REQUIRES {
            self.init_required(0, entry, table, row, tick);
        }
        self.dense[dense].entities.push(entity);
        if S::HAS_SPARSE {
            self.archetypes[table].sparse.entities.push(entity);
        }
        self.locations[entity.slot as usize].location = Some(Location { table, row });
        self.refresh_len(table);
    }
    pub fn spawn(&mut self) -> Entity {
        self.flush_reserved();
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
        let location = self
            .location(entity)
            .expect("entity is dead or belongs to another world");
        // Replace hooks see old values; hooks never move entities.
        let before = B::mask(&self.tables[location.table].present);
        B::fire::<Replace>(self, &[entity], before);
        let (destination, entry) = self.adding::<B>(location.table);
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
        if B::REQUIRES {
            self.init_required(location.table, entry, destination, row, tick);
        }
        self.transfer(location, destination);
        B::fire::<Add>(self, &[entity], B::ALL & !before);
        if B::REQUIRES {
            self.fire_required::<Add>(location.table, entry, &[entity]);
        }
        B::fire::<Insert>(self, &[entity], B::ALL);
        if B::REQUIRES {
            self.fire_required::<Insert>(location.table, entry, &[entity]);
        }
        self.settle();
        replaced
    }
    /// Remove all present members with one transition; absent members return None.
    pub fn remove_bundle<B: Bundle<S>>(&mut self, entity: Entity) -> B::Replaced {
        Self::check_bundle::<B>();
        let Some(location) = self.location(entity) else {
            return B::Replaced::init();
        };
        let present = B::mask(&self.tables[location.table].present);
        B::fire::<Replace>(self, &[entity], present);
        B::fire::<Remove>(self, &[entity], present);
        let destination = self.destination::<B>(location.table, false);
        self.prepare_move(location, destination);
        let removed = unsafe { B::take(&self.tables[location.table], location.row) };
        self.transfer(location, destination);
        self.settle();
        removed
    }
    pub fn insert<T: 'static>(&mut self, entity: Entity, value: T) -> Option<T>
    where
        S: ComponentLookup<T>,
    {
        self.insert_bundle(entity, (value,)).0
    }
    pub fn remove<T: 'static>(&mut self, entity: Entity) -> Option<T>
    where
        S: ComponentLookup<T>,
    {
        self.remove_bundle::<(T,)>(entity).0
    }
    fn not_state<T: 'static>() {
        const {
            assert!(
                !<T as crate::state::IsState>::IS,
                "states change with `set_state`/`clear_state`, which run their transitions"
            );
        }
    }
    /// The resource entity, made on first use in its own empty table.
    pub(crate) fn resource_entity(&mut self) -> Entity {
        if let Some(entity) = self.resources
            && self.contains(entity)
        {
            return entity;
        }
        self.flush_reserved();
        let table = self.table_for(vec![false; S::LEN], Box::new([]), true);
        self.reserve_slots(1);
        self.reserve_rows(table, 1, true);
        let entity = self.allocate();
        let dense = self.archetypes[table].dense;
        let row = self.tables[table].entities.len();
        if S::HAS_SPARSE {
            self.archetypes[table]
                .rows
                .push(self.dense[dense].entities.len());
            self.archetypes[table].sparse.entities.push(entity);
        }
        self.dense[dense].entities.push(entity);
        self.locations[entity.slot as usize].location = Some(Location { table, row });
        self.refresh_len(table);
        self.resources = Some(entity);
        entity
    }
    /// Insert or replace resource `T`, returning the previous value. Its
    /// hooks run as for any component.
    pub fn insert_resource<T: 'static>(&mut self, value: T) -> Option<T>
    where
        S: ComponentLookup<T>,
    {
        Self::not_state::<T>();
        let entity = self.resource_entity();
        self.insert(entity, value)
    }
    /// Insert `T::default()` unless resource `T` exists.
    pub fn init_resource<T: Default + 'static>(&mut self)
    where
        S: ComponentLookup<T>,
    {
        if self.resource::<T>().is_none() {
            self.insert_resource(T::default());
        }
    }
    pub fn remove_resource<T: 'static>(&mut self) -> Option<T>
    where
        S: ComponentLookup<T>,
    {
        Self::not_state::<T>();
        self.remove(self.resources?)
    }
    pub fn resource<T: 'static>(&self) -> Option<&T>
    where
        S: ComponentLookup<T>,
    {
        self.get(self.resources?)
    }
    pub fn resource_mut<T: 'static>(&mut self) -> Option<Mut<'_, T>>
    where
        S: ComponentLookup<T>,
    {
        self.get_mut(self.resources?)
    }
    /// Resource `T`'s column and row, for system parameters.
    /// # Safety
    /// `world` is live; no structural change runs.
    #[inline]
    pub(crate) unsafe fn resource_cell<T: 'static>(world: *const Self) -> Option<(ColumnPtr, usize)>
    where
        S: ComponentLookup<T>,
    {
        let world = unsafe { &*world };
        let location = world.location(world.resources?)?;
        let table = &world.tables[location.table];
        table.present[S::INDEX].then(|| (table.columns[S::INDEX], location.row))
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
                !<T as crate::immutable::IsImmutable>::IS,
                "immutable component: change it by inserting a new value or removing it (relation links: relate, unrelate, or insert `R.to(target)`)"
            );
        }
        self.links_mut(entity)
    }
    /// `get_mut` past the link gate, for relating.
    pub(crate) fn links_mut<T: 'static>(&mut self, entity: Entity) -> Option<Mut<'_, T>>
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
            column.touch(tick);
            Mut::new(
                &mut *column.value::<T>(location.row),
                &mut *column.changed(location.row),
                tick,
            )
        })
    }
    pub fn despawn(&mut self, entity: Entity) -> bool {
        if self.location(entity).is_none() {
            return false;
        }
        self.cascade(entity);
        // The cascade may have moved rows of this entity's table.
        let location = self.location(entity).unwrap();
        self.fire_present::<Replace>(entity, location.table);
        self.fire_present::<Remove>(entity, location.table);
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
        self.settle();
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
    type Queue = CommandQueue<S>;
    unsafe fn queue(world: *mut Self) -> *mut CommandQueue<S> {
        unsafe { &raw mut (*world).queue }
    }
    fn flush(&mut self) {
        World::flush(self);
    }
}
mod bundle_sealed {
    pub trait Sealed {}
}
/// Tuples of one through eight unique components.
pub trait Bundle<S: Storage>: bundle_sealed::Sealed + 'static {
    const INDICES: &'static [usize];
    #[doc(hidden)]
    const KEY: u64 = bundle_key(Self::INDICES);
    const VALID: bool;
    type Replaced: Init;
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
    /// One bit per member.
    #[doc(hidden)]
    const ALL: u16;
    /// The members `present` marks.
    #[doc(hidden)]
    fn mask(present: &[bool]) -> u16;
    /// Fire kind `K` hooks for the members in `members`, in member order.
    #[doc(hidden)]
    fn fire<K: Kind>(world: &mut World<S>, entities: &[Entity], members: u16);
    /// Some member implements `Requires`.
    #[doc(hidden)]
    const REQUIRES: bool;
    /// Everything the members require, recursively, once each.
    #[doc(hidden)]
    fn required(out: &mut Vec<RequiredInit>);
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
            const ALL: u16 = 0 $(| 1 << $i)+;
            const REQUIRES: bool = false $(|| <$t as HasRequired>::HAS)+;
            fn required(out: &mut Vec<RequiredInit>) {
                $(<$t as MaybeRequires<S>>::collect(out);)+
            }
            fn mask(present: &[bool]) -> u16 {
                0 $(| (present[<S as ComponentLookup<$t>>::INDEX] as u16) << $i)+
            }
            #[inline]
            fn fire<K: Kind>(world: &mut World<S>, entities: &[Entity], members: u16) {
                $(if members & 1 << $i != 0 { world.fire::<<K as Kind>::Of<$t>>(entities); })+
            }
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
bundle!(T0:0);
bundle!(T0:0, T1:1);
bundle!(T0:0, T1:1, T2:2);
bundle!(T0:0, T1:1, T2:2, T3:3);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7, T8:8);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7, T8:8, T9:9);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7, T8:8, T9:9, T10:10);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7, T8:8, T9:9, T10:10, T11:11);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7, T8:8, T9:9, T10:10, T11:11, T12:12);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7, T8:8, T9:9, T10:10, T11:11, T12:12, T13:13);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7, T8:8, T9:9, T10:10, T11:11, T12:12, T13:13, T14:14);
bundle!(T0:0, T1:1, T2:2, T3:3, T4:4, T5:5, T6:6, T7:7, T8:8, T9:9, T10:10, T11:11, T12:12, T13:13, T14:14, T15:15);
