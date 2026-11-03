use static_ecs::prelude::*;
fn main() {
    let mut app = App::new().add_systems((), (
        exclusive(|world: &mut World<_>| { world.spawn(); }),
        |_: Query<&u32>| {},
    ).parallel()).build();
    app.emit(&());
}
