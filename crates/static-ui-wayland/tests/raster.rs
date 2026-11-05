//! Headless: render lists, damage, and the CPU raster, without a display.
use static_ecs::prelude::*;
use static_ui::layout::Rect;
use static_ui::*;
use static_ui_wayland::glyph::{Point, Rasterizer};
use static_ui_wayland::{Atlas, Canvas, Fonts, paint};

const ITEM: Node = Node::row().width(Sizing::GROW).height(Sizing::fixed(50.0)).padding(Padding::xy(16, 0));

#[test]
fn restyling_one_button_rasters_only_its_box() {
    let mut app = App::new().add_plugins(UiPlugin::<Fonts>::default()).build();
    let list = app.world.spawn_ui(el((Node::column().width(Sizing::fixed(300.0)).padding(Padding::all(16)).gap(16), Background(Color::rgb(224, 215, 210)))));
    let buttons: Vec<Entity> = (0..5)
        .map(|i| app.world.spawn_ui_under(list, el((ITEM, Background(Color::rgb(225, 138, 50)))).children((el((Text::new(format!("Item {i}"), 20),)),))))
        .collect();
    let (width, height) = (640, 480);
    app.world.insert_resource(Viewport { width: width as f32, height: height as f32 });
    let mut pixels = vec![0u32; width * height];
    let mut atlas = Atlas::default();
    let mut frame = |app: &mut App<_, _>, pixels: &mut Vec<u32>| {
        app.emit(&UiFrame);
        let damage = app.world.resource_mut::<RenderList>().unwrap().take_damage();
        let mut canvas = Canvas { pixels, width, height, stride: width, touched: 0 };
        let world = &app.world;
        let painted = paint(
            &mut canvas,
            world.resource::<RenderList>().unwrap(),
            world.resource::<UiLayout>().unwrap(),
            world.resource::<Fonts>().unwrap(),
            &mut atlas,
            Color::WHITE,
            &damage,
        );
        (damage, painted)
    };
    let (first, _) = frame(&mut app, &mut pixels);
    assert!(!first.is_empty());
    let reference = pixels.clone();
    // Idle: no damage, nothing rastered.
    let (idle, painted) = frame(&mut app, &mut pixels);
    assert!(idle.is_empty() && painted.pixels == 0);
    // Hover-style restyle of button 2.
    let box2 = app.world.get::<static_ui::Rect>(buttons[2]).unwrap().0;
    app.world.insert(buttons[2], Background(Color::rgb(255, 170, 80)));
    let (damage, painted) = frame(&mut app, &mut pixels);
    assert_eq!(damage, [box2], "only the button's box is damaged");
    let area = (box2.width * box2.height) as u64;
    // Clear, the list and button backgrounds, and the label's glyphs.
    assert!(painted.pixels <= 4 * area, "{painted:?} for a {area}-pixel box");
    for y in 0..height {
        for x in 0..width {
            let inside = box2.contains(static_ui::Vec2::new(x as f32 + 0.5, y as f32 + 0.5));
            if !inside {
                assert_eq!(pixels[y * width + x], reference[y * width + x], "pixel ({x}, {y}) outside the box changed");
            }
        }
    }
    // Restoring the color restores the pixels exactly.
    app.world.insert(buttons[2], Background(Color::rgb(225, 138, 50)));
    frame(&mut app, &mut pixels);
    assert!(pixels == reference);
}

#[test]
fn closing_a_panel_damages_its_box() {
    let mut app = App::new().add_plugins(UiPlugin::<Fonts>::default()).build();
    let panel = app.world.spawn_ui(el((Node::column().width(Sizing::fixed(200.0)).height(Sizing::fixed(100.0)), Background(Color::BLACK))));
    app.emit(&UiFrame);
    app.world.resource_mut::<RenderList>().unwrap().take_damage();
    app.world.disable_subtree::<Closed, ChildOf>(panel);
    app.emit(&UiFrame);
    let damage = app.world.resource_mut::<RenderList>().unwrap().take_damage();
    assert_eq!(damage, [Rect::new(0.0, 0.0, 200.0, 100.0)]);
    assert!(app.world.resource::<RenderList>().unwrap().commands().is_empty());
}

#[test]
fn the_rasterizer_covers_shapes_exactly() {
    // A 4×4 square from (1, 1): full inside, empty outside.
    let mut raster = Rasterizer::default();
    raster.reset(6, 6);
    let square = [Point::new(1.0, 1.0), Point::new(5.0, 1.0), Point::new(5.0, 5.0), Point::new(1.0, 5.0)];
    for i in 0..4 {
        raster.line(square[i], square[(i + 1) % 4]);
    }
    let mut coverage = Vec::new();
    raster.coverage(&mut coverage);
    for y in 0..6 {
        for x in 0..6 {
            let inside = (1..5).contains(&x) && (1..5).contains(&y);
            assert_eq!(coverage[y * 6 + x], if inside { 255 } else { 0 }, "({x}, {y})");
        }
    }
    // A half-pixel edge covers half.
    raster.reset(4, 2);
    let half = [Point::new(0.5, 0.0), Point::new(4.0, 0.0), Point::new(4.0, 2.0), Point::new(0.5, 2.0)];
    for i in 0..4 {
        raster.line(half[i], half[(i + 1) % 4]);
    }
    coverage.clear();
    raster.coverage(&mut coverage);
    assert_eq!(&coverage[..4], &[128, 255, 255, 255]);
}

#[test]
fn layout_measures_text_with_the_advances_drawing_uses() {
    let mut fonts = Fonts::default();
    let config = TextConfig { size: 20, letter_spacing: 1, ..TextConfig::default() };
    let measured = fonts.measure("Hello", &config).width;
    let drawn: f32 = "Hello".chars().map(|c| fonts.advance(0, 20, c) + 1.0).sum();
    assert_eq!(measured, drawn);
    assert!(measured > 40.0 && measured < 70.0, "{measured}");
}
