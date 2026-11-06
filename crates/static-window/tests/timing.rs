//! Display timing against the scripted compositor: the time step follows
//! presentation reports (or frame callbacks without them), and the loop's
//! latency counters.
mod support;

use std::time::Duration;

use static_ecs::prelude::*;
use static_window::*;
use support::compositor::{Compositor, Globals};

const NOW: Option<Duration> = Some(Duration::ZERO);
/// A 100 Hz display, in nanoseconds.
const R: u64 = 10_000_000;

fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// The frame times `Step` saw, and whether to repaint on the next one.
#[derive(Default)]
struct Log {
    times: Vec<FrameTime>,
    repaint: bool,
}

macro_rules! test_app {
    () => {
        App::new()
            .add_plugins(WindowPlugin)
            .init_resource::<Log>()
            .add_systems(Step, |time: Res<FrameTime>, mut log: ResMut<Log>, mut fbs: Query<&mut Framebuffer>| {
                log.times.push(*time);
                if std::mem::take(&mut log.repaint) {
                    fbs.each(|mut fb| fb.request(PixelRect::new(0, 0, 1, 1)));
                }
            })
            .add_systems(label::<KeyInput>(), |_: &KeyInput, mut fbs: Query<&mut Framebuffer>| {
                fbs.each(|mut fb| fb.request(PixelRect::new(0, 0, 1, 1)))
            })
            .build()
    };
}

fn last<S: Storage + Stores<Log>>(world: &World<S>) -> FrameTime {
    *world.resource::<Log>().unwrap().times.last().unwrap()
}
fn repaint<S: Storage + Stores<Log>>(world: &mut World<S>) {
    world.resource_mut::<Log>().unwrap().repaint = true;
}

macro_rules! mapped {
    ($globals:expr) => {{
        let (compositor, socket) = Compositor::spawn($globals);
        let mut lp = WindowLoop::from_socket(socket).unwrap();
        let mut app = test_app!();
        let window = app.world.spawn_bundle((Window::new("timing").size(16, 16), Framebuffer::default()));
        lp.step(&mut app, NOW);
        compositor.with(|s, _| s.configure(0, 0, 0, &[]));
        lp.step(&mut app, NOW);
        (compositor, lp, app, window)
    }};
}

#[test]
fn the_step_is_the_gap_between_predicted_display_times() {
    let (compositor, mut lp, mut app, _) = mapped!(Globals::default());
    // The mapping commit shows a little in the future (so the report is not
    // stale by the time the loop reads it).
    let t0 = now_ns() + 200_000_000;
    compositor.with(move |s, _| s.present(0, t0, R as u32));
    repaint(&mut app.world);
    lp.step(&mut app, NOW);
    assert_eq!(last(&app.world).display_ns, t0 + R, "the next commit shows a refresh later");
    // That step committed; its report lands where predicted.
    compositor.with(move |s, _| s.present(0, t0 + R, R as u32));
    lp.step(&mut app, NOW);
    let time = last(&app.world);
    assert_eq!(time.display_ns, t0 + 2 * R);
    assert_eq!(time.dt, R as f32 / 1e9, "the step is one refresh, not the wall clock");
}

#[test]
fn without_presentation_time_frame_callbacks_and_the_output_mode_pace_it() {
    let (compositor, mut lp, mut app, window) = mapped!(Globals { presentation: false, ..Globals::default() });
    compositor.with(|s, _| s.enter(0, 0));
    app.world.insert(window, Animating);
    lp.step(&mut app, NOW);
    // The callback's millisecond timestamp, a little in the future.
    let shown_ms = (now_ns() / 1_000_000 + 200) as u32;
    compositor.with(move |s, _| assert_eq!(s.fire_frames(0, shown_ms), 1));
    lp.step(&mut app, NOW);
    // The harness's output refreshes at 120 Hz.
    let expected = shown_ms as u64 * 1_000_000 + 8_333_333;
    let display = last(&app.world).display_ns;
    assert!(display.abs_diff(expected) < 1_000_000, "{display} vs {expected}");
}

#[test]
fn latency_from_input_to_commit_and_to_the_screen_is_recorded_on_request() {
    let (compositor, mut lp, mut app, _) = mapped!(Globals::default());
    lp.record_latency(true);
    assert!(lp.stats().first_commit_ns > 0);
    compositor.with(|s, _| {
        s.present(0, now_ns(), R as u32);
        s.keyboard_enter(0, &[]);
        s.key(30, true);
    });
    lp.step(&mut app, NOW);
    let shown = now_ns() + 50_000_000;
    compositor.with(move |s, _| s.present(0, shown, R as u32));
    lp.step(&mut app, NOW);
    let stats = lp.stats();
    assert_eq!(stats.input_to_commit_ns.len(), 1);
    assert_eq!(stats.input_to_present_ns.len(), 1);
    assert!(stats.input_to_present_ns[0] > stats.input_to_commit_ns[0]);
    assert_eq!(stats.input_frames, 1);
}

#[test]
fn latency_is_not_kept_unless_asked() {
    let (compositor, mut lp, mut app, _) = mapped!(Globals::default());
    compositor.with(|s, _| {
        s.keyboard_enter(0, &[]);
        s.key(30, true);
    });
    lp.step(&mut app, NOW);
    compositor.with(move |s, _| s.present(0, now_ns(), R as u32));
    lp.step(&mut app, NOW);
    let stats = lp.stats();
    assert!(stats.input_to_commit_ns.is_empty() && stats.input_to_present_ns.is_empty());
}
