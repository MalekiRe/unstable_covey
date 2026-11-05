use static_ecs::prelude::*;
struct A;
struct B;
fn main() {
    App::new()
        .add_systems((), (
            |_: Query<&mut u32, Or<(With<A>, With<B>)>>| {},
            |_: Query<&mut u32, Without<A>>| {},
        ).parallel())
        .build();
}
