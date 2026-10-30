# static-ecs

A small standalone ECS with **nightly full specialization** for open-ended event
and schedule routing. No Bevy, upfront event registry, runtime type registry,
`TypeId`, or boxed systems. System/event callbacks remain statically dispatched.
The library has no external dependencies. Raw component storage retains
destructor function pointers; only published cross-worker continuations use an
erased branch entry. There is no per-system callback table or task-index dispatch.

The pinned toolchain is `nightly-2026-10-25`. Full specialization and the
`gca_min_const_items`/`gca_const_items` features remain experimental; their
compiler warnings are deliberately not suppressed. Associated-constant array
dimensions let rustc compute arbitrary graph topology without a runtime graph
builder. This is not a claim that these Rust features are sound. Typed parameter
fetching uses internal unsafe table-column projections guarded by compile-time
access checks and sealed implementations. `#![deny(unsafe_op_in_unsafe_fn)]`
keeps unsafe operations explicit.

```sh
cargo run --release -p dispatch-demo
cargo run --release --example simulation
cargo test --workspace
cargo test --workspace --release
cargo clippy --workspace --all-targets -- -D clippy::all
```

## The specialization rule

`src/dispatch.rs` stores listeners inline in a heterogeneous `Cons<Head, Tail>`
list. Each `On<Event, Handler>` retains its event type and concrete handler.

```rust
impl<E: 'static, S, H> MaybeHandle<E, S> for H {
    default fn maybe_handle(&mut self, _: &E, _: &mut S) {}
}

impl<E: 'static, S, H: Handle<E, S>> MaybeHandle<E, S> for On<E, H> {
    fn maybe_handle(&mut self, event: &E, state: &mut S) {
        self.handler.handle(event, state);
    }
}
```

`Dispatch` traverses the type-level list oldest-first. Matching event types
select the specialized implementation; other types select the no-op. There are
no runtime event-type comparisons. Calls are statically dispatched; inlining
and elimination of the no-op calls are optimizer decisions, not API guarantees.
The benchmarks below measure specific workloads, not general ECS speedups.

## Events and independent plugins

```rust
use static_ecs::{Bus, Subscribe, subscribe};
struct Click { x: u32 }

fn analytics(total: &mut u32) -> Subscribe<Click, impl FnMut(&Click) + '_> {
    let mut count = 0;
    subscribe(move |event: &Click| {
        count += 1;
        *total += event.x * count;
    })
}

let mut total = 0;
{
    let mut bus = Bus::new().install(analytics(&mut total));
    bus.emit(&Click { x: 3 });
    bus.emit(&Click { x: 3 });
    bus.emit(&42_u64); // Unhandled type: no-op, no declaration needed.
}
assert_eq!(total, 9);
```

- `Bus::on(callback)` adds a listener directly.
- `subscribe(callback)` returns a plugin fragment, without knowing any bus type.
- `.install(fragment)` consumes the bus and returns a new concrete type.
- Nested tuples compose fragments left-to-right, including multiple event types.
- `Subscribe<E, F>` is `On<E, Listener<F>>`; only the closure type is opaque.
  Preserve this event-bearing structure in cross-crate return types rather than
  hiding an entire list behind an opaque trait.
- `E: 'static` excludes event types containing non-static borrowed data. The
  emitted value need not live forever, and closure captures may borrow locals.
  This restriction avoids lifetime-dependent event matching; it is not a proof
  of soundness for the compiler's specialization implementation.


The workspace includes real, separate crates:

| Crate | Contribution |
|---|---|
| `demo-input` | Defines `Click`, without an event trait or registration |
| `demo-analytics` | Stateful `Click` listener |
| `demo-networking` | Another `Click` listener plus its own `PacketReceived` type |
| `dispatch-demo` | Explicitly composes both independent plugins and a local listener |

Neither plugin depends on the other or knows the final listener list. Merely
linking a crate does not register it: composition is explicit. There is no
linker slice, function-pointer registry, or build-time discovery system.

## Archetype storage

The world declares its component vocabulary with `Components<T>` (dense) and
`Sparse<T>` schema markers in nested pairs. Each component type must occur exactly
once. Markers do not own populated columns, and `T` need not implement `Default`.
Archetypes identify complete component sets; archetypes differing only in sparse
components share physical dense storage. Adding/removing a sparse component moves
membership and sparse values without moving that entity’s dense row. Dense-only
schemas compile out the sparse row mapping.

```rust
use static_ecs::prelude::*;
struct Position(f32);
struct Velocity(f32);
struct Active;
type ActorBundle = (Position, Velocity, Active);
type State = (
    (Components<Position>, (Components<Velocity>, Components<Active>)),
    BundleSlot<ActorBundle>,
);
let mut world = World::<State>::default();
let entity = world.spawn(); // Initially has no components.
assert!(world.insert(entity, Position(0.0)).is_none());
world.insert(entity, Velocity(2.0));
world.insert(entity, Active);
for (mut position, velocity) in &mut world.query_filtered::<(&mut Position, &Velocity), With<Active>>() {
    position.0 += velocity.0;
}
assert_eq!(world.get::<Position>(entity).unwrap().0, 2.0);
assert!(world.remove::<Active>(entity).is_some());
assert!(world.despawn(entity));
assert!(!world.contains(entity));
let next = world.spawn_bundle((Position(3.0), Velocity(1.0), Active));
assert_eq!(world.get::<Position>(next).unwrap().0, 3.0);
```

Declare each tuple bundle type once using a zero-sized `BundleSlot<B>` schema
marker, alongside the component markers. Multiple declarations use nested pairs,
for example `(BundleSlot<(Position,)>, BundleSlot<ActorBundle>)`. Then the normal
`world.spawn_bundle(bundle)` and `world.spawn_batch(iterable)` APIs are unchanged;
no handle needs to be prepared or passed.

The compiler selects a dedicated one-word cache field in the world for each
declared bundle. First use resolves or creates the actual archetype; subsequent
spawns load that field directly. There is no `TypeId`, hashing, heap-allocated
bundle registry, or runtime search for a cache slot. The first-use resolver is
kept off the inlinable cached path. Table/column storage still allocates as needed.

Tuple bundles support one through eight components. Missing/duplicate bundle
registrations and repeated/missing component types fail at compile time. A bundle
declaration is its exact tuple type: `(Position, Velocity)` and
`(Velocity, Position)` need separate slots if both forms are used, but resolve
to the same archetype. No intermediate archetype migrations are performed.
Slots are private, world-local, and remain valid when tables empty or grow.

`spawn()` plus individual `insert`/`remove` operations do not require bundle
declarations. Bundle slots consume no component indices or per-archetype storage.

Use `world.spawn_batch(iterable)` for many bundles of one type. It returns entity
handles in input order, resolves the destination once, reserves the iterator’s
lower size hint, and issues one change timestamp for the batch. Unknown-length
iterators grow safely; empty batches create no entities. If iteration panics,
the completed prefix remains in the world.

`insert_bundle(entity, tuple)` and `remove_bundle::<Tuple>(entity)` make one
destination transition for all members, returning a tuple of previous
`Option<T>` values. They require the same exact `BundleSlot<Tuple>` registration
as spawning; absent removal members return `None`. Other components are retained.

Bundle destinations and component/bundle transitions are cached per world.
Spawning writes directly into columns; migration merge-walks sorted present
columns. Reserve, transfer, and destruction traverse present columns, not the
whole component vocabulary. Columns share their store’s length and capacity;
component values and separate added/changed arrays use raw buffers. Only buffers
that grow refresh their pointers. Despawn cleanup finishes remaining columns
even if a component destructor unwinds. Signature/index maps still have
vocabulary-sized metadata; this is not an O(present-components) claim for cold
archetype creation or every transition-cache lookup.

`World::new(Default::default())` with an explicit schema type is also supported;
`World::new(())` has an empty vocabulary. `insert` returns the replaced value, and
`remove::<T>` returns ownership of the removed value. Dense-component changes
move the entity to its destination dense table. Swap-removal repairs locations
and sparse row mappings; query order is not stable. `get::<T>` and `get_mut::<T>`
address a live entity directly.

Entity handles contain a world ID, slot, and generation. Despawned slots are
reused with an incremented generation; exhausted generations retire their slot.
Foreign/stale handles cannot address live rows; insertion through them panics.
The location array now follows peak slot demand rather than cumulative spawns.
Archetypes and retained column capacities can still grow with structural variety.

There is no separate resource storage. Global state is ordinary component data
on singleton entities. Structural mutation requires exclusive world access,
between emissions or in an exclusive system or payload callback.

## Queries, singletons, and filters

- `Query<&T>`, `Query<&mut T>`, `Entity`, and pairs/triples of query data
  (including nested tuples) visit matching table rows. A table missing any
  requested component is skipped. Iterate with `&mut query` or `query.iter_mut()`,
  even for shared data. Typed column pointers are resolved before row iteration.
  Dense-only queries use direct row offsets; dense values in sparse schemas use
  the archetype’s physical-row mapping. Each invocation sees structural changes.
  Registered Query/Single parameters cache matching archetypes and scan only new
  candidates from a required component’s archetype index (choosing the smallest
  list when initializing the cache). Queries without a guaranteed required
  component fall back to the full archetype list. Row counts, change filters,
  and column pointers remain live. World replacement resets the cache. Cache
  data is lazily heap-owned to avoid enormous concrete-system stack frames;
  warmed caches allocate only when their matching-table lists grow.
- `world.query::<D>()` and `world.query_filtered::<D, F>()` borrow the world.
  Mutable items may be retained together from one iteration, but the query
  cannot be iterated again while those items remain borrowed, and the world
  cannot be structurally modified while query items remain borrowed.
- `Single<D, F = ()>` uses the same data and filter rules and requires exactly
  one matching entity across all matching tables. Zero or multiple matches skip
  the system for that invocation, without calling its body or advancing its
  locals. Every `Single` parameter must match; cardinality is checked again on
  the next invocation. Sequential and parallel siblings still run independently.
  An invalid `Single` in a `run_if` condition skips both the condition body and
  its decorated group.
  `Single` dereferences to the query item; `into_inner()` returns that item.
  For example, `Single<&mut T>` dereferences to `Mut<T>`, so a primitive value
  is assigned through `**single`. Struct fields use ordinary autodereferencing.
  Direct `query.single()`, `world.single::<D>()`, and
  `world.single_filtered::<D, F>()` return `Option<Single<...>>`: `None` means
  zero or multiple matches, rather than a panic.
- `With<T>` requires a component and `Without<T>` excludes it. Filter tuples of
  two or three are conjunctions; `Or<(F, G)>` is a disjunction. Filters can nest,
  and structural filters do not fetch or read marker values. All data and filter component
  types must exist uniquely in the declared vocabulary.
- Iterators provide remaining-size bounds. Pure structural queries have an exact
  hint and count table rows without fetching every component. Row-filtered
  queries retain conservative bounds and inspect rows where required; `Single`
  uses the same whole-table cardinality shortcuts.

### Contiguous access

`query.contiguous_mut()` returns `Result<QueryChunks<...>, ContiguousError>`,
yielding one slice (or tuple of slices) per nonempty matching archetype. Shared
data yields `&[T]`, mutable data yields `MutSlice<T>`, and `Entity` yields an
entity slice. There is no chunk-buffer allocation. All tables are validated
before any references are yielded: row filters return `RowFilter`, and scattered
physical rows return `Fragmented`. Sparse membership can fragment dense rows.

`MutSlice::iter_mut` and `get_mut` yield tracked `Mut<T>` values: only mutable
dereferencing marks a row. Taking a raw mutable slice through `as_mut_slice`,
`into_inner`, or mutable dereferencing conservatively marks the entire slice.

Declare `Components<T, true>` or `Sparse<T, true>` to opt into column summaries.
Added/Changed queries can then reject columns whose maximum timestamp precedes
the system’s window. Row timestamps remain authoritative; summaries are
conservative after removal/migration, and updates include atomic maximums.
The default remains disabled.

### Typed query example

```rust
use static_ecs::prelude::*;
struct Position(f32);
struct Velocity(f32);
struct Time(f32);
struct Global;
struct Active;
fn integrate(
    mut query: Query<(&mut Position, &Velocity), With<Active>>,
    time: Single<&Time, With<Global>>,
) {
    for (mut position, velocity) in &mut query {
        position.0 += velocity.0 * time.0;
    }
}
```

## Change detection

Mutable query items and `World::get_mut` return `Mut<T>`. Bind these items with
`mut` before writing. Shared dereferencing does not mark a change; mutable
dereferencing does, even when the eventual value is equal. `Mut::into_inner`
marks a change before returning the underlying `&mut T`.

- `Added<T>` matches insertions since that system’s last successful invocation.
- `Changed<T>` also matches replacements and mutable dereferencing.
- Each system has its own cursor; all its parameters use the same change window.
  First execution sees existing components. Systems skipped by conditions or
  invalid `Single` parameters retain pending changes. Replacing `app.world`
  resets change windows for the new world while preserving system locals.
  A condition that executes advances its own cursor even if it returns false.
  Read-only systems load the latest issued timestamp rather than incrementing
  the shared clock. Writers still receive distinct timestamps, including ordered
  writes within one update; this is not a frame-wide timestamp. Exclusive world
  mutations advance the clock non-atomically under the exclusive borrow.
- Row timestamps move with components through archetype migration and swap-removal.
  Removing and reinserting a component gives it a new added timestamp.
- Change predicates compose with structural filters, tuples, and `Or`.
  `Single` counts rows that pass the complete filter, not just table signatures.
  Filter metadata reads participate in compile-time conflicts, including
  `Query<Entity, Changed<T>>` versus a concurrent writer of `T`.
- Direct `World::query_filtered` calls have no persistent system cursor: their
  baseline is zero. Use a registered system for incremental changed-only work.

This implementation retains monotonic 64-bit per-world clocks, rather than
Bevy’s wrapping 32-bit ticks and aging maintenance. Writers receive timestamps;
read-only systems only observe the clock. Added and changed timestamps are stored
in separate arrays, so changed-only scans need not stream interleaved added
values. Exhaustion panics instead of wrapping. Raw timestamps are not
Bevy-compatible.

## Typed systems and composition

Systems are ordinary functions or capturing closures with up to four parameters.
`Local<T: Default>` belongs to one registration and persists across emissions.
Registering the same function twice creates independent locals.

`examples/simulation.rs` composes systems with:

```rust,ignore
let mut app = App::new(world).add_systems(
    Update,
    ((integrate, decay).parallel(), validate)
        .chain()
        .run_if(running),
);
app.emit(&Update);
```

- Tuples of two through four systems execute sequentially, left-to-right.
  `.chain()` makes that intent explicit, and groups can nest.
- `.parallel()` explicitly joins tuple branches. Access checks reject overlapping
  writes, including nested groups and conditions. Complementary structural
  filters such as `With<Tag>` and `Without<Tag>` permit writes to the same
  component on disjoint populations. Different positive markers alone do not
  prove disjointness, even if current populations happen not to overlap.
- `.run_if(condition)` runs one boolean-returning parameterized function per
  decorated group invocation. Conditions may read queries/singletons and mutate
  their own locals, but may not request mutable world data. Skipped systems do
  not advance their locals.
- `exclusive(|world: &mut World<State>| ...)` can spawn, insert, remove, and
  despawn. It conflicts with every parallel sibling.

`App<State, List>` owns a `World<State>` and concrete listener list.
`.add_systems(Label, systems)` retains the concrete system types; `.emit(&Label)`
selects schedules through specialization in registration order. There is no
runtime schedule lookup. Payload-aware `.on` callbacks and installable
`system(callback)` fragments take `(&Event, &mut World<State>)`. Event-only
plugins retain the independent `Bus` composition API above.

`app.with_execution_mode(ExecutionMode::Serial)` (also available on `Bus`) runs
nested groups directly on the caller without initializing workers. The default
`Parallel` mode also executes directly with a one-worker pool. Entirely sequential
schedules never initialize the default pool merely to count workers. Static
access checks still apply in serial mode.

### Compiled continuations and persistent workers

```rust
use static_ecs::prelude::*;
struct Update;
let pool = WorkerPool::new(4); // Caller plus three persistent worker threads.
let mut app = App::new(World::new(())).add_systems(Update, (|| {}, || {}).parallel());
pool.install(|| app.emit(&Update)); // The closure stays on this calling thread.
```

Without an explicit installation, parallel execution lazily uses a process-wide
pool sized by `available_parallelism`. Explicit pools join their threads on drop.
Concurrent callers sharing a pool serialize their invocations.

Execution structure is encoded in Rust types. `System::enter` takes a typed
projection and successor; `Then` calls the next concrete region directly.
Sequences do not return task IDs to a scheduler, and conditions are ordinary
branches rather than shared gate arrays. Skipping preserves parameter cursors
and locals while resuming the dependent continuation. Nested forks resume at
their typed joins; workers never block waiting for nested child jobs.

Labelled graph topology, access-conflict orientation, transitive reduction,
critical-chain weights, and join indices are constant-evaluated by rustc.
Generated successor calls contain constant-selected projections into concrete
system storage. No runtime topology, task list, task selector, ready queue, or
dependency scan remains. Pure sequential trees and statically serial graphs
execute directly, without constructing invocation state or starting workers.

Only potential cross-worker joins retain readiness state. Single-predecessor
chains pass their result as an ordinary argument. A structural fork first
reserves idle worker capacity: if reservation fails, both branches run locally
without initializing or touching its atomic join. General DAG joins merge
predecessor results and let the last arrival enter the successor directly.

Each persistent worker has one cache-line-aligned publication slot, not a queue.
A published branch carries the invocation pointer and a monomorphized entry
for its entire continuation. Workers prefer their own slot and can steal an
unstarted continuation; stealing transfers the invocation lease to the thief
and immediately frees the original worker. Idle workers park. Publication
precedes unpark, so its token survives the check/park race. Pool construction
waits for Rust worker-thread startup before returning.

`with_min_parallel_work(k)` now checks individual branch weights before
publication, not just total group weight. Functions weigh one, sequential
groups sum their weights, and graph branch estimates stop at the next merge.
The default is one. These are explicit static work units, not measured time
estimates or an automatic profitability model. No available worker means no
publication: the continuation stays local.

One submission mutex serializes independent callers sharing a pool. There is
no normal-path queue/control mutex; panic payload collection still uses a mutex.
Completion waits for every published or stolen invocation lease before releasing
borrowed state. Dynamic reentrant emissions execute serially; statically nested
groups use typed continuations within the same invocation. Systems must not
block waiting for another pool invocation.

This is compiled control flow with runtime placement and cross-worker
synchronization, not a claim that concurrency requires no runtime machinery.
Startup, join-storage construction, and storage/query-cache growth can allocate.
It does not add parallel query iteration or change tick semantics.

### Optional dependency-ready execution

```rust,ignore
app.add_systems(Update, ready((
    node(Input, receive),
    node(Physics, integrate).after(Input),
    node(Audio, update_audio),
)));
```

Labels are unique types within one `ready(...)` graph. Nodes may contain ordinary
systems, chains, parallel groups, and conditions; pairs/triples/quads nest to
compose larger graphs. Missing dependencies, duplicate labels, and cycles fail
during compilation, not while constructing a runtime graph.

Systems stay inline in concrete types. Label dependencies establish a topological
order; otherwise unordered access conflicts are oriented consistently with that
order, and transitive edges are removed. Consequently a ready node is safe without
scanning a running-system conflict matrix. This fixes an order between conflicting
nodes rather than opportunistically reordering them every invocation.

Ready nodes use the same typed continuations as ordinary parallel groups. They
advance when prerequisites finish, without waiting for unrelated roots. Skipped
nodes release dependents. Explicit serial mode compiles the topological order
into direct calls rather than iterating over an order vector.

The simulation uses singleton Time/Ticks/Running components, filtered dense
Position/Velocity/Health entities, parallel branches, a dependent validation
step, conditions, persistent locals, and structural changes.

## Access checks and boundaries

Missing or duplicate vocabulary, aliased mutable parameters, overlapping parallel
groups, and mutable conditions fail constant evaluation during compilation. Use
`cargo build` or `cargo test`: `cargo check` alone need not monomorphize these
assertions. Repeated shared reads are permitted. Filters do not make aliased
fields within one query valid.

Parallel groups require `Send` systems and `Send + Sync` world data. Worker count,
serial mode, and the explicit work threshold can limit concurrency, so systems
must not depend on simultaneous execution of siblings.

Panic handling is fail-fast: unclaimed work may be cancelled, unlike the previous
Rayon join behavior that also executed unstarted siblings. Already-running work
is joined before the panic propagates; no borrowed world/system state can escape
the invocation. All panic payloads are retained until workers finish, then handled
on the caller after releasing the pool submission lock. Completed mutations are
not rolled back. Already-claimed straight-line regions may finish before reaching
a cancellation boundary. The pool and systems remain reusable after an unwind.

The older test-suite and Miri results below are historical, not acceptance
evidence for the compiled-continuation cutover. Current verification uses an
executable smoke, generated-code inspection, and matched application workloads;
it does not rely on the existing test suite. Unsafe code and nightly compiler
features remain experimental.

This is Bevy-like syntax, not Bevy compatibility. There are no Commands/deferred
mutation, lifecycle hooks, dynamic observers, a dedicated states API, optional
query fields, global system sets or cross-registration dependencies, runtime system registration, or
automatic plugin discovery.

## Historical blended benchmark: 4,106 systems

The measurements in this section predate change tracking and bundle spawning.
The legacy programs remain runnable, but these numbers are not measurements of
the current ECS revision. The richer comparison below supersedes them.

The sibling `../ecs-asm-comparison` project contains `blend_static`,
`blend_bevy_ecs`, and `blend_bevy_app`, plus the `run_blend.py` launcher.
The Bevy variants use version 0.20.1; all three use the pinned nightly,
release optimization level 3, ThinLTO, and one codegen unit.

```sh
cd ../ecs-asm-comparison
cargo build --release --bin blend_static --bin blend_bevy_ecs --bin blend_bevy_app
python3 run_blend.py --output blend-results.json
```

Default workload:

- 100,000 actor entities with position/health, optional velocity, and activity
  and decorative markers. Global values are singleton components in all backends.
- 4,096 tiny stateful system registrations: four independent lanes of 1,024
  ordered systems. Each lane repeats 16 arithmetic variants 64 times, for 64
  distinct function monomorphizations and 4,096 separate persistent locals.
  Every invocation updates observable lane state. These are actual registrations,
  not a runtime loop inside one ECS system. Static calls remain eligible for fusion.
- Ten additional systems: frame setup, four filtered simulation passes, two
  pre-mutation reductions, exclusive structural mutation, an optional singleton
  subsystem, and a post-mutation reduction. The optional system skips on zero
  or two matches; the other 4,105 systems execute each frame.
- Every frame replaces 500 actors and toggles activity on 2,000 actors, keeping
  actor population constant. Mutation is direct exclusive world access, not Commands.
- Same dependency stages in all backends. Static execution uses balanced concrete
  tuple trees; Bevy uses normal chained configurations, without per-registration
  named sets. Bevy actor creation uses bundles rather than emulating our insert API.

The launcher rotates backend order across five fresh-process repetitions, using
one and four workers, pinned to the same number of distinct physical cores.
Bevy uses its single-threaded executor for one worker and multithreaded executor
for four. This historical static implementation used a matching-size Rayon pool. Worker-pool startup
is outside measurement; world construction and the first update (including lazy
schedule construction) are reported as initialization. Fifty further warmup
frames precede 200 individually measured frames per process. Oracle work is
outside timing and reported separately. Each run checks every final actor field,
all singleton outputs, and frame count against an independent scalar simulation.
Checksums must also match across all backends, worker counts, and repetitions.

Measured on a Ryzen 9 5900X, CPUs 0 / 0-3, rustc 1.101.0-nightly
`12433c307` (LLVM 23.1.4). Median is the median of five run medians; p95 pools
all 1,000 measured frames per backend/worker configuration. Times are milliseconds.

| Backend | Workers | Median frame | p95 frame | Median initialization | Peak RSS, median MiB |
|---|---:|---:|---:|---:|---:|
| Static | 1 | 0.6299 | 0.8742 | 31.28 | 16.6 |
| Bevy ECS | 1 | 0.7735 | 1.1007 | 1366.44 | 1018.1 |
| Bevy App | 1 | 0.8184 | 1.0974 | 1438.50 | 1018.1 |
| Static | 4 | 0.5179 | 0.7710 | 30.60 | 16.7 |
| Bevy ECS | 4 | 3.7508 | 5.0088 | 1474.52 | 1018.0 |
| Bevy App | 4 | 3.9223 | 4.8101 | 1439.65 | 1017.9 |

Peak RSS is the process high-water mark including initialization, captured before
snapshot/oracle allocation; it is not steady-state ECS storage size. The raw
`blend-results.json` retains every frame sample, configuration, affinity, checksum,
and verification result. `blend-empty.json`, `blend-single-entity.json`, and
`blend-no-churn.json` cover empty populations, saturated mutation, wrapping values,
and disabled churn. The launcher exposes entity/frame/warmup/mutation/seed/worker
counts and CPU selection; `--help` lists options.

This deliberately stresses thousands of tiny systems and four long dependency
chains. It does not represent thousands of unique gameplay implementations or
prove a universal speedup. Bevy retains semantics absent from this prototype,
including change tracking. Minimal Bevy App includes its ordinary app schedule
machinery but no renderer, assets, networking, or DefaultPlugins. These are local
wall-clock measurements on a non-isolated workstation, not controlled lab results.

## Matched complex simulation: 4,113 systems

The current comparison lives in `../ecs-asm-comparison/complex_*` with
`run_complex.py` as its launcher. `complex_systems.rs` is included verbatim by
both ECS adapters; `complex_world.rs` shares population, exclusive mutation, and
canonical snapshot logic. `complex_model.rs` shares component definitions and
algorithms. Adapter differences are world APIs and schedule construction.

### Workload and feature parity

Default population: 100,000 actors, 10,000 projectiles, 20,000 items, 25,000
hierarchy nodes, and two singleton entities. All are real ECS entities. Actors
carry eight separate components, not one opaque simulation buffer.

- A rebuilt 128-by-128 spatial grid drives neighbor perception, contacts,
  projectile hits, and item pickup. Candidate order is deterministic.
- Four snapshot-based contact-relaxation iterations per actor, plus steering,
  drag, velocity clamping, and movement. This is a small integer simulation, not
  a production rigid-body physics engine.
- Staggered perception (every four updates), decisions (eight), and bounded
  obstacle-graph search (64), plus a synchronized burst every 120 updates.
  Search examines at most 64 nodes and influences steering; it is not a full
  navigation-mesh or route-following implementation.
- Armor, four timed effects, melee damage, projectiles, eight inventory stacks,
  item-stack merging, death markers, and delayed despawn/respawn. Structural
  changes follow simulation outcomes rather than fixed churn quotas.
- Parent-first transform propagation; four-camera visibility and render-list
  sorting; changed-only replication packing with four client-interest regions.
- Running/paused state and transition counts are singleton components in both
  versions. Both use the same run conditions, not Bevy state schedules.
- `Added<Actor>` processes births; `Changed<Pose>` selects replication updates.
  Both use ordinary mutation-aware query wrappers and tuple bundle spawning.
- 4,096 small, stateful registrations in four lanes remain in the workload.
  They have four arithmetic/control families and 64 concrete function variants,
  not 4,096 independently authored gameplay algorithms. Their policy state is
  consumed by AI and included in final-state comparison. There are 17 additional
  simulation systems. Counts exclude condition evaluations and Bevy App
  maintenance systems; paused frames skip some systems.

Both ECSes use singleton components, the same dependency stages, per-registration
`Local` state, complementary filters, and exclusive structural-mutation stages.
Neither workload uses hooks, observers, deferred Commands, assets, rendering APIs,
network transport, or Bevy resources for simulation data. Bevy retains its normal
internal bookkeeping and App machinery. This is matched **workload feature use**,
not a claim that this ECS implements all of Bevy. The explicit stage boundaries
also constrain Bevy: this is not a comparison against every possible dynamic DAG.

### Verification and reproduction

```sh
cd ../ecs-asm-comparison
cargo build --release --bin complex_static --bin complex_bevy_ecs --bin complex_bevy_app
python3 run_complex.py --output complex-results.json

# Separate, untimed validation: full state after every update.
python3 run_complex.py --actors 4096 --frames 125 --warmup 0 --repeat 1 \
  --verify-each --output complex-interaction-check.json
```

The launcher rotates backend and worker-count order, starts fresh processes, and
pins one or four distinct physical cores. Default measurements use five process
repetitions, one initialization update, 20 warmup updates, and 100 measured updates.
Median means the median of run medians; p95 pools measured frame samples. Timing
includes the full update, all joins, structural changes, and buffer preparation.
Initialization includes population construction and the first update, including
Bevy’s lazy executor setup. Peak RSS is read before final snapshot allocation; it
still includes initialization peaks, not just steady-state component storage.

Every run compares complete canonical final-state bytes across backends and worker
counts, not merely a checksum. Final snapshots include every actor component,
projectile, item, hierarchy node, singleton result, render list, and replication
packet. Entity handles, ECS-private metadata, allocation capacities, and a rebuilt
grid scratch cursor are intentionally excluded. This is differential verification
of shared algorithms, **not an independent scalar oracle**. `--verify-each` compares
the same state after every update; its timings and RSS are diagnostic, not headline
measurements. Empty, single-actor, 128-actor, and interacting 4,096-actor cases were
exercised with both worker counts and all three backends; the latter spans pause,
resume, burst, hit, pickup, death, and respawn transitions.

Raw samples, binary SHA-256 hashes, compiler version, CPU model, affinity, exact
state-match flags, and aggregates are saved in the requested result JSON. One
canonical final state is retained beside it as `.state.json`. Source and profile
settings are the same for all backends: pinned nightly, Bevy 0.20.1, release `-O3`,
ThinLTO, and one codegen unit. These are local, non-isolated workstation results.

### Historical results before storage/query optimizations

Ryzen 9 5900X; rustc 1.101.0-nightly (`12433c307`, LLVM 23.1.4).
These measurements describe the pre-optimization implementation and remain in
`complex-results.json`. They are not measurements of the current binaries.
All 30 measured processes produced byte-identical canonical final states.

| Workers | Backend | Median frame ms | Pooled p95 ms | Median init ms | Median peak RSS MiB |
|---:|---|---:|---:|---:|---:|
| 1 | static-ecs | 100.152 | 104.473 | 131.3 | 137.1 |
| 1 | Bevy ECS | 101.700 | 109.248 | 1432.5 | 1052.2 |
| 1 | Bevy App | 102.687 | 150.894 | 1432.7 | 1051.8 |
| 4 | static-ecs | 82.936 | 89.214 | 117.7 | 137.2 |
| 4 | Bevy ECS | 87.272 | 94.907 | 1468.9 | 1051.9 |
| 4 | Bevy App | 88.367 | 96.229 | 1493.1 | 1051.7 |

The substantial useful work collapses the earlier scheduler-heavy gap. Our
median frame time was 1.5% below Bevy ECS with one worker and 5.0% below it with
four; versus Bevy App, the corresponding reductions were 2.5% and 6.1%. The
one-worker difference is small compared with run-to-run variability. Both ECSes
improved with four workers here. These numbers do not support a general 7x claim.

One one-worker Bevy App repetition had a 149.304 ms median, versus approximately
102–105 ms in its other repetitions. It is retained in the raw data and pooled
p95; its cause was not established. Other runs also varied (static four-worker
medians ranged from 82.820 to 87.947 ms). Tests on separate physical cores ran
during part of collection; CPU affinity does not isolate frequency, caches, or
memory bandwidth. Treat small differences accordingly, not as lab-grade causal
measurements.

Across 121 updates, every backend agreed on 168,783 actor respawns, 1,056,652
projectile hits, 2,259,424 item pickups, 10,665,902 changed-actor replication
updates, and four running/paused transitions. A separate invariant check also
verified lifecycle totals, spatial membership, parent transforms, the complete
render list, and decoded packet contents against the final state.

RSS includes Bevy’s schedule-initialization peak. In this historical version,
entity slots were never reused and memory grew with cumulative structural churn.
The current implementation recycles generational slots; the old RSS numbers
are not evidence for its long-running memory use. Snapshot allocation is excluded
from headline RSS; there is no isolated steady-state storage measurement.

### Storage/query optimization results (before static bundle registration)

The measured implementation removes vocabulary-wide temporary rows, caches bundle
destinations and archetype transitions, and reserves table capacity in batches.
Both adapters use batch spawning for initial actors, items, and hierarchy nodes.
Query/Single parameters cache matching table IDs. Read-only invocations no longer
increment the change clock; exclusive mutations use non-atomic clock access.
System bodies and stage boundaries are unchanged. There is **no parallel query
iteration** in either adapter.

These measurements and binary hashes predate the replacement of the bundle
`TypeId` map with statically selected `BundleSlot` fields. They are not timings
of the current implementation. At the static-bundle cutover, 37 checks passed
in debug/release, including registration compile failures; its bundle, storage,
and change-tracking suites passed strict-provenance Miri. Per-update equivalence
over 126 updates with 4,096 actors, one/four workers and all three backends was
recorded in ../ecs-asm-comparison/static-bundle-equivalence.json. The newer
implementation and measurements are described below.

Five fresh-process repetitions per backend/worker count, 20 warmup and 100 measured
updates, with the same compiler/profile and core-affinity policy as above:

| Workers | Backend | Median frame ms | Pooled p95 ms | Median init ms | Median peak RSS MiB |
|---:|---|---:|---:|---:|---:|
| 1 | static-ecs | 96.484 | 99.593 | 119.0 | 138.7 |
| 1 | Bevy ECS | 101.619 | 107.085 | 1448.0 | 1051.9 |
| 1 | Bevy App | 101.308 | 105.555 | 1416.5 | 1051.6 |
| 4 | static-ecs | 79.333 | 83.977 | 101.8 | 138.8 |
| 4 | Bevy ECS | 87.146 | 94.674 | 1485.4 | 1051.7 |
| 4 | Bevy App | 87.593 | 92.776 | 1490.3 | 1051.3 |

Raw measurements and executable hashes: ../ecs-asm-comparison/storage-opt-results.json.
All 30 runs produced identical canonical final-state bytes. Relative to Bevy ECS,
median frame time is about 5.1% lower with one worker and 9.0% lower with four.
These are non-isolated workstation results; verification ran on separate cores
during part of collection. Initialization still includes the first full update,
not just world creation. Query caches add memory and first-use allocations rather
than promising lower RSS.

Verification: 36 checks pass in debug and release; 27 sequential tests pass
strict-provenance Miri. Clippy passes for the ECS workspace and benchmark binaries.
Per-update byte comparisons cover 4,096 actors over 126 updates (pause/resume,
burst, death/respawn), plus empty and singleton populations, with one and four
workers in all three backends. Results are in storage-opt-equivalence.json,
storage-opt-empty.json, and storage-opt-single.json. Added regressions exercise
batch iterator unwinding, nested destructor cleanup, new/repopulated archetypes,
world replacement with different table IDs, and read/write cursor ordering.

Reproduce the workload (the command writes a new result, not the historical data):

```sh
cd ../ecs-asm-comparison
cargo build --release --bin complex_static --bin complex_bevy_ecs --bin complex_bevy_app
python3 run_complex.py --output current-results.json
```

#### Paired comparison against the preserved pre-optimization executable

Five repetitions per version/worker count, alternating old/new order, pinned to
CPU 0 or CPUs 0–3. These shorter runs use 5 warmup and 20 measured frames after
the initialization update; do not mix their absolute times with the 100-frame
table above. All 20 processes produced identical final-state bytes.

| Workers | Before frame ms | After frame ms | Frame reduction | Before init ms | After init ms |
|---:|---:|---:|---:|---:|---:|
| 1 | 96.099 | 92.187 | 4.1% | 134.344 | 120.934 |
| 4 | 78.772 | 76.003 | 3.5% | 116.058 | 101.392 |

Initialization, including the first full update, falls by about 10.0% and 12.6%.
Raw runs, configuration, affinity and both executable hashes are retained in
../ecs-asm-comparison/storage-opt-paired.json. The baseline executable is retained
at /tmp/complex_static-before-storage. This comparison measures the combined
changes, not an attribution of savings to each individual optimization.

### Historical storage/query optimization verification (before executor replacement)

That implementation added compact present-column storage with shared
capacity, split 64-bit tick arrays, batch clock/reservation hoisting, whole-bundle
mutation, generational slot reuse, indexed query matching and cardinality, sparse
components, tracked contiguous slices, optional summaries, direct serial
execution, and opt-in dependency-ready graphs. The matched simulation keeps its
original dense components, system bodies, and fork/join stage boundaries; it does
not switch to sparse storage, slices, summaries, ready graphs, or parallel query
iteration to manufacture a backend-only advantage.

Verification: 51 checks pass in debug and release; workspace Clippy and rustdoc
pass. Strict-provenance Miri passes 34 selected storage/query/parameter/serial
execution tests. Parallel paths pass native tests, but Crossbeam’s
integer-to-pointer casts prevent strict-provenance Miri coverage of Rayon.
Native graph regressions cover early dependency release, conflicts, skips,
cycles, nested one-worker execution, and panic joining/reuse.

The real simulation and cross-crate plugin demo run successfully. Canonical
state matches Bevy ECS/App after every update over 126 updates with 4,096 actors,
one/four workers (`full-opt-equivalence.json`), plus empty/singleton cases
(`full-opt-empty.json`, `full-opt-single.json`) under ../ecs-asm-comparison.

A temporary allocator-counted executable verified one allocation to collect
10,000 values and zero allocations over 10,000 warmed despawn/respawn pairs.
After explicitly starting both workers, 1,000 in-pool ready-graph emissions
allocated zero times and produced the expected component values. This is not
a blanket allocation-free claim: external-thread submission and lazy Rayon/
Crossbeam worker setup incurred allocations; startup backtraces identified
worker construction and epoch-collector registration. Storage growth, query
cache growth, and graph construction also allocate.

#### Matched performance before the Rayon removal

100,000 actors, 4,113 systems, 20 warmup and 100 measured updates, five fresh
processes per backend/worker count. Same pinned compiler, release profile,
Ryzen 9 5900X, and CPU 0 / CPUs 0–3 affinity policy as the earlier runs.
These are non-isolated workstation measurements, not universal speed claims.

| Workers | Backend | Median frame ms | Pooled p95 ms | Median init ms | Median peak RSS MiB |
|---:|---|---:|---:|---:|---:|
| 1 | static-ecs | 96.895 | 100.690 | 122.3 | 63.3 |
| 1 | Bevy ECS | 97.898 | 103.839 | 1443.4 | 1051.9 |
| 1 | Bevy App | 97.748 | 102.353 | 1440.8 | 1051.6 |
| 4 | static-ecs | 78.839 | 81.889 | 104.2 | 61.6 |
| 4 | Bevy ECS | 86.964 | 92.128 | 1485.9 | 1051.8 |
| 4 | Bevy App | 86.787 | 90.761 | 1476.2 | 1051.3 |

All 30 runs produced identical canonical final-state bytes. Raw measurements,
configuration, hashes, and retained final state: ../ecs-asm-comparison/full-opt-results.json
and full-opt-results.state.json. Initialization includes the first update; peak
RSS includes initialization and is captured before snapshot construction.

An alternating old/new comparison isolates the combined cutover better than
comparing separate historical runs. Five repetitions per version/worker count,
100,000 actors, five warmup and 20 measured updates, with the same affinity:

| Workers | Before frame ms | After frame ms | Frame-time change | Before peak RSS MiB | After peak RSS MiB |
|---:|---:|---:|---:|---:|---:|
| 1 | 91.461 | 93.358 | +2.1% | 77.1 | 61.6 |
| 4 | 75.215 | 75.862 | +0.9% | 76.1 | 62.4 |

This does **not** demonstrate a frame-time improvement: the paired medians are
slightly slower, while peak RSS falls 20.1% / 18.0%. Initialization also rose
from 118.469 to 122.786 ms with one worker and 100.100 to 102.467 ms with four.
New sparse/slice/summary/ready-graph capabilities are not used by this workload.
All 20 paired processes produced identical state bytes. Raw runs and both
binary hashes are in ../ecs-asm-comparison/full-opt-paired.json; the baseline
executable is /tmp/complex-static-before-full-optimization. The shorter paired
runs must not be mixed with the full-workload table’s absolute timings.

All benchmark binaries also build in release mode and pass Clippy. The blended
workload passes a one/four-worker smoke run (`full-opt-blend-smoke.json`), and
all three assembly-probe executables report the same checked counter/checksum
for 100 ticks over 1,024 entities. Temporary allocation and paired-run harnesses
were removed; raw benchmark results and the preserved baseline remain.

### Historical runtime-task-graph executor cutover

Rayon is removed from the library, its tests, and the static benchmark adapters.
`cargo tree -p static-ecs` lists only this crate. Bevy remains an external
comparison backend, not an implementation dependency. This section records the
superseded runtime task-graph executor, not the current typed-continuation design.
The system bodies and storage were unchanged in this comparison.

Verification after replacement:

- Workspace release tests pass 58 checks; debug tests, Clippy, and rustdoc pass.
  Both README examples also pass as doctests.
- Strict-provenance Miri passed 31 tests in `continuations`, `execution`, and
  `params`, including parallel paths previously blocked by Crossbeam.
- The simulation and cross-crate plugin demo run successfully. Canonical state
  matches Bevy ECS/App after every update over 126 updates with 4,096 actors
  and one/four workers (`own-executor-equivalence.json`).
- An allocator-counted executable observed **zero allocation/reallocation calls**
  over 10,000 warmed nested-graph updates with both two and four workers. It
  checked final component values, verified that default sequential and explicit
  serial dispatch created no workers, and verified pool destruction joined all
  worker threads. This does not include startup, plan/cache growth, or panics.
- All benchmark binaries build in release mode and pass Clippy. The blended
  workload passes one/four-worker smoke comparisons (`own-executor-blend-smoke.json`).
  All three assembly probes agree on the checked counter/checksum over 100
  ticks with 1,024 entities.

#### Alternating Rayon/custom-executor measurements

Five repetitions per executor/worker count, seed 7, CPU 0 for one worker and
CPUs 0–3 for four. Same pinned toolchain, release profile, and 4,113-system
simulation. Each pair alternates execution order. Empty/4,096/100,000-actor
cases use 200/100/20 measured updates after 10/10/5 warmups respectively.
Reported values are medians of process frame-time medians, not pooled p95s.
Worker creation is outside measurement; initialization includes the first
update and lazy plan construction. These are non-isolated workstation results.

| Actors | Workers | Rayon frame ms | Custom frame ms | Frame-time change |
|---:|---:|---:|---:|---:|
| 0 | 1 | 0.072566 | 0.068840 | -5.1% |
| 0 | 4 | 0.095650 | 0.127059 | +32.8% |
| 4,096 | 1 | 1.274418 | 1.237428 | -2.9% |
| 4,096 | 4 | 1.079491 | 1.067118 | -1.1% |
| 100,000 | 1 | 94.403628 | 92.166680 | -2.4% |
| 100,000 | 4 | 77.825171 | 78.483880 | +0.8% |

This is **not an overall speedup claim**. The empty four-worker case regresses
by 32.8%; populated cases are close, and the four-worker heavy case is slightly
slower. Empty actors still execute the system schedule. The experiment does not
isolate the cause of the regression or establish an optimal scheduling policy.
The direct serial path and explicit work threshold are available, but no
threshold was tuned to hide the four-worker result.

All 60 processes produced identical canonical final-state bytes within each
actor-count case, across executors and worker counts. Raw runs, configuration,
snapshot hashes, and executable hashes are retained in
../ecs-asm-comparison/own-executor-paired.json. The Rayon baseline is preserved
at /tmp/complex-static-before-own-executor. Temporary allocator/paired-run
harnesses were removed after verification; measurement artifacts remain.

### Compiled-continuation cutover

The runtime `Program`, `Builder`, `Step`, task IDs, ready queues, and per-system
task selection have been removed. `src/plan.rs` lowers groups into typed
successors; `src/graph.rs` constant-evaluates labelled topology and emits
constant-selected calls; `src/executor.rs` owns worker publication slots,
cross-worker joins, invocation lifetimes, and panic propagation.

This cutover was exercised independently of the existing test suite:

- A release executable ran 10,000 warmed nested graph updates on each of one,
  two, and four workers with zero allocator calls during those update loops.
  It checked condition skips/resumption, persistent locals, and dependent reads.
- A dependent continuation completed before an unrelated root that was waiting
  for it, on both two and four workers. Borrowed graph captures, simultaneous
  callers, dynamic reentry, and pool reuse after an unwind were exercised.
- Diamond-shaped DAG joins, branches kept local by the publication threshold,
  and conflicts ordered against registration order passed on one/two/four workers.
  The complete executable also passed strict-provenance Miri, using ten rather
  than 10,000 iterations for its long update loops.
- Canonical state matched Bevy ECS and Bevy App after every update over 126
  updates, with zero, one, and 4,096 actors, on one and four workers: 18 matched
  processes. Blended workload and all three small assembly-probe binaries also
  completed with matching results.
- A reverse-registered three-node dependency chain was compiled through normal
  `System::run`, not a special serial probe. Its optimized machine code contains
  value updates and change-clock loads, with no calls, branches, task selection,
  dependency loop, or atomic read-modify-write instructions. The disassembly is
  retained in `../ecs-asm-comparison/typed-chain.asm`. This is a specific generated
  code check, not a claim that all parallel schedules are synchronization-free.
- A two-node dependency cycle was rejected by rustc with E0080:
  `evaluation panicked: dependency graph contains a cycle`.
- The simulation and cross-crate demo run; Clippy for workspace libraries,
  binaries and examples, rustdoc, formatting, and release benchmark builds pass.
  `cargo tree -p static-ecs` still lists no external dependencies.

The compile-time graph uses additional incomplete nightly const-generic
features. Monomorphized continuations trade runtime interpretation for compiler
work and generated code. Runtime worker placement, publication, joins, and
completion remain; there is no automatic timing-based profitability model.

#### Alternating runtime-graph/typed-continuation measurements

Five repetitions per configuration, alternating old/new executables and worker
order, using CPU 0 or CPUs 0–3 on a Ryzen 9 5900X. Values are medians of
per-process median frame times, in milliseconds. Empty runs measured 2,000
frames after 100 warmup frames; 4,096-actor runs measured 100 after 20;
100,000-actor runs measured 20 after five. An initial update is included in
initialization, outside the frame samples. System bodies, storage, and workload
schedule boundaries were unchanged.

| Actors | Workers | Runtime graph ms | Typed continuation ms | Frame-time change |
|---:|---:|---:|---:|---:|
| 0 | 1 | 0.071485 | 0.072316 | +1.2% |
| 0 | 4 | 0.127780 | 0.094648 | −25.9% |
| 4,096 | 1 | 1.236034 | 1.268365 | +2.6% |
| 4,096 | 4 | 1.066515 | 1.087485 | +2.0% |
| 100,000 | 1 | 94.407684 | 93.602229 | −0.9% |
| 100,000 | 4 | 77.305860 | 77.000106 | −0.4% |

The empty four-worker workload improves substantially in this run; the populated
workloads do not show a consistent speedup. These are non-isolated workstation
measurements, not evidence of universal optimality. All 60 processes produced
identical canonical final-state bytes within each actor configuration.

Raw samples, binary hashes, summaries, per-update equivalence records, generated
code, and smoke/Miri/negative-compilation logs are retained under
`../ecs-asm-comparison/typed-*`. The old executable is preserved at
`/tmp/complex-static-before-typed-continuations`. Temporary executable and paired
measurement harnesses were removed after verification.
