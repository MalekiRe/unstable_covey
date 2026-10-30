use static_ecs::prelude::*;
fn main() {
    type Schema = (Components<u32>, (BundleSlot<(u32,)>, BundleSlot<(u32,)>));
    World::<Schema>::default().spawn_bundle((1_u32,));
}
