use static_ecs::prelude::*;
use static_ecs::{From, To};
struct Docked;
impl Relation for Docked {
    type Kind = OneToMany;
}
struct Partner;
impl Relation for Partner {
    type Kind = OneToOne;
}
struct Likes;
impl Relation for Likes {
    type Kind = ManyToMany;
}
struct Update;
impl Schedule for Update {}
fn main() {
    let system = |mut q: Query<To<Likes, &mut u32>>| q.each(|targets| targets.for_each(|mut t| *t += 1));
    let mut app = App::new()
        .relation::<Docked>()
        .relation::<Partner>()
        .relation::<Likes>()
        .add_systems(Update, system)
        .build();
    app.emit(&Update);
}
