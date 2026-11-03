use static_ecs::prelude::*;
fn main() {
    App::new().add_systems((), (
        |mut value: Single<&mut u32>| **value += 1,
        |mut values: Query<&u32>| { for value in &mut values { std::hint::black_box(value); } },
    ).parallel()).build();
}
