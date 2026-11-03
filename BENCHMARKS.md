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
