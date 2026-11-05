use static_ecs::prelude::*;
fn condition(mut value: Single<&mut u32>) -> bool {
    **value += 1;
    **value > 0
}
fn system() {}
fn main() {
    App::new().add_systems((), system.run_if(condition)).build();
}
