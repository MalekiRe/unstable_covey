use static_ecs::prelude::*;
struct A;
struct B;
fn main() {
    let mut world = World::<(Components<u32>, (Components<A>, Components<B>))>::default();
    let left = world.spawn();
    world.insert(left, 1_u32);
    world.insert(left, A);
    let right = world.spawn();
    world.insert(right, 2_u32);
    world.insert(right, B);
    // Currently distinct populations do not prove structural disjointness.
    let mut app = App::new(world).add_systems((), (
        |_: Query<&mut u32, With<A>>| {},
        |_: Query<&mut u32, With<B>>| {},
    ).parallel());
    app.emit(&());
}
