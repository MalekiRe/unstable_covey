# static-ecs workspace

| Path | What it is |
|---|---|
| [`crates/static-ecs`](crates/static-ecs) | The ECS: the whole app is one Rust type ([README](crates/static-ecs/README.md), [benchmarks](crates/static-ecs/BENCHMARKS.md)) |
| [`demo/`](demo) | Apps built from plugins, including a CPU-rendered Wayland client |
| [`tools/mcp-bridge`](tools/mcp-bridge) | Relays an MCP client's stdio to an app's devtools socket |
| [`UI_PLAN.md`](UI_PLAN.md) | The plan for an async, Clay-layout UI on static-ecs |

```sh
cargo test --workspace --release
cargo run --release -p flappy
```

Nightly only; the toolchain is pinned in `rust-toolchain.toml`.
