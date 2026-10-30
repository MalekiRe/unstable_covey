use static_ecs::prelude::*;
fn main() {
    let mut world = World::<(Components<u32>, (Components<bool>, Components<u32>))>::default();
    let entity = world.spawn();
    world.insert(entity, 1_u32);
}
