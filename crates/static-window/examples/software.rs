//! A software-rendered window: a gradient, with a square sweeping across it
//! at the display's rate, repainting only the square's old and new boxes.
//! `software 3` closes after three seconds.
use static_ecs::prelude::*;
use static_window::*;

/// Where the square is, in logical units, and when to stop.
struct Sweep {
    x: f32,
    drawn: Option<PixelRect>,
    until_ns: Option<u64>,
}

fn square(x: f32, scale: Scale) -> PixelRect {
    let s = scale.factor();
    PixelRect::new((x * s) as u32, (60.0 * s) as u32, (40.0 * s) as u32, (40.0 * s) as u32)
}

fn main() {
    let seconds: Option<f64> = std::env::args().nth(1).and_then(|s| s.parse().ok());
    let mut app = App::new()
        .add_plugins(WindowPlugin)
        .insert_resource(Sweep { x: 0.0, drawn: None, until_ns: None })
        .add_systems(
            Step,
            move |time: Res<FrameTime>, mut sweep: ResMut<Sweep>, mut windows: Query<(Entity, &Surface, &mut Framebuffer)>, mut commands: Commands<_>| {
                let until = *sweep.until_ns.get_or_insert_with(|| seconds.map_or(u64::MAX, |s| time.now_ns + (s * 1e9) as u64));
                windows.each(|(window, surface, mut fb)| {
                    if time.now_ns >= until {
                        commands.despawn(window);
                        return;
                    }
                    sweep.x = (sweep.x + 240.0 * time.dt) % (surface.width as f32 - 40.0).max(1.0);
                    let next = square(sweep.x, surface.scale);
                    if let Some(old) = sweep.drawn.replace(next) {
                        fb.request(old);
                    }
                    fb.request(next);
                });
            },
        )
        .add_systems(label::<Redraw>(), |redraw: &Redraw, sweep: Res<Sweep>, mut fbs: Query<&mut Framebuffer>| {
            let Some(mut fb) = fbs.get_mut(redraw.window) else { return };
            let Some(target) = fb.target() else { return };
            let square = square(sweep.x, target.scale);
            for r in target.regions {
                for y in r.y..r.y + r.height {
                    for x in r.x..r.x + r.width {
                        let inside = (square.x..square.x + square.width).contains(&x) && (square.y..square.y + square.height).contains(&y);
                        let shade = (x * 255 / target.width.max(1)) << 8 | (y * 255 / target.height.max(1));
                        target.pixels[(y * target.width + x) as usize] = if inside { 0xFFFF_FFFF } else { 0xFF00_0000 | shade };
                    }
                }
            }
        })
        .build();
    app.world.spawn_bundle((Window::new("software").size(480, 200).app_id("static-window.software"), Framebuffer::default(), Animating));
    match run(&mut app) {
        Ok(stats) => println!(
            "{} wakeups, {} frames ({} from frame callbacks), {} commits, first commit {:.2} ms after start",
            stats.wakeups,
            stats.frames,
            stats.callback_frames,
            stats.commits,
            stats.first_commit_ns as f64 / 1e6
        ),
        Err(e) => eprintln!("{e}"),
    }
}
