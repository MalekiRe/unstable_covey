//! Widgets, headless: synthetic pointer and keyboard input through the real
//! input handling, behaviors, and layout.
use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_ui::widgets::*;
use static_ui::*;

macro_rules! app {
    () => {
        App::new()
            .add_plugins((UiPlugin::<Monospace>::default(), InputPlugin, widgets::<Monospace>()))
            .build()
    };
}
fn frame<S: Storage, L>(app: &mut App<S, L>)
where
    L: static_ecs::Dispatch<UiUpdate, World<S>> + static_ecs::Dispatch<UiFrame, World<S>>,
{
    app.emit(&UiUpdate);
    app.emit(&UiFrame);
}
fn center<S: Storage>(world: &World<S>, entity: Entity) -> (f32, f32) {
    let r = world.get::<static_ui::Rect>(entity).unwrap().0;
    (r.x + r.width / 2.0, r.y + r.height / 2.0)
}
macro_rules! click_at {
    ($app:expr, $x:expr, $y:expr) => {{
        $app.emit(&Input::PointerMoved { x: $x, y: $y });
        $app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
        $app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
        frame(&mut $app);
    }};
}
macro_rules! click {
    ($app:expr, $entity:expr) => {{
        let (x, y) = center(&$app.world, $entity);
        click_at!($app, x, y);
    }};
}
fn key(key: Key) -> Input {
    Input::Key { key, pressed: true, modifiers: Modifiers::default() }
}
fn visible<S: Storage>(world: &World<S>, entity: Entity) -> bool {
    world.resource::<UiLayout>().unwrap().element(entity).is_some()
}

#[test]
fn buttons_restyle_on_hover_and_press_and_count_activations() {
    let mut app = app!();
    let b = app.world.spawn_ui(button("OK"));
    frame(&mut app);
    let (x, y) = center(&app.world, b);
    let color = |app: &App<_, _>| app.world.get::<Background>(b).unwrap().0;
    app.emit(&Input::PointerMoved { x, y });
    frame(&mut app);
    assert_eq!(color(&app), theme::BUTTON_HOVER);
    app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
    frame(&mut app);
    assert_eq!(color(&app), theme::BUTTON_PRESSED);
    app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
    frame(&mut app);
    assert_eq!(color(&app), theme::BUTTON_HOVER);
    assert_eq!(app.world.get::<Clicks>(b).unwrap().0, 1);
    // Focused (the click focused it), Enter activates too.
    app.emit(&key(Key::Enter));
    frame(&mut app);
    assert_eq!(app.world.get::<Clicks>(b).unwrap().0, 2);
    app.emit(&Input::PointerMoved { x: 500.0, y: 500.0 });
    frame(&mut app);
    assert_eq!(color(&app), theme::BUTTON);
}

#[test]
fn checkboxes_toggle_and_show_their_mark() {
    let mut app = app!();
    let checkbox = app.world.spawn_ui(checkbox(false));
    frame(&mut app);
    frame(&mut app);
    let mark = app.world.sources::<ChildOf>(checkbox).next().unwrap();
    assert!(!visible(&app.world, mark));
    click!(app, checkbox);
    frame(&mut app);
    assert_eq!(app.world.get::<Checked>(checkbox), Some(&Checked(true)));
    assert!(visible(&app.world, mark));
    click!(app, checkbox);
    frame(&mut app);
    assert_eq!(app.world.get::<Checked>(checkbox), Some(&Checked(false)));
    assert!(!visible(&app.world, mark));
}

#[test]
fn sliders_follow_clicks_and_drags_within_range() {
    let mut app = app!();
    let track = app.world.spawn_ui(slider(SliderRange { min: 0.0, max: 100.0 }, 0.0));
    frame(&mut app);
    let rect = app.world.get::<static_ui::Rect>(track).unwrap().0;
    let travel = rect.width - THUMB;
    let value = |app: &App<_, _>| app.world.get::<SliderValue>(track).unwrap().0;
    // Click at the middle of the travel.
    click_at!(app, rect.x + THUMB / 2.0 + travel / 2.0, rect.y + 10.0);
    assert_eq!(value(&app), 50.0);
    // Drag from there to a quarter, then past the end.
    app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
    app.emit(&Input::PointerMoved { x: rect.x + THUMB / 2.0 + travel / 4.0, y: rect.y + 10.0 });
    frame(&mut app);
    assert_eq!(value(&app), 25.0);
    app.emit(&Input::PointerMoved { x: rect.x + 1000.0, y: rect.y + 10.0 });
    app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
    frame(&mut app);
    assert_eq!(value(&app), 100.0);
    // The filled part spans the travel.
    let fill = app.world.sources::<ChildOf>(track).next().unwrap();
    assert_eq!(app.world.get::<static_ui::Rect>(fill).unwrap().0.width, travel);
}

#[test]
fn dropdowns_open_their_prebuilt_list_and_pick_without_spawning() {
    let mut app = app!();
    let header = spawn_dropdown(&mut app.world, None, &["Low", "Medium", "High"], 0);
    frame(&mut app);
    let list = app.world.sources::<ChildOf>(header).nth(1).unwrap();
    assert!(!visible(&app.world, list));
    let entities = app.world.query_filtered::<Entity, MayHave<Closed>>().iter_mut().count();
    click!(app, header);
    frame(&mut app);
    assert!(visible(&app.world, list), "one open");
    let high = app.world.sources::<ChildOf>(list).nth(2).unwrap();
    click!(app, high);
    frame(&mut app);
    assert!(!visible(&app.world, list));
    assert_eq!(app.world.get::<Selected>(header), Some(&Selected(2)));
    let label = app.world.sources::<ChildOf>(header).next().unwrap();
    assert_eq!(app.world.get::<Text>(label).unwrap().text, "High");
    assert_eq!(app.world.query_filtered::<Entity, MayHave<Closed>>().iter_mut().count(), entities, "nothing spawned");
    // The header closes an open list without choosing.
    click!(app, header);
    frame(&mut app);
    click!(app, header);
    frame(&mut app);
    assert!(!visible(&app.world, list));
    assert_eq!(app.world.get::<Selected>(header), Some(&Selected(2)));
}

#[test]
fn text_fields_edit_with_cursor_and_selection() {
    let mut app = app!();
    let field = app.world.spawn_ui(text_field("", 200.0));
    frame(&mut app);
    frame(&mut app);
    let caret = app.world.sources::<ChildOf>(field).nth(2).unwrap();
    assert!(!visible(&app.world, caret), "no caret until focused");
    click!(app, field);
    frame(&mut app);
    assert!(visible(&app.world, caret));
    for ch in "hello".chars() {
        app.emit(&Input::Text(ch));
    }
    frame(&mut app);
    assert_eq!(app.world.get::<TextValue>(field).unwrap().0, "hello");
    app.emit(&key(Key::Left));
    app.emit(&key(Key::Backspace));
    frame(&mut app);
    assert_eq!(app.world.get::<TextValue>(field).unwrap().0, "helo");
    assert_eq!(app.world.get::<Cursor>(field), Some(&Cursor { position: 3, anchor: 3 }));
    // Shift+Home selects to the start; typing replaces the selection.
    app.emit(&Input::Key { key: Key::Home, pressed: true, modifiers: Modifiers { shift: true, ..Modifiers::default() } });
    app.emit(&Input::Text('J'));
    frame(&mut app);
    frame(&mut app);
    assert_eq!(app.world.get::<TextValue>(field).unwrap().0, "Jo");
    let text = app.world.sources::<ChildOf>(field).nth(1).unwrap();
    assert_eq!(app.world.get::<Text>(text).unwrap().text, "Jo");
    // The caret sits after "J": padding plus one 8-pixel character.
    let field_rect = app.world.get::<static_ui::Rect>(field).unwrap().0;
    assert_eq!(app.world.get::<static_ui::Rect>(caret).unwrap().0.x, field_rect.x + 6.0 + 8.0);
    // Focus elsewhere hides the caret.
    click_at!(app, 900.0, 900.0);
    frame(&mut app);
    assert!(!visible(&app.world, caret));
}

#[test]
fn virtual_lists_recycle_a_pool_of_rows_while_scrolling() {
    let mut app = App::new()
        .add_plugins((UiPlugin::<Monospace>::default(), InputPlugin, widgets::<Monospace>()))
        .build();
    let list = spawn_virtual_list(&mut app.world, None, VirtualList { count: 1000, row_height: 20.0 }, 200.0, 100.0, || {
        el((Node::row().width(Sizing::GROW).height(Sizing::fixed(20.0)),))
    });
    frame(&mut app);
    let entities = app.world.query_filtered::<Entity, MayHave<Closed>>().iter_mut().count();
    let rows = || app.world.sources::<ChildOf>(list).skip(1).take(6).collect::<Vec<_>>();
    let rows: Vec<Entity> = rows();
    assert_eq!(rows.len(), 6, "a 100-pixel view of 20-pixel rows pools 6");
    app.emit(&Input::PointerMoved { x: 50.0, y: 50.0 });
    app.emit(&Input::Scroll { dx: 0.0, dy: 410.0 });
    frame(&mut app);
    frame(&mut app);
    let indices: Vec<usize> = rows.iter().map(|&r| app.world.get::<RowIndex>(r).unwrap().0).collect();
    assert_eq!(indices, [20, 21, 22, 23, 24, 25]);
    // Row 20 is drawn at its place in the 20,000-pixel content: 400 - 410.
    assert_eq!(app.world.get::<static_ui::Rect>(rows[0]).unwrap().0.y, -10.0);
    app.emit(&Input::Scroll { dx: 0.0, dy: 1e9 });
    frame(&mut app);
    frame(&mut app);
    assert_eq!(app.world.get::<RowIndex>(rows[5]).unwrap().0, 999);
    assert_eq!(app.world.query_filtered::<Entity, MayHave<Closed>>().iter_mut().count(), entities, "no spawning while scrolling");
    // Fewer items than rows: spares wait closed in the pool.
    app.world.insert(list, VirtualList { count: 3, row_height: 20.0 });
    app.world.insert(list, Scroll::default());
    frame(&mut app);
    frame(&mut app);
    assert_eq!(rows.iter().filter(|&&r| visible(&app.world, r)).count(), 3);
}

#[test]
fn tabs_keep_one_panel_open() {
    let mut app = app!();
    let mut pages = Vec::new();
    let tabs = spawn_tabs(&mut app.world, None, &["One", "Two", "Three"], |world, i, page| {
        world.spawn_ui_under(page, el((Text::new(format!("page {i}"), 16),)));
    });
    frame(&mut app);
    let panels = app.world.sources::<ChildOf>(tabs).nth(1).unwrap();
    pages.extend(app.world.sources::<ChildOf>(panels));
    let bar = app.world.sources::<ChildOf>(tabs).next().unwrap();
    let buttons: Vec<Entity> = app.world.sources::<ChildOf>(bar).collect();
    let open = |app: &App<_, _>| pages.iter().map(|&p| visible(&app.world, p)).collect::<Vec<_>>();
    assert_eq!(open(&app), [true, false, false]);
    click!(app, buttons[2]);
    frame(&mut app);
    assert_eq!(open(&app), [false, false, true]);
    assert_eq!(app.world.get::<ActiveTab>(tabs), Some(&ActiveTab(2)));
    click!(app, buttons[0]);
    frame(&mut app);
    assert_eq!(open(&app), [true, false, false]);
}

#[test]
fn tooltips_appear_after_a_rest_and_go_with_the_pointer() {
    let mut app = app!();
    let start = Instant::now();
    app.world.insert_resource(UiClock::Manual(start));
    let target = app.world.spawn_ui(el((Node::row().width(Sizing::fixed(80.0)).height(Sizing::fixed(30.0)), Tooltip("Saves the file".into()))));
    frame(&mut app);
    let tips = |app: &mut App<_, _>| app.world.query_filtered::<Entity, With<widgets::TooltipBox>>().iter_mut().count();
    app.emit(&Input::PointerMoved { x: 10.0, y: 10.0 });
    frame(&mut app);
    app.world.resource_mut::<UiClock>().unwrap().advance(Duration::from_millis(200));
    frame(&mut app);
    assert_eq!(tips(&mut app), 0, "not yet");
    app.world.resource_mut::<UiClock>().unwrap().advance(Duration::from_millis(250));
    frame(&mut app);
    frame(&mut app);
    assert_eq!(tips(&mut app), 1);
    let tip = app.world.query_filtered::<Entity, With<widgets::TooltipBox>>().iter_mut().next().unwrap();
    // Centered under the target, 6 pixels down.
    let (target_rect, tip_rect) = (app.world.get::<static_ui::Rect>(target).unwrap().0, app.world.get::<static_ui::Rect>(tip).unwrap().0);
    assert_eq!(tip_rect.y, target_rect.y + target_rect.height + 6.0);
    assert_eq!(tip_rect.x + tip_rect.width / 2.0, target_rect.x + target_rect.width / 2.0);
    app.emit(&Input::PointerMoved { x: 500.0, y: 500.0 });
    frame(&mut app);
    assert_eq!(tips(&mut app), 0);
}

#[test]
fn modals_close_on_backdrop_clicks_and_escape_but_not_inside() {
    let mut app = app!();
    let ok = std::cell::Cell::new(None);
    let modal = spawn_modal(&mut app.world, |world, dialog| {
        world.spawn_ui_under(dialog, el((Text::new("Are you sure?", 16),)));
        ok.set(Some(world.spawn_ui_under(dialog, button("Yes"))));
    });
    frame(&mut app);
    assert!(!visible(&app.world, modal));
    app.world.enable_subtree::<Closed, ChildOf>(modal);
    frame(&mut app);
    frame(&mut app);
    assert!(visible(&app.world, modal));
    // A click inside the dialog (on its button) keeps it open.
    click!(app, ok.get().unwrap());
    frame(&mut app);
    assert!(visible(&app.world, modal));
    assert_eq!(app.world.get::<Clicks>(ok.get().unwrap()).unwrap().0, 1);
    // The backdrop closes it.
    click_at!(app, 5.0, 5.0);
    frame(&mut app);
    assert!(!visible(&app.world, modal));
    // Escape, with the backdrop focused.
    app.world.enable_subtree::<Closed, ChildOf>(modal);
    frame(&mut app);
    click_at!(app, 5.0, 590.0);
    frame(&mut app);
    app.world.enable_subtree::<Closed, ChildOf>(modal);
    frame(&mut app);
    app.emit(&key(Key::Escape));
    frame(&mut app);
    assert!(!visible(&app.world, modal));
}

#[test]
fn the_inspector_lists_closed_panels_too() {
    let mut app = app!();
    let panel = app.world.spawn_ui(el((Node::column(),)).children((el((Text::new("inside", 12),)),)));
    let inspector = app.world.spawn_ui(el((Text::new("", 12), Inspector)));
    frame(&mut app);
    app.world.disable_subtree::<Closed, ChildOf>(panel);
    frame(&mut app);
    frame(&mut app);
    let listing = app.world.get::<Text>(inspector).unwrap().text.clone();
    assert!(listing.contains("[closed]"), "{listing}");
    assert!(listing.contains("\"inside\" [closed]"), "{listing}");
    let _ = inspector;
}
