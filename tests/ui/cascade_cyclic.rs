use static_ecs::prelude::*;
use static_ecs::To;
struct ChildOf;
impl Relation for ChildOf {
    type Kind = OneToMany;
}
struct Update;
impl Schedule for Update {}
fn main() {
    // Without ACYCLIC, a cycle of rows has no root to start from.
    let mut app = App::new()
        .relation::<ChildOf, _>()
        .add_systems(Update, |mut q: Query<(To<ChildOf, &u32>, &mut u32)>| {
            q.cascade(|(parent, mut value)| *value += parent.map_or(0, |p| *p));
        })
        .build();
    app.emit(&Update);
}
