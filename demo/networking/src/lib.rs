use demo_input::Click;
use static_ecs::{Plugin, Ref, Schedule, Storage, Systems, schedule};

// Introduced here: neither the app nor analytics knows this label.
pub struct PacketReceived(pub usize);
impl Schedule for PacketReceived {
    type In = Ref<Self>;
}

pub struct Networking<'a> {
    pub clicks: &'a mut u32,
    pub bytes: &'a mut usize,
}
impl Plugin for Networking<'_> {
    type Components = ();
    fn systems<S: Storage>(self) -> impl Systems<S> {
        let Networking { clicks, bytes } = self;
        (
            schedule(Click, move |_: &Click| *clicks += 1),
            schedule(PacketReceived, move |packet: &PacketReceived| {
                *bytes += packet.0
            }),
        )
    }
}
