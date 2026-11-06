//! Input vocabulary and state. Keys come twice, as sk_app reports them: the
//! physical key ([`KeyCode`], layout-independent) and what the layout makes
//! of it ([`Key`]); typed text is a separate event that never carries
//! control characters.
use static_ecs::prelude::*;

/// A physical key, by Linux evdev code: `KeyCode::A` is the key where A sits
/// on a US keyboard, whatever the layout prints on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct KeyCode(pub u16);

#[allow(missing_docs)]
impl KeyCode {
    pub const ESCAPE: Self = Self(1);
    pub const DIGIT1: Self = Self(2);
    pub const DIGIT2: Self = Self(3);
    pub const DIGIT3: Self = Self(4);
    pub const DIGIT4: Self = Self(5);
    pub const DIGIT5: Self = Self(6);
    pub const DIGIT6: Self = Self(7);
    pub const DIGIT7: Self = Self(8);
    pub const DIGIT8: Self = Self(9);
    pub const DIGIT9: Self = Self(10);
    pub const DIGIT0: Self = Self(11);
    pub const MINUS: Self = Self(12);
    pub const EQUAL: Self = Self(13);
    pub const BACKSPACE: Self = Self(14);
    pub const TAB: Self = Self(15);
    pub const Q: Self = Self(16);
    pub const W: Self = Self(17);
    pub const E: Self = Self(18);
    pub const R: Self = Self(19);
    pub const T: Self = Self(20);
    pub const Y: Self = Self(21);
    pub const U: Self = Self(22);
    pub const I: Self = Self(23);
    pub const O: Self = Self(24);
    pub const P: Self = Self(25);
    pub const LEFT_BRACKET: Self = Self(26);
    pub const RIGHT_BRACKET: Self = Self(27);
    pub const ENTER: Self = Self(28);
    pub const LEFT_CONTROL: Self = Self(29);
    pub const A: Self = Self(30);
    pub const S: Self = Self(31);
    pub const D: Self = Self(32);
    pub const F: Self = Self(33);
    pub const G: Self = Self(34);
    pub const H: Self = Self(35);
    pub const J: Self = Self(36);
    pub const K: Self = Self(37);
    pub const L: Self = Self(38);
    pub const SEMICOLON: Self = Self(39);
    pub const APOSTROPHE: Self = Self(40);
    pub const GRAVE: Self = Self(41);
    pub const LEFT_SHIFT: Self = Self(42);
    pub const BACKSLASH: Self = Self(43);
    pub const Z: Self = Self(44);
    pub const X: Self = Self(45);
    pub const C: Self = Self(46);
    pub const V: Self = Self(47);
    pub const B: Self = Self(48);
    pub const N: Self = Self(49);
    pub const M: Self = Self(50);
    pub const COMMA: Self = Self(51);
    pub const PERIOD: Self = Self(52);
    pub const SLASH: Self = Self(53);
    pub const RIGHT_SHIFT: Self = Self(54);
    pub const LEFT_ALT: Self = Self(56);
    pub const SPACE: Self = Self(57);
    pub const CAPS_LOCK: Self = Self(58);
    pub const F1: Self = Self(59);
    pub const F2: Self = Self(60);
    pub const F3: Self = Self(61);
    pub const F4: Self = Self(62);
    pub const F5: Self = Self(63);
    pub const F6: Self = Self(64);
    pub const F7: Self = Self(65);
    pub const F8: Self = Self(66);
    pub const F9: Self = Self(67);
    pub const F10: Self = Self(68);
    pub const F11: Self = Self(87);
    pub const F12: Self = Self(88);
    pub const RIGHT_CONTROL: Self = Self(97);
    pub const RIGHT_ALT: Self = Self(100);
    pub const HOME: Self = Self(102);
    pub const UP: Self = Self(103);
    pub const PAGE_UP: Self = Self(104);
    pub const LEFT: Self = Self(105);
    pub const RIGHT: Self = Self(106);
    pub const END: Self = Self(107);
    pub const DOWN: Self = Self(108);
    pub const PAGE_DOWN: Self = Self(109);
    pub const INSERT: Self = Self(110);
    pub const DELETE: Self = Self(111);
    pub const LEFT_SUPER: Self = Self(125);
    pub const RIGHT_SUPER: Self = Self(126);

    /// Evdev codes run to `KEY_MAX` (0x2ff).
    pub const COUNT: usize = 0x300;

    fn modifier(self) -> Option<Modifier> {
        Some(match self {
            Self::LEFT_SHIFT | Self::RIGHT_SHIFT => Modifier::Shift,
            Self::LEFT_CONTROL | Self::RIGHT_CONTROL => Modifier::Control,
            Self::LEFT_ALT | Self::RIGHT_ALT => Modifier::Alt,
            Self::LEFT_SUPER | Self::RIGHT_SUPER => Modifier::Super,
            _ => return None,
        })
    }
    /// Whether this is a modifier key (shift, control, alt, super).
    pub fn is_modifier(self) -> bool {
        self.modifier().is_some()
    }
}

enum Modifier {
    Shift,
    Control,
    Alt,
    Super,
}

/// A key as the active layout reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Enter,
    Tab,
    Escape,
    Backspace,
    Delete,
    Insert,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Shift,
    Control,
    Alt,
    Super,
    CapsLock,
    /// F1–F24.
    F(u8),
    /// A key that types a character, at the current shift level: `'a'`,
    /// `'A'` with shift, `' '` for space.
    Char(char),
    /// Anything else (media keys, a dead key on its own).
    Unidentified,
}

/// Modifier keys held, derived from tracked key state: the compositor's
/// modifier event arrives after the key it changes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub shift: bool,
    pub control: bool,
    pub alt: bool,
    pub super_: bool,
}

/// A pointer button.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Right,
    Middle,
    Back,
    Forward,
    /// Anything else, by evdev code.
    Other(u16),
}
impl MouseButton {
    /// From a `wl_pointer.button` evdev code (`BTN_LEFT` is 0x110).
    pub fn from_evdev(code: u32) -> Self {
        match code {
            0x110 => Self::Left,
            0x111 => Self::Right,
            0x112 => Self::Middle,
            0x113 => Self::Back,
            0x114 => Self::Forward,
            other => Self::Other(other as u16),
        }
    }
    fn bit(self) -> u32 {
        match self {
            Self::Left => 1,
            Self::Right => 2,
            Self::Middle => 4,
            Self::Back => 8,
            Self::Forward => 16,
            Self::Other(_) => 0,
        }
    }
}

/// The keyboard: which physical keys are down and the modifiers they make.
/// A resource, updated before each key event is emitted.
#[derive(Clone, Debug)]
pub struct Keys {
    pressed: [u64; KeyCode::COUNT / 64],
    modifiers: Modifiers,
}
impl Default for Keys {
    fn default() -> Self {
        Self { pressed: [0; KeyCode::COUNT / 64], modifiers: Modifiers::default() }
    }
}
impl Keys {
    pub fn pressed(&self, code: KeyCode) -> bool {
        let i = code.0 as usize;
        i < KeyCode::COUNT && self.pressed[i / 64] & (1 << (i % 64)) != 0
    }
    pub fn modifiers(&self) -> Modifiers {
        self.modifiers
    }
    /// Record `code` going up or down; returns whether that changed anything.
    pub(crate) fn set(&mut self, code: KeyCode, down: bool) -> bool {
        let i = code.0 as usize;
        if i >= KeyCode::COUNT || self.pressed(code) == down {
            return false;
        }
        self.pressed[i / 64] ^= 1 << (i % 64);
        if code.is_modifier() {
            self.modifiers = self.derive();
        }
        true
    }
    /// Release one held key, if any, for releasing everything on focus loss
    /// one event at a time (each with the modifiers left after it).
    pub(crate) fn release_one(&mut self) -> Option<KeyCode> {
        let word = self.pressed.iter().position(|&w| w != 0)?;
        let code = KeyCode((word * 64 + self.pressed[word].trailing_zeros() as usize) as u16);
        self.set(code, false);
        Some(code)
    }
    fn derive(&self) -> Modifiers {
        let mut m = Modifiers::default();
        for (i, &word) in self.pressed.iter().enumerate().take(2) {
            let mut bits = word;
            while bits != 0 {
                let code = KeyCode((i * 64 + bits.trailing_zeros() as usize) as u16);
                bits &= bits - 1;
                match code.modifier() {
                    Some(Modifier::Shift) => m.shift = true,
                    Some(Modifier::Control) => m.control = true,
                    Some(Modifier::Alt) => m.alt = true,
                    Some(Modifier::Super) => m.super_ = true,
                    None => {}
                }
            }
        }
        m
    }
}

/// The pointer: where it is, over which window, and the buttons down. A
/// resource, updated before each pointer event is emitted.
#[derive(Clone, Debug, Default)]
pub struct Pointer {
    /// The window under the pointer.
    pub window: Option<Entity>,
    /// Position in that window, in logical units.
    pub x: f32,
    pub y: f32,
    buttons: u32,
    delta: (f32, f32),
}
impl Pointer {
    pub fn pressed(&self, button: MouseButton) -> bool {
        self.buttons & button.bit() != 0
    }
    /// Motion since the last call, for a per-frame reader that would miss
    /// some of several motion events.
    pub fn take_delta(&mut self) -> (f32, f32) {
        std::mem::take(&mut self.delta)
    }
    pub(crate) fn moved(&mut self, x: f32, y: f32) {
        self.delta.0 += x - self.x;
        self.delta.1 += y - self.y;
        self.x = x;
        self.y = y;
    }
    pub(crate) fn set_button(&mut self, button: MouseButton, down: bool) {
        if down {
            self.buttons |= button.bit();
        } else {
            self.buttons &= !button.bit();
        }
    }
    /// The buttons down, for releasing them when focus leaves.
    pub(crate) fn held(&self) -> impl Iterator<Item = MouseButton> + use<> {
        let buttons = self.buttons;
        [MouseButton::Left, MouseButton::Right, MouseButton::Middle, MouseButton::Back, MouseButton::Forward]
            .into_iter()
            .filter(move |b| buttons & b.bit() != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifiers_follow_held_keys_on_either_side() {
        let mut keys = Keys::default();
        assert!(keys.set(KeyCode::LEFT_SHIFT, true));
        assert!(keys.set(KeyCode::RIGHT_SHIFT, true));
        assert!(!keys.set(KeyCode::RIGHT_SHIFT, true), "already down");
        keys.set(KeyCode::LEFT_SHIFT, false);
        assert!(keys.modifiers().shift, "the right shift still holds it");
        keys.set(KeyCode::RIGHT_SHIFT, false);
        keys.set(KeyCode::RIGHT_SUPER, true);
        assert_eq!(keys.modifiers(), Modifiers { super_: true, ..Modifiers::default() });
    }

    #[test]
    fn releasing_all_sheds_modifiers_one_key_at_a_time() {
        let mut keys = Keys::default();
        keys.set(KeyCode::LEFT_CONTROL, true);
        keys.set(KeyCode::C, true);
        keys.set(KeyCode(0x2ff), true);
        let mut released = Vec::new();
        while let Some(code) = keys.release_one() {
            released.push((code, keys.modifiers().control));
        }
        assert_eq!(released, [(KeyCode::LEFT_CONTROL, false), (KeyCode::C, false), (KeyCode(0x2ff), false)]);
        assert!(!keys.pressed(KeyCode::C));
    }

    #[test]
    fn pointer_delta_accumulates_until_taken() {
        let mut p = Pointer::default();
        p.moved(10.0, 5.0);
        p.moved(12.0, 4.0);
        assert_eq!(p.take_delta(), (12.0, 4.0));
        assert_eq!(p.take_delta(), (0.0, 0.0));
        p.set_button(MouseButton::Left, true);
        p.set_button(MouseButton::Forward, true);
        assert_eq!(p.held().collect::<Vec<_>>(), [MouseButton::Left, MouseButton::Forward]);
    }
}
