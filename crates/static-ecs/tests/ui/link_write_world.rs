use static_ecs::prelude::*;
struct Docked;
impl Relation for Docked {
    type Kind = OneToMany;
}
fn main() {
    let mut app = App::new().relation::<Docked>().build();
    let (ship, station) = (app.world.spawn(), app.world.spawn());
    app.world.relate(ship, Docked, station);
    std::hint::black_box(app.world.get_mut::<Sources<Docked>>(station));
}
