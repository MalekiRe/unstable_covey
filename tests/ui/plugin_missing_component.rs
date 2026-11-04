use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct Pos(i32);
struct Vel(i32);
// `Vel` is used but not declared: the world never stores it.
struct Physics;
impl Plugin for Physics {
    type Components = (Pos,);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        schedule(Update, |mut q: Query<(&mut Pos, &Vel)>| {
            for (mut pos, vel) in &mut q {
                pos.0 += vel.0;
            }
        })
    }
}
fn main() {
    let mut app = App::new().add_plugins(Physics).build();
    app.emit(&Update);
}
