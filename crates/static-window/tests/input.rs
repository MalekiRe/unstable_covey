//! Input against the scripted compositor: keys and text through the
//! compositor's keymap, modifiers, focus, client-side repeat, the pointer,
//! and deliveries to behaviors.
mod support;

use std::time::Duration;

use static_ecs::prelude::*;
use static_ecs::{Behavior, Cx, behavior};
use static_window::*;
use support::compositor::{Compositor, Globals};

#[derive(Default)]
struct Seen(Vec<String>);

macro_rules! test_app {
    () => {
        App::new()
            .add_plugins(WindowPlugin)
            .init_resource::<Seen>()
            .add_systems(label::<KeyInput>(), |e: &KeyInput, mut seen: ResMut<Seen>| {
                let state = if e.pressed { if e.repeat { "repeat" } else { "down" } } else { "up" };
                let shift = if e.modifiers.shift { " +shift" } else { "" };
                seen.0.push(format!("key {} {:?} {state}{shift}", e.code.0, e.key));
            })
            .add_systems(label::<TextInput>(), |e: &TextInput, mut seen: ResMut<Seen>| seen.0.push(format!("text {}", e.text())))
            .add_systems(label::<FocusChanged>(), |e: &FocusChanged, mut seen: ResMut<Seen>| seen.0.push(format!("focus {}", e.focused)))
            .add_systems(label::<PointerMoved>(), |e: &PointerMoved, mut seen: ResMut<Seen>| seen.0.push(format!("moved {},{}", e.x, e.y)))
            .add_systems(label::<PointerLeft>(), |_: &PointerLeft, mut seen: ResMut<Seen>| seen.0.push("left".into()))
            .add_systems(label::<PointerButton>(), |e: &PointerButton, mut seen: ResMut<Seen>| {
                seen.0.push(format!("button {:?} {}", e.button, if e.pressed { "down" } else { "up" }))
            })
            .add_systems(label::<Scroll>(), |e: &Scroll, mut seen: ResMut<Seen>| seen.0.push(format!("scroll {},{} {:?}", e.dx, e.dy, e.lines)))
            .build()
    };
}

const NOW: Option<Duration> = Some(Duration::ZERO);
const A: u32 = 30;
const LEFT_SHIFT: u32 = 42;
const BTN_LEFT: u32 = 0x110;
/// The us keymap's shift mask.
const SHIFT: u32 = 1;

fn seen<S: Storage + Stores<Seen>>(world: &mut World<S>) -> Vec<String> {
    std::mem::take(&mut world.resource_mut::<Seen>().unwrap().0)
}

/// A configured window, its loop, and the app.
macro_rules! opened {
    ($app:expr) => {{
        let (compositor, socket) = Compositor::spawn(Globals::default());
        let mut lp = WindowLoop::from_socket(socket).unwrap();
        let mut app = $app;
        let window = app.world.spawn_bundle((Window::new("input"),));
        lp.step(&mut app, NOW);
        compositor.with(|s, _| s.configure(0, 0, 0, &[]));
        lp.step(&mut app, NOW);
        (compositor, lp, app, window)
    }};
}

#[test]
fn keys_are_read_through_the_compositors_keymap() {
    let (compositor, mut lp, mut app, window) = opened!(test_app!());
    compositor.with(|s, _| {
        s.keyboard_enter(0, &[]);
        s.key(A, true);
        s.key(A, false);
        s.key(LEFT_SHIFT, true);
        s.modifiers(SHIFT, 0, 0);
        s.key(A, true);
    });
    lp.step(&mut app, NOW);
    assert_eq!(
        seen(&mut app.world),
        [
            "focus true",
            "key 30 Char('a') down",
            "text a",
            "key 30 Char('a') up",
            "key 42 Shift down +shift",
            "key 30 Char('A') down +shift",
            "text A",
        ]
    );
    assert!(app.world.get::<KeyboardFocus>(window).is_some());
    let keys = app.world.resource::<Keys>().unwrap();
    assert!(keys.pressed(KeyCode::A) && keys.pressed(KeyCode::LEFT_SHIFT) && keys.modifiers().shift);
}

#[test]
fn losing_focus_releases_held_keys_and_entering_keeps_held_modifiers() {
    let (compositor, mut lp, mut app, window) = opened!(test_app!());
    compositor.with(|s, _| {
        // Shift was already down when the window got focus.
        s.keyboard_enter(0, &[LEFT_SHIFT, A]);
        s.key(A, true);
        s.keyboard_leave(0);
    });
    lp.step(&mut app, NOW);
    assert_eq!(
        seen(&mut app.world),
        [
            "key 42 Shift down +shift",
            "focus true",
            "key 30 Char('a') down +shift",
            "text a",
            // Released in code order, each with the modifiers left after it.
            "key 30 Char('a') up +shift",
            "key 42 Shift up",
            "focus false",
        ]
    );
    assert!(app.world.get::<KeyboardFocus>(window).is_none());
    assert!(!app.world.resource::<Keys>().unwrap().pressed(KeyCode::LEFT_SHIFT));
}

#[test]
fn held_keys_repeat_on_the_clients_clock_until_released() {
    let (compositor, mut lp, mut app, _) = opened!(test_app!());
    // Repeat at once, then once a second: one repeat is due now, the next
    // long after this test.
    compositor.with(|s, _| {
        s.keyboard_enter(0, &[]);
        s.repeat_info(1, 0);
        s.key(A, true);
    });
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), ["focus true", "key 30 Char('a') down", "text a", "key 30 Char('a') repeat", "text a"]);
    lp.step(&mut app, NOW);
    assert!(seen(&mut app.world).is_empty());
    compositor.with(|s, _| {
        s.key(A, false);
        s.repeat_info(1000, 0);
        s.key(LEFT_SHIFT, true);
    });
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), ["key 30 Char('a') up", "key 42 Shift down +shift"], "modifiers never repeat");
}

#[test]
fn the_pointer_moves_clicks_scrolls_and_leaves() {
    let (compositor, mut lp, mut app, window) = opened!(test_app!());
    compositor.with(|s, _| {
        s.pointer_enter(0, 10.5, 20.0);
        s.motion(30.0, 40.0);
        s.button(BTN_LEFT, true);
        s.wheel(15.0, 120);
    });
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), ["moved 10.5,20", "moved 30,40", "button Left down", "scroll 0,15 Some((0.0, 1.0))"]);
    assert!(app.world.get::<PointerOver>(window).is_some());
    let pointer = app.world.resource_mut::<Pointer>().map(Mut::into_inner).unwrap();
    assert_eq!((pointer.window, pointer.x, pointer.y), (Some(window), 30.0, 40.0));
    assert!(pointer.pressed(MouseButton::Left));
    assert_eq!(pointer.take_delta(), (19.5, 20.0));
    compositor.with(|s, _| {
        s.button(BTN_LEFT, false);
        s.pointer_leave(0);
    });
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), ["button Left up", "left"]);
    assert!(app.world.get::<PointerOver>(window).is_none());
    assert_eq!(app.world.resource::<Pointer>().unwrap().window, None);
}

#[test]
fn input_for_a_window_despawned_mid_batch_is_dropped() {
    let (compositor, mut lp, mut app, _) = opened!(
        App::new()
            .add_plugins(WindowPlugin)
            .init_resource::<Seen>()
            .add_systems(label::<KeyInput>(), |e: &KeyInput, mut seen: ResMut<Seen>, mut commands: Commands<_>| {
                seen.0.push(format!("key {}", e.code.0));
                commands.despawn(e.window);
            })
            .build()
    );
    compositor.with(|s, _| {
        s.keyboard_enter(0, &[]);
        s.key(A, true);
        s.key(A, false);
    });
    assert_eq!(lp.step(&mut app, NOW), Flow::Done);
    assert_eq!(seen(&mut app.world), ["key 30"]);
}

/// Types into its window's title: a behavior awaiting `TextInput`.
struct TitleEditor;
impl Behavior for TitleEditor {
    async fn start<S: Storage>(me: Entity, cx: Cx<S>) {
        loop {
            let typed = cx.on::<TextInput>(me).await;
            let _ = cx.with(|mut windows: Query<&mut Window>| {
                if let Some(mut window) = windows.get_mut(me) {
                    window.title.push_str(typed.text());
                }
            });
        }
    }
}

#[test]
fn behaviors_receive_input_delivered_to_their_window() {
    let (compositor, mut lp, mut app, window) = opened!(
        App::new().add_plugins(WindowPlugin).register::<TitleEditor>().add_plugins(behavior::<TitleEditor, _>(Step)).build()
    );
    app.world.insert(window, TitleEditor);
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        s.keyboard_enter(0, &[]);
        for _ in 0..3 {
            s.key(A, true);
            s.key(A, false);
        }
    });
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    assert_eq!(app.world.get::<Window>(window).unwrap().title, "inputaaa");
    assert_eq!(compositor.with(|s, _| s.toplevel(0).title.clone()), "inputaaa");
}
