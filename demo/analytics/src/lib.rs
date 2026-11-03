use demo_input::Click;
use static_ecs::{Listener, schedule};

/// A plugin is a registration value; it never names the app's world.
pub fn plugin(total: &mut u32) -> Listener<Click, impl FnMut(&Click) + '_> {
    let mut count = 0;
    schedule(Click, move |click: &Click| {
        count += 1;
        *total += click.0 * count;
    })
}
