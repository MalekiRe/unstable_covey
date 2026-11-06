//! Flappy bird on static-ecs and static-window. Space, up, or a click flaps;
//! escape quits. With `FLAPPY_BENCH` set it exits once its first frame is
//! committed and prints when each launch step finished, relative to
//! `FLAPPY_T0_NS` (the launcher's CLOCK_MONOTONIC before spawn).
mod game;

use game::{Flappy, Game, HEIGHT, Tick, WIDTH};
use static_ecs::prelude::*;
use static_window::{
    Animating, Flow, FrameTime, Framebuffer, KeyCode, KeyInput, PointerButton, Scale, Window, WindowLoop, WindowPlugin,
};

fn now_ns() -> u64 {
    let t = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    t.tv_sec as u64 * 1_000_000_000 + t.tv_nsec as u64
}

/// Space or up flaps; escape closes the window (and so the game).
fn keys<S: Storage>(key: &KeyInput, mut game: Single<&mut Game>, mut commands: Commands<S>) {
    if !key.pressed || key.repeat {
        return;
    }
    match key.code {
        KeyCode::SPACE | KeyCode::UP => game.flap = true,
        KeyCode::ESCAPE => commands.despawn(key.window),
        _ => {}
    }
}
/// A click flaps.
fn click(button: &PointerButton, mut game: Single<&mut Game>) {
    if button.pressed {
        game.flap = true;
    }
}

fn main() {
    let start = now_ns();
    let launched = std::env::var("FLAPPY_T0_NS").ok().and_then(|t| t.parse().ok()).unwrap_or(start);
    let bench = std::env::var_os("FLAPPY_BENCH").is_some();

    let mut app = App::new()
        .add_plugins((WindowPlugin, Flappy))
        .add_systems(label::<KeyInput>(), keys::<_>)
        .add_systems(label::<PointerButton>(), click)
        .build();
    game::spawn(&mut app.world, start);
    // The game's own pixels, whatever the display's scale; always moving.
    app.world.spawn_bundle((
        Window::new("Flappy (static-ecs)").app_id("static-ecs.flappy").size(WIDTH as u32, HEIGHT as u32).resizable(false),
        Framebuffer::at_scale(Scale::ONE),
        Animating,
    ));
    let built = now_ns();
    let mut window_loop = WindowLoop::connect().expect("no Wayland display");
    window_loop.launched_at(launched);
    // After the window's `Step`, the game's: long stalls (dragging, hidden
    // windows) must not tunnel the bird.
    let tick = |app: &mut App<_, _>| {
        let dt = app.world.resource::<FrameTime>().map_or(0.0, |t| t.dt);
        app.emit(&Tick(dt.min(1.0 / 30.0)));
    };
    if bench {
        while window_loop.stats().first_commit_ns == 0 && window_loop.step_with(&mut app, None, tick) == Flow::Continue {}
        let ms = |t: u64| t.saturating_sub(launched) as f64 / 1e6;
        println!(
            "exec={:.3} built={:.3} commit={:.3}",
            ms(start),
            ms(built),
            window_loop.stats().first_commit_ns as f64 / 1e6
        );
        return;
    }
    window_loop.run_with(&mut app, tick);
}
