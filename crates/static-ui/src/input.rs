//! Raw input, as the window system reports it: a payload label, emitted once
//! per event before the frame's `UiFrame`.
use std::time::Instant;

use static_ecs::prelude::*;

/// A mouse button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Other(u16),
}
/// A key, by meaning (US layout), not by scancode.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Tab,
    Enter,
    Escape,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
    Shift,
    Control,
    Alt,
    /// A printable key, unshifted: `'a'`, `'1'`, `'/'`.
    Char(char),
    /// Anything else, by evdev code.
    Other(u32),
}
/// Modifier keys held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
}

/// One input event: emit with `app.emit(&Input::...)`; systems under
/// `Input` take `&Input` first.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Input {
    /// The pointer moved to `(x, y)`, window coordinates.
    PointerMoved { x: f32, y: f32 },
    /// The pointer left the window.
    PointerLeft,
    Button { button: MouseButton, pressed: bool },
    /// Wheel or touchpad scroll, in pixels (positive: down / right).
    Scroll { dx: f32, dy: f32 },
    Key { key: Key, pressed: bool, modifiers: Modifiers },
    /// Text typed: a printable character, layout applied.
    Text(char),
    /// The window lost keyboard focus.
    FocusLost,
}
impl Schedule for Input {
    type In = Ref<Self>;
}

/// What the window loop should do next: keep requesting frames while
/// something animates, and wake at `deadline` for a timer. A resource;
/// with neither, an idle UI sleeps until input arrives.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Redraw {
    /// Request a frame callback after this frame.
    pub animating: bool,
    /// Emit a frame no later than this.
    pub deadline: Option<Instant>,
}

/// The evdev keycode `code` as a key, US layout.
pub fn key_from_evdev(code: u32) -> Key {
    const ROW1: &[u8] = b"1234567890-=";
    const ROW2: &[u8] = b"qwertyuiop[]";
    const ROW3: &[u8] = b"asdfghjkl;'`";
    const ROW4: &[u8] = b"\\zxcvbnm,./";
    match code {
        1 => Key::Escape,
        2..=13 => Key::Char(ROW1[code as usize - 2] as char),
        14 => Key::Backspace,
        15 => Key::Tab,
        16..=27 => Key::Char(ROW2[code as usize - 16] as char),
        28 | 96 => Key::Enter,
        29 | 97 => Key::Control,
        30..=41 => Key::Char(ROW3[code as usize - 30] as char),
        42 | 54 => Key::Shift,
        43..=53 => Key::Char(ROW4[code as usize - 43] as char),
        56 | 100 => Key::Alt,
        57 => Key::Space,
        102 => Key::Home,
        103 => Key::Up,
        104 => Key::PageUp,
        105 => Key::Left,
        106 => Key::Right,
        107 => Key::End,
        108 => Key::Down,
        109 => Key::PageDown,
        111 => Key::Delete,
        other => Key::Other(other),
    }
}
/// The character a key types, US layout, or `None`.
pub fn key_text(key: Key, shift: bool) -> Option<char> {
    match key {
        Key::Space => Some(' '),
        Key::Char(c) if !shift => Some(c),
        Key::Char(c) => Some(match c {
            'a'..='z' => c.to_ascii_uppercase(),
            '1' => '!',
            '2' => '@',
            '3' => '#',
            '4' => '$',
            '5' => '%',
            '6' => '^',
            '7' => '&',
            '8' => '*',
            '9' => '(',
            '0' => ')',
            '-' => '_',
            '=' => '+',
            '[' => '{',
            ']' => '}',
            ';' => ':',
            '\'' => '"',
            '`' => '~',
            '\\' => '|',
            ',' => '<',
            '.' => '>',
            '/' => '?',
            other => other,
        }),
        _ => None,
    }
}
