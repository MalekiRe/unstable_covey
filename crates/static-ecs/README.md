# static-ecs

Bevy's ECS model (entities, components, systems, schedules, plugins) with one
difference that changes everything else: **the whole app is a single Rust
type**. Every system, its parameters, its place in the schedule, every plugin,
and the set of components the world stores are part of that type, so the
compiler sees the entire frame at once. It checks access conflicts, wiring,
and storage at compile time, and emits each frame as one program of direct,
inlinable calls.

```rust
use static_ecs::prelude::*;

struct Update;
impl Schedule for Update {}

struct Position(f32); // any 'static type is a component: no derives
struct Velocity(f32);

let mut app = App::new()
    .add_systems(Update, |mut query: Query<(&mut Position, &Velocity)>| {
        query.iter_mut().for_each(|(mut position, velocity)| position.0 += velocity.0);
    })
    .build(); // the world stores exactly what the systems use: Position, Velocity

let entity = app.world.spawn_bundle((Position(0.0), Velocity(2.0)));
app.emit(&Update);
assert_eq!(app.world.get::<Position>(entity).unwrap().0, 2.0);
```

> **Status: an experiment.** It needs nightly Rust (`nightly-2026-10-25`, pinned
> in `rust-toolchain.toml`) for full specialization and generic const
> arguments; their incomplete-feature warnings are left visible.
> Specialization is the one with known soundness holes, and they all come from
> impls that differ only in lifetimes: lifetimes are erased before code
> generation picks an impl. This crate avoids them deliberately: every
> specializing impl is keyed on types that must be `'static` (components,
> labels, groups, states), so impl selection never depends on a lifetime. The
> library also uses internal `unsafe`, behind compile-time access checks and
> sealed traits.

## Why not just Bevy?

### A frame is one compiled program

In Bevy, systems are boxed trait objects stored in a schedule. Each frame, the
executor decides from runtime access bitsets which systems may run, calls each
through a vtable, and looks components up by runtime ids. None of that exists
here:

- **Systems are types, sequences are direct calls.** A system's successor is a
  type parameter, so `(a, b, c).chain()` compiles to `a(); b(); c();` and LLVM
  inlines system bodies into each other. Emitting a label nobody registered
  for compiles to nothing.
- **Component lookups are constants.** The world's component set is a type, so
  "which column is `Position`" is a compile-time index. Queries resolve column
  pointers once per table, then run plain loops: `for_each` over 1M entities
  runs as fast as hand-written slice code (0.20–0.23 vs 0.25–0.29 ns/entity,
  vectorized, in [`examples/query_loop.rs`](examples/query_loop.rs)).
- **Access checking costs nothing at runtime.** Which systems may run in
  parallel is decided while compiling; the executor never compares access
  sets.
- **Synchronization only where the schedule forks or joins.** A fork hands its
  second branch to a worker only if one is idle; otherwise both branches run
  inline with no atomic writes. A join costs two atomic operations per arrival.
- **No shared change counter.** Change ticks are a Lamport clock carried along
  the schedule in a register; parallel systems never contend on a world-wide
  tick.
- **Code size stays linear.** A continuation reachable from several places (after
  a join, either side of a condition) is emitted once, out of line, and
  returned to a loop rather than called, so stack depth stays constant across
  any number of joins.

Measured on a matched simulation (4,113 systems, 100,000 actors, the same
system bodies on both; differential checks confirm identical state every
frame), Ryzen 9 5900X:

| Workers | | static-ecs | Bevy 0.20.1 ECS |
|---:|---|---:|---:|
| 1 | frame (median) | **94.9 ms** | 98.1 ms |
| 4 | frame (median) | **80.3 ms** | 87.7 ms |
| 4 | startup (build + first frame) | **107 ms** | 1,466 ms |
| 4 | peak memory | **63 MiB** | 1,052 MiB |

With 4,096 tiny systems, where scheduling overhead dominates, a frame takes
0.177 ms against Bevy's 2.495 ms on four workers. Methodology, raw data, and
every per-change measurement are in [BENCHMARKS.md](BENCHMARKS.md).

### Mistakes are compile errors, not panics

Bevy validates system parameters when a system first initializes and panics
on conflicts (error B0001); here the same mistakes do not compile:

```rust,compile_fail,E0080
use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct Health(u32);
// Two parameters alias Health: rejected while compiling.
App::new()
    .add_systems(Update, |_: Query<&mut Health>, _: Query<&Health>| {})
    .build();
```

```rust,compile_fail,E0080
use static_ecs::prelude::*;
struct Health(u32);
// Asked to run in parallel, but both touch Health.
App::new()
    .add_systems((), (
        |mut q: Query<&mut Health>| q.iter_mut().for_each(|mut h| h.0 += 1),
        |mut q: Query<&Health>| q.iter_mut().for_each(|h| assert!(h.0 > 0)),
    ).parallel())
    .build();
```

The compiler also rejects:

- using a component the world does not store, or a storage choice made after
  systems already use the component;
- writing an `Immutable` component in place, through queries, `get_mut`, or
  `ResMut`;
- a bundle that repeats a component or holds two members of one `OneOf` group;
- a system that does not take its label's input (`&Damage` under `Damage`);
- dependency graphs with cycles, missing nodes, or unknown labels (checked
  during const evaluation);
- relation joins whose mutable access could alias across edges;
- bridging into a sync point the app never places, or outside its declared
  envelope;
- `insert_resource` of a state, which would skip its transitions.

Disjointness is proven too: `With<T>`/`Without<T>` filters, and filters on
members of one `OneOf` group, let two `&mut` queries coexist in one system or
run in parallel. Bevy needs `ParamSet` or explicit `Without` filters for the
same.

### Less to write

- **No derives or registration.** `struct Pos(f32);` is a component; resources,
  states, and relations are plain types with one-line trait impls.
- **The world is inferred.** `build` creates a world storing exactly the
  components your systems and plugins mention. Lookups stay compile-time
  constants; there is no registry to populate.
- **Events are labels.** `impl Schedule for Damage { type In = Ref<Self>; }`,
  then every system under `Damage` takes `&Damage` first. `app.emit(&Damage(5))`
  runs them; there are no event queues to read or clear.
- **Plugins are types** that name their components and build their systems for
  whatever world the app ends up with, without ever naming it.
- **Fewer special cases.** Resources are components of a hidden entity, states
  are resources, and hooks and state transitions are systems under lifecycle
  labels, so one set of parameters (`Query`, `Single`, `Res`, `Commands`, ...)
  works everywhere.

### Features Bevy does not have

- **Mutually exclusive components** (`OneOf`): inserting `Running` removes
  `Walking` in the same table move, and the scheduler knows queries filtered on
  each never overlap. Open in Bevy as
  [#17707](https://github.com/bevyengine/bevy/issues/17707).
- **Many-to-many relations and relational queries**: `To<R, Q>` and
  `From<R, Q>` fetch data across edges inside a query, with item types that
  follow the relation's cardinality, mutable access checked per cardinality,
  and `cascade` for parent-first walks. Bevy's relationships are one-to-many
  ([#18121](https://github.com/bevyengine/bevy/issues/18121)); relational
  queries are being designed
  ([#23018](https://github.com/bevyengine/bevy/issues/23018)). Relations can
  also be fragmenting: the target becomes part of the table key.
- **Async tasks** that are part of the schedule: a task awaits *bridges* that
  run closures with system parameters at its place in the frame. Closures may
  borrow the task's locals mutably; nothing needs to be `Send`.
- **Remote bridges**: futures on any thread or runtime (tokio, a thread pool)
  queue closures that run inside the frame, granted in parallel waves of
  non-conflicting access checked with compile-time masks.
- **Compile-time dependency graphs**: `ready((node(A, a), node(B, b).after(A)))`
  is validated, ordered, and transitively reduced during const evaluation.

### Smaller

The library is about 12,000 lines with no dependencies; `bevy_ecs` 0.20.1's
`src/` alone is about 120,000. Startup is 14× faster and peak memory 17×
lower in the simulation above, mostly because nothing is built at runtime that
the compiler already knows.

## Coming from Bevy

| Bevy | static-ecs |
|---|---|
| `#[derive(Component)] struct Pos;` | `struct Pos;` |
| `#[derive(Resource)]`, `Res`, `ResMut` | plain type, `Res`, `ResMut` |
| `app.add_systems(Update, f)` then `app.update()` | `app.add_systems(Update, f).build()` then `app.emit(&Update)` |
| events or observers | payload labels: `app.emit(&Damage(5))` |
| `impl Plugin { fn build(&self, app) }` | `impl Plugin { type Components; fn systems<S>() }` |
| `#[require(Pos)]` | `impl Requires for Bird { type Required = (Pos,); }` |
| `#[component(immutable)]` | `impl Immutable for T {}` |
| `#[derive(States)] enum`, `OnEnter(Game::Playing)` | `impl State for Playing { type Group = Game; }`, `OnEnter::<Playing>` |
| component hooks, `on_add` | `add_systems(OnAdd::<T>, ...)` |
| `ChildOf` / `Children` | `impl Relation for ChildOf { type Kind = OneToMany; }`, `To` / `From` |
| `Disabled`, `register_disabling_component`, `Allow<T>` | `Disabled`, `impl Disabling for T {}`, `MayHave<T>` |
| `ParamSet` | disjoint filters, proven at compile time |
| `ApplyDeferred` | commands apply when the `emit` finishes (hooks: before the operation returns) |

## Guide

### Schedules and events

`add_systems(label, systems)` registers systems. The label's `Schedule` impl
fixes what each system under it takes first:

| Label | `impl` | First parameter | Runs |
|---|---|---|---|
| plain, e.g. `Update` | `impl Schedule for Update {}` | none | `app.emit(&Update)` |
| payload, e.g. `Damage(u32)` | `type In = Ref<Self>;` | `&Damage` | `app.emit(&Damage(5))` |
| lifecycle `OnAdd`/`OnInsert`/`OnReplace`/`OnRemove::<T>` | provided | `Trigger` | inside structural changes |

```rust
use static_ecs::prelude::*;
struct Damage(u32);
impl Schedule for Damage {
    type In = Ref<Self>;
}
struct Health(u32);

let mut app = App::new()
    .add_systems(Damage, |hit: &Damage, mut health: Query<&mut Health>| {
        health.iter_mut().for_each(|mut health| health.0 = health.0.saturating_sub(hit.0));
    })
    .build();
let entity = app.world.spawn_bundle((Health(10),));
app.emit(&Damage(3));
assert_eq!(app.world.get::<Health>(entity).unwrap().0, 7);
```

Systems registered for one label run in registration order as one frame.
`()` is a ready-made plain label.

### Plugins

A plugin names the components its systems use and builds those systems for the
app's final world `S`:

```rust
use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct Click(u32);
impl Schedule for Click {
    type In = Ref<Self>;
}
pub struct Clicks(u32);

pub struct Analytics;
impl Plugin for Analytics {
    type Components = (Clicks,);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        (
            schedule(Click, |click: &Click, mut clicks: Query<&mut Clicks>| {
                clicks.iter_mut().for_each(|mut c| c.0 += click.0)
            }),
            // Systems that name the world take it from `S`.
            schedule(Update, |mut commands: Commands<S>, mut q: Query<&Clicks>| {
                if q.iter_mut().next().is_none() {
                    commands.spawn((Clicks(0),));
                }
            }),
        )
    }
}

let mut app = App::new().add_plugins(Analytics).build();
app.emit(&Update);
app.emit(&Click(3));
assert_eq!(app.world.query::<&Clicks>().iter_mut().next().unwrap().0, 3);
```

`systems` returns `schedule(label, systems)`, `task(body)`, another plugin's
systems, or tuples of them. `type Relations` (default `()`) installs relations
once, however many plugins name them. A plugin's systems are checked against
the world once, at `build`, which also keeps compile times down for large apps.

### The world

`build` creates the world from what systems mention. `.register::<T>()` adds a
component no system names (say, a tag only `main` inserts);
`.storage::<Sparse<T>>()` picks sparse storage, which toggles without moving
dense rows. A world can also be used alone, with its schema spelled out:

```rust
use static_ecs::prelude::*;
struct Position(f32);
struct Velocity(f32);
struct Active;
type Schema = (Components<Position>, (Components<Velocity>, Sparse<Active>));

let mut world = World::<Schema>::default();
let entity = world.spawn_bundle((Position(0.0), Velocity(2.0), Active));
world
    .query_filtered::<(&mut Position, &Velocity), With<Active>>()
    .iter_mut()
    .for_each(|(mut p, v)| p.0 += v.0);
assert_eq!(world.get::<Position>(entity).unwrap().0, 2.0);
assert!(world.remove::<Active>(entity).is_some());
assert!(world.despawn(entity));
```

Entities are generational handles; tables hold dense columns. Tuple bundles of
1–16 components need no declaration. Components and resources must be
`Send + Sync`, since parallel branches run on worker threads; storing an `Rc`
does not compile.

### Component behavior

Three one-line traits change what a component means. Types without them
compile to the plain insert path.

```rust
use static_ecs::prelude::*;
struct Locomotion;
#[derive(Default)]
struct Speed(f32);
struct Walking;
struct Running;
impl OneOf for Walking { type Groups = (Locomotion,); }
impl OneOf for Running { type Groups = (Locomotion,); }
// Walking or running brings a Speed.
impl Requires for Walking { type Required = (Speed,); }
impl Requires for Running { type Required = (Speed,); }
// An id that never changes in place.
struct Id(u64);
impl Immutable for Id {}

let mut world = App::new().register::<Walking>().register::<Running>().register::<Id>().build().world;
let entity = world.spawn_bundle((Id(7), Walking));
assert!(world.get::<Speed>(entity).is_some()); // required, as Default
world.insert(entity, Running); // replaces Walking in the same move
assert!(world.get::<Walking>(entity).is_none());
```

- **`Requires`**: inserting the component also inserts what it requires as
  `Default`, recursively, unless present or given, in one table move.
- **`OneOf`**: an entity holds at most one member of each group; a component
  may join up to 8 groups. Removed members' `OnReplace`/`OnRemove` hooks run
  first, with the old value readable. Exclusion is per group, not transitive.
- **`Immutable`**: no `&mut` access compiles, so the value changes only by
  insert or removal, and its hooks see every change.

### Resources

Resources are components of a hidden entity that queries never see.

```rust,ignore
App::new()
    .insert_resource(Gravity(9.8))
    .init_resource::<Score>() // Default, unless already inserted
    .add_systems(Update, |gravity: Res<Gravity>, mut score: ResMut<Score>| score.0 += 1)
```

`Res`/`ResMut` schedule like queries of `T`. A missing resource skips the
system; `Option<Res<T>>` (any `Option<P>`) runs anyway. Worlds and `Commands`
insert and remove resources too.

### States

Each state is its own type, so its data needs no enum matching:

```rust,ignore
struct Game;
struct Menu;
struct Playing { level: u32 }
impl State for Menu { type Group = Game; }
impl State for Playing { type Group = Game; }

App::new()
    .init_state(Menu)
    .add_sub_state::<Playing, Running>() // Running::default() while Playing
    .add_computed_state(|p: &Playing| (p.level >= 3).then_some(Hard))
    .add_systems(OnEnter::<Playing>, |playing: Res<Playing>| { /* ... */ })
    .add_systems(OnTransition::<Menu, Playing>, spawn_level)
    .add_systems(Update, tick.run_if(in_state::<Playing>()))
```

`world.set_state(Playing { level: 2 })` (or through `Commands`) exits the
group's active state and enters the new one. Substates and computed states
exit innermost first and enter outermost first; a transition is atomic, so
commands its hooks queue apply after all of it.

### Queries

- `Query<D, F>`: `D` is `&T`, `&mut T`, `Entity`, `Option<D>`, `Has<T>`,
  relation joins, or tuples of up to 16. `F` is `With`, `Without`, `MayHave`,
  `Added`, `Changed`, `Related`, `Relinked`, tuples (and), or `Or<(...)>`.
- `Single<D, F>` requires exactly one match, otherwise the system is skipped.
- Prefer `for_each` (or `contiguous_mut` for slices) in hot loops: it runs one
  tight, vectorizable loop per table. A `for` loop goes through `next`.
- Change detection: mutable items are `Mut<T>`, marking rows changed on write.
  `Components<T, true>` adds per-column summaries so `Added`/`Changed` skip
  whole columns.
- Default filters: queries skip entities holding a `Disabling` component
  (`Disabled` is built in; `impl Disabling for Prefab {}` makes your own)
  unless they mention it, by fetching it, `Option<&T>`, `Has<T>`, a filter on
  it, or `MayHave<T>`, which matches every row. Each disabling component needs
  its own mention. Joins skip such targets the same way, unless their inner
  query mentions the component. `World::get` and commands still see them.
  `world.disable_subtree::<D, ChildOf>(root)` inserts `D` down a hierarchy
  without allocating, and `enable_subtree` removes it, leaving subtrees that
  were disabled on their own (their roots carry `DisabledRoot<D>`) disabled.

### Composing systems

```rust,ignore
app.add_systems(Update, ((integrate, decay).parallel(), validate).chain().run_if(running))
```

Tuples run in order, `.parallel()` runs branches concurrently (or fails to
compile), `.run_if` gates a group on a read-only boolean system,
`exclusive(|world: &mut World<_>| ...)` gets the whole world, and
`ready((node(...), ...))` builds a checked dependency graph. Systems take up to
16 parameters: `Query`, `Single`, `Res`, `ResMut`, `Option<P>`, `Local<T>`,
`Commands<_>`.

`Commands` record structural changes without structural access, so they run in
parallel groups. Each system's commands live inline in its own queue (no
boxing, no locks) and apply when the `emit` finishes, in system order.

### Hooks

Hooks are systems under lifecycle labels:

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

`OnAdd` (newly added), `OnInsert` (any write), `OnReplace` (before an old value
is overwritten or removed), `OnRemove`. Their commands apply before the
outermost structural operation returns. Each (component, label) pair compiles
to one function; worlds without hooks skip the lookup entirely.

### Relations

A relation is a type whose value is the edge's data. Insert `R.to(target)`
like any component, then read across edges inside queries:

```rust
use static_ecs::prelude::*;
use static_ecs::{From, To}; // not in the prelude: `From` would shadow std's

struct DockedAt { berth: u8 }
impl Relation for DockedAt {
    type Kind = OneToMany; // each ship docks at one station
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
app.emit(&Update);
assert_eq!(app.world.get::<Cargo>(station).unwrap().0, 12);
assert_eq!(app.world.get::<Pos>(wing).unwrap().0, 90);

// `From` lists a target's sources.
let mut stations = app.world.query::<From<DockedAt, &Name>>();
assert_eq!(stations.get_mut(station).unwrap().count(), 2);
```

| | `OneToOne` | `OneToMany` | `ManyToMany` |
|---|---|---|---|
| `To<R, Q>` | `Option` | `Option` | iterator |
| `From<R, Q>` | `Option` | iterator | iterator |

`ACYCLIC` refuses edges that would close a cycle, `LINKED` despawns sources
with their target, `ORDERED` keeps link lists in insertion order (with
`relate_at` to place a source among its siblings), and `FRAGMENTING` makes the
target part of the table key so one target's sources are contiguous. Links are
`Immutable`, so `&mut` access cannot corrupt the edge index.

### Async tasks and remote bridges

`task(body)` is a plugin: an async body polled during `emit`, at its place in
the schedule. A bridge runs a closure with system parameters when the frame
reaches the task:

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

Futures elsewhere (tokio, other threads) reach the world through a sync point:
place `sync_point(Net)` in the schedule and call `world.remote().bridge(Net,
...)` from anywhere. The driver grants queued bridges in parallel waves of
non-conflicting access; warm bridges allocate nothing, and an idle sync point
costs one load. [`examples/tokio_web.rs`](examples/tokio_web.rs) fetches pages
on tokio and inserts the responses while frames keep running.

### Behaviors

A behavior is async logic per entity: `impl Behavior for Slider` gives the
future each entity holding the `Slider` marker runs, and
`.add_plugins(behavior::<Slider, _>(Frame))` polls them at that place under
`Frame`. Adding the marker starts the future; removing it or despawning the
entity drops it.

```rust,ignore
impl Behavior for Slider {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            let Drag(dx) = cx.on::<Drag>(me).await; // delivered to `me` only
            cx.with(|mut q: Query<&mut Value>| q.get_mut(me).unwrap().0 += dx).unwrap();
        }
    }
}
world.deliver(slider, Drag(4.0)); // wakes exactly the futures awaiting it
```

- **Only woken futures run.** `world.deliver(entity, event)` (or `Commands`)
  wakes what awaits `E` on that entity through `Inbox<E>`, a resource the
  world must store; any waker a future hands out (timers, tokio) queues it
  too and calls the app's `Wakeup` resource, so an event loop can sleep.
  Ten thousand idle behaviors cost nothing per frame.
- **`cx.with(|params...| ...)`** runs a closure with system parameters
  immediately, validated at compile time like a system; its parameter state
  persists per call site.
- **Futures stay put**: they live in chunks with stable addresses, one
  reusable waker each; warm deliveries and polls allocate nothing.
- **Hidden entities freeze**: a behavior woken while its entity holds a
  `Disabling` component waits until it is shown again.
- Futures are owned by the app, not the world, and must be `Send`.

### Execution model

- **One invocation per emit**: every system under the label runs as one chain
  of typed continuations; sequences, conditions, parallel groups, and graphs
  lower into it.
- **Persistent workers**: `WorkerPool::new(n)` starts `n - 1` threads; the
  caller is worker 0 and helps while it waits. Idle workers steal unstarted
  branches, spin briefly, then park. Without `install`, a process-wide pool
  sized by `available_parallelism` is used. `ExecutionMode::Serial` uses no
  workers at all.
- **Panics** cancel unpublished work; `emit` resumes the first panic after
  every published branch finished. The pool stays usable.

## Devtools

Built for agents (and people) debugging a running app. Enable the `devtools`
feature; without it none of this exists, and the generated code is
instruction-for-instruction what it was (checked on every example binary).

- **Change reporting**: every change tick is unique and remembers who issued
  it: a system, the commands a system queued (with the queuing line), a
  direct `World` call (with its file:line), a task, or a devtools edit.
  `world.changes_since(tick)` lists live rows with their writers.
- **History**: each `emit` records the values that changed (described
  through `Debug`, or through compile-time reflection of their fields when
  there is none) and every structural event. `world.timeline(entity)`
  replays an entity's life, despawn included; `world.history()` answers
  questions over frames: `value_at`, `first`, `when` (runs of frames where a
  condition held), and response checks (whenever A, then B within N frames).
  Conditions are text: `x < 3 && y > 0`, `== Idle`.
- **Provenance**: `world.explain(entity, "Hp", depth)` says which system
  wrote a value, what that system reads on the same entity, and who wrote
  those, frame by frame back.
- **"Why" questions**: `app.why_not(system, entity)` names, per query, the
  missing component, the rejecting filter, the unmentioned `Disabling`
  component, or the change window; `app.why_component("Wings")` lists the
  systems using it, the components requiring it, and the builder calls that
  stored it; `app.why_order(a, b)` names the chain, parallel group, graph, or
  registration order between two systems.
- **Stepping**: `app.step(&Update)` runs one system, run condition, or task
  of the label's pass at a time, reporting what it changed; the pass applies
  its commands when it ends, as `emit` would.
- **Time travel**: `app.set_auto_snapshots(every, keep)` snapshots the world
  (components that are `Clone`, or have no drop glue); `app.rewind(frame)`
  restores one; `app.fork()` and `app.in_fork(&mut fork, |app| …)` run ahead
  in a copy with the app's own systems; `world.diff(other)` compares.
- **Task console**: `app.tasks()` shows each task's state, the label its
  bridge waits for, polls, wakes, and how long it has been idle.
- **Edits**: `world.set_field(entity, "Armor", "value", "7")` writes a scalar
  field in place, like a debugger: only inside plain structs, never
  `Immutable` components.
- **Assembly per system**: from a binary built with
  `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` (code generation unchanged),
  `devtools::asm::system(binary, "game::integrate")` (or `file.rs:LINE` for a
  closure) collects every instruction whose inline chain passes through the
  system, and `asm::diff(before, after, …)` diffs two builds.

### MCP server

All of the above is served to agents over the Model Context Protocol, on the
app's own thread between frames:

```rust,ignore
let mut tools = static_ecs::devtools::mcp::Session::unix("/tmp/game.sock")?;
tools.label("Update", Update); // agents may emit and step it
loop {
    tools.pump(&mut app); // serves requests; blocks while an agent pauses
    app.emit(&Update);
}
```

Point the agent at `static-ecs-mcp /tmp/game.sock` (the
[`tools/mcp-bridge`](../../tools/mcp-bridge) crate relays stdio to the socket), or
use `Session::stdio()` when the app does not print. `Session::serve` hands
the loop to the agent instead. Tools disclose progressively: `overview` first,
then `systems`, `entities`, `entity`, `changes`, `explain`, `timeline`,
`history`, `why_not`, `why_component`, `why_order`, `tasks`, `step`, `emit`,
`pause`/`resume`, `snapshot`/`rewind`, `what_if` (edits run in one fork, not
in another, and the two are diffed), `set`, and `asm`. Lists page with
`offset`/`limit` and say how to go deeper; entities are `12v0`, systems `#3`.
[`examples/devtools.rs`](examples/devtools.rs) is a complete session.

## Tradeoffs

- **Nightly only**, on incomplete language features.
- **Compile time** grows with the app: every system is part of one type. The
  4,113-system benchmark type-checks in about 44 s as loose systems, or 4.7 s
  when registered through a plugin (checked once at `build`).
- **Debug binaries are large**: the app's type names are enormous, and full
  debug info stores them (2.3 GB for the benchmark). In the crate that builds
  the app, `[profile.dev] debug = "line-tables-only"` roughly halves that.
- **No runtime system registration**: the schedule is fixed when the app type
  is compiled. Changing a system changes the world's type.
- Very long chains may need a higher `#![recursion_limit]`; graph compile time
  grows quadratically with nodes per graph; parallel groups need `Send`
  systems.
- Not yet: Bevy's broader engine (rendering, assets, input).
  The [`demo/`](../../demo) crates show apps built on plugins, including a
  CPU-rendered Wayland client whose first frame is drawn 1.5 ms after launch.

## Run it

```sh
cargo run --release --example simulation
cargo run --release --example relations
cargo run --release --example tokio_web   # remote bridges from tokio (network)
cargo run --release -p flappy             # CPU-rendered Wayland client
cargo test --workspace --release          # from the workspace root
```
