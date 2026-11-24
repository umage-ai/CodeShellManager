//! The keyboard map (docs/design/keyboard.md, section 5): one table of actions and their default
//! chords, the person's overrides, and the checks that keep the result usable.
//!
//! Pure: no egui. The UI maps egui keys to [`Key`] and back, and matches a [`Chord`] against the
//! input; the engine's `keymap_*` tools read and change the overrides kept in the settings. Every
//! label (the help list, the palette's hints, the menus) reads the resolved [`Keymap`], so there is
//! one source for them all.
//!
//! Damaged input never fails: an override that is not a list of chord strings, or a chord that
//! does not parse, is left out and reported ([`Keymap::skipped`]); an action id this build does not
//! know is kept in the settings (a newer build may own it) and reported ([`Keymap::unknown`]).

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// The person's overrides: action id → chord strings (`"Ctrl+Shift+T"`). An empty list removes the
/// action's chords. Missing ids keep their defaults.
pub type Overrides = BTreeMap<String, Vec<String>>;

/// A key a chord may name. Only keys a shortcut can sensibly use are here; Escape, the text keys and
/// the modifiers are not, so a binding can never take them from a dialog or a terminal by accident.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Key {
    /// `A` to `Z`.
    Letter(char),
    /// `0` to `9`.
    Digit(u8),
    /// `F1` to `F12`.
    F(u8),
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Tab,
    Space,
    Enter,
    Delete,
    Insert,
    Comma,
    Period,
    Slash,
    Minus,
    Equals,
}

impl Key {
    /// The spelling a chord is written with (`PgUp`, `F10`, `K`).
    pub fn name(self) -> String {
        match self {
            Key::Letter(c) => c.to_string(),
            Key::Digit(d) => d.to_string(),
            Key::F(n) => format!("F{n}"),
            Key::Left => "Left".into(),
            Key::Right => "Right".into(),
            Key::Up => "Up".into(),
            Key::Down => "Down".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::PageUp => "PgUp".into(),
            Key::PageDown => "PgDn".into(),
            Key::Tab => "Tab".into(),
            Key::Space => "Space".into(),
            Key::Enter => "Enter".into(),
            Key::Delete => "Del".into(),
            Key::Insert => "Ins".into(),
            Key::Comma => ",".into(),
            Key::Period => ".".into(),
            Key::Slash => "/".into(),
            Key::Minus => "-".into(),
            Key::Equals => "=".into(),
        }
    }

    /// Reads a key name, case-insensitively, with the usual aliases (`PageUp`, `ArrowLeft`, `Del`).
    pub fn parse(text: &str) -> Option<Key> {
        let t = text.trim().to_ascii_lowercase();
        let key = match t.as_str() {
            "left" | "arrowleft" => Key::Left,
            "right" | "arrowright" => Key::Right,
            "up" | "arrowup" => Key::Up,
            "down" | "arrowdown" => Key::Down,
            "home" => Key::Home,
            "end" => Key::End,
            "pgup" | "pageup" => Key::PageUp,
            "pgdn" | "pgdown" | "pagedown" => Key::PageDown,
            "tab" => Key::Tab,
            "space" => Key::Space,
            "enter" | "return" => Key::Enter,
            "del" | "delete" => Key::Delete,
            "ins" | "insert" => Key::Insert,
            "," | "comma" => Key::Comma,
            "." | "period" => Key::Period,
            "/" | "slash" => Key::Slash,
            "-" | "minus" => Key::Minus,
            "=" | "equals" => Key::Equals,
            _ => return parse_char_or_function_key(&t),
        };
        Some(key)
    }
}

fn parse_char_or_function_key(t: &str) -> Option<Key> {
    let mut chars = t.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return match c {
            'a'..='z' => Some(Key::Letter(c.to_ascii_uppercase())),
            '0'..='9' => c.to_digit(10).and_then(|d| u8::try_from(d).ok()).map(Key::Digit),
            _ => None,
        };
    }
    let n: u8 = t.strip_prefix('f')?.parse().ok()?;
    (1..=12).contains(&n).then_some(Key::F(n))
}

/// A key with its modifiers: `Ctrl+Shift+T`, `F10`, `Ctrl+Alt+PgUp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Chord {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub key: Key,
}

/// Why a chord string was refused. The message names the part that was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChordError(pub String);

impl fmt::Display for ChordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Chord {
    /// Reads `Ctrl+Alt+PgUp` (modifiers in any order and case, `+` between the parts, one key last).
    pub fn parse(text: &str) -> Result<Chord, ChordError> {
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (key_part, mods) = parts.split_last().ok_or_else(|| ChordError("the chord is empty".into()))?;
        let (mut ctrl, mut alt, mut shift) = (false, false, false);
        for m in mods {
            let slot = match m.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => &mut ctrl,
                "alt" => &mut alt,
                "shift" => &mut shift,
                _ => return Err(ChordError(format!("`{m}` is not a modifier (Ctrl, Alt or Shift)"))),
            };
            if *slot {
                return Err(ChordError(format!("`{m}` is repeated in `{text}`")));
            }
            *slot = true;
        }
        let key = Key::parse(key_part).ok_or_else(|| ChordError(format!("there is no key named `{key_part}`")))?;
        Ok(Chord { ctrl, alt, shift, key })
    }

    /// Whether a chord may be bound. Text keys need Ctrl or Alt, so typing is never taken
    /// (Shift alone with a letter is a capital, and Alt with a letter is the terminal's meta
    /// prefix). Function keys may stand alone or carry modifiers.
    pub fn is_bindable(&self) -> bool {
        matches!(self.key, Key::F(_)) || self.ctrl
    }
}

impl fmt::Display for Chord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("Ctrl+")?;
        }
        if self.alt {
            f.write_str("Alt+")?;
        }
        if self.shift {
            f.write_str("Shift+")?;
        }
        f.write_str(&self.key.name())
    }
}

/// One action of the table: its id (a tool id, or `ui.<name>` for a UI-only action), its name for
/// the settings and the help, and its default chords.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Binding {
    pub id: &'static str,
    pub name: &'static str,
    pub defaults: &'static [&'static str],
}

/// The default bindings, in dispatch order: when two would share a chord (an override that the
/// table cannot prevent), the earlier one's claim is the one kept.
pub const BINDINGS: &[Binding] = &[
    Binding { id: "ui.main_menu", name: "Main actions menu", defaults: &["F10"] },
    Binding { id: "ui.menu_focused", name: "Menu of the focused item", defaults: &["Shift+F10"] },
    Binding { id: "session_new", name: "New session", defaults: &["Ctrl+T"] },
    Binding { id: "session_duplicate", name: "Duplicate the active session", defaults: &["Ctrl+Alt+T"] },
    Binding { id: "session_reopen_closed", name: "Reopen the last closed session", defaults: &["Ctrl+Shift+T"] },
    Binding { id: "session_recent", name: "Recent sessions", defaults: &["Ctrl+Shift+E"] },
    Binding { id: "inbox_next", name: "Next session that needs you", defaults: &["Ctrl+J"] },
    Binding { id: "inspector_show", name: "Show or hide the inspector", defaults: &["Ctrl+I"] },
    Binding { id: "ui.inspector_notes", name: "Notes for the active session", defaults: &["Ctrl+Shift+N"] },
    Binding { id: "session_restart", name: "Restart the active session", defaults: &["Ctrl+Shift+R"] },
    Binding { id: "session_close", name: "Close the active session", defaults: &["Ctrl+W"] },
    Binding { id: "claude_reply_get", name: "Copy Claude's last reply", defaults: &["Ctrl+Shift+C"] },
    Binding { id: "ui.pane_menu", name: "The active pane's menu", defaults: &["Ctrl+."] },
    Binding { id: "ui.palette", name: "Command palette", defaults: &["Ctrl+K"] },
    Binding { id: "search_output", name: "Search all output and notes", defaults: &["Ctrl+F"] },
    Binding { id: "run_start", name: "Run the default run command", defaults: &["F5"] },
    Binding { id: "run_stop", name: "Stop the run command", defaults: &["Shift+F5"] },
    Binding { id: "session_rename", name: "Rename the active session", defaults: &["F2"] },
    Binding { id: "ui.region_next", name: "Next keyboard region", defaults: &["F6"] },
    Binding { id: "ui.region_prev", name: "Previous keyboard region", defaults: &["Shift+F6"] },
    Binding { id: "ui.pane_left", name: "Focus the pane to the left", defaults: &["Ctrl+Alt+Left"] },
    Binding { id: "ui.pane_right", name: "Focus the pane to the right", defaults: &["Ctrl+Alt+Right"] },
    Binding { id: "ui.pane_up", name: "Focus the pane above", defaults: &["Ctrl+Alt+Up"] },
    Binding { id: "ui.pane_down", name: "Focus the pane below", defaults: &["Ctrl+Alt+Down"] },
    Binding { id: "ui.session_next", name: "Next session in this group", defaults: &["Ctrl+Tab"] },
    Binding { id: "ui.session_prev", name: "Previous session", defaults: &["Ctrl+Shift+Tab"] },
    Binding { id: "ui.rail_tab_1", name: "Rail tab 1 (All)", defaults: &["Ctrl+1"] },
    Binding { id: "ui.rail_tab_2", name: "Rail tab 2", defaults: &["Ctrl+2"] },
    Binding { id: "ui.rail_tab_3", name: "Rail tab 3", defaults: &["Ctrl+3"] },
    Binding { id: "ui.rail_tab_4", name: "Rail tab 4", defaults: &["Ctrl+4"] },
    Binding { id: "ui.rail_tab_5", name: "Rail tab 5", defaults: &["Ctrl+5"] },
    Binding { id: "ui.rail_tab_6", name: "Rail tab 6", defaults: &["Ctrl+6"] },
    Binding { id: "ui.rail_tab_7", name: "Rail tab 7", defaults: &["Ctrl+7"] },
    Binding { id: "ui.rail_tab_8", name: "Rail tab 8", defaults: &["Ctrl+8"] },
    Binding { id: "ui.rail_tab_9", name: "Rail tab 9", defaults: &["Ctrl+9"] },
    Binding { id: "screen_tab_prev", name: "Previous add-on screen tab", defaults: &["Ctrl+PgUp"] },
    Binding { id: "screen_tab_next", name: "Next add-on screen tab", defaults: &["Ctrl+PgDn"] },
    Binding { id: "rail_tab_prev", name: "Previous rail tab", defaults: &["Ctrl+Alt+PgUp"] },
    Binding { id: "rail_tab_next", name: "Next rail tab", defaults: &["Ctrl+Alt+PgDn"] },
    Binding { id: "ui.settings", name: "Settings", defaults: &["Ctrl+,"] },
    Binding { id: "ui.zoom_in", name: "Larger interface", defaults: &["Ctrl+="] },
    Binding { id: "ui.zoom_out", name: "Smaller interface", defaults: &["Ctrl+-"] },
    Binding { id: "ui.zoom_reset", name: "Interface at 100%", defaults: &["Ctrl+0"] },
    Binding { id: "ui.help", name: "Keyboard shortcuts", defaults: &["F1", "Ctrl+/"] },
];

/// Parses a chord list, keeping the first of each chord. Returns the first problem, if any.
fn parse_chords(texts: &[String]) -> Result<Vec<Chord>, (String, ChordError)> {
    let mut out: Vec<Chord> = Vec::new();
    for text in texts {
        let chord = Chord::parse(text).map_err(|e| (text.clone(), e))?;
        if !chord.is_bindable() {
            return Err((text.clone(), ChordError(format!("`{text}` needs Ctrl or Alt, or be a function key"))));
        }
        if !out.contains(&chord) {
            out.push(chord);
        }
    }
    Ok(out)
}

fn default_chords(b: &Binding) -> Vec<Chord> {
    b.defaults.iter().filter_map(|t| Chord::parse(t).ok()).collect()
}

/// Two actions claim one chord. The one named `dropped` keeps its other chords and loses this one;
/// `kept` is the claim that holds (an override before a default, else table order).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub chord: Chord,
    pub kept: &'static str,
    pub dropped: &'static str,
}

/// An override that could not be used, and why. The setting itself is kept as written.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub id: String,
    pub chord: String,
    pub reason: String,
}

/// One action with its chords as resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    id: &'static str,
    name: &'static str,
    default: Vec<Chord>,
    chords: Vec<Chord>,
    customised: bool,
}

impl Entry {
    pub fn id(&self) -> &'static str {
        self.id
    }
    pub fn name(&self) -> &'static str {
        self.name
    }
    /// The chords in force, after overrides and conflicts.
    pub fn chords(&self) -> &[Chord] {
        &self.chords
    }
    pub fn default_chords(&self) -> &[Chord] {
        &self.default
    }
    /// Whether the person set this action's chords (even to the same as the default).
    pub fn customised(&self) -> bool {
        self.customised
    }
}

/// The resolved keyboard map: [`BINDINGS`] with the overrides applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    entries: Vec<Entry>,
    conflicts: Vec<Conflict>,
    skipped: Vec<Skipped>,
    unknown: Vec<String>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::new(&Overrides::new())
    }
}

impl Keymap {
    /// The defaults, with `overrides` applied. Never fails: see the module notes.
    pub fn new(overrides: &Overrides) -> Keymap {
        let mut skipped = Vec::new();
        let mut unknown = Vec::new();
        let mut entries: Vec<Entry> = BINDINGS
            .iter()
            .map(|b| Entry { id: b.id, name: b.name, default: default_chords(b), chords: default_chords(b), customised: false })
            .collect();
        for (id, texts) in overrides {
            let Some(entry) = entries.iter_mut().find(|e| e.id == id.as_str()) else {
                unknown.push(id.clone());
                continue;
            };
            let mut ok = Vec::new();
            for text in texts {
                match Chord::parse(text) {
                    Ok(c) if c.is_bindable() => {
                        if !ok.contains(&c) {
                            ok.push(c);
                        }
                    }
                    Ok(_) => skipped.push(Skipped { id: id.clone(), chord: text.clone(), reason: "needs Ctrl or Alt, or be a function key".into() }),
                    Err(e) => skipped.push(Skipped { id: id.clone(), chord: text.clone(), reason: e.0 }),
                }
            }
            entry.chords = ok;
            entry.customised = true;
        }
        // Claims in order: the person's overrides first, then the defaults in table order.
        let mut order: Vec<usize> = (0..entries.len()).collect();
        order.sort_by_key(|&i| entries.get(i).is_none_or(|e| !e.customised));
        let mut claimed: BTreeMap<Chord, &'static str> = BTreeMap::new();
        let mut conflicts = Vec::new();
        for i in order {
            let Some(entry) = entries.get_mut(i) else { continue };
            let id = entry.id;
            let mut kept = Vec::new();
            for chord in entry.chords.clone() {
                match claimed.get(&chord) {
                    Some(&owner) => conflicts.push(Conflict { chord, kept: owner, dropped: id }),
                    None => {
                        claimed.insert(chord, id);
                        kept.push(chord);
                    }
                }
            }
            entry.chords = kept;
        }
        Keymap { entries, conflicts, skipped, unknown }
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    /// The chords in force for an action (empty for an unknown id or a removed binding).
    pub fn chords(&self, id: &str) -> &[Chord] {
        self.entries.iter().find(|e| e.id == id).map_or(&[], |e| e.chords.as_slice())
    }

    /// The action that owns a chord, if any.
    pub fn action_for(&self, chord: &Chord) -> Option<&'static str> {
        self.entries.iter().find(|e| e.chords.contains(chord)).map(|e| e.id)
    }

    /// The first chord of an action as a label (`Ctrl+T`), for a hint or a menu row.
    pub fn label(&self, id: &str) -> Option<String> {
        self.chords(id).first().map(ToString::to_string)
    }

    /// Every chord of an action as a label, separated by " or ", for the help list.
    pub fn label_all(&self, id: &str) -> Option<String> {
        let all: Vec<String> = self.chords(id).iter().map(ToString::to_string).collect();
        (!all.is_empty()).then(|| all.join(" or "))
    }

    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
    }

    pub fn skipped(&self) -> &[Skipped] {
        &self.skipped
    }

    pub fn unknown(&self) -> &[String] {
        &self.unknown
    }
}

/// Why a chord change was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetError {
    /// No action has this id.
    UnknownAction(String),
    /// A chord string that does not parse, or cannot be bound.
    BadChord(String),
    /// The chord already belongs to another action; the message names it.
    InUse { chord: Chord, owner: &'static str },
}

impl fmt::Display for SetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SetError::UnknownAction(id) => write!(f, "there is no action named `{id}`"),
            SetError::BadChord(why) => f.write_str(why),
            SetError::InUse { chord, owner } => write!(f, "{chord} is already used by {owner}; clear that one first"),
        }
    }
}

/// The overrides after setting `chords` for `id`. Refused when a chord belongs to another action
/// (nothing is stolen silently), when a chord cannot be read or bound, or when the id is unknown.
/// Setting an action back to its defaults removes its override.
pub fn set(overrides: &Overrides, id: &str, chords: &[String]) -> Result<Overrides, SetError> {
    let before = Keymap::new(overrides);
    let entry = before.entries().iter().find(|e| e.id == id).ok_or_else(|| SetError::UnknownAction(id.to_owned()))?;
    let parsed = parse_chords(chords).map_err(|(text, e)| SetError::BadChord(format!("`{text}`: {}", e.0)))?;
    for chord in &parsed {
        let other = before.entries().iter().find(|e| e.id != entry.id && e.chords.contains(chord));
        if let Some(other) = other {
            return Err(SetError::InUse { chord: *chord, owner: other.name });
        }
    }
    let mut out = overrides.clone();
    if parsed.len() == entry.default.len() && parsed.iter().all(|c| entry.default.contains(c)) {
        out.remove(id);
    } else {
        out.insert(id.to_owned(), parsed.iter().map(ToString::to_string).collect());
    }
    Ok(out)
}

/// The overrides with one action (`Some`) or every action (`None`) back at its defaults.
pub fn reset(overrides: &Overrides, id: Option<&str>) -> Overrides {
    match id {
        Some(id) => {
            let mut out = overrides.clone();
            out.remove(id);
            out
        }
        None => Overrides::new(),
    }
}

/// Reads the `Keymap` setting leniently: anything that is not an object of lists of strings is
/// dropped, entry by entry, so a damaged file loses only the damaged part.
pub fn overrides_from_value(v: &Value) -> Overrides {
    let Value::Object(map) = v else { return Overrides::new() };
    map.iter()
        .filter_map(|(id, list)| {
            let Value::Array(items) = list else { return None };
            let texts: Vec<String> = items.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect();
            Some((id.clone(), texts))
        })
        .collect()
}

/// Serde hook for the settings field: [`overrides_from_value`], never an error.
pub fn deserialize_overrides<'de, D: Deserializer<'de>>(d: D) -> Result<Overrides, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(overrides_from_value(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chord(text: &str) -> Chord {
        Chord::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"))
    }

    fn ov(pairs: &[(&str, &[&str])]) -> Overrides {
        pairs.iter().map(|(id, list)| ((*id).to_owned(), list.iter().map(|s| (*s).to_owned()).collect())).collect()
    }

    #[test]
    fn parses_and_formats_one_canonical_spelling() {
        let c = chord("ctrl+alt+pageup");
        assert_eq!(c, Chord { ctrl: true, alt: true, shift: false, key: Key::PageUp });
        assert_eq!(c.to_string(), "Ctrl+Alt+PgUp");
        assert_eq!(chord("Shift+Ctrl+t").to_string(), "Ctrl+Shift+T");
        assert_eq!(chord("Ctrl+,").to_string(), "Ctrl+,");
        assert_eq!(chord("F10").to_string(), "F10");
        assert_eq!(chord("Ctrl+ArrowLeft").key, Key::Left);
        assert_eq!(chord("Ctrl+Del").key, Key::Delete);
        // Every spelling a chord can have parses back to itself.
        for text in ["Ctrl+Alt+Left", "Shift+F6", "Ctrl+Shift+Tab", "Ctrl+=", "Ctrl+9", "Ctrl+Space", "Alt+F4"] {
            let c = chord(text);
            assert_eq!(chord(&c.to_string()), c, "{text}");
        }
    }

    #[test]
    fn refuses_damaged_chord_strings() {
        for bad in ["", "Ctrl+", "+", "Ctrl+Ctrl+T", "Hyper+T", "Ctrl+Foo", "Ctrl+F13", "Ctrl+Esc", "Ctrl+Shift+Shift+T"] {
            assert!(Chord::parse(bad).is_err(), "{bad:?} should not parse");
        }
    }

    #[test]
    fn text_keys_need_ctrl_or_are_refused() {
        assert!(!chord("T").is_bindable(), "a plain letter types");
        assert!(!chord("Shift+T").is_bindable(), "Shift+letter types a capital");
        assert!(!chord("Alt+T").is_bindable(), "Alt+letter is the terminal's meta prefix");
        assert!(!chord("Shift+Tab").is_bindable(), "Tab moves within a dialog");
        assert!(!chord("Left").is_bindable());
        assert!(chord("Ctrl+T").is_bindable());
        assert!(chord("Ctrl+Alt+Left").is_bindable());
        assert!(chord("F2").is_bindable());
        assert!(chord("Shift+F10").is_bindable());
    }

    #[test]
    fn defaults_parse_bind_and_do_not_conflict() {
        let mut ids = std::collections::BTreeSet::new();
        for b in BINDINGS {
            assert!(ids.insert(b.id), "duplicate id {}", b.id);
            assert!(!b.defaults.is_empty(), "{} has no default", b.id);
            for text in b.defaults {
                assert!(Chord::parse(text).is_ok_and(|c| c.is_bindable()), "{}: default {text}", b.id);
            }
        }
        let map = Keymap::default();
        assert!(map.conflicts().is_empty(), "{:?}", map.conflicts());
        assert!(map.skipped().is_empty());
        assert_eq!(map.label("session_new").as_deref(), Some("Ctrl+T"));
        assert_eq!(map.label("ui.help").as_deref(), Some("F1"));
        assert_eq!(map.label_all("ui.help").as_deref(), Some("F1 or Ctrl+/"));
        assert_eq!(map.action_for(&chord("Ctrl+Shift+T")), Some("session_reopen_closed"));
        assert_eq!(map.label("no.such.action"), None);
    }

    #[test]
    fn overrides_replace_defaults_and_empty_removes() {
        let map = Keymap::new(&ov(&[("ui.palette", &["Ctrl+P"]), ("session_new", &[])]));
        assert_eq!(map.label("ui.palette").as_deref(), Some("Ctrl+P"));
        assert_eq!(map.action_for(&chord("Ctrl+K")), None, "the old chord is free");
        assert!(map.chords("session_new").is_empty());
        assert!(map.entries().iter().find(|e| e.id() == "session_new").is_some_and(Entry::customised));
        assert_eq!(map.entries().iter().find(|e| e.id() == "session_new").map(|e| e.default_chords().len()), Some(1));
    }

    #[test]
    fn a_conflict_keeps_the_override_and_drops_the_default() {
        // Ctrl+J is inbox_next by default; the override takes it for the palette.
        let map = Keymap::new(&ov(&[("ui.palette", &["Ctrl+J"])]));
        assert_eq!(map.action_for(&chord("Ctrl+J")), Some("ui.palette"));
        assert!(map.chords("inbox_next").is_empty());
        assert_eq!(map.conflicts(), [Conflict { chord: chord("Ctrl+J"), kept: "ui.palette", dropped: "inbox_next" }]);
    }

    #[test]
    fn damaged_settings_lose_only_the_damaged_part() {
        let raw = serde_json::json!({
            "ui.palette": ["Ctrl+P", 7, "Ctrl+Hyper+X", "T"],
            "session_new": "Ctrl+N",
            "ui.help": ["F1", null],
            "from.a.newer.build": ["Ctrl+G"],
        });
        let o = overrides_from_value(&raw);
        assert!(!o.contains_key("session_new"), "a string is not a list");
        let map = Keymap::new(&o);
        assert_eq!(map.chords("ui.palette").len(), 1, "only the valid chord is kept");
        assert_eq!(map.label("ui.palette").as_deref(), Some("Ctrl+P"));
        assert_eq!(map.label("ui.help").as_deref(), Some("F1"));
        assert_eq!(map.label("session_new").as_deref(), Some("Ctrl+T"), "a damaged entry keeps the default");
        assert_eq!(map.unknown(), ["from.a.newer.build".to_owned()]);
        assert_eq!(map.skipped().len(), 2, "{:?}", map.skipped());
        assert!(overrides_from_value(&Value::Null).is_empty());
        assert!(overrides_from_value(&serde_json::json!([1, 2])).is_empty());
    }

    #[test]
    fn set_refuses_a_chord_in_use_and_names_the_owner() {
        let err = set(&Overrides::new(), "ui.palette", &["Ctrl+T".into()]).err();
        assert_eq!(err, Some(SetError::InUse { chord: chord("Ctrl+T"), owner: "New session" }));
        assert!(err.is_some_and(|e| e.to_string().contains("already used by New session")));
    }

    #[test]
    fn set_stores_a_free_chord_and_defaults_drop_the_override() {
        let o = set(&Overrides::new(), "ui.palette", &["Ctrl+P".into(), "Ctrl+P".into()]).unwrap_or_default();
        assert_eq!(o, ov(&[("ui.palette", &["Ctrl+P"])]));
        // Going back to the default removes the entry, so state.json stays clean.
        let back = set(&o, "ui.palette", &["Ctrl+K".into()]).unwrap_or_default();
        assert!(back.is_empty());
        // Setting a chord the action already has is fine.
        assert!(set(&Overrides::new(), "session_new", &["Ctrl+T".into()]).is_ok());
    }

    #[test]
    fn set_refuses_unknown_actions_and_unbindable_chords() {
        assert!(matches!(set(&Overrides::new(), "nope", &[]), Err(SetError::UnknownAction(_))));
        assert!(matches!(set(&Overrides::new(), "ui.palette", &["T".into()]), Err(SetError::BadChord(_))));
        assert!(matches!(set(&Overrides::new(), "ui.palette", &["Ctrl+Nope".into()]), Err(SetError::BadChord(_))));
    }

    #[test]
    fn reset_restores_one_or_all() {
        let o = ov(&[("ui.palette", &["Ctrl+P"]), ("ui.help", &["F3"])]);
        assert_eq!(reset(&o, Some("ui.palette")), ov(&[("ui.help", &["F3"])]));
        assert!(reset(&o, None).is_empty());
        assert_eq!(Keymap::new(&reset(&o, None)), Keymap::default());
    }
}
