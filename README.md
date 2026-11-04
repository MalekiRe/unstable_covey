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
cargo run --release --example tokio_web # remote bridges from tokio (network)
cargo run --release -p dispatch-demo
cargo run --release -p flappy        # CPU-rendered Wayland client (space flaps)
cargo run --release -p flappy --bin flappy-launch  # launch-to-pixels timing
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

A plugin is a type. It names the components (and relations) its systems use,
then builds those systems for whatever world the app ends up with, so it never
names that world:

```rust
use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct Click(u32);
impl Schedule for Click {
    type In = Ref<Self>;
}
pub struct Clicks(u32);

pub struct Analytics<'a> {
    pub total: &'a mut u32,
}
impl Plugin for Analytics<'_> {
    type Components = (Clicks,);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        let mut count = 0;
        (
            schedule(Click, move |click: &Click| {
                count += 1;
                *self.total += click.0 * count;
            }),
            // `S` is the app's world: systems that name it take it from here.
            schedule(Update, |mut commands: Commands<S>, mut q: Query<&Clicks>| {
                if q.iter_mut().next().is_none() {
                    commands.spawn((Clicks(0),));
                }
            }),
        )
    }
}

let mut total = 0;
{
    let mut app = App::new().add_plugins(Analytics { total: &mut total }).build();
    app.emit(&Click(3));
    app.emit(&Click(3));
    app.emit(&Update);
    assert_eq!(app.world.query::<&Clicks>().iter_mut().count(), 1);
}
assert_eq!(total, 9);
```

- `type Components` is a tuple of up to 32 component types; `type Relations`
  (default `()`) a tuple of relations, installed once however many plugins
  name them. A system using a component the plugin did not declare (and
  nothing else stores) fails to compile at `build`.
- `systems` returns `schedule(label, systems)`, `task(body)`, another plugin's
  `systems::<S>()`, or a tuple of up to 16 of them, nested freely. Inside, `S`
  is the final world: `Commands<S>`, `&mut World<S>`, `AsyncWorld<S>` (or
  `_`). It is called once, by `build`, and its systems run where the plugin was
  added among the app's registrations. A plugin's hooks run after the app's
  own, and like all hooks they own what they capture (`'static`).
- `add_plugins` takes a plugin, a `task(...)`, or a tuple of up to 16 of them,
  applied left to right. The `demo/` crates compose two independent plugin
  crates.
- A plugin's systems are `Send` (they capture `Mutex`, atomics, or owned data,
  not `RefCell` or `Rc`), so an app built from plugins can move to another
  thread whenever its own systems can. They stay opaque in the app's type;
  dispatch into them is resolved when the app is compiled, statically as for
  any other system.

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
  it needs: `fn tag_new<S: Stores<Tag>>() -> ExclusiveSystem<impl FnMut(&mut
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
- Tuple bundles (1–16 components) work with `spawn_bundle`, `spawn_batch`,
  `insert_bundle`, and `remove_bundle`, without declaration. Each world caches
  multi-component transitions in a small table keyed by a compile-time hash of
  the bundle's component indices; single components use per-archetype arrays.
- Global state is ordinary component data on singleton entities.
- Structural changes need `&mut World`: between emissions, in `exclusive`
  systems, or in payload handlers.

## Queries

- `Query<D, F>`: `D` is `&T`, `&mut T`, `Entity`, `Option<D>` (every row,
  `None` where `D` does not match; schedules like `D`), `Has<T>` (whether the
  row has `T`, a `bool`; reads table membership only, so it never conflicts
  with writers of `T`), joins, or tuples of up to 16 of those. `F` is
  `With<T>`, `Without<T>`, `Added<T>`, `Changed<T>`, `Related<R>`,
  `Relinked<R>`, tuples of up to 16 (and), or `Or<(F, G, ...)>`.
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
to 16 parameters: `Query`, `Single`, `Local<T>` (per-registration state), and
`Commands<_>`.

```rust,ignore
app.add_systems(Update, ((integrate, decay).parallel(), validate).chain().run_if(running))
```

- Tuples (2–16) run in order; `.chain()` states it explicitly.
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

A relation is a type; its value is the edge's data. Relate by inserting
`R.to(target)` like any component, then read across edges inside queries:

```rust
use static_ecs::prelude::*;
use static_ecs::{From, To}; // not in the prelude: `From` would shadow std's

struct DockedAt { berth: u8 } // edge data
impl Relation for DockedAt {
    type Kind = OneToMany; // each ship docks at one station
}
struct Allied;
impl Relation for Allied {
    type Kind = ManyToMany;
}
struct Follows;
impl Relation for Follows {
    type Kind = OneToMany;
    const ACYCLIC: bool = true; // convoys have leaders: `cascade` walks them
}
struct Name(&'static str);
struct Cargo(u32);
struct Pos(i32);
struct Gap(i32);
struct Update;
impl Schedule for Update {}

let mut app = App::new()
    .relation::<DockedAt>()
    .relation::<Allied>()
    .relation::<Follows>()
    .register::<Name>()
    // Many ships share a station: `&mut` across that edge is one row at a time.
    .add_systems(Update, |mut ships: Query<(&Cargo, To<DockedAt, &mut Cargo>)>| {
        ships.each(|(cargo, station)| {
            if let Some(mut station) = station {
                station.0 += cargo.0;
            }
        })
    })
    // Leaders before followers, so each reads its leader's new position.
    .add_systems(Update, |mut convoy: Query<(To<Follows, &Pos>, (&Gap, &mut Pos))>| {
        convoy.cascade(|(leader, (gap, mut pos))| {
            if let Some(leader) = leader {
                pos.0 = leader.0 + gap.0;
            }
        })
    })
    .build();
let world = &mut app.world;
let station = world.spawn_bundle((Name("Ceres"), Cargo(0)));
let lead = world.spawn_bundle((Name("Kestrel"), Cargo(5), Pos(100), Gap(0)));
let wing = world.spawn_bundle((Name("Osprey"), Cargo(7), Pos(0), Gap(-10), Follows.to(lead)));
world.insert(lead, DockedAt { berth: 2 }.to(station));
world.insert(wing, DockedAt { berth: 3 }.to(station));
world.relate(lead, Allied, wing);
app.emit(&Update);
let world = &mut app.world;
assert_eq!(world.get::<Cargo>(station).unwrap().0, 12);
assert_eq!(world.get::<Pos>(wing).unwrap().0, 90);

// Edge data and the target, side by side; `From` lists a target's sources.
let mut ships = world.query::<(&Name, &Relates<DockedAt>, To<DockedAt, &Name>)>();
let (name, dock, at) = ships.get_mut(wing).unwrap();
assert_eq!((name.0, dock.berth, at.unwrap().0), ("Osprey", 3, "Ceres"));
let mut stations = world.query::<From<DockedAt, &Name>>();
assert_eq!(stations.get_mut(station).unwrap().count(), 2);
let mut allies = world.query::<To<Allied, &Name>>();
assert_eq!(allies.get_mut(lead).unwrap().map(|n| n.0).collect::<Vec<_>>(), ["Osprey"]);
```

- **Writing links.** Insert `R.to(target)` (in spawn bundles and commands
  too); inserting again retargets, and for many-to-many replaces every edge.
  `relate(source, value, target)` adds one edge (or replaces its value);
  removing `Relates<R>`, `unrelate`, or despawning either side unlinks.
  `Relates<R>` and `Sources<R>` are read-only otherwise: `&mut` to them does
  not compile, since swapping two rows' lists would corrupt the index. A
  relation with a one-side refuses self-edges; an `ACYCLIC` one refuses edges
  closing a cycle; `LINKED` despawns sources with their target.
- **Joins.** `To<R, Q>` fetches `Q` on the row's targets, `From<R, Q>` on its
  sources; entities not matching `Q` are skipped. The item follows
  cardinality at compile time:

  | | `OneToOne` | `OneToMany` | `ManyToMany` |
  |---|---|---|---|
  | `To<R, Q>` | `Option` | `Option` | iterator |
  | `From<R, Q>` | `Option` | iterator | iterator |

  Joins nest (`From<ChildOf, From<ChildOf, Entity>>`) and schedule like the
  components they reach.
- **Mutable joins.** Checked at compile time per relation: many-to-many joins
  never write; two joins in one row cannot conflict (both may reach one
  entity), nor can nested joins with anything. `From<R, &mut T>` on a
  one-source relation and `OneToOne` joins give distinct values per row, so
  `iter_mut` and `for` work. When rows can share a value (`To<OneToMany, &mut
  T>`, or a row writing `T` while joining `&T`), iterate with `each` or
  `iter_lending()` / `fetch_next()`, which keep one row alive at a time.
  `cascade` is `each` in parent-first order along the query's leading
  `To<R, _>` (one-target, `ACYCLIC`).
- **Storage.** Non-fragmenting (default): `Relates<R>` holds a source's
  targets and edge value, `Sources<R>` a target's sources; each entry stores
  its index in the other side's list, so unlinking is O(1). Fragmenting
  (`const FRAGMENTING: bool = true`, data-less, one-to-many or many-to-many):
  the `(relation, target)` pair is part of the source's table key and stored
  nowhere else; one target's sources are contiguous, `To` resolves a table's
  target once and `From` walks whole tables through a pair index. Relating is
  one table move; the costs are a table per distinct target set (dead ones
  reused) and a pair-index lookup per fragmenting relation on despawn.
- **Filters and walks.** `Related<R>` matches sources; `Relinked<R>` those
  whose links changed since the system last ran. `world.targets`/`sources`/
  `descendants::<R>(entity)` iterate either storage, parents before children.
  `.relation::<R>()` installs a relation; relating an uninstalled one panics
  (non-fragmenting) or fails to compile (fragmenting).

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
sync points. A sync point is a marker type: `impl SyncPoint for Net {}`.
`sync_point(Net)` is the system that drives it; place it under a label like
any system (`add_systems(Update, sync_point(Net))`, or inside a chain,
parallel group, or graph). A world's `Remote` (`world.remote()`, or a
`&Remote<_>` system parameter) reaches every sync point from anywhere:
`remote.bridge(Net, ...)`. Placing the driver declares `Net` in the inferred
schema, so its queue sits at a compile-time index in the world, and bridging
to a point the app never places fails to compile.
[`examples/tokio_web.rs`](examples/tokio_web.rs) fetches pages on a tokio
runtime and inserts the responses as components while frames keep running.

```rust
use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct Net;
impl SyncPoint for Net {}
struct Score(u32);

let mut app = App::new()
    .register::<Score>()
    .add_systems(Update, sync_point(Net))
    .build();
let entity = app.world.spawn_bundle((Score(0),));
let remote = app.world.remote();
let worker = std::thread::spawn(move || {
    let mut fetched = 40; // e.g. from the network; borrowed mutably below
    let bridge = remote.bridge(Net, |mut q: Query<&mut Score>| {
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
  even after it was granted. Dropping the world closes its sync points:
  pending and later bridges fail with `BridgeError::Closed`.
- The driver waits for granted bridges, so poll remote bridges on threads
  other than the app's. Inside a task, use `AsyncWorld::bridge`; awaiting a
  remote bridge there panics rather than deadlocking.
- **Envelopes.** By default a sync point is exclusive: it orders against
  everything and applies each wave's commands at once. With `type Env =
  Query<'static, &'static mut Score>;` (system parameters: one, or a tuple
  of up to 16) its driver runs in parallel with systems that do not
  conflict with the envelope, the envelope's components join the inferred
  world, a bridge reaching outside it fails to compile, and commands (always
  allowed) apply when the emit ends.
- One point placed twice is driven by one driver at a time. While an
  envelope point waits for its bridges, its worker stays useful: it lends its
  slot to the pool before granting, runs branches published to it meanwhile,
  and holds the slot again once the wave is done.

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
