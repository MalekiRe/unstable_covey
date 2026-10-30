use demo_input::Click;
use static_ecs::{Subscribe, subscribe};

// Introduced here: neither the dispatcher nor analytics knows this type.
pub struct PacketReceived {
    pub bytes: usize,
}

pub type Plugin<C, P> = (Subscribe<Click, C>, Subscribe<PacketReceived, P>);

pub fn plugin<'a>(
    clicks: &'a mut u32,
    bytes: &'a mut usize,
) -> Plugin<impl FnMut(&Click) + 'a, impl FnMut(&PacketReceived) + 'a> {
    (
        subscribe(move |_: &Click| *clicks += 1),
        subscribe(move |packet: &PacketReceived| *bytes += packet.bytes),
    )
}
