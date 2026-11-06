//! The compositor's keymap, through kbvm: what a key means at the current
//! modifiers, and the text it types (dead keys composed, control characters
//! never).
use kbvm::lookup::LookupTable;
use kbvm::xkb::Context;
use kbvm::xkb::compose::{ComposeTable, FeedResult, State as ComposeState};
use kbvm::xkb::diagnostic::{Diagnostic, DiagnosticHandler};
use kbvm::{Components, GroupIndex, Keycode, Keysym, ModifierMask, syms};

use crate::input::Key;

/// Keymap diagnostics are not the app's concern: a keymap that fails to
/// compile only costs layout-aware keys.
struct Ignore;
impl DiagnosticHandler for Ignore {
    fn handle(&mut self, _: Diagnostic) {}
}

/// What a key press typed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Typed {
    Nothing,
    /// Part of a compose sequence (a dead key): nothing yet.
    Composing,
    /// Insertable text, in [`Keymap::text`].
    Text,
}

pub(crate) struct Keymap {
    table: LookupTable,
    components: Components,
    compose: Option<(ComposeTable, ComposeState)>,
    /// The last press's text, reused so typing allocates nothing.
    text: String,
}

impl Keymap {
    /// Compile the keymap the compositor sent; the compose table comes from
    /// the locale (`LC_ALL`, `LC_CTYPE`, `LANG`), as libxkbcommon's does.
    pub fn from_bytes(keymap: &[u8]) -> Option<Self> {
        let context = Context::default();
        let compose = context.compose_table_builder().build(Ignore);
        Self::with_compose(&context, keymap, compose)
    }
    pub fn with_compose(context: &Context, keymap: &[u8], compose: Option<ComposeTable>) -> Option<Self> {
        // The protocol's keymap string ends in a NUL.
        let keymap = keymap.strip_suffix(&[0]).unwrap_or(keymap);
        let keymap = context.keymap_from_bytes(Ignore, None, keymap).ok()?;
        let compose = compose.map(|table| {
            let state = table.create_state();
            (table, state)
        });
        Some(Self { table: keymap.to_builder().build_lookup_table(), components: Components::default(), compose, text: String::new() })
    }
    /// `wl_keyboard.modifiers`.
    pub fn set_modifiers(&mut self, depressed: u32, latched: u32, locked: u32, group: u32) {
        let c = &mut self.components;
        c.mods_pressed = ModifierMask(depressed);
        c.mods_latched = ModifierMask(latched);
        c.mods_locked = ModifierMask(locked);
        c.group_locked = GroupIndex(group);
        c.update_effective();
    }
    pub fn repeats(&self, evdev: u32) -> bool {
        self.table.repeats(Keycode::from_evdev(evdev))
    }
    fn keysym(&self, evdev: u32, ctrl_transform: bool) -> Option<kbvm::lookup::KeysymProps> {
        let c = &self.components;
        self.table.lookup(c.group, c.mods, Keycode::from_evdev(evdev)).with_ctrl_transform(ctrl_transform).into_iter().next()
    }
    /// The key as the layout reads it at the current shift level; control
    /// does not turn letters into control codes here.
    pub fn key(&self, evdev: u32) -> Key {
        self.keysym(evdev, false).map_or(Key::Unidentified, |props| key(props.keysym(), props.char()))
    }
    /// What a press of `evdev` types; the text is then in [`Self::text`].
    pub fn typed(&mut self, evdev: u32) -> Typed {
        self.text.clear();
        let Some(props) = self.keysym(evdev, true) else { return Typed::Nothing };
        if let Some((table, state)) = &mut self.compose {
            match table.feed(state, props.keysym()) {
                Some(FeedResult::Pending) => return Typed::Composing,
                // A dead key followed by one that doesn't combine: sk_app
                // drops both, as libxkbcommon's cancelled state does.
                Some(FeedResult::Aborted) => return Typed::Nothing,
                Some(FeedResult::Composed { string: Some(text), .. }) => self.text.push_str(text),
                Some(FeedResult::Composed { string: None, keysym }) => self.text.extend(keysym.and_then(Keysym::char)),
                None => {
                    let c = &self.components;
                    self.text.extend(self.table.lookup(c.group, c.mods, Keycode::from_evdev(evdev)).into_iter().filter_map(|p| p.char()));
                }
            }
        } else {
            let c = &self.components;
            self.text.extend(self.table.lookup(c.group, c.mods, Keycode::from_evdev(evdev)).into_iter().filter_map(|p| p.char()));
        }
        if crate::event::insertable(&self.text) { Typed::Text } else { Typed::Nothing }
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Focus left mid-sequence: the next window starts clean.
    pub fn reset_compose(&mut self) {
        if let Some((table, state)) = &mut self.compose {
            *state = table.create_state();
        }
    }
}

fn key(sym: Keysym, ch: Option<char>) -> Key {
    match sym {
        syms::Return | syms::KP_Enter => Key::Enter,
        syms::Tab | syms::ISO_Left_Tab | syms::KP_Tab => Key::Tab,
        syms::Escape => Key::Escape,
        syms::BackSpace => Key::Backspace,
        syms::Delete | syms::KP_Delete => Key::Delete,
        syms::Insert | syms::KP_Insert => Key::Insert,
        syms::Left | syms::KP_Left => Key::Left,
        syms::Right | syms::KP_Right => Key::Right,
        syms::Up | syms::KP_Up => Key::Up,
        syms::Down | syms::KP_Down => Key::Down,
        syms::Home | syms::KP_Home => Key::Home,
        syms::End | syms::KP_End => Key::End,
        syms::Prior | syms::KP_Prior => Key::PageUp,
        syms::Next | syms::KP_Next => Key::PageDown,
        syms::Shift_L | syms::Shift_R => Key::Shift,
        syms::Control_L | syms::Control_R => Key::Control,
        syms::Alt_L | syms::Alt_R | syms::Meta_L | syms::Meta_R => Key::Alt,
        syms::Super_L | syms::Super_R => Key::Super,
        syms::Caps_Lock => Key::CapsLock,
        s if (syms::F1.0..=syms::F24.0).contains(&s.0) => Key::F((s.0 - syms::F1.0 + 1) as u8),
        _ => match ch {
            Some(c) if !c.is_control() => Key::Char(c),
            _ => Key::Unidentified,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const US: &[u8] = include_bytes!("../tests/support/us.xkb");
    const US_INTL: &[u8] = include_bytes!("../tests/support/us-intl.xkb");
    // Evdev codes.
    const A: u32 = 30;
    const E: u32 = 18;
    const APOSTROPHE: u32 = 40;
    const ENTER: u32 = 28;
    const F5: u32 = 63;
    const SPACE: u32 = 57;
    const LEFT_SHIFT: u32 = 42;
    // The us keymap's modifier masks.
    const SHIFT: u32 = 1;
    const CONTROL: u32 = 4;

    fn keymap(bytes: &[u8], compose: Option<&[u8]>) -> Keymap {
        let context = Context::default();
        let compose = compose.map(|rules| {
            let mut builder = context.compose_table_builder();
            builder.buffer(rules);
            builder.build(Ignore).expect("compose rules")
        });
        Keymap::with_compose(&context, bytes, compose).expect("keymap")
    }
    /// What a press types, as a string ("…" while composing).
    fn typed(k: &mut Keymap, code: u32) -> String {
        match k.typed(code) {
            Typed::Nothing => String::new(),
            Typed::Composing => "…".into(),
            Typed::Text => k.text().into(),
        }
    }

    #[test]
    fn keys_follow_the_shift_level_but_not_control() {
        let mut k = keymap(US, None);
        assert_eq!((k.key(A), typed(&mut k, A).as_str()), (Key::Char('a'), "a"));
        k.set_modifiers(SHIFT, 0, 0, 0);
        assert_eq!((k.key(A), typed(&mut k, A).as_str()), (Key::Char('A'), "A"));
        k.set_modifiers(CONTROL, 0, 0, 0);
        assert_eq!(k.key(A), Key::Char('a'), "Ctrl+A is still the A key");
        assert_eq!(typed(&mut k, A), "", "and types no control character");
        k.set_modifiers(0, 0, 0, 0);
        assert_eq!((k.key(ENTER), typed(&mut k, ENTER).as_str()), (Key::Enter, ""));
        assert_eq!(k.key(F5), Key::F(5));
        assert_eq!((k.key(SPACE), typed(&mut k, SPACE).as_str()), (Key::Char(' '), " "));
        assert_eq!(k.key(LEFT_SHIFT), Key::Shift);
        assert!(k.repeats(A) && !k.repeats(LEFT_SHIFT));
    }

    #[test]
    fn dead_keys_compose_and_unmatched_sequences_type_nothing() {
        let mut k = keymap(US_INTL, Some(b"<dead_acute> <e> : \"\xc3\xa9\"\n".as_slice()));
        assert_eq!(k.key(APOSTROPHE), Key::Unidentified, "a dead key on its own");
        assert_eq!(typed(&mut k, APOSTROPHE), "…");
        assert_eq!(typed(&mut k, E), "é");
        assert_eq!(typed(&mut k, APOSTROPHE), "…");
        assert_eq!(typed(&mut k, A), "");
        typed(&mut k, APOSTROPHE);
        k.reset_compose();
        assert_eq!(typed(&mut k, E), "e", "a reset sequence starts clean");
    }
}
