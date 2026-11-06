//! The UI in a window: what the render list says changed becomes the
//! window's framebuffer damage, and each [`Redraw`] paints just that, at the
//! window's scale. [`run`] drives [`static_window`]'s loop with the UI's
//! update and frame after each `Step`.
use static_ecs::prelude::*;
use static_ecs::Dispatch as Handles;
use static_ui::layout::Rect;
use static_ui::{ClearColor, RenderList, UiClock, UiFrame, UiLayout, UiUpdate, UiWindow};
use static_window::{Error, FrameTime, Framebuffer, PixelRect, Redraw, Stats, Window, WindowLabels, WindowLoop, WindowWorld};

use crate::canvas::Canvas;
use crate::font::{Atlas, Fonts};
use crate::paint::paint;

/// Paints the UI into its [`UiWindow`]. Add it after `UiPlugin<Fonts>`.
pub struct RasterPlugin;
impl Plugin for RasterPlugin {
    type Components = (Framebuffer, UiWindow, Atlas, Fonts, RenderList, UiLayout, ClearColor);
    fn systems<S: Storage>(self) -> impl Systems<S> {
        (
            schedule(UiFrame, exclusive(|world: &mut World<S>| world.init_resource::<Atlas>())),
            schedule(UiFrame, request_damage),
            schedule(label::<Redraw>(), paint_window),
        )
    }
}

/// A window the UI fills, drawn by [`RasterPlugin`].
pub fn ui_window(window: Window) -> (Window, Framebuffer, UiWindow) {
    (window, Framebuffer::default(), UiWindow)
}

/// `UiFrame`, after the render list: what changed, in each UI window's
/// pixels.
fn request_damage(mut list: ResMut<RenderList>, mut windows: Query<&mut Framebuffer, With<UiWindow>>) {
    let damage = list.take_damage();
    if damage.is_empty() {
        return;
    }
    windows.each(|mut fb| {
        let s = fb.scale().factor();
        for r in &damage {
            let (x0, y0) = ((r.x * s).floor().max(0.0), (r.y * s).floor().max(0.0));
            let (x1, y1) = (((r.x + r.width) * s).ceil(), ((r.y + r.height) * s).ceil());
            fb.request(PixelRect::new(x0 as u32, y0 as u32, (x1 - x0).max(0.0) as u32, (y1 - y0).max(0.0) as u32));
        }
    });
}

/// `Redraw`: paint the regions the framebuffer asks for.
fn paint_window(
    redraw: &Redraw,
    list: Res<RenderList>,
    ui: Res<UiLayout>,
    fonts: Res<Fonts>,
    clear: Option<Res<ClearColor>>,
    mut atlas: ResMut<Atlas>,
    mut windows: Query<&mut Framebuffer, With<UiWindow>>,
) {
    let Some(mut fb) = windows.get_mut(redraw.window) else { return };
    let Some(target) = fb.target() else { return };
    let regions = target.regions.iter().map(|r| Rect::new(r.x as f32, r.y as f32, r.width as f32, r.height as f32));
    let (width, height) = (target.width as usize, target.height as usize);
    let mut canvas = Canvas { pixels: target.pixels, width, height, stride: width, touched: 0 };
    let clear = clear.map_or(ClearColor::default().0, |c| c.0);
    paint(&mut canvas, &list, &ui, &fonts, &mut atlas, clear, target.scale.factor(), regions);
}

/// Labels a UI app emits besides the window's.
pub trait UiLabels<S: Storage>: WindowLabels<S> + Handles<UiUpdate, World<S>> + Handles<UiFrame, World<S>> {}
impl<S: Storage, L: WindowLabels<S> + Handles<UiUpdate, World<S>> + Handles<UiFrame, World<S>>> UiLabels<S> for L {}

/// The UI's update and frame, after each `Step`. Unless a test drives it
/// (`UiClock::Manual`), the UI's clock reads the frame's predicted display
/// time ([`FrameTime`]) throughout, and the system clock again after.
pub fn ui_frame<S: WindowWorld + Stores<UiClock>, L: UiLabels<S>>(app: &mut App<S, L>) {
    let manual = matches!(app.world.resource::<UiClock>(), Some(UiClock::Manual(_)));
    if !manual {
        let ahead = app.world.resource::<FrameTime>().map_or(0, |t| t.display_ns.saturating_sub(t.now_ns));
        let now = std::time::Instant::now() + std::time::Duration::from_nanos(ahead);
        app.world.insert_resource(UiClock::Frame(now));
    }
    app.emit(&UiUpdate);
    app.emit(&UiFrame);
    if !manual {
        app.world.insert_resource(UiClock::System);
    }
}

/// Connect to the compositor and run the app's windows (and its UI) until
/// they are gone. Optional globals the compositor lacks are reported once.
pub fn run<S: WindowWorld, L: UiLabels<S>>(app: &mut App<S, L>) -> Result<Stats, Error> {
    let window_loop = WindowLoop::connect()?;
    for missing in window_loop.missing() {
        eprintln!("static-window: the compositor lacks {missing}");
    }
    Ok(window_loop.run_with(app, ui_frame))
}
