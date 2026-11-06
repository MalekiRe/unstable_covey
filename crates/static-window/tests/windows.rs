//! Windows against the scripted compositor: creation, configure, scale,
//! requests, closing, idling, waking, and losing the compositor.
mod support;

use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_ecs::{Behavior, Cx, behavior};
use static_window::*;
use support::compositor::{Ask, Compositor, Globals};
use wayland_protocols::xdg::shell::server::xdg_toplevel::State as Xdg;

/// What the app's systems saw, in order.
#[derive(Default)]
struct Seen(Vec<String>);

macro_rules! test_app {
    () => {
        App::new()
            .add_plugins(WindowPlugin)
            .init_resource::<Seen>()
            .add_systems(label::<Resized>(), |e: &Resized, mut seen: ResMut<Seen>| {
                seen.0.push(format!("resized {}x{} @{}", e.surface.width, e.surface.height, e.surface.scale.0))
            })
            .add_systems(label::<CloseRequested>(), |_: &CloseRequested, mut seen: ResMut<Seen>| seen.0.push("close".into()))
            .build()
    };
}

/// Step without waiting: whatever the compositor flushed is already in the
/// socket.
const NOW: Option<Duration> = Some(Duration::ZERO);

fn seen<S: Storage + Stores<Seen>>(world: &mut World<S>) -> Vec<String> {
    std::mem::take(&mut world.resource_mut::<Seen>().unwrap().0)
}

fn open(globals: Globals) -> (Compositor, WindowLoop) {
    let (compositor, socket) = Compositor::spawn(globals);
    (compositor, WindowLoop::from_socket(socket).unwrap())
}

#[test]
fn a_window_opens_with_its_settings_and_maps_at_the_first_configure() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    let window = app.world.spawn_bundle((Window::new("Notes").size(640, 480).app_id("notes").resizable(false),));
    assert_eq!(lp.step(&mut app, NOW), Flow::Continue);
    compositor.with(|s, _| {
        let t = s.toplevel(0);
        assert_eq!((t.title.as_str(), t.app_id.as_str()), ("Notes", "notes"));
        assert_eq!((t.min_size, t.max_size), ((640, 480), (640, 480)), "not resizable: fixed size");
        assert_eq!(t.decoration_modes, [2], "server-side decorations requested");
        let commits = &s.surface_of(0).commits;
        assert_eq!(commits.len(), 1);
        assert_eq!(commits[0].buffer, None, "the initial commit carries no buffer");
    });
    assert_eq!(app.world.get::<Surface>(window), None, "not configured yet");

    let serial = compositor.with(|s, _| s.configure(0, 0, 0, &[Xdg::Activated]));
    lp.step(&mut app, NOW);
    assert_eq!(app.world.get::<Surface>(window), Some(&Surface { width: 640, height: 480, scale: Scale::ONE }));
    assert!(app.world.get::<WindowState>(window).unwrap().activated);
    assert!(seen(&mut app.world).is_empty(), "the first configure is not a resize");
    compositor.with(move |s, _| {
        assert_eq!(s.toplevel(0).acked, [serial]);
        let surface = s.surface_of(0);
        let buffer = surface.buffer.expect("a placeholder maps the window");
        assert_eq!((s.buffers[buffer].width, s.buffers[buffer].height), (1, 1));
        assert_eq!(surface.viewport_destination, Some((640, 480)));
        assert_eq!(s.pixels(buffer), [0xFF00_0000]);
    });
}

#[test]
fn resizes_are_reported_once_per_real_change() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    let window = app.world.spawn_bundle((Window::new("w"),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 0, 0, &[]));
    lp.step(&mut app, NOW);
    // An interactive resize resends the same size many times.
    compositor.with(|s, _| {
        s.configure(0, 900, 700, &[Xdg::Resizing, Xdg::Activated]);
        s.configure(0, 900, 700, &[Xdg::Resizing, Xdg::Activated]);
    });
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 900, 700, &[Xdg::Activated]));
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), ["resized 900x700 @120"]);
    assert_eq!(app.world.get::<WindowState>(window), Some(&WindowState { activated: true, ..WindowState::default() }));
    compositor.with(|s, _| s.configure(0, 0, 0, &[Xdg::Maximized]));
    lp.step(&mut app, NOW);
    assert!(seen(&mut app.world).is_empty(), "a 0 size keeps ours");
    assert!(app.world.get::<WindowState>(window).unwrap().maximized);
}

#[test]
fn fractional_scale_changes_pixels_not_logical_size() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    let window = app.world.spawn_bundle((Window::new("w").size(801, 600),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 0, 0, &[]));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.preferred_scale(0, 150));
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), ["resized 801x600 @150"]);
    let surface = *app.world.get::<Surface>(window).unwrap();
    assert_eq!(surface.pixel_size(), (1002, 750));
    assert_eq!(compositor.with(|s, _| s.surface_of(0).viewport_destination), Some((801, 600)));
}

#[test]
fn without_fractional_scale_the_outputs_integer_scale_applies() {
    let (compositor, mut lp) = open(Globals { fractional_scale: false, viewporter: false, ..Globals::default() });
    let mut app = test_app!();
    let window = app.world.spawn_bundle((Window::new("w").size(300, 200),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 0, 0, &[]));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        s.set_output_scale(0, 2);
        s.enter(0, 0);
    });
    lp.step(&mut app, NOW);
    assert_eq!(app.world.get::<Surface>(window).unwrap().scale, Scale::integer(2));
    assert_eq!(seen(&mut app.world), ["resized 300x200 @240"]);
    compositor.with(|s, _| {
        let surface = s.surface_of(0);
        let buffer = surface.buffer.unwrap();
        // No viewport: the placeholder is the pixel size, at buffer scale 2.
        assert_eq!((s.buffers[buffer].width, s.buffers[buffer].height, surface.buffer_scale), (600, 400, 2));
    });
}

#[test]
fn component_writes_become_requests() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    let window = app.world.spawn_bundle((Window::new("before"),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 0, 0, &[]));
    lp.step(&mut app, NOW);

    app.world.get_mut::<Window>(window).unwrap().title = "after".into();
    app.world.insert(window, WindowMode::Maximized);
    lp.step(&mut app, NOW);
    app.world.insert(window, WindowMode::Fullscreen);
    lp.step(&mut app, NOW);
    app.world.insert(window, WindowMode::Windowed);
    app.world.insert(window, Minimize);
    lp.step(&mut app, NOW);
    assert!(app.world.get::<Minimize>(window).is_none(), "a one-shot request");
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        let t = s.toplevel(0);
        assert_eq!(t.title, "after");
        assert_eq!(t.asks, [Ask::SetMaximized, Ask::UnsetMaximized, Ask::SetFullscreen, Ask::UnsetFullscreen, Ask::SetMinimized]);
    });

    // A floating window's size is the app's to change.
    app.world.get_mut::<Window>(window).unwrap().width = 1000;
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), ["resized 1000x600 @120"]);
    assert_eq!(compositor.with(|s, _| s.surface_of(0).viewport_destination), Some((1000, 600)));
}

#[test]
fn closing_despawns_unless_the_app_decides() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    let kept = app.world.spawn_bundle((Window::new("kept").close_on_request(false),));
    let closed = app.world.spawn_bundle((Window::new("closed"),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        s.configure(0, 0, 0, &[]);
        s.configure(1, 0, 0, &[]);
        s.close(0);
        s.close(1);
    });
    assert_eq!(lp.step(&mut app, NOW), Flow::Continue);
    assert_eq!(seen(&mut app.world), ["close", "close"]);
    assert!(app.world.get::<Window>(kept).is_some());
    assert!(!app.world.contains(closed));
    compositor.with(|s, _| {
        assert!(!s.toplevel(0).destroyed);
        assert!(s.toplevel(1).destroyed && s.surface_of(1).destroyed);
    });
    // The app closes the other one itself.
    app.world.despawn(kept);
    assert_eq!(lp.step(&mut app, NOW), Flow::Done);
    assert!(compositor.with(|s, _| s.toplevel(0).destroyed && s.surface_of(0).destroyed));
}

#[test]
fn an_idle_window_commits_nothing() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    app.world.spawn_bundle((Window::new("idle"),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 0, 0, &[]));
    lp.step(&mut app, NOW);
    let (commits, ours) = (compositor.with(|s, _| s.surface_of(0).commits.len()), lp.stats.commits);
    for _ in 0..10 {
        lp.step(&mut app, NOW);
    }
    assert_eq!(compositor.with(|s, _| s.surface_of(0).commits.len()), commits);
    assert_eq!(lp.stats.commits, ours);
}

#[test]
fn the_wakeup_resource_wakes_a_sleeping_loop() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    app.world.spawn_bundle((Window::new("w"),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 0, 0, &[]));
    lp.step(&mut app, NOW);
    let wake = app.world.resource::<static_ecs::Wakeup>().unwrap().0.clone();
    std::thread::spawn(move || wake()).join().unwrap();
    // Without the wakeup this would sleep for the whole timeout.
    let started = Instant::now();
    lp.step(&mut app, Some(Duration::from_secs(30)));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn a_deadline_bounds_the_sleep() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    app.world.spawn_bundle((Window::new("w"),));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| s.configure(0, 0, 0, &[]));
    lp.step(&mut app, NOW);
    app.world.resource_mut::<WakeAt>().unwrap().request(Instant::now());
    let started = Instant::now();
    lp.step(&mut app, Some(Duration::from_secs(30)));
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn losing_the_compositor_ends_the_loop() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    app.world.spawn_bundle((Window::new("w"),));
    lp.step(&mut app, NOW);
    drop(compositor);
    assert_eq!(lp.step(&mut app, None), Flow::Disconnected);
}

#[test]
fn optional_globals_the_compositor_lacks_are_listed() {
    let (_compositor, lp) = open(Globals { decoration: false, ..Globals::default() });
    assert_eq!(lp.missing().len(), 1);
    assert!(lp.missing()[0].starts_with("xdg-decoration"));
}

#[test]
fn the_compositor_is_asked_to_draw_title_bars_except_for_undecorated_windows() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = test_app!();
    app.world.spawn_bundle((Window::new("framed"),));
    let bare = app.world.spawn_bundle((Window::new("bare").decorated(false),));
    lp.step(&mut app, NOW);
    app.world.get_mut::<Window>(bare).unwrap().decorated = true;
    lp.step(&mut app, NOW);
    // Modes: 2 server side, 1 client side (from us: none at all).
    compositor.with(|s, _| {
        assert_eq!(s.toplevel(0).decoration_modes, [2]);
        assert_eq!(s.toplevel(1).decoration_modes, [1, 2]);
    });
}

struct ConfirmClose;
impl Behavior for ConfirmClose {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        cx.on::<CloseRequested>(me).await;
        let _ = cx.with(|mut world: Commands<S>| world.insert(me, (Window::new("closing?").close_on_request(false),)));
    }
}

#[test]
fn behaviors_await_window_events_on_the_window_entity() {
    let (compositor, mut lp) = open(Globals::default());
    let mut app = App::new()
        .add_plugins(WindowPlugin)
        .register::<ConfirmClose>()
        .add_plugins(behavior::<ConfirmClose, _>(Step))
        .build();
    let window = app.world.spawn_bundle((Window::new("w").close_on_request(false), ConfirmClose));
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        s.configure(0, 0, 0, &[]);
        s.close(0);
    });
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    assert_eq!(app.world.get::<Window>(window).unwrap().title, "closing?");
    assert_eq!(compositor.with(|s, _| s.toplevel(0).title.clone()), "closing?");
}
