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
    App::new()
        .relation::<Docked, _>()
        .add_systems(Update, (
            |mut q: Query<To<Docked, &mut u32>>| q.each(|t| { std::hint::black_box(t); }),
            |mut values: Query<&u32>| { for value in &mut values { std::hint::black_box(value); } },
        ).parallel())
        .build();
}
