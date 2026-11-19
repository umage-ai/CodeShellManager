//! Turning user input into bytes for the PTY. UI-toolkit-free so it is unit-tested.
//!
//! Two kinds of input, as two types (CLAUDE.md "`SendToTerminal` types; `PasteToTerminal`
//! pastes"): key presses are [`AppAuthored`] bytes built here; text the user did not type key by
//! key — clipboard, drops, run output — is [`Untrusted`] and only ever reaches the PTY through
//! [`paste`], which wraps it in bracketed-paste markers when the program enabled them and strips
//! anything that could end the paste early or inject escape sequences.

use alacritty_terminal::term::TermMode;

/// Bytes the app itself authored (a keystroke encoding, a fixed reply). Safe to write raw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppAuthored(pub Vec<u8>);

/// Text from outside the user's keystrokes. Never written raw; see [`paste`].
#[derive(Debug, Clone, Copy)]
pub struct Untrusted<'a>(pub &'a str);

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Mods {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Mods {
    /// The xterm modifier parameter (1 = none).
    fn param(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
    fn any(self) -> bool {
        self.shift || self.alt || self.ctrl
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    /// F1..=F12.
    F(u8),
    /// A character key pressed together with Ctrl and/or Alt (plain text arrives as text).
    Char(char),
}

/// Encodes a key press. `None` when the key has no encoding (e.g. F13).
pub fn encode_key(key: Key, mods: Mods, mode: TermMode) -> Option<AppAuthored> {
    let app_cursor = mode.contains(TermMode::APP_CURSOR);
    let csi_mod = |final_byte: char| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[1;{}{final_byte}", mods.param()).into_bytes()
        } else if app_cursor {
            format!("\x1bO{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if mods.any() {
            format!("\x1b[{n};{}~", mods.param()).into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let alt_prefix = |mut v: Vec<u8>| -> Vec<u8> {
        if mods.alt {
            v.insert(0, 0x1b);
        }
        v
    };
    let bytes = match key {
        Key::Enter => alt_prefix(vec![b'\r']),
        Key::Tab if mods.shift => b"\x1b[Z".to_vec(),
        Key::Tab => alt_prefix(vec![b'\t']),
        Key::Backspace if mods.ctrl => alt_prefix(vec![0x08]),
        Key::Backspace => alt_prefix(vec![0x7f]),
        Key::Escape => alt_prefix(vec![0x1b]),
        Key::Up => csi_mod('A'),
        Key::Down => csi_mod('B'),
        Key::Right => csi_mod('C'),
        Key::Left => csi_mod('D'),
        Key::Home => csi_mod('H'),
        Key::End => csi_mod('F'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::F(n @ 1..=4) => {
            let c = (b'P' + (n - 1)) as char;
            if mods.any() {
                format!("\x1b[1;{}{c}", mods.param()).into_bytes()
            } else {
                format!("\x1bO{c}").into_bytes()
            }
        }
        Key::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)]),
        Key::F(_) => return None,
        Key::Char(c) => {
            let mut v = if mods.ctrl { ctrl_char(c)? } else { c.to_string().into_bytes() };
            if mods.alt {
                v.insert(0, 0x1b);
            }
            v
        }
    };
    Some(AppAuthored(bytes))
}

/// Ctrl+<char> → C0 control code.
fn ctrl_char(c: char) -> Option<Vec<u8>> {
    let b = match c {
        'a'..='z' => c as u8 - b'a' + 1,
        'A'..='Z' => c as u8 - b'A' + 1,
        ' ' | '@' | '2' => 0,
        '[' | '3' => 0x1b,
        '\\' | '4' => 0x1c,
        ']' | '5' => 0x1d,
        '^' | '6' => 0x1e,
        '_' | '-' | '7' => 0x1f,
        '8' | '?' => 0x7f,
        _ => return None,
    };
    Some(vec![b])
}

/// Text typed by the user (keyboard text events, IME commits). Encoded as-is: these are the
/// user's own keystrokes.
pub fn typed(text: &str, alt: bool) -> AppAuthored {
    let mut v = Vec::with_capacity(text.len() + 1);
    if alt {
        v.push(0x1b);
    }
    v.extend_from_slice(text.as_bytes());
    AppAuthored(v)
}

/// Encodes untrusted text as a paste: newlines become CR (what a terminal sends for Enter), C0
/// controls other than tab/CR and every ESC / C1 control are removed (so pasted text cannot
/// carry its own escape sequences, including a fake end-of-paste), and the result is wrapped in
/// `ESC[200~ … ESC[201~` when the program enabled bracketed paste.
pub fn paste(text: Untrusted<'_>, mode: TermMode) -> Vec<u8> {
    let normalized = text.0.replace("\r\n", "\r").replace('\n', "\r");
    let cleaned: String = normalized
        .chars()
        .filter(|&c| c == '\t' || c == '\r' || !(c.is_control()))
        .collect();
    let mut out = Vec::with_capacity(cleaned.len() + 12);
    let bracketed = mode.contains(TermMode::BRACKETED_PASTE);
    if bracketed {
        out.extend_from_slice(b"\x1b[200~");
    }
    out.extend_from_slice(cleaned.as_bytes());
    if bracketed {
        out.extend_from_slice(b"\x1b[201~");
    }
    out
}

/// Focus in/out report, when the program asked for them (`CSI ? 1004 h`).
pub fn focus(focused: bool, mode: TermMode) -> Option<AppAuthored> {
    mode.contains(TermMode::FOCUS_IN_OUT)
        .then(|| AppAuthored(if focused { b"\x1b[I".to_vec() } else { b"\x1b[O".to_vec() }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press(MouseButton),
    Release(MouseButton),
    /// Pointer moved; `held` is the button held, if any.
    Motion { held: Option<MouseButton> },
    WheelUp,
    WheelDown,
}

/// Whether the program wants mouse events at all (otherwise the host does selection/scroll).
pub fn mouse_reporting(mode: TermMode) -> bool {
    mode.intersects(TermMode::MOUSE_MODE)
}

/// Encodes a mouse event at 0-based cell (`col`, `row`), or `None` when the current mode does
/// not report it (or the position cannot be expressed in the legacy encoding).
pub fn encode_mouse(action: MouseAction, col: u32, row: u32, mods: Mods, mode: TermMode) -> Option<AppAuthored> {
    if !mouse_reporting(mode) {
        return None;
    }
    let button_code = |b: MouseButton| match b {
        MouseButton::Left => 0u32,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match action {
        MouseAction::Press(b) => (button_code(b), false),
        MouseAction::Release(b) => (button_code(b), true),
        MouseAction::Motion { held } => {
            let reports = mode.contains(TermMode::MOUSE_MOTION)
                || (held.is_some() && mode.contains(TermMode::MOUSE_DRAG));
            if !reports {
                return None;
            }
            (32 + held.map_or(3, button_code), false)
        }
        MouseAction::WheelUp => (64, false),
        MouseAction::WheelDown => (65, false),
    };
    code += 4 * u32::from(mods.shift) + 8 * u32::from(mods.alt) + 16 * u32::from(mods.ctrl);
    if mode.contains(TermMode::SGR_MOUSE) {
        let suffix = if release { 'm' } else { 'M' };
        return Some(AppAuthored(format!("\x1b[<{code};{};{}{suffix}", col + 1, row + 1).into_bytes()));
    }
    // Legacy encodings: a release does not say which button.
    if release {
        code = 3 + (code & !3);
    }
    let mut out = b"\x1b[M".to_vec();
    out.push(u8::try_from(32 + code).ok()?);
    for v in [col, row] {
        let v = 32 + v + 1;
        if mode.contains(TermMode::UTF8_MOUSE) {
            let c = char::from_u32(v)?;
            if v > 2047 {
                return None;
            }
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        } else {
            out.push(u8::try_from(v).ok()?);
        }
    }
    Some(AppAuthored(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none() -> Mods {
        Mods::default()
    }

    #[test]
    fn arrows_follow_app_cursor_mode_and_modifiers() {
        assert_eq!(encode_key(Key::Up, none(), TermMode::empty()).unwrap().0, b"\x1b[A");
        assert_eq!(encode_key(Key::Up, none(), TermMode::APP_CURSOR).unwrap().0, b"\x1bOA");
        let ctrl = Mods { ctrl: true, ..none() };
        assert_eq!(encode_key(Key::Right, ctrl, TermMode::APP_CURSOR).unwrap().0, b"\x1b[1;5C");
        let shift = Mods { shift: true, ..none() };
        assert_eq!(encode_key(Key::Tab, shift, TermMode::empty()).unwrap().0, b"\x1b[Z");
        assert_eq!(encode_key(Key::F(5), none(), TermMode::empty()).unwrap().0, b"\x1b[15~");
        assert_eq!(encode_key(Key::F(1), none(), TermMode::empty()).unwrap().0, b"\x1bOP");
        assert_eq!(encode_key(Key::Delete, ctrl, TermMode::empty()).unwrap().0, b"\x1b[3;5~");
        assert!(encode_key(Key::F(20), none(), TermMode::empty()).is_none());
    }

    #[test]
    fn ctrl_and_alt_chars() {
        let ctrl = Mods { ctrl: true, ..none() };
        assert_eq!(encode_key(Key::Char('c'), ctrl, TermMode::empty()).unwrap().0, vec![3]);
        assert_eq!(encode_key(Key::Char('['), ctrl, TermMode::empty()).unwrap().0, vec![0x1b]);
        let alt = Mods { alt: true, ..none() };
        assert_eq!(encode_key(Key::Char('x'), alt, TermMode::empty()).unwrap().0, b"\x1bx");
        assert_eq!(encode_key(Key::Backspace, alt, TermMode::empty()).unwrap().0, b"\x1b\x7f");
    }

    #[test]
    fn paste_is_bracketed_and_cannot_break_out() {
        let evil = "line1\nrm -rf /\x1b[201~\x1b]7501;?\x07\u{9b}31m\r\ntail\tx";
        let out = paste(Untrusted(evil), TermMode::BRACKETED_PASTE);
        let s = String::from_utf8(out).unwrap();
        assert!(s.starts_with("\x1b[200~"));
        assert!(s.ends_with("\x1b[201~"));
        let inner = &s[6..s.len() - 6];
        assert!(!inner.contains('\x1b'), "escape survived: {inner:?}");
        assert!(!inner.contains('\u{9b}'));
        assert!(!inner.contains('\n'));
        assert!(!inner.contains('\x07'));
        assert_eq!(inner, "line1\rrm -rf /[201~]7501;?31m\rtail\tx");
        // Without bracketed paste mode: same cleaning, no markers.
        let plain = paste(Untrusted("a\nb"), TermMode::empty());
        assert_eq!(plain, b"a\rb");
    }

    #[test]
    fn focus_reports_only_when_requested() {
        assert!(focus(true, TermMode::empty()).is_none());
        assert_eq!(focus(true, TermMode::FOCUS_IN_OUT).unwrap().0, b"\x1b[I");
        assert_eq!(focus(false, TermMode::FOCUS_IN_OUT).unwrap().0, b"\x1b[O");
    }

    #[test]
    fn mouse_sgr_and_legacy() {
        let mode = TermMode::MOUSE_REPORT_CLICK | TermMode::SGR_MOUSE;
        let press = encode_mouse(MouseAction::Press(MouseButton::Left), 4, 9, none(), mode).unwrap();
        assert_eq!(press.0, b"\x1b[<0;5;10M");
        let rel = encode_mouse(MouseAction::Release(MouseButton::Left), 4, 9, none(), mode).unwrap();
        assert_eq!(rel.0, b"\x1b[<0;5;10m");
        let wheel = encode_mouse(MouseAction::WheelDown, 0, 0, none(), mode).unwrap();
        assert_eq!(wheel.0, b"\x1b[<65;1;1M");
        // Motion without MOUSE_MOTION/DRAG is not reported.
        assert!(encode_mouse(MouseAction::Motion { held: None }, 1, 1, none(), mode).is_none());
        let drag = TermMode::MOUSE_DRAG | TermMode::SGR_MOUSE;
        let m = encode_mouse(MouseAction::Motion { held: Some(MouseButton::Left) }, 1, 1, none(), drag).unwrap();
        assert_eq!(m.0, b"\x1b[<32;2;2M");
        // Legacy X10 encoding.
        let legacy = TermMode::MOUSE_REPORT_CLICK;
        let p = encode_mouse(MouseAction::Press(MouseButton::Right), 0, 0, none(), legacy).unwrap();
        assert_eq!(p.0, vec![0x1b, b'[', b'M', 32 + 2, 33, 33]);
        let r = encode_mouse(MouseAction::Release(MouseButton::Right), 0, 0, none(), legacy).unwrap();
        assert_eq!(r.0, vec![0x1b, b'[', b'M', 32 + 3, 33, 33]);
        // Out of range for legacy: dropped, not wrapped.
        assert!(encode_mouse(MouseAction::Press(MouseButton::Left), 300, 0, none(), legacy).is_none());
        // Not in a mouse mode at all.
        assert!(encode_mouse(MouseAction::Press(MouseButton::Left), 0, 0, none(), TermMode::empty()).is_none());
    }
}
