use demo_input::Click;
use static_ecs::{Plugin, Storage, Systems, schedule};

/// A plugin is a type: it names what it stores, then builds its systems for
/// whatever world the app ends up with. It never names that world.
pub struct Analytics<'a> {
    pub total: &'a mut u32,
}
impl Plugin for Analytics<'_> {
    type Components = ();
    fn systems<S: Storage>(self) -> impl Systems<S> {
        let mut count = 0;
        schedule(Click, move |click: &Click| {
            count += 1;
            *self.total += click.0 * count;
        })
    }
}
