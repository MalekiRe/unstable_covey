# static-ecs

A small ECS whose schedules are Rust types. Plugins, systems, and event routing
compose into one concrete application type; `emit` runs a frame as statically
dispatched, inlinable code. The world's component set is inferred from the
systems you add, at compile time. No schema, `TypeId`, runtime registries, boxed
systems, or external dependencies.

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

## Plugins and events

Listeners live inline in a type-level list `Cons<On<Event, Handler>, Tail>`.
Specialization selects matching handlers; every other listener is a no-op:

```rust,ignore
impl<E: 'static, S, H> MaybeHandle<E, S> for H {
    default fn maybe_handle(&mut self, _: &E, _: &mut S) {}
}
impl<E: 'static, S, H: Handle<E, S>> MaybeHandle<E, S> for On<E, H> {
    fn maybe_handle(&mut self, event: &E, state: &mut S) { self.handler.handle(event, state) }
}
```

There is no runtime event-type comparison. A plugin is a value that installs into
any app, without knowing the application or other plugins; plugins that ignore
the world stay generic over it:

```rust
use static_ecs::{App, Callback, On, system};
struct Click { x: u32 }

fn analytics<W>(total: &mut u32) -> On<Click, Callback<impl FnMut(&Click, &mut W) + '_>> {
    let mut count = 0;
    system(move |event: &Click, _: &mut W| {
        count += 1;
        *total += event.x * count;
    })
}

let mut total = 0;
{
    let mut app = App::new().install(analytics(&mut total)).build();
    app.emit(&Click { x: 3 });
    app.emit(&Click { x: 3 });
    app.emit(&42_u64); // Unhandled type: no-op, no declaration needed.
}
assert_eq!(total, 9);
```

`install` consumes the builder and returns a new type; nested tuples install
left to right. Keep the event type visible in cross-crate return types
(`On<E, Callback<F>>`). Events are `'static` types. The `demo/` crates compose
two independent plugin crates.

`App::new()` returns a builder. `add_systems(Label, systems)` registers systems
for a zero-sized label; `.on(|event: &E, world: &mut World<_>| ...)` and
`system(...)` fragments receive the payload and exclusive world access.
Handlers run in registration order. `.build()` creates the world and checks
every system against it.

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
- Exclusive systems and payload handlers name the world as `World<_>`, inferred at
  `build`. A plugin's exclusive system can instead be generic over any world
  storing what it needs: `fn tag_new<S: Has<Tag>>() -> ExclusiveSystem<impl
  FnMut(&mut World<S>)>`.

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

Systems are functions or closures taking up to four parameters: `Query`,
`Single`, `Local<T>` (per-registration state), and `Commands<_>`.

```rust,ignore
app.add_systems(Update, ((integrate, decay).parallel(), validate).chain().run_if(running))
```

- Tuples (2–4) run in order; `.chain()` states it explicitly.
- `.parallel()` runs branches concurrently. Overlapping access fails to compile;
  complementary filters (`With<T>`/`Without<T>`) prove disjointness.
- `.run_if(condition)` runs a read-only boolean system before the group.
- `exclusive(|world| ...)` gets `&mut World` and conflicts with everything.
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

Hooks are ordinary systems registered for a component's lifecycle event:

```rust
use static_ecs::prelude::*;
struct Health(u32);
struct Alive;

let mut app = App::new()
    .on_add::<Health, _, _>(|added: Trigger, mut commands: Commands<_>| {
        for &entity in added.iter() {
            commands.insert(entity, (Alive,));
        }
    })
    .register::<Alive>()
    .build();
let entity = app.world.spawn_bundle((Health(10),));
assert!(app.world.get::<Alive>(entity).is_some()); // applied before spawn returned
```

- `on_add` (newly added), `on_insert` (any write), `on_replace` (before an
  existing value is overwritten, removed, or despawned), `on_remove` (before
  removal or despawn). Several hooks for one event run in registration order;
  plugins install them with the `on_add`/... functions.
- `Trigger` holds the affected entities: one per call, all of a `spawn_batch`.
- Hooks run serially inside the structural operation, so they take system
  parameters only (`Query`, `Single`, `Local`, `Commands`); exclusive hooks fail
  to compile. Their commands apply, breadth first, before the outermost
  structural operation returns.
- The built world owns its hooks, which must be `Send + Sync + 'static`. Each
  (component, event) with hooks compiles to one function running all of them;
  an operation reaches it through one table lookup and indirect call. Worlds
  without hooks skip the table entirely.

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
Missing Bevy features include observers, relations, states, resources as a
separate store, and runtime system registration.

Benchmarks against Bevy 0.20 are in [BENCHMARKS.md](BENCHMARKS.md).
