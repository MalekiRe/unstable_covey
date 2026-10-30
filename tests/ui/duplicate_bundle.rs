use static_ecs::prelude::*;
fn main() {
    type Schema = (Components<u32>, BundleSlot<(u32, u32)>);
    let mut world = World::<Schema>::default();
    world.spawn_bundle((1_u32, 2_u32));
}
