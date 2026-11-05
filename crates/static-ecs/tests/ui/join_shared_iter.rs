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
    let system = |mut q: Query<To<Docked, &mut u32>>| {
        // Two sources docked at one target would yield two `&mut` at once.
        for target in &mut q {
            std::hint::black_box(target);
        }
    };
    let mut app = App::new()
        .relation::<Docked>()
        .relation::<Partner>()
        .relation::<Likes>()
        .add_systems(Update, system)
        .build();
    app.emit(&Update);
}
