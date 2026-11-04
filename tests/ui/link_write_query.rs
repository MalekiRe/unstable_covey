use static_ecs::prelude::*;
struct Docked;
impl Relation for Docked {
    type Kind = OneToMany;
}
struct Update;
impl Schedule for Update {}
fn main() {
    // Swapping two rows' link lists would corrupt the relation's index.
    let mut app = App::new()
        .relation::<Docked, _>()
        .add_systems(Update, |mut q: Query<&mut Relates<Docked>>| {
            q.each(|link| { std::hint::black_box(link); })
        })
        .build();
    app.emit(&Update);
}
