//! The settings of CodeShellManager 0.x (the WPF app) mapped onto version 2's (`state_import_wpf`,
//! on the first start and by hand). Pure: the caller reads the file and hands over its
//! `Settings` object as JSON.
//!
//! The WPF `state.json` is somebody else's file, so it is read **value by value**: a missing key
//! keeps ours, a key of the wrong type or out of range is skipped and reported (never an error,
//! never a panic), and keys this version does not know are reported and ignored. Only settings
//! that mean the same thing here and that version 2 acts on are taken; the table in
//! `docs/release.md` ("0.x settings") lists every 0.x setting and what happens to it.
//!
//! The 0.x **Open with** list is not applied here: it is a list of programs to run, so it is
//! returned for the caller to sanitise and offer for consent (`open_with_pending`).

use serde_json::{Map, Value};

use crate::models::{AppSettings, GroupDisplayMode, OpenWithEntry, ProgramStatusProbeMode};

/// 0.x's `TerminalFontSize` default (CSS pixels). A size left at it is not carried: version 2
/// has its own default for its own font.
pub const WPF_DEFAULT_FONT_SIZE: i64 = 14;
/// The terminal font sizes taken (version 2's points are logical pixels, as CSS pixels are).
pub const FONT_POINTS: std::ops::RangeInclusive<i64> = 6..=32;
/// The Claude launch stagger taken, in ms (the Settings screen's range).
pub const STAGGER_MS: std::ops::RangeInclusive<i64> = 0..=10_000;
/// At most this many launch commands and Open with entries are read.
pub const MAX_LIST: usize = 64;
/// A command longer than this is not a command.
pub const MAX_COMMAND_CHARS: usize = 2000;

/// What the 0.x settings give, applied to ours.
#[derive(Debug, Clone, PartialEq)]
pub struct WpfSettingsImport {
    /// Our settings with the 0.x values taken.
    pub settings: AppSettings,
    /// The version-2 keys (as in `state.json`) whose value changed.
    pub changed: Vec<&'static str>,
    /// What was skipped and why, one line each, for the log.
    pub skipped: Vec<String>,
    /// The 0.x Open with list, read leniently but NOT checked: sanitise it, then ask.
    pub open_with: Vec<OpenWithEntry>,
    /// 0.x's `OpenWithSeeded`: false means its list was never set up, so there is nothing to offer.
    pub open_with_seeded: bool,
}

/// 0.x settings that exist but are deliberately not carried (see docs/release.md); not reported
/// as unknown.
const NOT_CARRIED: [&str; 24] = [
    "AnthropicApiKey",
    "AutoFocusTerminalOnSelect",
    "FilterGridByActiveGroup",
    "PerGroupLayout",
    "ShowGroupsTab",
    "ShowWorktreeClusters",
    "UngroupedSectionExpanded",
    "LegacyDefaultGroupCleared",
    "SearchCollapseAfterNavigate",
    "MaxSearchResults",
    "ShowTerminalStatusDot",
    "SidebarActionIconsMode",
    "IndexTerminalOutput",
    "OutputRetentionDays",
    "DebugTerminalTrace",
    "UseBundledConPty",
    "TerminalFontFamily",
    "TerminalFontLigatures",
    "TerminalFontWeight",
    "TerminalLetterSpacing",
    "TerminalLineHeight",
    // Read into `open_with` / `open_with_seeded`, applied only after consent.
    "OpenWith",
    "OpenWithSeeded",
    // A dev-build key the WPF app itself ignores.
    "AnswerProgramStatusProbe",
];

const TAKEN: [&str; 14] = [
    "Theme",
    "DefaultCommand",
    "DefaultWorkingFolder",
    "LaunchCommands",
    "AutoRestoreSessions",
    "AutoResumeClaude",
    "ClaudeLaunchStaggerMs",
    "WarnOnCloseWhenBusy",
    "ShowToastNotifications",
    "ShowNotificationSound",
    "ShowGitBranch",
    "ImportWindowsTerminalProfiles",
    "GroupDisplayMode",
    "ProgramStatusProbe",
];

/// `TerminalFontSize` is read too; it becomes `TerminalFontPoints`.
const FONT_SIZE: &str = "TerminalFontSize";

fn shown(v: &Value) -> String {
    let s = v.to_string();
    let mut out: String = s.chars().filter(|c| !c.is_control()).take(60).collect();
    if s.chars().count() > 60 {
        out.push('…');
    }
    out
}

fn clean_line(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty() && !t.chars().any(char::is_control) && t.chars().count() <= MAX_COMMAND_CHARS).then(|| t.to_owned())
}

struct Reader<'a> {
    m: &'a Map<String, Value>,
    skipped: Vec<String>,
}

impl<'a> Reader<'a> {
    fn skip(&mut self, key: &str, v: &Value, why: &str) {
        self.skipped.push(format!("{key}: {why} (was {})", shown(v)));
    }

    fn bool(&mut self, key: &str) -> Option<bool> {
        let m: &'a Map<String, Value> = self.m;
        let v = m.get(key)?;
        match v {
            Value::Bool(b) => Some(*b),
            Value::Null => None,
            _ => {
                self.skip(key, v, "not true or false");
                None
            }
        }
    }

    fn int(&mut self, key: &str, range: &std::ops::RangeInclusive<i64>) -> Option<i64> {
        let m: &'a Map<String, Value> = self.m;
        let v = m.get(key)?;
        let n = match v {
            Value::Null => return None,
            Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0).map(|f| f as i64)),
            _ => None,
        };
        match n {
            Some(n) if range.contains(&n) => Some(n),
            Some(_) => {
                self.skip(key, v, &format!("outside {}..={}", range.start(), range.end()));
                None
            }
            None => {
                self.skip(key, v, "not a whole number");
                None
            }
        }
    }

    fn str(&mut self, key: &str) -> Option<&'a str> {
        let m: &'a Map<String, Value> = self.m;
        let v = m.get(key)?;
        match v {
            Value::String(s) => Some(s.as_str()),
            Value::Null => None,
            _ => {
                self.skip(key, v, "not text");
                None
            }
        }
    }
}

/// Applies the 0.x `Settings` object `raw` to `ours`. Never fails.
pub fn map(raw: &Value, ours: &AppSettings) -> WpfSettingsImport {
    let mut out =
        WpfSettingsImport { settings: ours.clone(), changed: Vec::new(), skipped: Vec::new(), open_with: Vec::new(), open_with_seeded: false };
    let m = match raw {
        Value::Object(m) => m,
        Value::Null => return out,
        other => {
            out.skipped.push(format!("Settings: not an object (was {})", shown(other)));
            return out;
        }
    };
    let mut r = Reader { m, skipped: Vec::new() };
    let s = &mut out.settings;

    if let Some(t) = r.str("Theme") {
        match t.trim().to_ascii_lowercase().as_str() {
            v @ ("dark" | "light" | "system") => s.theme = v.to_owned(),
            _ => r.skip("Theme", &Value::String(t.to_owned()), "not dark, light or system"),
        }
    }
    if let Some(c) = r.str("DefaultCommand") {
        match clean_line(c) {
            Some(c) => s.default_command = c,
            None => r.skip("DefaultCommand", &Value::String(c.to_owned()), "blank, too long or has control characters"),
        }
    }
    if let Some(f) = r.str("DefaultWorkingFolder") {
        if f.chars().any(char::is_control) || f.chars().count() > MAX_COMMAND_CHARS {
            r.skip("DefaultWorkingFolder", &Value::String(f.to_owned()), "has control characters or is too long");
        } else {
            s.default_working_folder = f.trim().to_owned();
        }
    }
    if let Some(v) = m.get("LaunchCommands") {
        match v {
            Value::Array(items) => {
                let mut list: Vec<String> = Vec::new();
                let mut bad = 0;
                for item in items.iter().take(MAX_LIST) {
                    match item.as_str().and_then(clean_line) {
                        Some(c) if !list.contains(&c) => list.push(c),
                        Some(_) => {}
                        None => bad += 1,
                    }
                }
                if bad > 0 {
                    r.skipped.push(format!("LaunchCommands: {bad} entries were not usable commands"));
                }
                if items.len() > MAX_LIST {
                    r.skipped.push(format!("LaunchCommands: only the first {MAX_LIST} were read"));
                }
                s.launch_commands = list;
            }
            Value::Null => {}
            _ => r.skip("LaunchCommands", v, "not a list"),
        }
    }
    let bools: [(&str, &mut bool); 8] = [
        ("AutoRestoreSessions", &mut s.auto_restore_sessions),
        ("AutoResumeClaude", &mut s.auto_resume_claude),
        ("WarnOnCloseWhenBusy", &mut s.warn_on_close_when_busy),
        ("ShowToastNotifications", &mut s.show_toast_notifications),
        ("ShowNotificationSound", &mut s.show_notification_sound),
        ("ShowGitBranch", &mut s.show_git_branch),
        ("ImportWindowsTerminalProfiles", &mut s.import_windows_terminal_profiles),
        ("OpenWithSeeded", &mut out.open_with_seeded),
    ];
    for (key, slot) in bools {
        if let Some(b) = r.bool(key) {
            *slot = b;
        }
    }
    if let Some(n) = r.int("ClaudeLaunchStaggerMs", &STAGGER_MS) {
        s.claude_launch_stagger_ms = i32::try_from(n).unwrap_or(s.claude_launch_stagger_ms);
    }
    if let Some(n) = r.int(FONT_SIZE, &(1..=i64::from(i32::MAX))) {
        if n == WPF_DEFAULT_FONT_SIZE {
            // Left at 0.x's default: version 2 keeps its own.
        } else if FONT_POINTS.contains(&n) {
            s.terminal_font_points = i32::try_from(n).unwrap_or(s.terminal_font_points);
        } else if let Some(v) = m.get(FONT_SIZE) {
            r.skip(FONT_SIZE, v, &format!("outside {}..={}", FONT_POINTS.start(), FONT_POINTS.end()));
        }
    }
    if let Some(n) = r.int("GroupDisplayMode", &(0..=2)) {
        s.group_display_mode = match n {
            0 => GroupDisplayMode::None,
            2 => GroupDisplayMode::InlineHeaders,
            _ => GroupDisplayMode::FilterStrip,
        };
    }
    if let Some(n) = r.int("ProgramStatusProbe", &(0..=2)) {
        s.program_status_probe = match n {
            1 => ProgramStatusProbeMode::On,
            2 => ProgramStatusProbeMode::Off,
            _ => ProgramStatusProbeMode::Auto,
        };
    }
    if let Some(v) = m.get("OpenWith") {
        match v {
            Value::Array(items) => {
                let mut bad = 0;
                for item in items.iter().take(MAX_LIST) {
                    match serde_json::from_value::<OpenWithEntry>(item.clone()) {
                        Ok(e) => out.open_with.push(e),
                        Err(_) => bad += 1,
                    }
                }
                if bad > 0 {
                    r.skipped.push(format!("OpenWith: {bad} entries could not be read"));
                }
            }
            Value::Null => {}
            _ => r.skip("OpenWith", v, "not a list"),
        }
    }
    for key in m.keys() {
        let k = key.as_str();
        if !TAKEN.contains(&k) && !NOT_CARRIED.contains(&k) && k != FONT_SIZE {
            let shown_key: String = k.chars().filter(|c| !c.is_control()).take(60).collect();
            r.skipped.push(format!("{shown_key}: not a 0.x setting this version knows; ignored"));
        }
    }
    out.skipped = r.skipped;
    out.changed = changed_keys(ours, &out.settings);
    out
}

/// The `state.json` keys that differ between two settings (of the ones [`map`] can change).
fn changed_keys(a: &AppSettings, b: &AppSettings) -> Vec<&'static str> {
    let pairs: [(&'static str, bool); 15] = [
        ("Theme", a.theme != b.theme),
        ("DefaultCommand", a.default_command != b.default_command),
        ("DefaultWorkingFolder", a.default_working_folder != b.default_working_folder),
        ("LaunchCommands", a.launch_commands != b.launch_commands),
        ("AutoRestoreSessions", a.auto_restore_sessions != b.auto_restore_sessions),
        ("AutoResumeClaude", a.auto_resume_claude != b.auto_resume_claude),
        ("ClaudeLaunchStaggerMs", a.claude_launch_stagger_ms != b.claude_launch_stagger_ms),
        ("WarnOnCloseWhenBusy", a.warn_on_close_when_busy != b.warn_on_close_when_busy),
        ("ShowToastNotifications", a.show_toast_notifications != b.show_toast_notifications),
        ("ShowNotificationSound", a.show_notification_sound != b.show_notification_sound),
        ("ShowGitBranch", a.show_git_branch != b.show_git_branch),
        ("ImportWindowsTerminalProfiles", a.import_windows_terminal_profiles != b.import_windows_terminal_profiles),
        ("TerminalFontPoints", a.terminal_font_points != b.terminal_font_points),
        ("GroupDisplayMode", a.group_display_mode != b.group_display_mode),
        ("ProgramStatusProbe", a.program_status_probe != b.program_status_probe),
    ];
    pairs.into_iter().filter(|(_, d)| *d).map(|(k, _)| k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A 0.x settings block as the WPF app writes it (numbers for enums, CSS font stack).
    fn wpf_fixture() -> Value {
        serde_json::from_str(include_str!("../fixtures/wpf-0.11-settings.json")).unwrap()
    }

    #[test]
    fn a_full_wpf_settings_block_maps_onto_ours() {
        let ours = AppSettings::default();
        let got = map(&wpf_fixture(), &ours);
        let s = &got.settings;
        assert_eq!(s.theme, "light");
        assert_eq!(s.default_command, "claude --continue");
        assert_eq!(s.default_working_folder, "C:\\src");
        assert_eq!(s.launch_commands, ["claude", "codex", "pwsh"]);
        assert!(!s.auto_restore_sessions && s.auto_resume_claude && !s.warn_on_close_when_busy);
        assert!(s.show_toast_notifications && s.show_notification_sound && !s.show_git_branch);
        assert!(s.import_windows_terminal_profiles);
        assert_eq!(s.claude_launch_stagger_ms, 3000);
        assert_eq!(s.terminal_font_points, 16);
        assert_eq!(s.group_display_mode, GroupDisplayMode::InlineHeaders);
        assert_eq!(s.program_status_probe, ProgramStatusProbeMode::Off);
        // Not carried.
        assert_eq!(s.anthropic_api_key, "", "the API key is not copied");
        assert!(s.use_bundled_con_pty, "version 2's own ConPTY choice stays");
        assert_eq!(s.terminal_font_family, ours.terminal_font_family);
        assert!(s.open_with.is_empty(), "the Open with list is never applied here");
        // Offered instead.
        assert_eq!(got.open_with.len(), 2);
        assert!(got.open_with_seeded);
        assert!(got.changed.contains(&"Theme") && got.changed.contains(&"TerminalFontPoints"));
        assert!(!got.changed.contains(&"AutoResumeClaude"), "unchanged values are not reported: {:?}", got.changed);
        assert_eq!(got.skipped, ["LaunchCommands: 1 entries were not usable commands"], "{:?}", got.skipped);
    }

    #[test]
    fn missing_settings_keep_ours() {
        let ours = AppSettings { theme: "light".into(), terminal_font_points: 15, ..AppSettings::default() };
        for raw in [json!({}), Value::Null] {
            let got = map(&raw, &ours);
            assert_eq!(got.settings, ours);
            assert!(got.changed.is_empty() && got.skipped.is_empty() && got.open_with.is_empty());
        }
        // 0.x's default font size is not carried over our own.
        let got = map(&json!({"TerminalFontSize": 14}), &ours);
        assert_eq!(got.settings.terminal_font_points, 15);
    }

    #[test]
    fn garbage_is_skipped_and_reported_never_fatal() {
        let ours = AppSettings::default();
        let raw = json!({
            "Theme": "neon",
            "DefaultCommand": "evil\ncmd",
            "DefaultWorkingFolder": 42,
            "LaunchCommands": "claude",
            "AutoRestoreSessions": "yes",
            "ClaudeLaunchStaggerMs": -5,
            "TerminalFontSize": 400,
            "GroupDisplayMode": 9,
            "ProgramStatusProbe": 1.5,
            "ShowGitBranch": null,
            "OpenWith": [{"Label": 7}, "x", {"Label": "ok", "Executable": "C:\\ok.exe"}],
            "SomethingNew": {"deep": [1, 2, 3]},
            "\u{1b}[31mBad": true,
        });
        let got = map(&raw, &ours);
        assert_eq!(got.settings, ours, "nothing garbage was taken");
        let joined = got.skipped.join("\n");
        for key in [
            "Theme",
            "DefaultCommand",
            "DefaultWorkingFolder",
            "LaunchCommands",
            "AutoRestoreSessions",
            "ClaudeLaunchStaggerMs",
            "TerminalFontSize",
            "GroupDisplayMode",
            "ProgramStatusProbe",
            "OpenWith: 2 entries",
            "SomethingNew",
        ] {
            assert!(joined.contains(key), "{key} not reported in:\n{joined}");
        }
        assert!(!joined.contains('\u{1b}'), "control characters never reach the log");
        assert!(!joined.contains("ShowGitBranch"), "null is the same as missing");
        assert_eq!(got.open_with.len(), 1);
        assert!(!got.open_with_seeded);
        for raw in [json!([1, 2]), json!("x"), json!(7)] {
            let got = map(&raw, &ours);
            assert_eq!(got.settings, ours);
            assert_eq!(got.skipped.len(), 1);
        }
    }

    #[test]
    fn long_lists_are_capped() {
        let many: Vec<String> = (0..200).map(|i| format!("cmd{i}")).collect();
        let got = map(&json!({"LaunchCommands": many}), &AppSettings::default());
        assert_eq!(got.settings.launch_commands.len(), MAX_LIST);
        assert!(got.skipped.iter().any(|s| s.contains("only the first")));
    }

    #[test]
    fn every_known_key_is_either_taken_or_documented_as_not_carried() {
        let fixture = wpf_fixture();
        let Value::Object(m) = &fixture else { unreachable!() };
        for k in m.keys() {
            assert!(TAKEN.contains(&k.as_str()) || NOT_CARRIED.contains(&k.as_str()) || k == FONT_SIZE, "{k}");
        }
    }
}
