use static_ecs::prelude::*;
// States change only through set_state/clear_state, which run transitions.
struct Game;
struct Menu;
impl State for Menu {
    type Group = Game;
}
fn main() {
    let mut world = App::new().init_state(Menu).build().world;
    world.insert_resource(Menu);
}
