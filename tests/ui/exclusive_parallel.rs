use static_ecs::prelude::*;
fn main() {
    let mut app = App::new(World::<Components<u32>>::default()).add_systems((), (
        exclusive(|world: &mut World<Components<u32>>| { world.spawn(); }),
        |_: Query<&u32>| {},
    ).parallel());
    app.emit(&());
}
