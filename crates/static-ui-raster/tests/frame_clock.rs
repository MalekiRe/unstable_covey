//! The UI's clock during a window frame: the frame's predicted display time,
//! unless a test drives the clock by hand.
#![recursion_limit = "256"]
use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_ui::*;
use static_ui_raster::{Fonts, RasterPlugin, ui_frame};
use static_window::{FrameTime, WindowPlugin};

/// What `UiClock` read during the last `UiUpdate`.
#[derive(Default)]
struct Seen(Option<Instant>);

macro_rules! app {
    () => {
        App::new()
            .add_plugins((WindowPlugin, UiPlugin::<Fonts>::default(), RasterPlugin))
            .init_resource::<Seen>()
            .add_systems(UiUpdate, |clock: Res<UiClock>, mut seen: ResMut<Seen>| seen.0 = Some(clock.now()))
            .build()
    };
}

#[test]
fn a_frame_reads_its_display_time_and_the_system_clock_resumes_after() {
    let mut app = app!();
    // The screen will show this frame 50 ms from now.
    app.world.insert_resource(FrameTime { now_ns: 1_000_000_000, display_ns: 1_050_000_000, dt: 0.0 });
    let before = Instant::now();
    ui_frame(&mut app);
    let after = Instant::now();
    let seen = app.world.resource::<Seen>().unwrap().0.unwrap();
    assert!(seen >= before + Duration::from_millis(50) && seen <= after + Duration::from_millis(50));
    assert_eq!(app.world.resource::<UiClock>(), Some(&UiClock::System));
}

#[test]
fn a_manual_clock_is_left_to_the_test() {
    let mut app = app!();
    let at = Instant::now() + Duration::from_secs(5);
    app.world.insert_resource(UiClock::Manual(at));
    app.world.insert_resource(FrameTime { now_ns: 1_000_000_000, display_ns: 1_050_000_000, dt: 0.0 });
    ui_frame(&mut app);
    assert_eq!(app.world.resource::<Seen>().unwrap().0, Some(at));
    assert_eq!(app.world.resource::<UiClock>(), Some(&UiClock::Manual(at)));
}
