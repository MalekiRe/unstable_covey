# Benchmarks

Local, non-isolated workstation measurements: AMD Ryzen 9 5900X, CPUs 0–3,
rustc 1.101.0-nightly (`12433c307`, LLVM 23.1.4), Bevy 0.20.1. Every build uses
release `-O3`, ThinLTO, and one codegen unit. These numbers describe these
workloads, not ECS performance in general.

The harness lives in `../ecs-asm-comparison`. Raw samples, binary SHA-256
hashes, and per-run state hashes are in the `final-*.json` files there.

## Matched simulation: 4,113 systems

`complex_*` runs the same simulation on this ECS, Bevy ECS, and Bevy App:
`complex_systems.rs` is included verbatim by every adapter, and population,
structural mutation, and snapshots are shared. Adapters differ only in world APIs
and schedule construction.

- 100,000 actors (eight components each), 10,000 projectiles, 20,000 items,
  25,000 hierarchy nodes, and two singleton entities.
- A rebuilt spatial grid drives perception, contacts, projectile hits, and
  pickups. Contact relaxation, steering, staggered AI and path search, combat,
  effects, inventory, death and respawn, transform propagation, visibility,
  render sorting, and changed-only replication.
- `Added`/`Changed` filters, run conditions, per-system locals, complementary
  filters, exclusive structural-mutation stages, and tuple bundles.
- 4,096 small stateful systems in four lanes (64 distinct function variants).

Both ECSes use the same stage boundaries, so this is not a comparison against
every possible Bevy schedule. Neither uses Commands, hooks, observers, or
resources for simulation data.

### Results

Five fresh processes per configuration, in rotating backend order. Each process
runs one initialization update, 20 warmup updates, and 100 measured updates.
Median is the median of per-process medians; p95 pools every measured frame.

| Workers | Backend | Median ms | p95 ms | Init ms | Peak RSS MiB |
|---:|---|---:|---:|---:|---:|
| 1 | static-ecs | **94.9** | 97.7 | 120 | 62.5 |
| 1 | Bevy ECS | 98.1 | 101.9 | 1,429 | 1,051.9 |
| 1 | Bevy App | 98.2 | 103.0 | 1,422 | 1,051.6 |
| 4 | static-ecs | **80.3** | 88.5 | 107 | 63.0 |
| 4 | Bevy ECS | 87.7 | 93.3 | 1,466 | 1,051.6 |
| 4 | Bevy App | 87.6 | 91.7 | 1,454 | 1,051.3 |

Initialization includes population and the first update, including Bevy's lazy
executor setup. Peak RSS includes initialization peaks.

### Correctness

Every run compares the complete canonical final state across backends and worker
counts: every actor component, projectile, item, hierarchy node, singleton
result, render list, and replication packet. Separately, `--verify-each`
compared the full state after each of 126 updates with zero, one, and 4,096
actors, on one and four workers, across all three backends. This is
differential verification of shared algorithms, not an independent oracle.

## Executor changes (`4881691` → `5e0b7a0`)

Same simulation and system bodies, comparing the commit before the current
executor (`4881691`) with `5e0b7a0`. Five alternating repetitions per row.

| Actors | Workers | Before ms | After ms | Change |
|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0723 | 0.0662 | −8.4% |
| 0 | 4 | 0.0960 | 0.0265 | **−72.4%** |
| 4,096 | 1 | 1.263 | 1.249 | −1.1% |
| 4,096 | 4 | 1.076 | 0.998 | −7.2% |
| 100,000 | 1 | 89.8 | 91.3 | +1.7% |
| 100,000 | 4 | 76.5 | 77.6 | +1.4% |

The empty-world rows isolate scheduling cost for 4,113 systems. Before, four
workers were slower than one: profiling attributed 76% of samples to the lanes,
whose hottest instructions were the compare-and-swap retry loop on the shared
world change counter. Carrying the change clock along continuations removed it.
Populated frames are dominated by system bodies; their ±2% differences are
within this machine's run-to-run variation.

## Inferred world (`5e0b7a0` → current)

The world's component set is now inferred, and bundles need no declaration: a
world-level cache keyed by a compile-time bundle hash replaces the per-bundle
fields. The benchmark adapter registers its 25 components up front, so shared
helper code can name the world type; system bodies are unchanged. Five
alternating repetitions per row:

| Actors | Workers | Before ms | After ms | Change |
|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0691 | 0.0694 | +0.5% |
| 0 | 4 | 0.0268 | 0.0268 | +0.3% |
| 4,096 | 1 | 1.269 | 1.280 | +0.9% |
| 4,096 | 4 | 1.025 | 1.015 | −1.0% |
| 100,000 | 1 | 96.9 | 98.4 | +1.5% |
| 100,000 | 4 | 79.2 | 79.9 | +0.9% |

All differences are within run-to-run variation; all 60 processes produced
identical final states, and per-update states still match Bevy for zero, one, and
4,096 actors. Results: `inferred-*.json`.

## Query loop (`4826f3b` → current)

`examples/query_loop.rs`: one system adds `Velocity` to `Position` for 1,048,576
entities in one dense archetype, compared against the same loop over raw slices
(`ideal`, which compiles to SSE `addps` plus vector tick stores). Measured as a
standalone crate with fat LTO and `panic = "abort"`, two runs each:

| Variant | Before ns/entity | After ns/entity |
|---|---:|---:|
| raw slices | 0.24–0.27 | 0.25–0.29 |
| `for` loop (`next`) | 1.39–1.43 (5.5×) | 0.91–0.94 (3.4×) |
| `for_each` (`fold`) | 0.90–0.93 (3.5×) | 0.20–0.23 (≈1×) |
| `contiguous_mut` | 0.20–0.22 | 0.19–0.22 |

Per row, the old loop branched on two row maps and a summary pointer, ran the
summary's `lock cmpxchg` when one existed, and reloaded the iterator from the
stack. Now writable summaries are raised once per table, `fold` picks the
direct-row loop once per table, and the outlined table search no longer takes
the iterator's address. A `for` loop keeps two well-predicted row-map branches:
`Query` does not name the world, so dense-only storage is not a compile-time
fact there.

Matched simulation, five alternating repetitions per row (`query-loop-paired.json`):

| Actors | Workers | Before ms | After ms | Change |
|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0666 | 0.0656 | −1.5% |
| 0 | 4 | 0.0306 | 0.0267 | −12.9% |
| 4,096 | 1 | 1.262 | 1.226 | −2.8% |
| 4,096 | 4 | 1.127 | 1.057 | −6.2% |
| 100,000 | 1 | 95.6 | 96.4 | +0.9% |
| 100,000 | 4 | 80.0 | 80.9 | +1.1% |

The 100,000-actor rows are within run-to-run variation; final-state digests
match in all 60 processes.

Rewriting all 14 query loops in the shared benchmark systems as `for_each`
(both backends, `foreach-results.json`) leaves the standings unchanged: static
is 3.3% faster than Bevy ECS with one worker and 7.4% faster with four (8.4%
with `for` loops). At 100,000 actors a frame is dominated by grid neighbor scans
and pathfinding inside the loop bodies, not by iteration.

## Commands, hooks, and relations (`ae7b98e` → `5344e82`)

These add a command queue, hook dispatch on every structural operation, and
pair-keyed tables. The matched simulation uses none of them, so this measures
their cost when unused. Five alternating repetitions per row
(`relations-paired.json`):

| Actors | Workers | Before ms | After ms | Change | Init before/after ms |
|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0666 | 0.0671 | +0.8% | 1.0 / 1.1 |
| 0 | 4 | 0.0261 | 0.0242 | −7.4% | 0.6 / 0.6 |
| 4,096 | 1 | 1.245 | 1.239 | −0.4% | 3.2 / 3.7 |
| 4,096 | 4 | 1.010 | 1.015 | +0.6% | 2.5 / 2.6 |
| 100,000 | 1 | 100.4 | 100.6 | +0.2% | 127 / 128 |
| 100,000 | 4 | 83.2 | 83.5 | +0.3% | 111 / 112 |

Frame times are within run-to-run variation (the −7.4% row included); final
states match in all 60 processes. Table creation now also compares pairs and
maintains the pair index, visible as +0.5 ms init at 4,096 actors.

## Schedule inputs (`3476d77` → current)

Systems now receive their label's input through the frame, and `add_systems`
is the only registration path. The simulation's systems are unchanged apart
from `impl Schedule for Update {}`. Five alternating repetitions per row
(`schedules-paired.json`):

| Actors | Workers | Before ms | After ms | Change | Init before/after ms |
|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0662 | 0.0672 | +1.5% | 1.1 / 1.5 |
| 0 | 4 | 0.0237 | 0.0237 | +0.1% | 0.7 / 1.0 |
| 4,096 | 1 | 1.246 | 1.237 | −0.7% | 3.2 / 3.6 |
| 4,096 | 4 | 1.012 | 1.001 | −1.0% | 2.5 / 3.0 |
| 100,000 | 1 | 99.9 | 99.9 | −0.0% | 129 / 129 |
| 100,000 | 4 | 82.8 | 82.7 | −0.0% | 110 / 110 |

Frame times are within run-to-run variation; final states match in all 60
processes. Initialization (build plus the first frame) is 0.3–0.5 ms slower in
the small cases, consistently across repetitions.

## Idle async (`1cb8313`)

The same simulation, plus an idle exclusive sync point registered under
`Update` and a task waiting on a label that is never emitted, against the
unchanged simulation, built from the same commit. Five alternating
repetitions per row (`async-paired.json`):

| Actors | Workers | Without ms | With ms | Change | Init without/with ms |
|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0679 | 0.0673 | −0.8% | 1.5 / 1.5 |
| 0 | 4 | 0.0279 | 0.0278 | −0.4% | 1.3 / 1.6 |
| 4,096 | 1 | 1.254 | 1.224 | −2.4% | 3.5 / 3.5 |
| 4,096 | 4 | 1.096 | 1.051 | −4.1% | 3.2 / 3.2 |
| 100,000 | 1 | 98.7 | 96.0 | −2.7% | 125 / 129 |
| 100,000 | 4 | 88.0 | 85.3 | −3.1% | 122 / 111 |

An idle sync point costs one load and an idle task a few loads, so the
consistently lower times with them are code-layout or background-load
effects, not a speedup; the result is "no measurable cost". Final states
match in all 60 processes.

## Blended microbenchmark

`blend_*`: 4,096 tiny systems in four parallel lanes plus a small simulation over
1,024 entities. Single smoke run, 12 measured frames.

| Workers | static-ecs ms | Bevy ECS ms | Bevy App ms |
|---:|---:|---:|---:|
| 1 | 0.231 | 0.397 | 0.321 |
| 4 | 0.177 | 2.495 | 2.462 |

## Generated code

A chain of N forks inside one parallel group (a throwaway probe program, 2
workers) produces this much code for the whole schedule:

| Forks | Previous commit | Current |
|---:|---:|---:|
| 1 | 5.0 KB | 8.4 KB |
| 4 | 31 KB | 19.8 KB |
| 8 | 501 KB | 35.5 KB |
| 32 | build did not finish in 300 s | 74.9 KB |

Each continuation is emitted once, and outlined continuations never call each
other directly, so stack depth does not grow with the number of joins.

## Reproduce

```sh
cd ../ecs-asm-comparison
cargo build --release --bins
python3 run_complex.py --cpus 0,1,2,3 --output results.json
python3 run_complex.py --actors 4096 --frames 126 --warmup 0 --repeat 1 \
  --verify-each --cpus 0,1,2,3 --output check.json
python3 run_blend.py --entities 1024 --frames 12 --warmup 2 --repeat 1 \
  --cpus 0,1,2,3 --output blend.json
```
