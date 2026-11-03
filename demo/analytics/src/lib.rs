use demo_input::Click;
use static_ecs::{Callback, On, system};

/// Generic over the world: the app that installs it fixes `W` at `build`.
pub fn plugin<W>(total: &mut u32) -> On<Click, Callback<impl FnMut(&Click, &mut W) + '_>> {
    let mut count = 0;
    system(move |event: &Click, _: &mut W| {
        count += 1;
        *total += event.x * count;
    })
}
