//! Where the UI meets its window ([`static_window`]): the window events it
//! handles, the window it fills ([`UiWindow`]), and keeping that window
//! drawing while something animates.
use static_ecs::prelude::*;
use static_window::{Animating, Key, Modifiers, MouseButton, Surface};

use crate::Viewport;

/// The window the UI fills: its size is the [`Viewport`], and it keeps
/// drawing frames (it holds [`Animating`]) while the UI animates.
#[derive(Clone, Copy, Debug, Default)]
pub struct UiWindow;

/// One window event, in the shape the input handler takes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Input {
    PointerMoved { x: f32, y: f32 },
    PointerLeft,
    Button { button: MouseButton, pressed: bool },
    Scroll { dx: f32, dy: f32 },
    Key { key: Key, pressed: bool, modifiers: Modifiers },
    Text(char),
    FocusLost,
}

/// `UiUpdate`, first: the viewport follows the UI window's size.
pub(crate) fn follow_window(mut windows: Query<&Surface, With<UiWindow>>, mut viewport: ResMut<Viewport>) {
    if let Some(surface) = windows.iter_mut().next() {
        let size = Viewport { width: surface.width as f32, height: surface.height as f32 };
        if *viewport != size {
            *viewport = size;
        }
    }
}

/// Keep the UI window drawing frames while `animating`, and only then.
pub(crate) fn set_animating<S: Storage>(windows: &mut Query<(Entity, Has<Animating>), With<UiWindow>>, commands: &mut Commands<S>, animating: bool) {
    windows.each(|(window, has)| match (animating, has) {
        (true, false) => commands.insert(window, (Animating,)),
        (false, true) => commands.remove::<(Animating,)>(window),
        _ => {}
    });
}
