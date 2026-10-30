use static_ecs::prelude::*;
fn condition(mut value: Single<&mut u32>) -> bool {
    **value += 1;
    **value > 0
}
fn system() {}
fn main() {
    let mut world = World::<Components<u32>>::default();
    let entity = world.spawn();
    world.insert(entity, 0_u32);
    let mut app = App::new(world).add_systems((), system.run_if(condition));
    app.emit(&());
}
