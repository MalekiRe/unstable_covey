use static_ecs::prelude::*;
fn main() {
    let mut world = World::<Components<u32>>::default();
    world.spawn_bundle((1_u32, 2_u32));
}
