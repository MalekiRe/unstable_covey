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
    let system = |mut q: Query<(To<Partner, &mut u32>, From<Partner, &mut u32>)>| {
        // A partner's partner may be either side: both joins write one value.
        q.each(|pair| { std::hint::black_box(pair); })
    };
    let mut app = App::new()
        .relation::<Docked>()
        .relation::<Partner>()
        .relation::<Likes>()
        .add_systems(Update, system)
        .build();
    app.emit(&Update);
}
