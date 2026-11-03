use static_ecs::prelude::*;
fn main() {
    // No system uses u64 and it is not registered, so the world cannot store it.
    let mut app = App::new().add_systems((), |_: Query<&u32>| {}).build();
    app.world.spawn_bundle((1_u64,));
}
