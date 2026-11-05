//! Headless input: synthetic window events through hit testing, interaction
//! state, events, bubbling, scrolling and focus.
use std::future::poll_fn;
use std::pin::pin;
use std::task::Poll;

use static_ecs::prelude::*;
use static_ecs::{Behavior, Cx, behavior};
use static_ui::*;

/// Every event an entity's behavior heard, in order.
#[derive(Default)]
struct Log(Vec<String>);

/// Logs clicks, presses, drags, text and focus changes into `Log`.
struct Recorder;
impl Behavior for Recorder {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            let mut click = pin!(cx.on::<Click>(me));
            let mut drag = pin!(cx.on::<Drag>(me));
            let mut start = pin!(cx.on::<DragStart>(me));
            let mut end = pin!(cx.on::<DragEnd>(me));
            let mut text = pin!(cx.on::<TextInput>(me));
            let mut focus_in = pin!(cx.on::<FocusIn>(me));
            let mut focus_out = pin!(cx.on::<FocusOut>(me));
            let entry = poll_fn(|c| {
                if let Poll::Ready(e) = click.as_mut().poll(c) {
                    return Poll::Ready(format!("click {:?}", e.button));
                }
                if let Poll::Ready(e) = start.as_mut().poll(c) {
                    return Poll::Ready(format!("drag start {},{}", e.position.x, e.position.y));
                }
                if let Poll::Ready(e) = drag.as_mut().poll(c) {
                    return Poll::Ready(format!("drag {},{}", e.delta.x, e.delta.y));
                }
                if end.as_mut().poll(c).is_ready() {
                    return Poll::Ready("drag end".into());
                }
                if let Poll::Ready(TextInput(ch)) = text.as_mut().poll(c) {
                    return Poll::Ready(format!("text {ch}"));
                }
                if focus_in.as_mut().poll(c).is_ready() {
                    return Poll::Ready("focus in".into());
                }
                if focus_out.as_mut().poll(c).is_ready() {
                    return Poll::Ready("focus out".into());
                }
                Poll::Pending
            })
            .await;
            cx.with(|mut logs: Query<&mut Log>| logs.get_mut(me).unwrap().0.push(entry)).unwrap();
        }
    }
}

const BUTTON: Node = Node::row().width(Sizing::fixed(100.0)).height(Sizing::fixed(40.0));

fn log<S: Storage>(world: &World<S>, entity: Entity) -> Vec<String> {
    world.get::<Log>(entity).unwrap().0.clone()
}

macro_rules! ui_app {
    () => {
        App::new()
            .add_plugins((UiPlugin::<Monospace>::default(), InputPlugin, behavior::<Recorder, _>(UiFrame)))
            .register::<Log>()
            .build()
    };
}

#[test]
fn hover_press_and_click_update_interaction_and_deliver_click() {
    let mut app = ui_app!();
    let column = app.world.spawn_ui(el((Node::column().gap(10),)));
    let a = app.world.spawn_ui_under(column, el((BUTTON, Interaction::default(), Recorder, Log::default())));
    let b = app.world.spawn_ui_under(column, el((BUTTON, Interaction::default(), Recorder, Log::default())));
    app.emit(&UiFrame);
    app.emit(&Input::PointerMoved { x: 50.0, y: 20.0 });
    assert!(app.world.get::<Interaction>(a).unwrap().hovered);
    app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
    assert!(app.world.get::<Interaction>(a).unwrap().pressed);
    app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
    assert!(!app.world.get::<Interaction>(a).unwrap().pressed);
    app.emit(&UiFrame);
    assert_eq!(log(&app.world, a), ["click Left"]);
    // Moving onto b moves the hover.
    app.emit(&Input::PointerMoved { x: 50.0, y: 60.0 });
    assert!(!app.world.get::<Interaction>(a).unwrap().hovered);
    assert!(app.world.get::<Interaction>(b).unwrap().hovered);
    // Pressing on b and releasing on a is no click: it was a drag of b.
    app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
    app.emit(&Input::PointerMoved { x: 50.0, y: 20.0 });
    app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
    app.emit(&UiFrame);
    assert_eq!(log(&app.world, a), ["click Left"]);
    assert_eq!(log(&app.world, b), ["drag start 50,60", "drag 0,-40", "drag end"]);
    app.emit(&Input::PointerLeft);
    assert!(!app.world.get::<Interaction>(b).unwrap().hovered);
}

#[test]
fn events_bubble_to_the_nearest_listener_and_inert_ignores_input() {
    let mut app = ui_app!();
    let card = app.world.spawn_ui(el((Node::column().padding(Padding::all(10)), Recorder, Log::default())).children((
        el((BUTTON,)),
        el((BUTTON, Inert)),
    )));
    let label = app.world.sources::<ChildOf>(card).next().unwrap();
    app.emit(&UiFrame);
    app.emit(&UiFrame);
    for (x, y) in [(50.0, 30.0), (50.0, 70.0)] {
        app.emit(&Input::PointerMoved { x, y });
        app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
        app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
        app.emit(&UiFrame);
    }
    // The child has no listener: its click reached the card. The inert
    // child is not hit, so the card itself was clicked.
    assert_eq!(log(&app.world, card), ["click Left", "click Left"]);
    assert_eq!(app.world.resource::<Pointer>().unwrap().hovered, Some(card));
    let _ = label;
}

#[test]
fn dragging_delivers_start_deltas_and_end_to_the_taker() {
    let mut app = ui_app!();
    let thumb = app.world.spawn_ui(el((BUTTON, Recorder, Log::default())));
    app.emit(&UiFrame);
    app.emit(&UiFrame);
    app.emit(&Input::PointerMoved { x: 10.0, y: 10.0 });
    app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
    app.emit(&Input::PointerMoved { x: 11.0, y: 10.0 }); // under the threshold
    app.emit(&UiFrame);
    app.emit(&Input::PointerMoved { x: 20.0, y: 10.0 });
    app.emit(&UiFrame);
    app.emit(&Input::PointerMoved { x: 25.0, y: 12.0 });
    app.emit(&UiFrame);
    app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
    app.emit(&UiFrame);
    assert_eq!(log(&app.world, thumb), ["drag start 10,10", "drag 9,0", "drag 5,2", "drag end"]);
}

#[test]
fn wheel_and_drag_scroll_containers_within_their_content() {
    let mut app = ui_app!();
    let list = app.world.spawn_ui(el((
        Node::column().width(Sizing::fixed(100.0)).height(Sizing::fixed(100.0)),
        Clip { horizontal: false, vertical: true },
        Scroll::default(),
    )));
    let rows: Vec<Entity> = (0..10).map(|_| app.world.spawn_ui_under(list, el((BUTTON,)))).collect();
    app.emit(&UiFrame);
    app.emit(&Input::PointerMoved { x: 50.0, y: 50.0 });
    app.emit(&Input::Scroll { dx: 0.0, dy: 30.0 });
    app.emit(&UiFrame);
    assert_eq!(app.world.get::<Scroll>(list).unwrap().offset.y, 30.0);
    assert_eq!(app.world.get::<static_ui::Rect>(rows[1]).unwrap().0.y, 10.0, "rows moved up");
    app.emit(&Input::Scroll { dx: 0.0, dy: 1000.0 });
    assert_eq!(app.world.get::<Scroll>(list).unwrap().offset.y, 300.0, "clamped: 400 of content in 100");
    // Drag to scroll: no behavior took the drag.
    app.emit(&UiFrame);
    app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
    app.emit(&Input::PointerMoved { x: 50.0, y: 90.0 });
    app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
    assert_eq!(app.world.get::<Scroll>(list).unwrap().offset.y, 260.0);
}

#[test]
fn focus_follows_clicks_and_tab_and_receives_text() {
    let mut app = ui_app!();
    let form = app.world.spawn_ui(el((Node::column().gap(10),)));
    let first = app.world.spawn_ui_under(form, el((BUTTON, Focusable, Interaction::default(), Recorder, Log::default())));
    let second = app.world.spawn_ui_under(form, el((BUTTON, Focusable, Interaction::default(), Recorder, Log::default())));
    app.emit(&UiFrame);
    app.emit(&UiFrame);
    app.emit(&Input::PointerMoved { x: 10.0, y: 60.0 });
    app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
    app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
    assert_eq!(app.world.resource::<Focus>().unwrap().0, Some(second));
    assert!(app.world.get::<Interaction>(second).unwrap().focused);
    app.emit(&Input::Text('h'));
    app.emit(&Input::Text('i'));
    app.emit(&UiFrame);
    app.emit(&Input::Key { key: Key::Tab, pressed: true, modifiers: Modifiers::default() });
    assert_eq!(app.world.resource::<Focus>().unwrap().0, Some(first), "Tab wraps around");
    app.emit(&Input::Text('!'));
    app.emit(&UiFrame);
    // Within one wakeup the recorder takes clicks and text before focus (its
    // select order); each kind arrives in order.
    assert_eq!(log(&app.world, second), ["click Left", "text h", "text i", "focus in", "focus out"]);
    assert_eq!(log(&app.world, first), ["text !", "focus in"]);
    // Clicking outside every element clears focus.
    app.emit(&Input::PointerMoved { x: 500.0, y: 500.0 });
    app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
    assert_eq!(app.world.resource::<Focus>().unwrap().0, None);
}

#[test]
fn floating_elements_are_hit_first_unless_passthrough() {
    let mut app = ui_app!();
    let base = app.world.spawn_ui(el((Node::row().width(Sizing::fixed(200.0)).height(Sizing::fixed(200.0)),)));
    let popup = app.world.spawn_ui_under(base, el((BUTTON, Floating { offset: Vec2::new(0.0, 0.0), ..Floating::default() })));
    app.emit(&UiFrame);
    let hit = |app: &App<_, _>| hit_test(&app.world, Vec2::new(10.0, 10.0));
    assert_eq!(hit(&app), Some(popup));
    app.world.insert(popup, Floating { pointer: PointerCapture::Passthrough, ..Floating::default() });
    app.emit(&UiFrame);
    assert_eq!(hit(&app), Some(base));
    // Closed panels are not hit.
    app.world.disable_subtree::<Closed, ChildOf>(base);
    app.emit(&UiFrame);
    assert_eq!(hit(&app), None);
}
