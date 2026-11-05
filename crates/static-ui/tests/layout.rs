use std::sync::Mutex;

use static_ecs::prelude::*;
use static_ui::layout::{Config, TextCache, Tree};
use static_ui::*;

const PANEL: Node = Node::column().padding(Padding::all(16)).gap(8).width(Sizing::fixed(300.0)).height(Sizing::GROW);
const ROW: Node = Node::row().gap(4).width(Sizing::GROW);

fn rect<S: Storage>(world: &World<S>, entity: Entity) -> layout::Rect {
    world.get::<Rect>(entity).unwrap().0
}
fn runs<S: Storage>(world: &World<S>) -> (u64, u64) {
    let ui = world.resource::<UiLayout>().unwrap();
    (ui.runs(), ui.rebuilds())
}

/// A sidebar like Clay's README example, and a content area.
fn sidebar<S: Storage>(world: &mut World<S>) -> (Entity, Entity, Entity) {
    let screen = world.spawn_ui(el((Node::row().width(Sizing::GROW).height(Sizing::GROW).gap(16),)));
    let panel = world.spawn_ui_under(
        screen,
        el((PANEL, Background(Color::rgb(224, 215, 210)))).children((
            el((ROW,)).children((el((Text::new("Clay - UI Library", 24),)),)),
            el((ROW,)).children((el((Text::new("Item one", 16),)), el((Text::new("Item two", 16),)))),
            el((Node::row().width(Sizing::GROW).height(Sizing::fixed(40.0)),)),
        )),
    );
    let content = world.spawn_ui_under(screen, el((Node::column().width(Sizing::GROW).height(Sizing::GROW),)));
    (screen, panel, content)
}

#[test]
fn an_unchanged_ui_runs_no_layout() {
    let mut app = App::new().add_plugins(UiPlugin::<Monospace>::default()).build();
    let (_, panel, content) = sidebar(&mut app.world);
    app.emit(&UiFrame);
    assert_eq!(runs(&app.world), (1, 1));
    for _ in 0..10 {
        app.emit(&UiFrame);
    }
    assert_eq!(runs(&app.world), (1, 1), "nothing changed: no layout");
    assert_eq!(rect(&app.world, panel), layout::Rect::new(0.0, 0.0, 300.0, 600.0));
    assert_eq!(rect(&app.world, content), layout::Rect::new(316.0, 0.0, 484.0, 600.0));
}

/// Entities whose `Rect` changed since this probe last ran.
static MOVED: Mutex<Vec<Entity>> = Mutex::new(Vec::new());
struct Probe;
impl Schedule for Probe {}

#[test]
fn changing_a_node_patches_it_and_writes_only_moved_rects() {
    let mut app = App::new()
        .add_plugins(UiPlugin::<Monospace>::default())
        .add_systems(Probe, |mut moved: Query<Entity, Changed<Rect>>| {
            *MOVED.lock().unwrap() = moved.iter_mut().collect();
        })
        .build();
    let (screen, panel, content) = sidebar(&mut app.world);
    app.emit(&UiFrame);
    app.emit(&Probe);
    app.world.insert(panel, PANEL.width(Sizing::fixed(200.0)));
    app.emit(&UiFrame);
    assert_eq!(runs(&app.world), (2, 1), "patched in place, not flattened again");
    app.emit(&Probe);
    let moved = MOVED.lock().unwrap().clone();
    assert!(moved.contains(&panel) && moved.contains(&content));
    assert!(!moved.contains(&screen), "the screen kept its box");
    assert_eq!(rect(&app.world, content), layout::Rect::new(216.0, 0.0, 584.0, 600.0));
    // Damage covers the panel's old and new boxes.
    let damage: Vec<_> = app.world.resource_mut::<UiLayout>().unwrap().take_damage().collect();
    assert!(damage.contains(&layout::Rect::new(0.0, 0.0, 300.0, 600.0)));
}

#[test]
fn changing_text_rewraps_without_flattening() {
    let mut app = App::new().add_plugins(UiPlugin::<Monospace>::default()).build();
    let label = app.world.spawn_ui(el((Text::new("short", 10),)));
    app.emit(&UiFrame);
    assert_eq!(rect(&app.world, label).width, 25.0);
    app.world.get_mut::<Text>(label).unwrap().text = "a much longer label".into();
    app.emit(&UiFrame);
    assert_eq!(runs(&app.world), (2, 1));
    assert_eq!(rect(&app.world, label).width, 95.0);
    assert_eq!(app.world.resource::<UiLayout>().unwrap().lines(label).len(), 1);
}

#[test]
fn closing_a_panel_removes_it_from_the_tree_and_reopening_restores_it() {
    let mut app = App::new().add_plugins(UiPlugin::<Monospace>::default()).build();
    let (_, panel, content) = sidebar(&mut app.world);
    app.emit(&UiFrame);
    let before = app.world.resource::<UiLayout>().unwrap().tree().len();
    let first_row = app.world.sources::<ChildOf>(panel).next().unwrap();
    app.world.disable_subtree::<Closed, ChildOf>(panel);
    app.emit(&UiFrame);
    let ui = app.world.resource::<UiLayout>().unwrap();
    assert_eq!(ui.tree().len(), before - 7, "the panel and its 6 descendants left");
    assert!(ui.element(panel).is_none() && ui.element(first_row).is_none());
    assert_eq!(rect(&app.world, content), layout::Rect::new(0.0, 0.0, 800.0, 600.0), "content took the space");
    // The closed panel kept everything, including its last box.
    assert_eq!(rect(&app.world, panel), layout::Rect::new(0.0, 0.0, 300.0, 600.0));
    app.world.enable_subtree::<Closed, ChildOf>(panel);
    app.emit(&UiFrame);
    assert_eq!(app.world.resource::<UiLayout>().unwrap().tree().len(), before);
    assert_eq!(rect(&app.world, content), layout::Rect::new(316.0, 0.0, 484.0, 600.0));
}

#[test]
fn ecs_layout_matches_the_layout_crate_exactly() {
    let mut app = App::new().add_plugins(UiPlugin::<Monospace>::default()).build();
    let (screen, panel, content) = sidebar(&mut app.world);
    let floating = app.world.spawn_ui_under(
        content,
        el((
            Node::column().padding(Padding::all(4)),
            Floating { attach: Attach::To(panel), parent: AttachPoint::RightTop, offset: Vec2::new(8.0, 8.0), z: 2, ..Floating::default() },
        ))
        .children((el((Text::new("tooltip text that wraps", 12),)),)),
    );
    app.world.insert_resource(Viewport { width: 640.0, height: 480.0 });
    app.emit(&UiFrame);
    app.emit(&UiFrame);

    // The same tree, declared directly (in the order the ECS flattens it).
    let mut tree = Tree::new();
    let mut cache = TextCache::default();
    let mut measure = Monospace;
    tree.begin(640.0, 480.0);
    let e_screen = tree.open(Node::row().width(Sizing::GROW).height(Sizing::GROW).gap(16).0);
    let e_panel = tree.open(PANEL.0);
    tree.open(ROW.0);
    tree.text("Clay - UI Library", TextConfig { size: 24, ..TextConfig::default() });
    tree.close();
    tree.open(ROW.0);
    tree.text("Item one", TextConfig { size: 16, ..TextConfig::default() });
    tree.text("Item two", TextConfig { size: 16, ..TextConfig::default() });
    tree.close();
    tree.open(Node::row().width(Sizing::GROW).height(Sizing::fixed(40.0)).0);
    tree.close();
    tree.close();
    let e_content = tree.open(Node::column().width(Sizing::GROW).height(Sizing::GROW).0);
    let mut float_config = Config::from(Node::column().padding(Padding::all(4)).0);
    float_config.floating = layout::Floating {
        attach_to: layout::AttachTo::Element(e_panel),
        parent: AttachPoint::RightTop,
        offset: Vec2::new(8.0, 8.0),
        z: 2,
        ..layout::Floating::default()
    };
    let e_floating = tree.open(float_config);
    tree.text("tooltip text that wraps", TextConfig { size: 12, ..TextConfig::default() });
    tree.close();
    tree.close();
    tree.close();
    tree.end();
    for _ in 0..2 {
        tree.layout(&mut cache, |t, c| measure.measure(t, c));
    }
    for (entity, element) in [(screen, e_screen), (panel, e_panel), (content, e_content), (floating, e_floating)] {
        assert_eq!(rect(&app.world, entity), tree.rect(element));
    }
    let ui = app.world.resource::<UiLayout>().unwrap();
    for (i, &entity) in ui.entities().iter().enumerate() {
        assert_eq!(rect(&app.world, entity), tree.rect(i as u32 + 1), "element {}", i + 1);
    }
}

#[test]
fn despawning_a_subtree_and_resizing_relayout() {
    let mut app = App::new().add_plugins(UiPlugin::<Monospace>::default()).build();
    let (screen, panel, content) = sidebar(&mut app.world);
    app.emit(&UiFrame);
    app.world.despawn(panel);
    app.emit(&UiFrame);
    assert_eq!(app.world.sources::<ChildOf>(screen).count(), 1);
    assert_eq!(rect(&app.world, content).width, 800.0);
    app.world.insert_resource(Viewport { width: 1024.0, height: 768.0 });
    app.emit(&UiFrame);
    assert_eq!(rect(&app.world, content), layout::Rect::new(0.0, 0.0, 1024.0, 768.0));
}
