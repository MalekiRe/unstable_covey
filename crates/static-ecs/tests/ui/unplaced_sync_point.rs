use static_ecs::prelude::*;
struct A;
struct Net;
impl SyncPoint for Net {}
fn main() {
    let app = App::new().register::<A>().build();
    let _ = app.world.remote().bridge(Net, |_: Query<&A>| {});
}
