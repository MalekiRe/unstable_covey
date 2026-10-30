use static_ecs::prelude::*;
struct Missing;
fn main() {
    let mut app = App::new(World::<Components<u32>>::default())
        .add_systems((), |_: Query<&u32, Without<Missing>>| {});
    app.emit(&());
}
