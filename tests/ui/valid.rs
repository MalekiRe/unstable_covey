use static_ecs::prelude::*;
struct Tag;
fn main() {
    let mut world = World::<(Components<u32>, Components<Tag>)>::default();
    let a = world.spawn();
    let b = world.spawn();
    world.insert(a, 0_u32);
    world.insert(a, Tag);
    world.insert(b, 10_u32);
    let mut app = App::new(world).add_systems((), (
        |mut value: Single<&mut u32, With<Tag>>| **value += 1,
        |mut values: Query<&mut u32, Without<Tag>>| { for mut value in &mut values { *value += 2; } },
    ).parallel());
    app.emit(&());
    assert_eq!(app.world.get::<u32>(a), Some(&1));
    assert_eq!(app.world.get::<u32>(b), Some(&12));
}
