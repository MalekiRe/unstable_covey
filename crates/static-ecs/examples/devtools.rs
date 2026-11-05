//! A small simulation serving devtools to an agent over MCP.
//!
//! ```sh
//! cargo run --release --features devtools --example devtools -- /tmp/sim.sock
//! ```
//! then point an MCP client at `static-ecs-mcp /tmp/sim.sock` (the
//! `tools/mcp-bridge` crate). The agent drives frames with `emit`/`step`;
//! `quit` ends the session.
use static_ecs::devtools::mcp::Session;
use static_ecs::prelude::*;

#[derive(Clone, Debug)]
struct Position(f32);
#[derive(Clone, Debug)]
struct Velocity(f32);
#[derive(Clone, Debug)]
struct Health(u32);
#[derive(Clone, Debug)]
struct Poisoned;
struct Update;
impl Schedule for Update {}

fn integrate(mut query: Query<(&mut Position, &Velocity)>) {
    for (mut position, velocity) in &mut query {
        position.0 += velocity.0;
    }
}

fn poison(mut query: Query<&mut Health, With<Poisoned>>) {
    for mut health in &mut query {
        health.0 = health.0.saturating_sub(7);
    }
}

fn main() {
    let socket = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/tmp/static-ecs-devtools.sock".into());
    let mut app = App::new()
        .add_systems(
            Update,
            (
                (integrate, poison).chain(),
                |mut q: Query<(Entity, &Health)>, mut commands: Commands<_>| {
                    for (entity, health) in &mut q {
                        if health.0 == 0 {
                            commands.despawn(entity);
                        }
                    }
                },
            ),
        )
        .build();
    app.set_auto_snapshots(1, 256);
    for i in 0..8 {
        let entity = app
            .world
            .spawn_bundle((Position(0.0), Velocity(i as f32), Health(30)));
        if i % 3 == 0 {
            app.world.insert(entity, Poisoned);
        }
    }
    let mut session = Session::unix(&socket).expect("bind the devtools socket");
    session.label("Update", Update);
    eprintln!("devtools on {socket}");
    session.serve(&mut app);
}
