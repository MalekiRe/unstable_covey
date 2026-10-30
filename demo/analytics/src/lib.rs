use demo_input::Click;
use static_ecs::{Subscribe, subscribe};

pub fn plugin(total: &mut u32) -> Subscribe<Click, impl FnMut(&Click) + '_> {
    let mut count = 0;
    subscribe(move |event: &Click| {
        count += 1;
        *total += event.x * count;
    })
}
