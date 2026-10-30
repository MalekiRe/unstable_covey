use static_ecs::prelude::*;
fn main() {
    let mut world = World::<Components<u32>>::default();
    let entity = world.spawn();
    world.insert(entity, 0_u32);
    let mut app = App::new(world).add_systems((), (
        |mut value: Single<&mut u32>| **value += 1,
        |mut values: Query<&u32>| { for value in &mut values { std::hint::black_box(value); } },
    ).parallel());
    app.emit(&());
}
