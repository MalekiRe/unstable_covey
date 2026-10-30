# static-ecs

A small ECS whose schedules are Rust types. Plugins, systems, and event routing
compose into one concrete application type; `emit` runs a frame as statically
dispatched, inlinable code. No `TypeId`, runtime registries, boxed systems, or
external dependencies.

It relies on **nightly full specialization** (`nightly-2026-10-25`), an incomplete
feature with known soundness holes; the compiler warning is left visible. Typed
parameter fetching and the executor use internal `unsafe`, guarded by
compile-time access checks and sealed traits. This is an experiment.

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

let mut world = World::<(Components<Position>, Components<Velocity>)>::default();
let entity = world.spawn();
world.insert(entity, Position(0.0));
world.insert(entity, Velocity(2.0));

let mut app = App::new(world).add_systems(
    Update,
    |mut query: Query<(&mut Position, &Velocity)>| {
        for (mut position, velocity) in &mut query {
            position.0 += velocity.0;
        }
    },
);
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
any list, without knowing the application or other plugins:

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

`install` consumes the bus or app and returns a new type; nested tuples install
left to right. Keep the event type visible in cross-crate return types
(`Subscribe<E, F>` is `On<E, Listener<F>>`). Events are `'static` types. The
`demo/` crates compose two independent plugin crates.

`App<State, List>` adds a world. `add_systems(Label, systems)` registers systems
for a zero-sized label; `.on(|event: &E, world| ...)` and `system(...)` fragments
receive the payload and exclusive world access. Handlers run in registration order.

## Storage

The world declares its component vocabulary as nested pairs of `Components<T>`
(dense) and `Sparse<T>` markers. Each type appears once; undeclared or duplicate
types fail at compile time. Entities are generational handles; archetype tables
hold dense columns, and sparse components move without moving dense rows.

```rust
use static_ecs::prelude::*;
struct Position(f32);
struct Velocity(f32);
struct Active;
type Actor = (Position, Velocity, Active);
type State = (
    (Components<Position>, (Components<Velocity>, Components<Active>)),
    BundleSlot<Actor>,
);
let mut world = World::<State>::default();
let entity = world.spawn_bundle((Position(0.0), Velocity(2.0), Active));
for (mut position, velocity) in &mut world.query_filtered::<(&mut Position, &Velocity), With<Active>>() {
    position.0 += velocity.0;
}
assert_eq!(world.get::<Position>(entity).unwrap().0, 2.0);
assert!(world.remove::<Active>(entity).is_some());
assert!(world.despawn(entity));
```

- `spawn`, `insert`, `remove`, `despawn`, `get`, `get_mut` work on any declared
  component. `insert` and `remove` return the previous value.
- `BundleSlot<B>` declares a tuple bundle (1–8 components) for `spawn_bundle`,
  `spawn_batch`, `insert_bundle`, and `remove_bundle`. Each bundle caches its
  destination archetype in a world field selected at compile time.
- Global state is ordinary component data on singleton entities.
- Structural changes need `&mut World`: between emissions, in `exclusive`
  systems, or in payload handlers.

## Queries

- `Query<D, F>`: `D` is `&T`, `&mut T`, `Entity`, or tuples of those. `F` is
  `With<T>`, `Without<T>`, `Added<T>`, `Changed<T>`, tuples (and), or `Or<(F, G)>`.
- `Single<D, F>` requires exactly one match; otherwise the system is skipped
  without running or advancing its locals.
- Registered queries cache matching archetypes and scan only new ones.
- `query.contiguous_mut()` yields one slice per matching table, or an error for
  row filters and fragmented rows.
- `world.query`, `world.query_filtered`, and `world.single` work outside systems.

Mutable items are `Mut<T>`: mutable dereferencing marks the row changed. Each
system has its own change window; skipped systems keep pending changes.
`Components<T, true>` enables per-column change summaries that let
`Added`/`Changed` skip whole columns.

Change ticks are a Lamport clock carried along the schedule rather than a shared
counter: a writer issues the next tick on its own path, joins take the maximum,
and the world stores the result when the frame ends. Systems that conflict are
always ordered, so every write is either visible to a reader's current window
or to its next one. Unordered, non-conflicting writers may share a tick value.

## Systems and composition

Systems are functions or closures taking up to four parameters: `Query`,
`Single`, and `Local<T>` (per-registration state).

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

Limits: very long continuation chains may need a higher `#![recursion_limit]` in
the crate that builds the app. Graph compile time grows quadratically with nodes
per graph. Parallel groups need `Send` systems and `Send + Sync` world data.
Missing Bevy features include Commands, hooks, observers, states, resources as a
separate store, and runtime system registration.

Benchmarks against Bevy 0.20 are in [BENCHMARKS.md](BENCHMARKS.md).
