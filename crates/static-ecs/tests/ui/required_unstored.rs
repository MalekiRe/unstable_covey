// Required components of an explicitly declared world must be stored, recursively.
use static_ecs::prelude::*;
use static_ecs::Components;
#[derive(Default)]
struct Pos;
#[derive(Default)]
struct Mass;
struct Bird;
impl Requires for Bird {
    type Required = (Pos,);
}
impl Requires for Pos {
    type Required = (Mass,);
}
fn main() {
    let mut world = World::<((Components<Bird>, Components<Pos>), ())>::default();
    world.spawn_bundle((Bird,));
}
