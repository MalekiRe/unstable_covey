use static_ecs::prelude::*;
struct Update;
struct A;
struct Missing;
fn main() {
    App::new(World::new(())).add_systems(Update, ready(node(A, || {}).after(Missing)));
}
