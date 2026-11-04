use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct A;
struct B;
struct Writes;
impl SyncPoint for Writes {
    type Env = Query<'static, &'static mut A>;
}
fn main() {
    let app = App::new().register::<B>().add_systems(Update, sync_point(Writes)).build();
    let _ = app.world.remote().bridge(Writes, |_: Query<&mut B>| {});
}
