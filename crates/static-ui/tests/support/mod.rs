//! Window input for UI tests, sent as static-window's loop would send it.
#![allow(dead_code)]
use static_ecs::prelude::*;
use static_window::*;

/// One window event, in a test's shorthand.
#[derive(Clone, Copy, Debug)]
pub enum Input {
    PointerMoved { x: f32, y: f32 },
    PointerLeft,
    Button { button: MouseButton, pressed: bool },
    Scroll { dx: f32, dy: f32 },
    Key { key: Key, pressed: bool, modifiers: Modifiers },
    Text(char),
    FocusLost,
}

/// Emit `input` as the window event it stands for.
pub fn send<S: Storage, L: WindowLabels<S>>(app: &mut App<S, L>, input: Input) {
    // The UI does not care which window: one stands in.
    let window = Entity::PLACEHOLDER;
    match input {
        Input::PointerMoved { x, y } => app.emit(&PointerMoved { window, x, y }),
        Input::PointerLeft => app.emit(&PointerLeft { window }),
        Input::Button { button, pressed } => app.emit(&PointerButton { window, button, pressed }),
        Input::Scroll { dx, dy } => app.emit(&Scroll { window, dx, dy, lines: None }),
        Input::Key { key, pressed, modifiers } => app.emit(&KeyInput { window, code: KeyCode(0), key, pressed, repeat: false, modifiers }),
        Input::Text(ch) => app.emit(&TextInput::new(window, ch.encode_utf8(&mut [0; 4])).unwrap()),
        Input::FocusLost => app.emit(&FocusChanged { window, focused: false }),
    }
}
