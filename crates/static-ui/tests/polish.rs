//! Transitions (with enter/exit on open/close) and the debug overlay.
use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_ui::transition::{BackgroundColor, Transition, Width, close_animated, open_animated, transitions};
use static_ui::*;

fn frame<S: Storage, L>(app: &mut App<S, L>)
where
    L: static_ecs::Dispatch<UiUpdate, World<S>> + static_ecs::Dispatch<UiFrame, World<S>>,
{
    app.emit(&UiUpdate);
    app.emit(&UiFrame);
}
macro_rules! app {
    () => {
        App::new().add_plugins((UiPlugin::<Monospace>::default(), transitions())).build()
    };
}
fn advance<S: Storage, L>(app: &mut App<S, L>, ms: u64) {
    app.world.resource_mut::<UiClock>().unwrap().advance(Duration::from_millis(ms));
}

#[test]
fn color_transitions_ease_to_the_target_and_stop_requesting_frames() {
    let mut app = app!();
    app.world.insert_resource(UiClock::Manual(Instant::now()));
    let panel = app.world.spawn_ui(el((Node::row().width(Sizing::fixed(10.0)).height(Sizing::fixed(10.0)), Background(Color::BLACK))));
    frame(&mut app);
    app.world.insert(panel, Transition::<BackgroundColor>::new(Color::BLACK, Color::rgb(200, 100, 0), Duration::from_millis(100), behave::linear));
    frame(&mut app); // starts at t = 0
    assert!(app.world.resource::<Redraw>().unwrap().animating);
    advance(&mut app, 50);
    frame(&mut app);
    assert_eq!(app.world.get::<Background>(panel).unwrap().0, Color::rgb(100, 50, 0));
    advance(&mut app, 60);
    frame(&mut app);
    assert_eq!(app.world.get::<Background>(panel).unwrap().0, Color::rgb(200, 100, 0), "the end value, exactly");
    frame(&mut app);
    assert!(app.world.get::<Transition<BackgroundColor>>(panel).is_none());
    assert!(!app.world.resource::<Redraw>().unwrap().animating);
}

#[test]
fn exit_transitions_play_before_closing_and_enter_ones_after_opening() {
    let mut app = App::new()
        .add_plugins((UiPlugin::<Monospace>::default(), transitions()))
        .add_systems(UiUpdate, |mut commands: Commands<_>, mut panels: Query<Entity, (With<Background>, MayHave<Closed>)>, mut toggle: ResMut<Toggle>| {
            let Some(request) = toggle.0.take() else { return };
            panels.each(|panel| {
                let width = |from: f32, to: f32| Transition::<Width>::new(from, to, Duration::from_millis(100), behave::ease_out);
                if request {
                    open_animated(&mut commands, panel, width(0.0, 120.0));
                } else {
                    close_animated(&mut commands, panel, width(120.0, 0.0));
                }
            });
        })
        .init_resource::<Toggle>()
        .build();
    app.world.insert_resource(UiClock::Manual(Instant::now()));
    let panel = app.world.spawn_ui(el((Node::row().width(Sizing::fixed(120.0)).height(Sizing::fixed(40.0)), Background(Color::BLACK))));
    frame(&mut app);
    let in_tree = |app: &App<_, _>| app.world.resource::<UiLayout>().unwrap().element(panel).is_some();
    let width = |app: &App<_, _>| app.world.get::<static_ui::Rect>(panel).unwrap().0.width;
    app.world.resource_mut::<Toggle>().unwrap().0 = Some(false);
    frame(&mut app);
    frame(&mut app);
    advance(&mut app, 50);
    frame(&mut app);
    assert!(in_tree(&app) && width(&app) < 120.0 && width(&app) > 0.0, "shrinking, still shown: {}", width(&app));
    advance(&mut app, 60);
    frame(&mut app);
    frame(&mut app);
    assert!(!in_tree(&app), "closed once the exit finished");
    app.world.resource_mut::<Toggle>().unwrap().0 = Some(true);
    frame(&mut app);
    frame(&mut app);
    assert!(in_tree(&app), "opened before the enter transition");
    advance(&mut app, 200);
    frame(&mut app);
    frame(&mut app);
    assert_eq!(width(&app), 120.0);
}
#[derive(Default)]
struct Toggle(Option<bool>);

#[test]
fn the_debug_overlay_outlines_boxes_flashes_damage_and_shows_timings() {
    let mut app = App::new().add_plugins(UiPlugin::<Monospace>::default()).build();
    let panel = app.world.spawn_ui(el((Node::column().padding(Padding::all(4)), Background(Color::BLACK))).children((el((Text::new("hi", 10),)),)));
    frame(&mut app);
    app.world.insert_resource(DebugOverlay { boxes: true, damage: true, timings: true });
    frame(&mut app);
    frame(&mut app);
    let list = app.world.resource::<RenderList>().unwrap();
    let outlines = list.commands().iter().filter(|c| matches!(c.draw, Draw::Border { .. }) && c.entity.is_some()).count();
    assert!(outlines >= 2, "every element outlined");
    let timings = app.world.query_filtered::<&Text, With<DebugTimings>>().iter_mut().next().map(|t| t.text.clone());
    assert!(timings.as_deref().is_some_and(|t| t.starts_with("layout")), "{timings:?}");
    // Restyle: this frame's damage is tinted; the next frame erases the tint.
    app.world.insert(panel, Background(Color::WHITE));
    frame(&mut app);
    let flashes: Vec<_> = app.world.resource::<RenderList>().unwrap().commands().iter().filter(|c| c.entity.is_none()).map(|c| c.rect).collect();
    let panel_box = app.world.get::<static_ui::Rect>(panel).unwrap().0;
    assert!(flashes.iter().any(|&f| f.intersect(panel_box) == panel_box), "{flashes:?}");
    app.world.resource_mut::<RenderList>().unwrap().take_damage();
    frame(&mut app);
    let damage = app.world.resource_mut::<RenderList>().unwrap().take_damage();
    assert!(damage.iter().any(|&d| d.intersect(panel_box) == panel_box), "the tint is erased: {damage:?}");
    // Off again: the timings corner goes away.
    app.world.insert_resource(DebugOverlay::default());
    frame(&mut app);
    frame(&mut app);
    assert_eq!(app.world.query_filtered::<&Text, With<DebugTimings>>().iter_mut().count(), 0);
}
