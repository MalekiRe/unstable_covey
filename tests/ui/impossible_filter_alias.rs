use static_ecs::prelude::*;
struct Tag;
fn main() {
    let mut app = App::new(World::<(Components<u32>, Components<Tag>)>::default())
        .add_systems((), |_: Query<(&mut u32, &u32), (With<Tag>, Without<Tag>)>| {});
    app.emit(&());
}
