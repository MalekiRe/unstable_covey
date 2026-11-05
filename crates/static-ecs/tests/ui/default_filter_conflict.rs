use static_ecs::prelude::*;

struct Hp(u32);
struct Prefab;
impl Disabling for Prefab {}

// `MayHave<Prefab>` sees prefab rows too, so it overlaps `With<Prefab>`.
fn main() {
    App::new()
        .add_systems(
            (),
            (
                |mut all: Query<&mut Hp, MayHave<Prefab>>| all.iter_mut().for_each(|mut hp| hp.0 += 1),
                |mut prefabs: Query<&mut Hp, With<Prefab>>| prefabs.iter_mut().for_each(|mut hp| hp.0 += 1),
            )
                .parallel(),
        )
        .build();
}
