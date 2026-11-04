use demo_input::Click;
use demo_networking::PacketReceived;
use static_ecs::{App, Schedule};

fn main() {
    let (mut analytics, mut clicks, mut bytes, mut local) = (0, 0, 0, 0);
    {
        let mut app = App::new()
            .add_plugins((
                demo_analytics::Analytics {
                    total: &mut analytics,
                },
                demo_networking::Networking {
                    clicks: &mut clicks,
                    bytes: &mut bytes,
                },
            ))
            .add_systems(Click, |click: &Click| local += click.0)
            .build();

        app.emit(&Click(3));
        app.emit(&PacketReceived(128));
        app.emit(&Click(3));
        struct Unhandled;
        impl Schedule for Unhandled {}
        app.emit(&Unhandled);
    }
    assert_eq!((analytics, clicks, bytes, local), (9, 2, 128, 6));
    println!(
        "cross-crate specialization: analytics={analytics}, clicks={clicks}, bytes={bytes}, local={local}"
    );
}
