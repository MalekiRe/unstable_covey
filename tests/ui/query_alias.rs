use static_ecs::prelude::*;
struct Update;
fn alias(mut query: Query<(&mut u32, &u32)>) {
    for (mut write, read) in &mut query {
        *write += *read;
    }
}
fn main() {
    App::new().add_systems(Update, alias).build();
}
