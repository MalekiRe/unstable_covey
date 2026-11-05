use static_ecs::prelude::*;
struct Tag;
fn main() {
    App::new()
        .add_systems((), |_: Query<&u32, With<Tag>>| {})
        .storage::<Sparse<Tag>>()
        .build();
}
