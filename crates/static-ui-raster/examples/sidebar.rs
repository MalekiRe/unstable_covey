//! Clay's README example, the sidebar, on static-ui. Opens a window (closing
//! after `--run-for <seconds>` if given), or with `--png <path>` renders
//! one frame offscreen and saves it.
//!
//! cargo run --release -p static-ui-raster --example sidebar
use std::time::{Duration, Instant};

use static_ecs::prelude::*;
use static_ui::*;
use static_ui_raster::{Atlas, Canvas, Fonts, RasterPlugin, paint, run, ui_window};
use static_window::{Step, WakeAt, Window, WindowPlugin};

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
    let args: Vec<String> = std::env::args().collect();
    let run_for = args.iter().position(|a| a == "--run-for").and_then(|i| args.get(i + 1)).map(|s| Duration::from_secs_f64(s.parse().expect("seconds")));
    let mut app = App::new()
        .add_plugins((WindowPlugin, UiPlugin::<Fonts>::default(), RasterPlugin))
        // With `--run-for`, close the window when the time is up.
        .add_systems(Step, move |mut until: Local<Option<Instant>>, mut wake: ResMut<WakeAt>, mut windows: Query<Entity, With<Window>>, mut commands: Commands<_>| {
            let Some(run_for) = run_for else { return };
            let until = *until.get_or_insert_with(|| Instant::now() + run_for);
            if Instant::now() >= until {
                windows.each(|window| commands.despawn(window));
            } else {
                wake.request(until);
            }
        })
        .build();
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
    if let Some(path) = args.iter().position(|a| a == "--png").and_then(|i| args.get(i + 1)) {
        let (width, height) = (1024usize, 640usize);
        app.world.insert_resource(Viewport { width: width as f32, height: height as f32 });
        app.emit(&UiFrame);
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
            1.0,
            [static_ui::layout::Rect::new(0.0, 0.0, width as f32, height as f32)],
        );
        let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
        for p in &pixels {
            ppm.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
        }
        std::fs::write(path, ppm).expect("write");
        eprintln!("{painted:?}");
        return;
    }
    app.world.spawn_bundle(ui_window(Window::new("Clay sidebar (static-ui)").size(1024, 640).app_id("static-ui.sidebar")));
    match run(&mut app) {
        Ok(stats) => eprintln!("{stats:?}"),
        Err(e) => eprintln!("{e}"),
    }
}
