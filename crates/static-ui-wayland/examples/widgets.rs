//! Every widget in one window. With `--png <path>`, renders one frame
//! offscreen (with the dropdown open and the text field focused) and saves
//! it as PPM.
//!
//! cargo run --release -p static-ui-wayland --example widgets
use static_ecs::prelude::*;
use static_ui::widgets::*;
use static_ui::*;
use static_ui_wayland::{Atlas, Canvas, Fonts, WindowOptions, paint, run};

fn heading(text: &str) -> El<(Text,)> {
    el((Text::new(text, 14).color(Color::rgb(110, 110, 120)),))
}

fn main() {
    let mut app = App::new()
        .add_plugins((UiPlugin::<Fonts>::default(), InputPlugin, widgets::<Fonts>()))
        // Rows show their index: a plain system over `Changed<RowIndex>`.
        .add_systems(UiUpdate, |mut rows: Query<(&RowIndex, &Sources<ChildOf>), Changed<RowIndex>>, mut texts: Query<&mut Text>| {
            rows.each(|(index, children)| {
                if let Some(mut text) = children.sources().next().and_then(|c| texts.get_mut(c)) {
                    text.text = format!("Row {}", index.0 + 1);
                }
            })
        })
        .build();
    let world = &mut app.world;
    let screen = world.spawn_ui(el((
        Node::row().width(Sizing::GROW).height(Sizing::GROW).padding(Padding::all(24)).gap(24),
        Background(theme::PANEL),
    )));
    let left = world.spawn_ui_under(screen, el((Node::column().gap(12).width(Sizing::fixed(260.0)),)));
    world.spawn_ui_under(left, heading("Buttons"));
    let row = world.spawn_ui_under(left, el((Node::row().gap(8),)));
    world.spawn_ui_under(row, button("Save"));
    world.spawn_ui_under(row, button("Cancel"));
    world.spawn_ui_under(left, heading("Checkbox"));
    let check = world.spawn_ui_under(left, el((Node::row().gap(8).align(AlignX::Left, AlignY::Center),)));
    world.spawn_ui_under(check, checkbox(true));
    world.spawn_ui_under(check, el((Text::new("Enable sync", 16).color(theme::TEXT),)));
    world.spawn_ui_under(left, heading("Slider"));
    world.spawn_ui_under(left, slider(SliderRange::default(), 0.6));
    world.spawn_ui_under(left, heading("Text field"));
    let field = world.spawn_ui_under(left, text_field("Hello, static-ui", 240.0));
    world.spawn_ui_under(left, heading("Dropdown"));
    let dropdown = spawn_dropdown(world, Some(left), &["Low", "Medium", "High"], 1);
    let tip = world.spawn_ui_under(left, button("Hover me"));
    world.insert(tip, Tooltip("A tooltip, in its behavior's scope".into()));

    let middle = world.spawn_ui_under(screen, el((Node::column().gap(12).width(Sizing::fixed(240.0)),)));
    world.spawn_ui_under(middle, heading("Virtual list (10,000 rows)"));
    spawn_virtual_list(world, Some(middle), VirtualList { count: 10_000, row_height: 24.0 }, 220.0, 240.0, || {
        el((Node::row().width(Sizing::GROW).height(Sizing::fixed(24.0)).padding(Padding::xy(8, 4)),)).children((el((Text::new("", 14).color(theme::TEXT),)),))
    });
    world.spawn_ui_under(middle, heading("Tabs"));
    spawn_tabs(world, Some(middle), &["One", "Two", "Three"], |world, i, page| {
        world.spawn_ui_under(page, el((Text::new(format!("This is page {}", i + 1), 16).color(theme::TEXT),)));
    });

    let right = world.spawn_ui_under(screen, el((Node::column().gap(12).width(Sizing::GROW).height(Sizing::GROW),)));
    world.spawn_ui_under(right, heading("Inspector (MayHave<Closed>)"));
    world.spawn_ui_under(
        right,
        el((Node::column().width(Sizing::GROW).height(Sizing::GROW).padding(Padding::all(8)), Background(theme::FIELD), Clip { horizontal: true, vertical: true }, Scroll::default()))
            .children((el((Text::new("", 11).color(theme::TEXT), Inspector)),)),
    );
    let modal = spawn_modal(world, |world, dialog| {
        world.spawn_ui_under(dialog, el((Text::new("A modal dialog", 18).color(theme::TEXT),)));
        world.spawn_ui_under(dialog, button("Close"));
    });
    let _ = modal;

    let args: Vec<String> = std::env::args().collect();
    if let Some(path) = args.iter().position(|a| a == "--png").and_then(|i| args.get(i + 1)) {
        let (width, height) = (1024usize, 640usize);
        app.world.insert_resource(Viewport { width: width as f32, height: height as f32 });
        app.emit(&UiUpdate);
        app.emit(&UiFrame);
        // Open the dropdown and focus the field, as a user would.
        for target in [dropdown, field] {
            let r = app.world.get::<static_ui::Rect>(target).unwrap().0;
            app.emit(&Input::PointerMoved { x: r.x + 10.0, y: r.y + r.height / 2.0 });
            app.emit(&Input::Button { button: MouseButton::Left, pressed: true });
            app.emit(&Input::Button { button: MouseButton::Left, pressed: false });
            app.emit(&UiUpdate);
            app.emit(&UiFrame);
        }
        app.emit(&UiUpdate);
        app.emit(&UiFrame);
        let mut pixels = vec![0u32; width * height];
        let mut canvas = Canvas { pixels: &mut pixels, width, height, stride: width, touched: 0 };
        let world = &app.world;
        let whole = [static_ui::layout::Rect::new(0.0, 0.0, width as f32, height as f32)];
        paint(
            &mut canvas,
            world.resource::<RenderList>().unwrap(),
            world.resource::<UiLayout>().unwrap(),
            world.resource::<Fonts>().unwrap(),
            &mut Atlas::default(),
            world.resource::<ClearColor>().unwrap().0,
            &whole,
        );
        let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
        for p in &pixels {
            ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        std::fs::write(path, ppm).expect("write");
        return;
    }
    let stats = run(&mut app, WindowOptions { title: "static-ui widgets".into(), width: 1024, height: 640, ..WindowOptions::default() });
    eprintln!("{stats:?}");
}
