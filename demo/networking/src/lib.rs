use demo_input::Click;
use static_ecs::{Callback, On, system};

// Introduced here: neither the app nor analytics knows this type.
pub struct PacketReceived {
    pub bytes: usize,
}

pub type Plugin<C, P> = (On<Click, Callback<C>>, On<PacketReceived, Callback<P>>);

pub fn plugin<'a, W>(
    clicks: &'a mut u32,
    bytes: &'a mut usize,
) -> Plugin<impl FnMut(&Click, &mut W) + 'a, impl FnMut(&PacketReceived, &mut W) + 'a> {
    (
        system(move |_: &Click, _: &mut W| *clicks += 1),
        system(move |packet: &PacketReceived, _: &mut W| *bytes += packet.bytes),
    )
}
