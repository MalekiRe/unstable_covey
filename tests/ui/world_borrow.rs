use static_ecs::prelude::*;
fn main() {
    let mut world = World::<Components<u32>>::default();
    let entity = world.spawn();
    world.insert(entity, 1_u32);
    let mut query = world.query::<&mut u32>();
    let item = query.iter_mut().next().unwrap();
    world.despawn(entity);
    *item += 1;
}
