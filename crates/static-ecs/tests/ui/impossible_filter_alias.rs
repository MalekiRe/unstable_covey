use static_ecs::prelude::*;
struct Tag;
fn main() {
    App::new()
        .add_systems((), |_: Query<(&mut u32, &u32), (With<Tag>, Without<Tag>)>| {})
        .build();
}
