use static_ecs::prelude::*;
struct A;
struct B;
fn main() {
    let mut app = App::new(World::<(Components<u32>, (Components<A>, Components<B>))>::default())
        .add_systems((), (
            |_: Query<&mut u32, Or<(With<A>, With<B>)>>| {},
            |_: Query<&mut u32, Without<A>>| {},
        ).parallel());
    app.emit(&());
}
