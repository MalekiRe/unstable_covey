use demo_input::Click;
use static_ecs::{Listener, Ref, Schedule, schedule};

// Introduced here: neither the app nor analytics knows this label.
pub struct PacketReceived(pub usize);
impl Schedule for PacketReceived {
    type In = Ref<Self>;
}

pub type Plugin<C, P> = (Listener<Click, C>, Listener<PacketReceived, P>);

pub fn plugin<'a>(
    clicks: &'a mut u32,
    bytes: &'a mut usize,
) -> Plugin<impl FnMut(&Click) + 'a, impl FnMut(&PacketReceived) + 'a> {
    (
        schedule(Click, move |_: &Click| *clicks += 1),
        schedule(PacketReceived, move |packet: &PacketReceived| {
            *bytes += packet.0
        }),
    )
}
