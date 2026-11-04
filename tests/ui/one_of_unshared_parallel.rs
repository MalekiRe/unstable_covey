// Exclusion is per group: Running and Dead share none, so an entity may hold
// both, and writers filtered by each still conflict.
use static_ecs::prelude::*;
struct Locomotion;
struct Life;
struct Running;
struct Dead;
impl OneOf for Running {
    type Groups = (Locomotion,);
}
impl OneOf for Dead {
    type Groups = (Life,);
}
fn main() {
    App::new().add_systems((), (
        |_: Query<&mut u32, With<Running>>| {},
        |_: Query<&mut u32, With<Dead>>| {},
    ).parallel()).build();
}
