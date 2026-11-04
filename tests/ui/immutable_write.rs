use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
// Changed only by inserting a new value: writing in place does not compile.
struct Level(u32);
impl Immutable for Level {}
fn main() {
    let mut app = App::new()
        .add_systems(Update, |mut q: Query<&mut Level>| {
            for mut level in &mut q {
                level.0 += 1;
            }
        })
        .build();
    app.emit(&Update);
}
