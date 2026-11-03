use static_ecs::prelude::*;
struct Tag;
fn main() {
    let mut app = App::new().add_systems((), (
        |mut value: Single<&mut u32, With<Tag>>| **value += 1,
        |mut values: Query<&mut u32, Without<Tag>>| { for mut value in &mut values { *value += 2; } },
    ).parallel()).build();
    let a = app.world.spawn_bundle((0_u32, Tag));
    let b = app.world.spawn_bundle((10_u32,));
    app.emit(&());
    assert_eq!(app.world.get::<u32>(a), Some(&1));
    assert_eq!(app.world.get::<u32>(b), Some(&12));
}
