use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
// Immutable resources (like states) are replaced, never written in place.
struct Level(u32);
impl Immutable for Level {}
fn main() {
    let mut app = App::new()
        .add_systems(Update, |mut level: ResMut<Level>| level.0 += 1)
        .build();
    app.emit(&Update);
}
