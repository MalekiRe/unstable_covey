use static_ecs::SyncPoint;
use static_ecs::prelude::*;
struct Update;
impl Schedule for Update {}
struct A;
struct B;
fn main() {
    let (point, remote) = SyncPoint::within::<Query<&mut A>>();
    let _app = App::new().register::<B>().add_systems(Update, point).build();
    let _ = remote.bridge(|_: Query<&mut B>| {});
}
