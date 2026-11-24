//! egui keys and modifiers as keyboard-map chords (`csm_core::keymap`). The only place that knows
//! both spellings: the matcher ([`consume`]) and the settings' key capture ([`chord_of`]).

use csm_core::keymap::{Chord, Key};
use egui::{InputState, Modifiers};

/// The egui key for a map key. `None` for a key the map has no name for (never for a bindable one).
fn egui_key(key: Key) -> egui::Key {
    use egui::Key as E;
    match key {
        Key::Letter(c) => LETTERS.get(usize::from(c as u8).wrapping_sub(b'A' as usize)).copied().unwrap_or(E::A),
        Key::Digit(d) => DIGITS.get(usize::from(d)).copied().unwrap_or(E::Num0),
        Key::F(n) => FUNCTION.get(usize::from(n).wrapping_sub(1)).copied().unwrap_or(E::F1),
        Key::Left => E::ArrowLeft,
        Key::Right => E::ArrowRight,
        Key::Up => E::ArrowUp,
        Key::Down => E::ArrowDown,
        Key::Home => E::Home,
        Key::End => E::End,
        Key::PageUp => E::PageUp,
        Key::PageDown => E::PageDown,
        Key::Tab => E::Tab,
        Key::Space => E::Space,
        Key::Enter => E::Enter,
        Key::Delete => E::Delete,
        Key::Insert => E::Insert,
        Key::Comma => E::Comma,
        Key::Period => E::Period,
        Key::Slash => E::Slash,
        Key::Minus => E::Minus,
        Key::Equals => E::Equals,
    }
}

const LETTERS: [egui::Key; 26] = [
    egui::Key::A,
    egui::Key::B,
    egui::Key::C,
    egui::Key::D,
    egui::Key::E,
    egui::Key::F,
    egui::Key::G,
    egui::Key::H,
    egui::Key::I,
    egui::Key::J,
    egui::Key::K,
    egui::Key::L,
    egui::Key::M,
    egui::Key::N,
    egui::Key::O,
    egui::Key::P,
    egui::Key::Q,
    egui::Key::R,
    egui::Key::S,
    egui::Key::T,
    egui::Key::U,
    egui::Key::V,
    egui::Key::W,
    egui::Key::X,
    egui::Key::Y,
    egui::Key::Z,
];

const DIGITS: [egui::Key; 10] = [
    egui::Key::Num0,
    egui::Key::Num1,
    egui::Key::Num2,
    egui::Key::Num3,
    egui::Key::Num4,
    egui::Key::Num5,
    egui::Key::Num6,
    egui::Key::Num7,
    egui::Key::Num8,
    egui::Key::Num9,
];

const FUNCTION: [egui::Key; 12] = [
    egui::Key::F1,
    egui::Key::F2,
    egui::Key::F3,
    egui::Key::F4,
    egui::Key::F5,
    egui::Key::F6,
    egui::Key::F7,
    egui::Key::F8,
    egui::Key::F9,
    egui::Key::F10,
    egui::Key::F11,
    egui::Key::F12,
];

/// The map key for an egui key, if a chord can name it.
fn map_key(key: egui::Key) -> Option<Key> {
    use egui::Key as E;
    let found = |list: &[egui::Key]| list.iter().position(|k| *k == key);
    if let Some(i) = found(&LETTERS) {
        return char::from_u32(u32::from(b'A') + u32::try_from(i).ok()?).map(Key::Letter);
    }
    if let Some(i) = found(&DIGITS) {
        return u8::try_from(i).ok().map(Key::Digit);
    }
    if let Some(i) = found(&FUNCTION) {
        return u8::try_from(i + 1).ok().map(Key::F);
    }
    Some(match key {
        E::ArrowLeft => Key::Left,
        E::ArrowRight => Key::Right,
        E::ArrowUp => Key::Up,
        E::ArrowDown => Key::Down,
        E::Home => Key::Home,
        E::End => Key::End,
        E::PageUp => Key::PageUp,
        E::PageDown => Key::PageDown,
        E::Tab => Key::Tab,
        E::Space => Key::Space,
        E::Enter => Key::Enter,
        E::Delete => Key::Delete,
        E::Insert => Key::Insert,
        E::Comma => Key::Comma,
        E::Period => Key::Period,
        E::Slash => Key::Slash,
        E::Minus => Key::Minus,
        E::Equals => Key::Equals,
        _ => return None,
    })
}

/// The chord a key press with these modifiers makes, when the map can name it. Ctrl is the
/// command key on Windows and Linux, so `command` and `ctrl` count alike.
pub fn chord_of(key: egui::Key, m: Modifiers) -> Option<Chord> {
    Some(Chord { ctrl: m.ctrl || m.command, alt: m.alt, shift: m.shift, key: map_key(key)? })
}

/// Takes the key press of `c` from this frame's input, if there was one (exact modifiers, so Ctrl+T
/// never takes Ctrl+Shift+T). Returns whether it did. Ctrl arrives as `command` or as `ctrl`
/// depending on the input (egui sets both on Windows), so either one is accepted.
pub fn consume(i: &mut InputState, c: &Chord) -> bool {
    // Ctrl+Alt is AltGr on Windows. When the OS made a character of the press (AltGr+2 is @ on a
    // Danish keyboard), the character is what was meant: no shortcut runs, and the text goes on to
    // the terminal or the field.
    if c.ctrl && c.alt && c.key.types_text() && typed_text(i) {
        return false;
    }
    let key = egui_key(c.key);
    let base = Modifiers { alt: c.alt, shift: c.shift, ..Modifiers::NONE };
    if c.ctrl {
        crate::app::consume_exact(i, Modifiers { command: true, ..base }, key) || crate::app::consume_exact(i, Modifiers { ctrl: true, ..base }, key)
    } else {
        crate::app::consume_exact(i, base, key)
    }
}

/// Whether this frame's input has a typed character (not a control character).
fn typed_text(i: &InputState) -> bool {
    i.events.iter().any(|e| matches!(e, egui::Event::Text(t) if !t.is_empty() && t.chars().all(|c| !c.is_control())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: egui::Key, text: Option<&str>) -> egui::RawInput {
        let m = Modifiers { ctrl: true, alt: true, command: true, ..Modifiers::NONE };
        let mut events = vec![egui::Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers: m }];
        if let Some(t) = text {
            events.push(egui::Event::Text(t.into()));
        }
        egui::RawInput { events, ..Default::default() }
    }

    fn consumed(raw: egui::RawInput, chord: &Chord) -> (bool, bool) {
        let ctx = egui::Context::default();
        let mut got = (false, false);
        let mut out = ctx.run_ui(raw, |ui| {
            let ctx = ui.ctx().clone();
            got.0 = ctx.input_mut(|i| consume(i, chord));
            got.1 = ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::Text(_))));
        });
        out.textures_delta.clear();
        got
    }

    #[test]
    fn an_altgr_character_is_not_a_shortcut_and_its_text_goes_on() {
        let chord = Chord { ctrl: true, alt: true, shift: false, key: Key::Digit(2) };
        let (took, text_left) = consumed(press(egui::Key::Num2, Some("@")), &chord);
        assert!(!took, "AltGr+2 that typed @ is not a shortcut");
        assert!(text_left, "the @ is still there for the terminal or the field");
        let (took, _) = consumed(press(egui::Key::Num2, None), &chord);
        assert!(took, "the same chord with no character is a shortcut");
        let dup = Chord { ctrl: true, alt: true, shift: false, key: Key::Letter('T') };
        assert!(consumed(press(egui::Key::T, None), &dup).0, "Ctrl+Alt+T types nothing: the shortcut runs");
        assert!(!consumed(press(egui::Key::T, Some("t")), &dup).0, "but not if a layout makes a character of it");
        let arrow = Chord { ctrl: true, alt: true, shift: false, key: Key::Left };
        assert!(consumed(press(egui::Key::ArrowLeft, Some("x")), &arrow).0, "arrows are never AltGr characters");
    }

    #[test]
    fn every_bindable_map_key_has_an_egui_key_and_back() {
        let mut keys: Vec<Key> = (b'A'..=b'Z').map(|c| Key::Letter(char::from(c))).collect();
        keys.extend((0..=9).map(Key::Digit));
        keys.extend((1..=12).map(Key::F));
        keys.extend([
            Key::Left,
            Key::Right,
            Key::Up,
            Key::Down,
            Key::Home,
            Key::End,
            Key::PageUp,
            Key::PageDown,
            Key::Tab,
            Key::Space,
            Key::Enter,
            Key::Delete,
            Key::Insert,
            Key::Comma,
            Key::Period,
            Key::Slash,
            Key::Minus,
            Key::Equals,
        ]);
        for k in keys {
            assert_eq!(map_key(egui_key(k)), Some(k), "{k:?}");
        }
    }

    #[test]
    fn capture_reads_the_modifiers_the_way_the_matcher_takes_them() {
        let ctrl = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
        let c = chord_of(egui::Key::T, ctrl).unwrap_or_else(|| panic!("Ctrl+T"));
        assert_eq!(c.to_string(), "Ctrl+T");
        let shifted = Modifiers { shift: true, ..ctrl };
        assert_eq!(chord_of(egui::Key::PageUp, shifted).map(|c| c.to_string()).as_deref(), Some(concat!("Ctrl+Shift+", "PgUp")));
        assert_eq!(chord_of(egui::Key::Escape, ctrl), None, "Escape is never bound");
    }
}
