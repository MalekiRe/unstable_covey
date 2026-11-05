# static-ecs workspace

| Path | What it is |
|---|---|
| [`crates/static-ecs`](crates/static-ecs) | The ECS: the whole app is one Rust type ([README](crates/static-ecs/README.md), [benchmarks](crates/static-ecs/BENCHMARKS.md)) |
| [`crates/static-ui`](crates/static-ui) | The UI on static-ecs: components, flattened Clay layout, `Closed` panels |
| [`crates/static-ui-wayland`](crates/static-ui-wayland) | static-ui on Wayland: CPU raster of damaged rectangles, TrueType glyphs, frames only when needed |
| [`crates/static-ui-layout`](crates/static-ui-layout) | Clay's layout algorithm over flat arrays, no dependencies |
| [`crates/static-ui-clay-oracle`](crates/static-ui-clay-oracle) | Test-only: clay.h as the oracle for the layout port (100,000 random trees), and a layout benchmark |
| [`demo/`](demo) | Apps built from plugins, including a CPU-rendered Wayland client |
| [`tools/mcp-bridge`](tools/mcp-bridge) | Relays an MCP client's stdio to an app's devtools socket |
| [`UI_PLAN.md`](UI_PLAN.md) | The plan for an async, Clay-layout UI on static-ecs |

```sh
cargo test --workspace --release
cargo run --release -p flappy
cargo run --release -p static-ui-wayland --example sidebar
```

Nightly only; the toolchain is pinned in `rust-toolchain.toml`.
