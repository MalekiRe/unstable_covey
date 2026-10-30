use demo_input::Click;
use demo_networking::PacketReceived;
use static_ecs::Bus;

fn main() {
    let (mut analytics, mut clicks, mut bytes, mut local) = (0, 0, 0, 0);
    {
        let mut bus = Bus::new()
            .install(demo_analytics::plugin(&mut analytics))
            .install(demo_networking::plugin(&mut clicks, &mut bytes))
            .on(|event: &Click| local += event.x);

        bus.emit(&Click { x: 3 });
        bus.emit(&PacketReceived { bytes: 128 });
        bus.emit(&Click { x: 3 });
        struct Unhandled;
        bus.emit(&Unhandled);
    }
    assert_eq!((analytics, clicks, bytes, local), (9, 2, 128, 6));
    println!(
        "cross-crate specialization: analytics={analytics}, clicks={clicks}, bytes={bytes}, local={local}"
    );
}
