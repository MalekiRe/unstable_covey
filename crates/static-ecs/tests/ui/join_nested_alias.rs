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
    let system = |mut q: Query<From<Docked, To<Partner, To<Docked, &mut u32>>>>| {
        // Sources' partners may dock at one target: one item, two `&mut`.
        q.each(|sources| sources.for_each(|t| { std::hint::black_box(t); }))
    };
    let mut app = App::new()
        .relation::<Docked>()
        .relation::<Partner>()
        .relation::<Likes>()
        .add_systems(Update, system)
        .build();
    app.emit(&Update);
}
