// A bundle may not hold two members sharing a `OneOf` group, here `Life`.
use static_ecs::prelude::*;
use static_ecs::Components;
struct Locomotion;
struct Life;
struct Walking;
struct Dead;
impl OneOf for Walking {
    type Groups = (Locomotion, Life);
}
impl OneOf for Dead {
    type Groups = (Life,);
}
fn main() {
    let mut world = World::<(Components<Walking>, Components<Dead>)>::default();
    world.spawn_bundle((Walking, Dead));
}
