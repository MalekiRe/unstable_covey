//! Behaviors with the UI: scopes, animation, timers, frozen panels, idle
//! cost.
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::Duration;

use static_ecs::prelude::*;
use static_ecs::{Behavior, Cx, behavior};
use static_ui::*;
use static_window::{Animating, MouseButton, WakeAt};

mod support;
use support::{Input, send};

/// Behaviors share counters: run these tests one at a time.
static SERIAL: Mutex<()> = Mutex::new(());

/// A frame as the window loop runs it: deadlines start over, then the UI's
/// update and frame.
fn frame<S: Storage + Stores<WakeAt>, L>(app: &mut App<S, L>)
where
    L: static_ecs::Dispatch<UiUpdate, World<S>> + static_ecs::Dispatch<UiFrame, World<S>>,
{
    if let Some(mut wake) = app.world.resource_mut::<WakeAt>() {
        *wake = WakeAt::default();
    }
    app.emit(&UiUpdate);
    app.emit(&UiFrame);
}

#[derive(Default)]
struct Shown;
/// Shows a tooltip in a scope while hovered; the scope closes on leave.
struct Tooltip;
impl Behavior for Tooltip {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            cx.on::<PointerEnter>(me).await;
            scoped(cx, async |scope: &Scope<S>| {
                scope.spawn(Some(me), el((Node::row(), Floating::default(), Shown)).children((el((Text::new("tip", 12),)),)));
                cx.on::<PointerLeave>(me).await;
            })
            .await;
        }
    }
}

#[test]
fn scoped_entities_die_with_their_scope_and_with_the_behavior() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = App::new()
        .add_plugins((UiPlugin::<Monospace>::default(), InputPlugin, behavior::<Tooltip, _>(UiUpdate)))
        .register::<Shown>()
        .build();
    let button = app.world.spawn_ui(el((Node::row().width(Sizing::fixed(80.0)).height(Sizing::fixed(30.0)), Tooltip)));
    frame(&mut app);
    let shown = |app: &mut App<_, _>| app.world.query_filtered::<Entity, With<Shown>>().iter_mut().count();
    send(&mut app, Input::PointerMoved { x: 10.0, y: 10.0 });
    frame(&mut app);
    assert_eq!(shown(&mut app), 1);
    send(&mut app, Input::PointerMoved { x: 300.0, y: 300.0 });
    frame(&mut app);
    assert_eq!(shown(&mut app), 0, "leaving closed the scope");
    send(&mut app, Input::PointerMoved { x: 10.0, y: 10.0 });
    frame(&mut app);
    assert_eq!(shown(&mut app), 1);
    // Dropping the behavior (removing its marker) drops the scope too.
    app.world.remove::<Tooltip>(button);
    frame(&mut app);
    assert_eq!(shown(&mut app), 0);
    assert_eq!(app.world.query::<&Text>().iter_mut().count(), 0, "the tooltip's text went with it");
}

#[derive(Default)]
struct Progress(Vec<f32>);
struct Fade;
impl Behavior for Fade {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        animate(cx, me, Duration::from_millis(30), behave::ease_in_out, |t| {
            cx.with(|mut q: Query<&mut Progress>| q.get_mut(me).unwrap().0.push(t)).unwrap();
        })
        .await;
    }
}

#[test]
fn animations_request_frames_until_they_reach_exactly_one() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = App::new()
        .add_plugins((UiPlugin::<Monospace>::default(), behavior::<Fade, _>(UiUpdate)))
        .register::<Progress>()
        .build();
    app.world.insert_resource(UiClock::Manual(std::time::Instant::now()));
    let entity = app.world.spawn_bundle((Fade, Progress::default()));
    let window = app.world.spawn_bundle((UiWindow,));
    frame(&mut app);
    assert!(app.world.get::<Animating>(window).is_some(), "the UI window draws every frame");
    let mut frames = 0;
    while app.world.get::<Animating>(window).is_some() {
        app.world.resource_mut::<UiClock>().unwrap().advance(Duration::from_millis(7));
        frame(&mut app);
        frames += 1;
        assert!(frames < 1000);
    }
    let progress = &app.world.get::<Progress>(entity).unwrap().0;
    assert!(progress.len() >= 2, "{progress:?}");
    assert!(progress.windows(2).all(|w| w[0] <= w[1]), "monotonic: {progress:?}");
    assert_eq!(*progress.last().unwrap(), 1.0);
    assert!(progress[..progress.len() - 1].iter().all(|&t| t < 1.0));
    // Done: no more frames.
    frame(&mut app);
    assert!(app.world.get::<Animating>(window).is_none());
}

#[derive(Default)]
struct Rang(bool);
struct Alarm;
impl Behavior for Alarm {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        sleep(cx, Duration::from_millis(20)).await;
        cx.with(|mut q: Query<&mut Rang>| q.get_mut(me).unwrap().0 = true).unwrap();
    }
}

#[test]
fn timers_set_the_deadline_and_resume_after_it() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = App::new()
        .add_plugins((UiPlugin::<Monospace>::default(), behavior::<Alarm, _>(UiUpdate)))
        .register::<Rang>()
        .build();
    let start = std::time::Instant::now();
    app.world.insert_resource(UiClock::Manual(start));
    let entity = app.world.spawn_bundle((Alarm, Rang::default()));
    frame(&mut app);
    let deadline = app.world.resource::<WakeAt>().unwrap().get().expect("a timer is pending");
    assert_eq!(deadline, start + Duration::from_millis(20));
    app.world.resource_mut::<UiClock>().unwrap().advance(Duration::from_millis(19));
    frame(&mut app);
    assert!(!app.world.get::<Rang>(entity).unwrap().0, "not before the deadline");
    app.world.resource_mut::<UiClock>().unwrap().advance(Duration::from_millis(1));
    frame(&mut app);
    assert!(app.world.get::<Rang>(entity).unwrap().0);
    assert_eq!(app.world.resource::<WakeAt>().unwrap().get(), None);
    assert!(app.world.resource::<Timers>().unwrap().is_empty());
}

/// Accumulates horizontal drag distance.
#[derive(Default)]
struct Dragged(f32);
static DRAG_POLLS: AtomicUsize = AtomicUsize::new(0);
struct Thumb;
impl Behavior for Thumb {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            cx.on::<DragStart>(me).await;
            loop {
                DRAG_POLLS.fetch_add(1, Relaxed);
                match select(cx.on::<Drag>(me), cx.on::<DragEnd>(me)).await {
                    Either::Left(drag) => cx.with(|mut q: Query<&mut Dragged>| q.get_mut(me).unwrap().0 += drag.delta.x).unwrap(),
                    Either::Right(_) => break,
                }
            }
        }
    }
}

#[test]
fn closing_a_panel_mid_drag_freezes_the_behavior_until_it_reopens() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = App::new()
        .add_plugins((UiPlugin::<Monospace>::default(), InputPlugin, behavior::<Thumb, _>(UiUpdate)))
        .register::<Dragged>()
        .build();
    let panel = app.world.spawn_ui(el((Node::column(),)));
    let thumb = app.world.spawn_ui_under(panel, el((Node::row().width(Sizing::fixed(20.0)).height(Sizing::fixed(20.0)), Thumb, Dragged::default())));
    frame(&mut app);
    send(&mut app, Input::PointerMoved { x: 5.0, y: 5.0 });
    send(&mut app, Input::Button { button: MouseButton::Left, pressed: true });
    send(&mut app, Input::PointerMoved { x: 15.0, y: 5.0 });
    frame(&mut app);
    assert_eq!(app.world.get::<Dragged>(thumb).unwrap().0, 10.0);
    // Close the panel mid-drag: the thumb keeps receiving the drag, frozen.
    app.world.disable_subtree::<Closed, ChildOf>(panel);
    DRAG_POLLS.store(0, Relaxed);
    send(&mut app, Input::PointerMoved { x: 20.0, y: 5.0 });
    send(&mut app, Input::PointerMoved { x: 30.0, y: 5.0 });
    frame(&mut app);
    frame(&mut app);
    assert_eq!(DRAG_POLLS.load(Relaxed), 0, "frozen");
    assert_eq!(app.world.get::<Dragged>(thumb).unwrap().0, 10.0);
    // Reopen: it resumes where it was and catches up on the queued drags.
    app.world.enable_subtree::<Closed, ChildOf>(panel);
    frame(&mut app);
    assert_eq!(app.world.get::<Dragged>(thumb).unwrap().0, 25.0);
    send(&mut app, Input::Button { button: MouseButton::Left, pressed: false });
    frame(&mut app);
    send(&mut app, Input::PointerMoved { x: 40.0, y: 5.0 });
    frame(&mut app);
    assert_eq!(app.world.get::<Dragged>(thumb).unwrap().0, 25.0, "the drag ended");
}

static IDLE_POLLS: AtomicUsize = AtomicUsize::new(0);
struct Idle;
impl Behavior for Idle {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            IDLE_POLLS.fetch_add(1, Relaxed);
            cx.on::<Click>(me).await;
        }
    }
}

#[test]
fn ten_thousand_idle_behaviors_do_no_work_per_frame() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut app = App::new()
        .add_plugins((UiPlugin::<Monospace>::default(), InputPlugin, behavior::<Idle, _>(UiUpdate)))
        .build();
    let list = app.world.spawn_ui(el((Node::column(),)));
    let items: Vec<Entity> =
        (0..10_000).map(|_| app.world.spawn_ui_under(list, el((Node::row().width(Sizing::fixed(10.0)).height(Sizing::fixed(1.0)), Idle)))).collect();
    IDLE_POLLS.store(0, Relaxed);
    frame(&mut app);
    assert_eq!(IDLE_POLLS.load(Relaxed), 10_000, "each started once");
    let (runs, builds) = (app.world.resource::<UiLayout>().unwrap().runs(), app.world.resource::<RenderList>().unwrap().builds());
    for _ in 0..20 {
        frame(&mut app);
    }
    assert_eq!(IDLE_POLLS.load(Relaxed), 10_000, "no polls while idle");
    assert_eq!(app.world.resource::<UiLayout>().unwrap().runs(), runs, "no layout");
    assert_eq!(app.world.resource::<RenderList>().unwrap().builds(), builds, "no render list");
    // A click polls exactly one.
    app.world.deliver(items[1234], Click { button: MouseButton::Left, position: Vec2::default() });
    frame(&mut app);
    assert_eq!(IDLE_POLLS.load(Relaxed), 10_001);
}
