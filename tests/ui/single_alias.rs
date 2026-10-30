use static_ecs::prelude::*;
fn main() {
    let mut world = World::<Components<u32>>::default();
    let entity = world.spawn();
    world.insert(entity, 1_u32);
    let mut app = App::new(world).add_systems((), |mut write: Single<&mut u32>, read: Single<&u32>| {
        **write += **read;
    });
    app.emit(&());
}
