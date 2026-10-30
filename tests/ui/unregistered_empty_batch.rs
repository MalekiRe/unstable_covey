use static_ecs::prelude::*;
fn main() {
    World::<Components<u32>>::default().spawn_batch(std::iter::empty::<(u32,)>());
}
