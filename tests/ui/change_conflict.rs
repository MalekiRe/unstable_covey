use static_ecs::prelude::*;
fn main() {
    let world = World::<Components<u32>>::default();
    let mut app = App::new(world).add_systems((), (
        |_: Query<Entity, Changed<u32>>| {},
        |_: Query<&mut u32>| {},
    ).parallel());
    app.emit(&());
}
