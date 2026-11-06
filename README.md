# static-ecs workspace

| Path | What it is |
|---|---|
| [`crates/static-ecs`](crates/static-ecs) | The ECS: the whole app is one Rust type ([README](crates/static-ecs/README.md), [benchmarks](crates/static-ecs/BENCHMARKS.md)) |
| [`crates/static-window`](crates/static-window) | Windows as entities on Wayland, modeled on StereoKit's sk_app: events as labels, input, software framebuffers, display-paced frames, title bars ([README](crates/static-window/README.md)) |
| [`crates/static-ui`](crates/static-ui) | The UI on static-ecs: components, flattened Clay layout, input, behaviors, widgets, transitions, `Closed` panels |
| [`crates/static-ui-raster`](crates/static-ui-raster) | static-ui drawn on the CPU into a static-window framebuffer: damaged rectangles only, TrueType glyphs, any display scale |
| [`crates/static-ui-layout`](crates/static-ui-layout) | Clay's layout algorithm over flat arrays, no dependencies |
| [`crates/static-ui-clay-oracle`](crates/static-ui-clay-oracle) | Test-only: clay.h as the oracle for the layout port (100,000 random trees), and a layout benchmark |
| [`demo/`](demo) | Apps built from plugins, including flappy bird in a static-window window |
| [`tools/mcp-bridge`](tools/mcp-bridge) | Relays an MCP client's stdio to an app's devtools socket |
| [`WINDOW_PLAN.md`](WINDOW_PLAN.md) | The design of static-window, after StereoKit's windowing |
| [`WINDOW_BENCHMARKS.md`](WINDOW_BENCHMARKS.md) | static-window against its budgets, the loops it replaced, and winit |
| [`UI_PLAN.md`](UI_PLAN.md) | The design of static-ui: async behaviors, Clay layout, `Disabling`-toggled panels |
| [`UI_BENCHMARKS.md`](UI_BENCHMARKS.md) | static-ui against its budgets, clay.h, and Bevy UI |

```sh
cargo test --workspace --release
cargo run --release -p flappy
cargo run --release -p static-window --example hello         # two windows; every event printed
cargo run --release -p static-ui-raster --example sidebar    # Clay's README example
cargo run --release -p static-ui-raster --example widgets    # every widget; F12 cycles the debug overlay
```

Nightly only; the toolchain is pinned in `rust-toolchain.toml`.
