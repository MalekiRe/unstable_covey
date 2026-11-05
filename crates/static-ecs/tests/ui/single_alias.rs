use static_ecs::prelude::*;
fn main() {
    App::new().add_systems((), |mut write: Single<&mut u32>, read: Single<&u32>| {
        **write += **read;
    }).build();
}
