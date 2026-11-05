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

## Relations redesign (`793da2e` → `b6436f9`)

Joins (`To`/`From`), access split into own/direct/far, `cascade`, and every
`QueryData::fetch` now receiving a `Reach` (world lookup context). The matched
simulation uses no joins, so this measures their cost when unused. Five
alternating repetitions per row, cores 4–7, no other load
(`relations-redesign-paired.json`):

| Actors | Workers | Before ms | After ms | Change | Init before/after ms |
|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0698 | 0.0701 | +0.5% | 1.6 / 1.6 |
| 0 | 4 | 0.0287 | 0.0298 | +3.7% | 1.0 / 1.1 |
| 4,096 | 1 | 1.248 | 1.263 | +1.2% | 3.7 / 3.7 |
| 4,096 | 4 | 1.205 | 1.218 | +1.1% | 3.2 / 3.4 |
| 100,000 | 1 | 98.6 | 100.9 | +2.4% | 131 / 131 |
| 100,000 | 4 | 83.5 | 84.2 | +0.8% | 110 / 111 |

Final states match in all 60 processes. Intermediate commits measured
0.0365–0.0368 ms on the empty 4-worker row (+27%) while `b6436f9`, with more
code, measured 0.0294: the same kind of build-to-build code generation swing
as the schema change above, not a per-frame cost of the new paths.

## Plugins as types (`20db359` → `b0f81c2`)

Plugins declare their components and build their systems for the final world;
`build` instantiates them. `complex_static_plugin` registers every system of
the matched simulation except the exclusive mutation stage through one plugin
(`type Components` lists all 25 types); `complex_static` is unchanged. Five
rotating repetitions per row, cores 4–7 (`plugin-paired.json`):

| Actors | Workers | Before ms | After ms | Change | As plugin ms | Change |
|---:|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0698 | 0.0704 | +0.8% | 0.0713 | +2.2% |
| 0 | 4 | 0.0345 | 0.0350 | +1.3% | 0.0372 | +7.8% |
| 4,096 | 1 | 1.282 | 1.269 | −1.1% | 1.273 | −0.7% |
| 4,096 | 4 | 1.239 | 1.212 | −2.2% | 1.209 | −2.4% |
| 100,000 | 1 | 101.0 | 100.0 | −1.0% | 100.1 | −0.9% |
| 100,000 | 4 | 86.0 | 84.7 | −1.5% | 84.2 | −2.1% |

Final states match in all 90 processes. The empty 4-worker row swings by about
5 µs between builds of identical source (0.0298 and 0.0345 ms for `20db359`);
a run on an earlier build of the plugin variant measured +28% there with 0.8%
more instructions and no extra context switches, so it reflects how the
parallel frame's waits line up, not per-frame work.

Compile time of the one binary crate (release, `-Ztime-passes`, single runs):

| Build | Type check s | Mono collection s | Total s |
|---|---:|---:|---:|
| before, `complex_static` | 41.2 | 22.2 | 69.6 |
| after, `complex_static` | 44.0 | 21.1 | 71.2 |
| after, `complex_static_plugin` | 4.7 | 21.7 | 32.7 |

A plugin's systems are checked against the world once, at `build`, instead of
each system extending the inferred component set in turn, so type checking
drops by 89% and the build by 53%.

## Wayland launch: `demo/flappy`

A CPU-rendered Wayland client (pure-Rust protocol, no toolkit, no GPU) whose
game is a static-ecs plugin. It asks for its window first, then builds the app
and draws the first frame into shared memory while the compositor replies.
`flappy-launch` spawns it 20 times and reports when each step finished, in ms
after spawn; `presented` is the compositor's scanout timestamp for the first
frame, and `steady` a later frame's commit-to-scanout latency. COSMIC
(cosmic-comp 1.2), 120 Hz output; `static` is `-C target-feature=+crt-static`:

| Step | Static median | Dynamic median |
|---|---:|---:|
| process running (`main`) | 0.50 | 0.83 |
| connected, globals bound | 0.65 | 0.97 |
| ECS app built, game spawned | 0.70 | — |
| first frame drawn | 1.52 | 1.80 |
| window configured | 1.54 | 1.82 |
| first frame committed | 1.55 | 1.83 |
| first frame on screen | 60.1 (min 56.9) | 65.9 (min 57.3) |
| later frame, commit to screen | 15.5 (min 9.5) | — |

The client is done 1.5 ms after spawn; most of drawing is the first touch of
the fresh 1.3 MB buffer (pre-populating both buffers with `MAP_POPULATE` moved
1.4 ms onto that path instead). The compositor shows a new window's first
frame about 55 ms after its commit, against about two refreshes for later
frames.

## Query caches allocate once (`22a6108` → current)

A query parameter's cache of matching tables now keeps its first four tables
inside its box, so a system's first run allocates once per query instead of
twice. Seven alternating repetitions per row (`inline-state-paired.json`):

| Actors | Workers | Frame before ms | Frame after ms | Change | Init before ms | Init after ms |
|---:|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0722 | 0.0693 | −4.0% | 1.86 | 1.44 |
| 0 | 4 | 0.0296 | 0.0290 | −2.2% | 1.14 | 1.23 |
| 4,096 | 1 | 1.291 | 1.279 | −0.9% | 3.81 | 3.63 |
| 4,096 | 4 | 1.233 | 1.208 | −2.0% | 3.56 | 3.28 |
| 100,000 | 1 | 100.0 | 100.5 | +0.5% | 129 | 130 |
| 100,000 | 4 | 85.2 | 85.8 | +0.7% | 114 | 113 |

Final states match in all 84 processes. The first frame alone drops from
1.08–1.28 to 0.88–0.90 ms (no actors, one worker). Storing the whole cache
inline in each system instead cut the empty-world frame by 8%, but the app
(198 KB here, built and returned by value) grew past what the main thread's
stack holds across the builder's moves, and init rose by 2.4 ms: caches stay
boxed.

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

## Immutable, required, resources, states (`760dd62` → current)

None of the four features adds work to apps that do not use them: bundles
without requirements take the plain insert path (a constant branch), query
caches never list resource tables, and hook entries are unchanged for plain
lifecycle labels. Seven alternating repetitions per row of the same app
(`features-paired.json`):

| Actors | Workers | Frame before ms | Frame after ms | Change | Init before ms | Init after ms |
|---:|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0695 | 0.0685 | −1.4% | 1.42 | 1.47 |
| 0 | 4 | 0.0298 | 0.0293 | −1.6% | 1.27 | 1.10 |
| 4,096 | 1 | 1.271 | 1.268 | −0.3% | 3.65 | 3.62 |
| 4,096 | 4 | 1.221 | 1.218 | −0.3% | 3.35 | 3.30 |
| 100,000 | 1 | 101.1 | 102.2 | +1.1% | 133 | 133 |
| 100,000 | 4 | 86.4 | 86.3 | −0.1% | 117 | 119 |

Final states match in all 84 processes. Rerun alone with eleven alternating
repetitions, the 100,000-actor single-worker row measures 100.83 → 100.82 ms
(0.0%). The all-plugin benchmark still compiles in about 33 s.

## Column line pairs (`32c005e` → `f8e3597`)

Implementing `OneOf` slowed the empty-world 4-worker frame from 0.029 to
0.036 ms in every process, with the same instruction count. Neither code
layout (64-byte function alignment changed nothing) nor parking (about 12
voluntary context switches per run in every build) explained it; glibc
malloc tunables alone flipped the same binary, so it was heap placement.
Parallel systems write different singleton columns every frame (`Clock`,
`Grid`, each `Lane<N>`): their rows and change ticks were tiny separate
allocations, and whichever two malloc packed into one cache-line pair, two
workers contended on. Column buffers and summaries now take whole 128-byte
pairs. Empty 4-worker frame, slow processes out of 7 per malloc layout:

| Build | default | `tcache_count=0` | `mmap_threshold=4096` | `top_pad=1M` |
|---|---:|---:|---:|---:|
| `32c005e` | 0/7 | 5/7 | 2/7 | 7/7 |
| `OneOf`, unpadded columns | 7/7 | 7/7 | 7/7 | 0/7 |
| `OneOf`, padded columns | 0/7 | 0/7 | 0/7 | 1/7 |

Seven alternating repetitions per row (`columns-paired.json`); final states
match in all 84 processes:

| Actors | Workers | Frame before ms | Frame after ms | Change | Init before ms | Init after ms |
|---:|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0674 | 0.0671 | −0.4% | 1.45 | 1.43 |
| 0 | 4 | 0.0288 | 0.0283 | −1.7% | 0.99 | 1.02 |
| 4,096 | 1 | 1.253 | 1.244 | −0.8% | 3.71 | 3.49 |
| 4,096 | 4 | 1.197 | 1.185 | −1.0% | 3.04 | 3.16 |
| 100,000 | 1 | 100.0 | 100.3 | +0.4% | 129 | 127 |
| 100,000 | 4 | 85.2 | 85.8 | +0.7% | 113 | 115 |

Cost: up to 3 × 127 bytes per column of each table.

## OneOf (`f8e3597` → `6424201`)

Apps without `OneOf` members take the plain insert path (a constant
branch); the sibling-filter rule (`42ecb27`) is compile-time only. Seven
alternating repetitions per row (`one-of-paired3.json`); final states match
in all 84 processes:

| Actors | Workers | Frame before ms | Frame after ms | Change | Init before ms | Init after ms |
|---:|---:|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0679 | 0.0675 | −0.5% | 1.54 | 1.45 |
| 0 | 4 | 0.0289 | 0.0287 | −0.7% | 1.12 | 1.21 |
| 4,096 | 1 | 1.285 | 1.273 | −0.9% | 3.92 | 3.55 |
| 4,096 | 4 | 1.219 | 1.225 | +0.5% | 3.17 | 3.23 |
| 100,000 | 1 | 96.4 | 95.8 | −0.6% | 123 | 127 |
| 100,000 | 4 | 85.0 | 85.0 | 0.0% | 115 | 115 |

The 0-actor 4-worker row overlapped a compile in that run (0.0336 before);
shown instead is a quiet rerun of 15 alternating processes per build:
`32c005e` 0.0292, `f8e3597` 0.0289, `6424201` 0.0287 ms (medians).

## Idle async

The same simulation, plus an idle exclusive sync point under `Update` and a
task waiting on a label that is never emitted, against the unchanged
simulation built from the same commit. Five alternating repetitions per row.

With sync points as values (`1cb8313`, `async-paired.json`), the world type
was unchanged:

| Actors | Workers | Without ms | With ms | Change |
|---:|---:|---:|---:|---:|
| 0 | 1 | 0.0679 | 0.0673 | −0.8% |
| 0 | 4 | 0.0279 | 0.0278 | −0.4% |
| 4,096 | 1 | 1.254 | 1.224 | −2.4% |
| 4,096 | 4 | 1.096 | 1.051 | −4.1% |
| 100,000 | 1 | 98.7 | 96.0 | −2.7% |
| 100,000 | 4 | 88.0 | 85.3 | −3.1% |

With sync points as marker types, placing one adds `Point<Net>` to the
inferred schema, so every system compiles against a different world type.
Two runs (`async-paired-markers*.json`) were 0.2–4.8% slower. A third
variant with the same schema but no running sync point and no task
(`schema-check.json`) is as slow:

| Actors | Workers | Baseline ms | Schema only | With async |
|---:|---:|---:|---:|---:|
| 4,096 | 1 | 1.244 | +2.0% | +2.1% |
| 4,096 | 4 | 1.055 | +3.2% | +0.9% |
| 100,000 | 4 | 79.9 | +3.4% | +2.2% |

So the difference is code generation for the changed world type, not the
idle point (one load per frame) or task (a few loads). Final states match in
every process.

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
cd ../ecs-asm-comparison   # next to the workspace root
cargo build --release --bins
python3 run_complex.py --cpus 0,1,2,3 --output results.json
python3 run_complex.py --actors 4096 --frames 126 --warmup 0 --repeat 1 \
  --verify-each --cpus 0,1,2,3 --output check.json
python3 run_blend.py --entities 1024 --frames 12 --warmup 2 --repeat 1 \
  --cpus 0,1,2,3 --output blend.json
```
