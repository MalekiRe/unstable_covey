//! Two windows; every window event is printed. In a window: type to see
//! keys and text, Ctrl+C copies its title, Ctrl+V prints the clipboard, L
//! locks the pointer (Escape lets go). Close them with their title bars' ×.
use static_ecs::prelude::*;
use static_window::*;

fn main() {
    let mut app = App::new()
        .add_plugins(WindowPlugin)
        .add_systems(label::<Resized>(), |e: &Resized| {
            println!("{:?} resized to {}x{} at {:.2}x", e.window, e.surface.width, e.surface.height, e.surface.scale.factor())
        })
        .add_systems(label::<CloseRequested>(), |e: &CloseRequested| println!("{:?} closing", e.window))
        .add_systems(label::<FocusChanged>(), |e: &FocusChanged| println!("{:?} focused: {}", e.window, e.focused))
        .add_systems(label::<TextInput>(), |e: &TextInput| println!("typed {:?}", e.text()))
        .add_systems(label::<PointerDelta>(), |e: &PointerDelta| println!("locked motion {:+.1},{:+.1}", e.dx, e.dy))
        .add_systems(label::<ClipboardText>(), |e: &ClipboardText| println!("clipboard: {:?}", e.text))
        .add_systems(
            label::<KeyInput>(),
            |e: &KeyInput, mut clipboard: ResMut<Clipboard>, mut windows: Query<&Window>, mut commands: Commands<_>| {
                println!("key {:?} ({:?}) {}", e.key, e.code, if e.pressed { "down" } else { "up" });
                if !e.pressed {
                    return;
                }
                match (e.key, e.modifiers.control) {
                    (Key::Char('c'), true) => clipboard.set(windows.get_mut(e.window).map_or("", |w| &w.title)),
                    (Key::Char('v'), true) => clipboard.request(),
                    (Key::Char('l'), false) => commands.insert(e.window, (PointerLock,)),
                    (Key::Escape, _) => commands.remove::<(PointerLock,)>(e.window),
                    _ => {}
                }
            },
        )
        .add_systems(Step, |mut windows: Query<(Entity, &Surface, &WindowState), Changed<WindowState>>| {
            windows.each(|(window, surface, state)| println!("{window:?}: {}x{} {state:?}", surface.width, surface.height))
        })
        .build();
    app.world.spawn_bundle((Window::new("static-window").size(480, 320).app_id("static-window.hello"), CursorIcon::Pointer));
    app.world.spawn_bundle((Window::new("fixed size").size(320, 200).resizable(false).app_id("static-window.hello"), CursorIcon::Crosshair));
    match run(&mut app) {
        Ok(stats) => println!("done: {} wakeups, {} commits", stats.wakeups, stats.commits),
        Err(e) => eprintln!("{e}"),
    }
}
