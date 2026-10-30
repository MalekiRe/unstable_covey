use static_ecs::prelude::*;
struct Update;
struct A;
fn main() {
    App::new(World::new(())).add_systems(Update, ready((node(A, || {}), node(A, || {}))));
}
