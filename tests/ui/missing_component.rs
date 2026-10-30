use static_ecs::prelude::*;
fn main() {
    let mut app = App::new(World::<Components<u32>>::default())
        .add_systems((), |_: Query<&u64>| {});
    app.emit(&());
}
