use static_ecs::prelude::*;
struct A;
struct B;
fn main() {
    // Distinct marker types do not prove structural disjointness.
    App::new().add_systems((), (
        |_: Query<&mut u32, With<A>>| {},
        |_: Query<&mut u32, With<B>>| {},
    ).parallel()).build();
}
