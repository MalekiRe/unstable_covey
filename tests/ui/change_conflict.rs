use static_ecs::prelude::*;
fn main() {
    let mut app = App::new().add_systems((), (
        |_: Query<Entity, Changed<u32>>| {},
        |_: Query<&mut u32>| {},
    ).parallel()).build();
    app.emit(&());
}
