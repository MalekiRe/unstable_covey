//! Cursors, pointer locks and the clipboard against the scripted compositor.
mod support;

use std::time::Duration;

use static_ecs::prelude::*;
use static_window::*;
use support::compositor::{Compositor, Globals};
use wayland_protocols::wp::cursor_shape::v1::server::wp_cursor_shape_device_v1::Shape;

const NOW: Option<Duration> = Some(Duration::ZERO);

#[derive(Default)]
struct Seen(Vec<String>);

macro_rules! test_app {
    () => {
        App::new()
            .add_plugins(WindowPlugin)
            .init_resource::<Seen>()
            .add_systems(label::<PointerDelta>(), |e: &PointerDelta, mut seen: ResMut<Seen>| seen.0.push(format!("delta {},{}", e.dx, e.dy)))
            .add_systems(label::<ClipboardText>(), |e: &ClipboardText, mut seen: ResMut<Seen>| seen.0.push(format!("clipboard {:?}", e.text)))
            .build()
    };
}

fn seen<S: Storage + Stores<Seen>>(world: &mut World<S>) -> Vec<String> {
    std::mem::take(&mut world.resource_mut::<Seen>().unwrap().0)
}

macro_rules! opened {
    ($globals:expr) => {{
        let (compositor, socket) = Compositor::spawn($globals);
        let mut lp = WindowLoop::from_socket(socket).unwrap();
        let mut app = test_app!();
        let window = app.world.spawn_bundle((Window::new("devices"),));
        lp.step(&mut app, NOW);
        compositor.with(|s, _| s.configure(0, 0, 0, &[]));
        lp.step(&mut app, NOW);
        (compositor, lp, app, window)
    }};
}

#[test]
fn the_window_under_the_pointer_names_its_cursor() {
    let (compositor, mut lp, mut app, window) = opened!(Globals::default());
    app.world.insert(window, CursorIcon::Text);
    lp.step(&mut app, NOW);
    assert!(compositor.with(|s, _| s.shapes.is_empty()), "not before the pointer arrives");
    compositor.with(|s, _| s.pointer_enter(0, 5.0, 5.0));
    lp.step(&mut app, NOW);
    app.world.insert(window, CursorIcon::Pointer);
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    // Each enter hands the cursor back: it is set again.
    compositor.with(|s, _| {
        s.pointer_leave(0);
        s.pointer_enter(0, 1.0, 1.0);
    });
    lp.step(&mut app, NOW);
    app.world.remove::<CursorIcon>(window);
    lp.step(&mut app, NOW);
    assert_eq!(compositor.with(|s, _| s.shapes.clone()), [Shape::Text, Shape::Pointer, Shape::Pointer, Shape::Default]);
    app.world.insert(window, CursorIcon::Hidden);
    lp.step(&mut app, NOW);
    assert_eq!(compositor.with(|s, _| s.set_cursor.clone()), [false], "hidden: no cursor surface");
}

#[test]
fn a_locked_pointer_reports_raw_motion_and_hides_the_cursor() {
    let (compositor, mut lp, mut app, window) = opened!(Globals::default());
    compositor.with(|s, _| s.pointer_enter(0, 5.0, 5.0));
    lp.step(&mut app, NOW);
    app.world.insert(window, PointerLock);
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        assert_eq!(s.locks, [(s.toplevel(0).surface, false)]);
        assert_eq!(s.set_cursor, [false]);
        s.relative_motion(5.0, -3.0);
        s.relative_motion(1.0, 1.0);
    });
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), ["delta 5,-3", "delta 1,1"]);
    assert_eq!(app.world.resource_mut::<Pointer>().unwrap().take_delta(), (6.0, -2.0));
    app.world.remove::<PointerLock>(window);
    lp.step(&mut app, NOW);
    compositor.with(|s, _| {
        assert!(s.locks[0].1, "let go");
        assert!(s.relative.is_none());
        assert_eq!(s.shapes.last(), Some(&Shape::Default), "the cursor is back");
    });
}

#[test]
fn setting_the_clipboard_offers_text_to_other_clients() {
    let (compositor, mut lp, mut app, _) = opened!(Globals::default());
    compositor.with(|s, _| {
        s.keyboard_enter(0, &[]);
        s.key(30, true);
    });
    lp.step(&mut app, NOW);
    app.world.resource_mut::<Clipboard>().unwrap().set("hello, clipboard");
    lp.step(&mut app, NOW);
    let pipe = compositor.with(|s, _| {
        let (_, mimes, serial) = s.selection.as_ref().expect("a selection");
        assert!(mimes.iter().any(|m| m == "text/plain;charset=utf-8"));
        assert_eq!(*serial, 3, "quoting the key press");
        s.paste("text/plain;charset=utf-8")
    });
    lp.step(&mut app, NOW);
    let mut text = String::new();
    std::io::Read::read_to_string(&mut std::fs::File::from(pipe), &mut text).unwrap();
    assert_eq!(text, "hello, clipboard");
    // Reading our own selection needs no roundtrip.
    app.world.resource_mut::<Clipboard>().unwrap().request();
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), [r#"clipboard "hello, clipboard""#]);
}

#[test]
fn requesting_the_clipboard_reads_another_clients_text() {
    let (compositor, mut lp, mut app, _) = opened!(Globals::default());
    app.world.resource_mut::<Clipboard>().unwrap().request();
    lp.step(&mut app, NOW);
    lp.step(&mut app, NOW);
    assert_eq!(seen(&mut app.world), [r#"clipboard """#], "nothing on the clipboard");
    compositor.with(|s, handle| s.offer_text(handle, "from elsewhere"));
    lp.step(&mut app, NOW);
    app.world.resource_mut::<Clipboard>().unwrap().request();
    lp.step(&mut app, NOW);
    // The owner writes when our request reaches it; the step waits for it.
    for _ in 0..10 {
        lp.step(&mut app, Some(Duration::from_secs(5)));
        if !app.world.resource::<Seen>().unwrap().0.is_empty() {
            break;
        }
    }
    assert_eq!(seen(&mut app.world), [r#"clipboard "from elsewhere""#]);
}
