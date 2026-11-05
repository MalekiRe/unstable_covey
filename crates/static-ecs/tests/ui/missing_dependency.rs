use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct A;
struct Missing;
fn main() {
    App::new().add_systems(Update, ready(node(A, || {}).after(Missing))).build();
}
