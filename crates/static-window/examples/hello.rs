//! Two windows; every window event is printed. Close them with their title
//! bars' ×.
use static_ecs::prelude::*;
use static_window::*;

fn main() {
    let mut app = App::new()
        .add_plugins(WindowPlugin)
        .add_systems(label::<Resized>(), |e: &Resized| {
            println!("{:?} resized to {}x{} at {:.2}x", e.window, e.surface.width, e.surface.height, e.surface.scale.factor())
        })
        .add_systems(label::<CloseRequested>(), |e: &CloseRequested| println!("{:?} closing", e.window))
        .add_systems(Step, |mut windows: Query<(Entity, &Surface, &WindowState), Changed<WindowState>>| {
            windows.each(|(window, surface, state)| println!("{window:?}: {}x{} {state:?}", surface.width, surface.height))
        })
        .build();
    app.world.spawn_bundle((Window::new("static-window").size(480, 320).app_id("static-window.hello"),));
    app.world.spawn_bundle((Window::new("fixed size").size(320, 200).resizable(false).app_id("static-window.hello"),));
    match run(&mut app) {
        Ok(stats) => println!("done: {} wakeups, {} commits", stats.wakeups, stats.commits),
        Err(e) => eprintln!("{e}"),
    }
}
