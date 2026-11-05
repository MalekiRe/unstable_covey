//! Clay's README example, the sidebar, on static-ui. Opens a window, or with
//! `--png <path>` renders one frame offscreen and saves it.
//!
//! cargo run --release -p static-ui-wayland --example sidebar
use static_ecs::prelude::*;
use static_ui::*;
use static_ui_wayland::{Atlas, Canvas, Fonts, WindowOptions, paint, run};

const COLOR_LIGHT: Color = Color::rgb(224, 215, 210);
const COLOR_RED: Color = Color::rgb(168, 66, 28);
const COLOR_ORANGE: Color = Color::rgb(225, 138, 50);

const OUTER: Node = Node::row().width(Sizing::GROW).height(Sizing::GROW).padding(Padding::all(16)).gap(16);
const SIDEBAR: Node = Node::column().width(Sizing::fixed(300.0)).height(Sizing::GROW).padding(Padding::all(16)).gap(16);
const PROFILE: Node = Node::row().width(Sizing::GROW).padding(Padding::all(16)).gap(16).align(AlignX::Left, AlignY::Center);
const ITEM: Node = Node::row().width(Sizing::GROW).height(Sizing::fixed(50.0)).padding(Padding::xy(16, 0)).align(AlignX::Left, AlignY::Center);

/// A rounded, labeled sidebar row.
type Item = El<(Node, Background, Radius), (El<(Text,)>,)>;
fn sidebar_item(label: &'static str) -> Item {
    el((ITEM, Background(COLOR_ORANGE), Radius::all(8.0))).children((el((Text::new(label, 20).color(Color::WHITE),)),))
}

fn main() {
    let mut app = App::new().add_plugins(UiPlugin::<Fonts>::default()).build();
    app.world.spawn_ui(el((OUTER, Background(Color::rgb(250, 250, 255)))).children((
        el((SIDEBAR, Background(COLOR_LIGHT))).children((
            el((PROFILE, Background(COLOR_RED), Radius::all(8.0))).children((
                el((Node::row().width(Sizing::fixed(60.0)).height(Sizing::fixed(60.0)), Background(COLOR_LIGHT), Radius::all(30.0))),
                el((Text::new("Clay - UI Library", 24).color(Color::WHITE),)),
            )),
            sidebar_item("Item 1"),
            sidebar_item("Item 2"),
            sidebar_item("Item 3"),
            sidebar_item("Item 4"),
            sidebar_item("Item 5"),
        )),
        el((Node::column().width(Sizing::GROW).height(Sizing::GROW).padding(Padding::all(16)), Background(COLOR_LIGHT), Border {
            color: COLOR_RED,
            width: BorderWidth::all(2),
        }))
        .children((el((Text::new("Main content: text wraps to the box it is given, as Clay's does.", 18),)),)),
    )));
    let args: Vec<String> = std::env::args().collect();
    if let Some(path) = args.iter().position(|a| a == "--png").and_then(|i| args.get(i + 1)) {
        let (width, height) = (1024usize, 640usize);
        app.world.insert_resource(Viewport { width: width as f32, height: height as f32 });
        app.emit(&UiFrame);
        let damage = app.world.resource_mut::<RenderList>().unwrap().take_damage();
        let mut pixels = vec![0u32; width * height];
        let mut canvas = Canvas { pixels: &mut pixels, width, height, stride: width, touched: 0 };
        let world = &app.world;
        let painted = paint(
            &mut canvas,
            world.resource::<RenderList>().unwrap(),
            world.resource::<UiLayout>().unwrap(),
            world.resource::<Fonts>().unwrap(),
            &mut Atlas::default(),
            world.resource::<ClearColor>().unwrap().0,
            &damage,
        );
        let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
        for p in &pixels {
            ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        std::fs::write(path, ppm).expect("write");
        eprintln!("{painted:?}");
        return;
    }
    let run_for = args
        .iter()
        .position(|a| a == "--run-for")
        .and_then(|i| args.get(i + 1))
        .map(|s| std::time::Duration::from_secs_f64(s.parse().expect("seconds")));
    let stats = run(&mut app, WindowOptions { title: "Clay sidebar (static-ui)".into(), width: 1024, height: 640, run_for, ..WindowOptions::default() });
    eprintln!("{stats:?}");
}
