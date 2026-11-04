# static-ecs

A small ECS whose schedules are Rust types. Plugins, systems, and event routing
compose into one concrete application type; `emit` runs a frame as statically
dispatched, inlinable code. The world's component set is inferred from the
systems you add, at compile time. No schema, runtime registries, boxed systems,
or external dependencies; `TypeId` appears only in async tasks' bookkeeping.

It relies on **nightly full specialization** and the minimal generic-const-argument
features (`nightly-2026-10-25`). These are incomplete features with known
soundness holes; the compiler warnings are left visible. Typed parameter fetching
and the executor use internal `unsafe`, guarded by compile-time access checks and
sealed traits. This is an experiment.

```sh
cargo run --release --example simulation
cargo run --release -p dispatch-demo
cargo test --workspace --release
cargo clippy --workspace --all-targets -- -D clippy::all
```

```rust
use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct Position(f32);
struct Velocity(f32);

let mut app = App::new()
    .add_systems(Update, |mut query: Query<(&mut Position, &Velocity)>| {
        for (mut position, velocity) in &mut query {
            position.0 += velocity.0;
        }
    })
    .build(); // The world stores Position and Velocity: what the systems use.
let entity = app.world.spawn_bundle((Position(0.0), Velocity(2.0)));
app.emit(&Update);
assert_eq!(app.world.get::<Position>(entity).unwrap().0, 2.0);
```

## Schedules

`add_systems(label, systems)` is the only way to register systems. The label's
`Schedule` impl fixes what every system under it takes **first**:

| Label | `impl` | First parameter | Runs |
|---|---|---|---|
| plain, e.g. `Update` | `impl Schedule for Update {}` | none | `app.emit(&Update)` |
| payload, e.g. `Damage(u32)` | `type In = Ref<Self>;` | `&Damage` | `app.emit(&Damage(5))` |
| lifecycle `OnAdd`/`OnInsert`/`OnReplace`/`OnRemove::<T>` | provided | `Trigger` | inside structural operations |

```rust
use static_ecs::prelude::*;
struct Damage(u32);
impl Schedule for Damage {
    type In = Ref<Self>;
}
struct Health(u32);

let mut app = App::new()
    .add_systems(Damage, |hit: &Damage, mut health: Query<&mut Health>| {
        for mut health in &mut health {
            health.0 = health.0.saturating_sub(hit.0);
        }
    })
    .build();
let entity = app.world.spawn_bundle((Health(10),));
app.emit(&Damage(3));
assert_eq!(app.world.get::<Health>(entity).unwrap().0, 7);
```

- The input is mandatory for every system under a label, including each member
  of a group (`(a, b).chain()`), `run_if` conditions, and graph nodes; a system
  that does not take it fails to compile. Exclusive systems take it before the
  world: `exclusive(|hit: &Damage, world: &mut World<_>| ...)`.
- A label is a unit value (`Update`) or a tuple struct's constructor (`Damage`,
  `OnInsert::<ChildOf>`), so payload labels need no dummy value. `()` is a
  ready-made plain label.
- Systems registered for the same label run in registration order as one
  frame; emitting a label nobody registered for compiles to nothing. Lists live
  inline as `Cons<On<Label, Systems>, Tail>`, and specialization selects the
  matching registrations; there is no runtime label comparison.

A plugin is a registration value that installs into any app without naming its
world: `schedule(label, systems)` builds what `add_systems` would register.

```rust
use static_ecs::{App, Listener, Ref, Schedule, schedule};
struct Click(u32);
impl Schedule for Click {
    type In = Ref<Self>;
}

fn analytics(total: &mut u32) -> Listener<Click, impl FnMut(&Click) + '_> {
    let mut count = 0;
    schedule(Click, move |click: &Click| {
        count += 1;
        *total += click.0 * count;
    })
}

let mut total = 0;
{
    let mut app = App::new().add_plugins(analytics(&mut total)).build();
    app.emit(&Click(3));
    app.emit(&Click(3));
}
assert_eq!(total, 9);
```

`Listener<E, F>` names a world-free system under payload label `E`; systems
with parameters are `Scheduled<E, P>`. `add_plugins` takes one plugin or a
tuple of up to 12 (nested freely), applied left to right; it consumes the
builder and returns a new type. The `demo/` crates
compose two independent plugin crates.
`.build()` creates the world and checks every system against it.

## The inferred world

Each system's type records what it accesses: `Query<(&mut Position, &Velocity),
With<Active>>` mentions `Position`, `Velocity`, and `Active`. As the builder
grows, it folds these into a component set, deduplicated by type at compile time,
and `build` creates a `World` storing exactly that set. Component lookups stay
compile-time constants.

- `.register::<T>()` adds a component no system mentions, such as a tag only
  inserted by `main` or an exclusive system. Using an unstored component is a
  compile error that says so.
- `.storage::<Sparse<T>>()` (or `Components<T, true>` for column change summaries)
  chooses a component's storage. It must precede systems that use `T`; otherwise
  compilation fails rather than silently keeping dense storage.
- Exclusive systems name the world as `World<_>`, inferred at `build`. A
  plugin's exclusive system can instead be generic over any world storing what
  it needs: `fn tag_new<S: Has<Tag>>() -> ExclusiveSystem<impl FnMut(&mut
  World<S>)>`.

The world exists only after `build`, so spawn entities afterwards. Changing a
system changes the world's type; that is purely a compile-time effect.

## Storage

Entities are generational handles; archetype tables hold dense columns, and sparse
components move without moving dense rows. A world can also be used without an
app, by spelling its schema as nested pairs of `Components<T>` and `Sparse<T>`:

```rust
use static_ecs::prelude::*;
struct Position(f32);
struct Velocity(f32);
struct Active;
type State = (Components<Position>, (Components<Velocity>, Components<Active>));
let mut world = World::<State>::default();
let entity = world.spawn_bundle((Position(0.0), Velocity(2.0), Active));
for (mut position, velocity) in &mut world.query_filtered::<(&mut Position, &Velocity), With<Active>>() {
    position.0 += velocity.0;
}
assert_eq!(world.get::<Position>(entity).unwrap().0, 2.0);
assert!(world.remove::<Active>(entity).is_some());
assert!(world.despawn(entity));
```

- `spawn`, `insert`, `remove`, `despawn`, `get`, `get_mut` work on any stored
  component. `insert` and `remove` return the previous value.
- Tuple bundles (1–8 components) work with `spawn_bundle`, `spawn_batch`,
  `insert_bundle`, and `remove_bundle`, without declaration. Each world caches
  multi-component transitions in a small table keyed by a compile-time hash of
  the bundle's component indices; single components use per-archetype arrays.
- Global state is ordinary component data on singleton entities.
- Structural changes need `&mut World`: between emissions, in `exclusive`
  systems, or in payload handlers.

## Queries

- `Query<D, F>`: `D` is `&T`, `&mut T`, `Entity`, or tuples of those. `F` is
  `With<T>`, `Without<T>`, `Added<T>`, `Changed<T>`, tuples (and), or `Or<(F, G)>`.
- `Single<D, F>` requires exactly one match; otherwise the system is skipped
  without running or advancing its locals.
- Registered queries cache matching archetypes and scan only new ones.
- `query.get_mut(entity)` returns one entity's item if it matches the query and
  its filters, including row filters such as `Changed`.
- `query.contiguous_mut()` yields one slice per matching table, or an error for
  row filters and fragmented rows.
- `world.query`, `world.query_filtered`, and `world.single` work outside systems.

Mutable items are `Mut<T>`: mutable dereferencing marks the row changed. Each
system has its own change window; skipped systems keep pending changes.
`Components<T, true>` enables per-column change summaries that let
`Added`/`Changed` skip whole columns. A summary is raised once per table when a
query fetches the column mutably, so per-row loops do no atomic work; columns
fetched for writing but left unchanged are scanned, not skipped.

Iterate hot queries with `for_each` (or `contiguous_mut`): it runs one tight,
vectorizable loop per table. A `for` loop goes through `next` and stays scalar.

Change ticks are a Lamport clock carried along the schedule rather than a shared
counter: a writer issues the next tick on its own path, joins take the maximum,
and the world stores the result when the frame ends. Systems that conflict are
always ordered, so every write is either visible to a reader's current window
or to its next one. Unordered, non-conflicting writers may share a tick value.

## Systems and composition

Systems are functions or closures taking their label's input (if any), then up
to four parameters: `Query`, `Single`, `Local<T>` (per-registration state), and
`Commands<_>`.

```rust,ignore
app.add_systems(Update, ((integrate, decay).parallel(), validate).chain().run_if(running))
```

- Tuples (2–4) run in order; `.chain()` states it explicitly.
- `.parallel()` runs branches concurrently. Overlapping access fails to compile;
  complementary filters (`With<T>`/`Without<T>`) prove disjointness.
- `.run_if(condition)` runs a read-only boolean system before the group.
- `exclusive(|world| ...)` gets `&mut World` and conflicts with everything.
- `System::run(input, world)` runs a converted system directly (`NoInput` for
  plain systems).
- `ready((node(A, a), node(B, b).after(A), ...))` builds a dependency graph of up
  to `MAX_NODES` nodes (nest graphs for more). Labels, missing dependencies, and
  cycles are checked at compile time; conflicting unordered nodes are ordered
  deterministically and transitive edges removed, all during const evaluation.

`ExecutionMode::Serial` runs everything on the calling thread without workers.

`Commands<_>` records structural changes (`spawn`, `insert`, `remove`,
`despawn`, or `queue` any `FnOnce(&mut World<S>)`) without structural access, so
it runs in parallel groups. `spawn` reserves the entity immediately; later
commands can use it. Each system's commands are stored inline in its own queue
(no boxing, no locks) and applied when the `emit` that ran it finishes, in system
order (`System::run` applies right after the run). Commands on entities
despawned before they apply do nothing. Like `World<_>`, the parameter names the
world, so components only ever inserted through commands must be registered.

### Hooks

Hooks are systems under a lifecycle label; their input is a `Trigger`:

```rust
use static_ecs::prelude::*;
struct Health(u32);
struct Alive;

let mut app = App::new()
    .add_systems(OnAdd::<Health>, |added: Trigger, mut commands: Commands<_>| {
        for &entity in added.iter() {
            commands.insert(entity, (Alive,));
        }
    })
    .register::<Alive>()
    .build();
let entity = app.world.spawn_bundle((Health(10),));
assert!(app.world.get::<Alive>(entity).is_some()); // applied before spawn returned
```

- `OnAdd<T>` (newly added), `OnInsert<T>` (any write), `OnReplace<T>` (before an
  existing value is overwritten, removed, or despawned), `OnRemove<T>` (before
  removal or despawn). Several hooks for one label run in registration order.
- `Trigger` holds the affected entities: one per call, all of a `spawn_batch`.
- Hooks run serially inside the structural operation, so they take system
  parameters only (`Query`, `Single`, `Local`, `Commands`); exclusive hooks fail
  to compile. Their commands apply, breadth first, before the outermost
  structural operation returns.
- The built world owns its hooks, which must be `Send + Sync + 'static`. Each
  (component, label) with hooks compiles to one function running all of them;
  an operation reaches it through one table lookup and indirect call. Worlds
  without hooks skip the table entirely.

### Relations

A relation kind fixes its cardinality and despawn behavior at compile time:

```rust
use static_ecs::prelude::*;
struct ChildOf;
impl Relation for ChildOf {
    type Kind = OneToMany;    // or OneToOne, ManyToMany
    const LINKED: bool = true; // despawning a parent despawns its children
}

let mut app = App::new().relation::<ChildOf, _>().build();
let world = &mut app.world;
let (parent, child) = (world.spawn(), world.spawn());
world.relate::<ChildOf>(child, parent);
assert_eq!(world.get::<Relates<ChildOf>>(child).unwrap().target(), parent);
assert_eq!(world.get::<Sources<ChildOf>>(parent).unwrap().len(), 1);
world.despawn(parent);
assert!(!world.contains(child));
```

- `world.relate`/`unrelate` and `commands.relate`/`unrelate` are the only
  writers. A one-target source moves to its new target; a one-source target
  drops its previous source. `world.targets`/`sources` read either storage.
- **Non-fragmenting** (default): entities with different targets share tables.
  Sources hold `Relates<R>` (`targets()`, or `target()` for one target),
  targets hold `Sources<R>` (`sources()`/`source()`); both are ordinary
  components. Each entry stores its index in the other side's list, so
  unlinking is O(1); one-sided ends store an `Option`, never a `Vec`. Two
  hooks unlink (or, if `LINKED`, despawn) when either side goes.
- **Fragmenting** (`const FRAGMENTING: bool = true`, one-to-many or
  many-to-many): the `(relation, target)` pair is part of the source's table
  key and stored nowhere else. One target's sources are contiguous; relating
  or reparenting is one table move and leaves the target untouched. A
  `Query<D, Related<R>>` adds `iter_related(target)` (only that pair's
  tables, via a pair index), `target(source)`, and `related_descendants`.
  Despawning a target despawns (`LINKED`) or detaches its sources through the
  pair index, terminating on cycles. Costs: a table per distinct target set
  (empty tables whose target died are reused) and a pair-index lookup per
  fragmenting relation on each despawn. One-to-one fragmenting fails to
  compile.
- `Related<R>` matches sources of either storage. `.relation::<R, _>()`
  installs a relation; relating an uninstalled one panics (non-fragmenting) or
  fails to compile (fragmenting).
- `world.descendants::<R>(root)` lists everything below `root`, parents before
  children, for either storage (in systems: `descendants` on a
  `Query<&Sources<R>>`, or `related_descendants` for fragmenting relations).

### Tasks and bridges

`task(body)` is a plugin: an async body the app owns and polls during `emit`.
It sits in registration order like a system, under every label at once. A
bridge, `world.bridge(Label, |params...| ...)`, runs a closure with system
parameters (`Query`, `Single`, `Commands`, `Local`, or `&mut World<_>` alone)
when the task's position is reached while that label is emitted: after the
systems registered before it, before those after. Its commands apply before
it returns, so the next bridge sees them.

```rust
use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct Health(u32);

let mut log = Vec::new();
{
    let log = &mut log;
    let mut app = App::new()
        .register::<Health>()
        .add_plugins(task(|world: AsyncWorld<_>| async move {
            let entity = world
                .bridge(Update, |mut commands: Commands<_>| commands.spawn((Health(3),)))
                .await
                .unwrap();
            loop {
                // Closures borrow the task's locals, `&mut` included: no `Send`.
                world
                    .bridge(Update, |mut q: Query<&mut Health>| {
                        let mut health = q.get_mut(entity).unwrap();
                        health.0 -= 1;
                        log.push(health.0);
                    })
                    .await
                    .unwrap();
            }
        }))
        .build();
    app.emit(&Update);
    app.emit(&Update);
}
assert_eq!(log, [2, 1]);
```

- A task starts at the first `emit`. Afterwards it is polled when an emit
  reaches it on a label it waits on, or after a waker (e.g. a channel or I/O
  future) wakes it; otherwise a visit is a few loads. A bridge site runs once
  per visit: the loop above runs once per `emit`, while a chain of different
  bridges completes in one.
- `bridge` fails with `BridgeError::Invalid` if a parameter cannot be fetched
  (a `Single` without exactly one match); `until` waits for the label's next
  emit instead, e.g. until another plugin has spawned what this one needs.
- `world.state()` keeps parameter state across bridges: `Local`s persist,
  `Changed` sees changes since its previous bridge, and query caches stay warm,
  so bridging through it allocates nothing (one-shot bridges build fresh state).
- Bridges run with exclusive world access and cannot nest. Components they use
  are checked against the world at `build` but are not inferred from them
  (the async body is opaque): `.register::<T>()` them, as the example does, or
  use them in a system.
- Nothing requires `Send`. A task that is `Send` runs wherever the frame's
  invocation reaches it, possibly a worker; with a non-`Send` task the app
  runs each registration as its own invocation on the calling thread.

### Remote bridges

Futures on other threads or runtimes (tokio, a thread pool) bridge in through
a sync point. `sync_point(Label)` returns a plugin and a `Remote` handle; the
plugin's position in registration order is where, during `emit` of `Label`,
queued remote bridges run.

```rust
use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct Score(u32);

let (point, remote) = sync_point(Update);
let mut app = App::new().register::<Score>().add_plugins(point).build();
let entity = app.world.spawn_bundle((Score(0),));
let worker = std::thread::spawn(move || {
    let mut fetched = 40; // e.g. from the network; borrowed mutably below
    let bridge = remote.bridge(|mut q: Query<&mut Score>| {
        fetched += 2;
        q.get_mut(entity).unwrap().0 = fetched;
    });
    // Any executor works; this one just re-polls.
    let mut bridge = std::pin::pin!(bridge);
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    while bridge.as_mut().poll(&mut cx).is_pending() {
        std::thread::yield_now();
    }
});
while !worker.is_finished() {
    app.emit(&Update);
}
assert_eq!(app.world.get::<Score>(entity).unwrap().0, 42);
```

- The driver grants queued bridges in **waves** of non-conflicting access,
  checked with compile-time component masks: each granted closure runs on its
  own task's thread, in parallel with the rest of its wave, so closures need
  not be `Send` and may borrow the caller's locals. The driver then applies
  their commands in grant order and grants the next wave. Exclusive
  (`&mut World<_>`) bridges and bridges sharing one `remote.state()` never
  share a wave.
- Synchronization is one short lock around the request list (enqueue,
  admission, cancellation), a release/acquire pair per grant, and one
  decrement per completion. Requests live inside their pinned futures, so warm
  bridges allocate nothing on either side; an idle sync point is one load.
- A bridge site runs once per task per drive: a remote loop runs once per
  emit, and a chain of different bridges may finish in one drive if each step
  is queued before the driver's last wave.
- `until` retries at later drives; dropping a pending future withdraws it,
  even after it was granted. Dropping the app closes its sync points: pending
  and later bridges fail with `BridgeError::Closed`.
- The driver waits for granted bridges, so poll remote bridges on threads
  other than the app's. Inside a task, use `AsyncWorld::bridge`; awaiting a
  remote bridge there panics rather than deadlocking.

## Execution model

A frame is one compiled program:

- **One invocation per emit.** Every handler registered for the event runs as a
  single chain of typed continuations, with no barrier between registrations.
  Nested sequences, conditions, parallel groups, and graphs lower into the same
  chain. Lists containing non-`Send` systems still work: each parallel group then
  runs as its own invocation.
- **Straight-line code is inlined.** A system's successor is a type, so sequences
  compile to direct, inlinable calls. A continuation reachable from several places
  (after a join, either side of a condition, a published branch) is emitted once
  out of line, so code size grows linearly with the schedule. Outlined
  continuations are returned to a loop rather than called, so stack depth stays
  constant across any number of joins.
- **Synchronization only at forks and joins.** No system touches shared state to
  run. A fork publishes its second branch only if a worker is idle; otherwise both
  branches run here without atomic read-modify-write operations. A cross-worker
  join costs two atomic operations per arrival (count and clock).
- **Persistent workers.** `WorkerPool::new(n)` starts `n - 1` threads; the caller is
  worker 0 and helps while it waits. Each worker has one publication slot.
  Idle workers steal unstarted branches, spin briefly, then park.
  `with_min_parallel_work(k)` keeps branches smaller than `k` systems local.
- **Panics.** A panic cancels unpublished work. `emit` returns only after every
  published branch has finished, then resumes the first panic on the caller.
  Completed mutations are not rolled back; the pool stays usable.

Without `install`, a process-wide pool sized by `available_parallelism` is used.
Callers sharing a pool serialize their invocations; nested emits inside systems
run serially.

Debug builds: every system is part of one concrete type, and those type names
are enormous. Full debug info stores them, making debug binaries gigabytes large
(2.3 GB for the 4,113-system benchmark). Set this in the crate that builds the
app (where the concrete types are compiled) to roughly halve that:

```toml
[profile.dev]
debug = "line-tables-only" # keeps file:line backtraces
```

Limits: very long continuation chains may need a higher `#![recursion_limit]` in
the crate that builds the app. Graph compile time grows quadratically with nodes
per graph. Parallel groups need `Send` systems and `Send + Sync` world data.
Missing Bevy features include observers, states, resources as a separate store,
and runtime system registration.

Benchmarks against Bevy 0.20 are in [BENCHMARKS.md](BENCHMARKS.md).
