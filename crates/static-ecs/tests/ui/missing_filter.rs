use static_ecs::prelude::*;
struct Missing;
fn main() {
    // An explicit standalone schema still rejects undeclared components.
    let mut world = World::<Components<u32>>::default();
    world.query_filtered::<&u32, Without<Missing>>();
}
