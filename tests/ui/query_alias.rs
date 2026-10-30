use static_ecs::prelude::*;

struct Update;

fn alias(mut query: Query<(&mut u32, &u32)>) {
    for (mut write, read) in &mut query {
        *write += *read;
    }
}

fn main() {
    let mut world = World::new(Components::<u32>::default());
    let entity = world.spawn();
    world.insert(entity, 1_u32);
    let mut app = App::new(world).add_systems(Update, alias);
    app.emit(&Update);
}
