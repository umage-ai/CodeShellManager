//! `AppSettings` and `AppState`, the JSON root of `state.json` (C# `Models/AppState.cs`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::group::SessionGroup;
use super::open_with::OpenWithEntry;
use super::recently_closed::RecentlyClosedEntry;
use super::serde_util::{null_default as nd, numeric_enum, ser_f64};
use super::session::ShellSession;
use super::slash_command::SlashCommand;
use super::ui_state::{UiState, lenient};

numeric_enum! {
    /// How the sidebar surfaces session groups.
    pub enum GroupDisplayMode { None = 0, FilterStrip = 1, InlineHeaders = 2 }
    default = FilterStrip;
}

numeric_enum! {
    /// How the per-row action icons are surfaced on each sidebar row.
    pub enum SidebarActionIconsMode { Hidden = 0, OnHover = 1, Always = 2 }
    default = OnHover;
}

numeric_enum! {
    /// What closing the main window does (`CloseAction`). Only where there is a tray icon to come
    /// back from; without one, closing always quits.
    pub enum CloseAction {
        /// Ask each time: keep running in the tray, or quit.
        Ask = 0,
        /// Hide to the tray; sessions keep running.
        Tray = 1,
        /// Quit (the shutdown board gives sessions time to exit).
        Quit = 2,
    }
    default = Ask;
}

numeric_enum! {
    /// When to answer the OSC 7501 feature query.
    pub enum ProgramStatusProbeMode {
        /// Answer on the bundled ConPTY (raw reply, in time); not on the inbox one.
        Auto = 0,
        /// Answer on both (inbox: experiments only, the reply arrives late there).
        On = 1,
        /// Never answer.
        Off = 2,
    }
    default = Auto;
}

fn t() -> bool {
    true
}
fn default_stagger() -> i32 {
    2000
}
fn default_command() -> String {
    "claude".to_owned()
}
fn default_theme() -> String {
    "dark".to_owned()
}
fn default_max_search() -> i32 {
    100
}
fn default_retention() -> i32 {
    30
}
fn default_font_family() -> String {
    "'Cascadia Code', 'Cascadia Mono', Consolas, 'Courier New', monospace".to_owned()
}
fn default_font_size() -> i32 {
    14
}
fn default_font_weight() -> String {
    "normal".to_owned()
}
fn default_padding() -> i32 {
    8
}
fn default_line_height() -> f64 {
    1.0
}
fn default_launch_commands() -> Vec<String> {
    [
        "claude",
        "claude --continue",
        "claude --model claude-opus-4-6",
        "claude --dangerously-skip-permissions",
        "codex",
        "gh copilot suggest",
        "pwsh",
        "cmd",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect()
}

/// User settings, persisted inside `state.json`. Every field has the C# default, so an old or
/// partial file loads with sensible values.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AppSettings {
    pub auto_restore_sessions: bool,
    pub auto_resume_claude: bool,
    /// Flat delay between consecutive Claude launches (and the cap on the post-exit settle at
    /// shutdown). 0 disables. Do NOT replace with an adaptive wait; see docs/wpf/lessons.md.
    pub claude_launch_stagger_ms: i32,
    pub auto_focus_terminal_on_select: bool,
    /// Ask before closing while any session is busy (see `busy_rules`).
    pub warn_on_close_when_busy: bool,
    pub show_toast_notifications: bool,
    pub show_notification_sound: bool,
    #[serde(deserialize_with = "nd")]
    pub anthropic_api_key: String,
    pub default_command: String,
    #[serde(deserialize_with = "nd")]
    pub default_working_folder: String,
    pub show_git_branch: bool,
    pub group_display_mode: GroupDisplayMode,
    pub filter_grid_by_active_group: bool,
    pub per_group_layout: bool,
    /// Legacy flag, kept for old files. The C# loader's "false + default mode migrates the
    /// mode to None" rule is not applied by [`crate::state`], as in C# (no code reads it).
    pub show_groups_tab: bool,
    pub show_worktree_clusters: bool,
    pub ungrouped_section_expanded: bool,
    /// One-shot guard for the legacy auto-created "Default" group migration.
    pub legacy_default_group_cleared: bool,
    pub search_collapse_after_navigate: bool,
    pub theme: String,
    pub max_search_results: i32,
    pub show_terminal_status_dot: bool,
    pub import_windows_terminal_profiles: bool,
    pub sidebar_action_icons_mode: SidebarActionIconsMode,

    // Storage
    pub index_terminal_output: bool,
    /// 0 = keep forever.
    pub output_retention_days: i32,
    pub debug_terminal_trace: bool,

    /// Host terminals on the bundled ConPTY. No UI; read once at startup.
    pub use_bundled_con_pty: bool,
    /// Whether to answer a program's OSC 7501 query. Stored as a number. The pre-release key
    /// `AnswerProgramStatusProbe` is deliberately NOT read: it meant "experimental, off".
    pub program_status_probe: ProgramStatusProbeMode,

    // Terminal font settings
    pub terminal_font_family: String,
    pub terminal_font_size: i32,
    pub terminal_font_ligatures: bool,
    pub terminal_font_weight: String,
    #[serde(serialize_with = "ser_f64")]
    pub terminal_letter_spacing: f64,
    #[serde(serialize_with = "ser_f64")]
    pub terminal_line_height: f64,

    /// "Open with..." editors. Seeded once by detection, gated on `open_with_seeded`.
    /// Sanitising an untrusted list is the open-with crate's job (`OpenWithService.Sanitize`).
    #[serde(deserialize_with = "nd")]
    pub open_with: Vec<OpenWithEntry>,
    pub open_with_seeded: bool,

    #[serde(deserialize_with = "nd")]
    pub launch_commands: Vec<String>,

    /// Offer CodeShellManager's tools to the Claude sessions it launches (`--mcp-config`).
    /// Opt-in: it changes the user's `claude` command line. (The Rust app's own settings below
    /// are written only when changed, so a file the WPF app reads back stays as it wrote it.)
    #[serde(skip_serializing_if = "is_false")]
    pub offer_mcp_to_claude: bool,
    /// Space between a pane's edge and the terminal cells, in points.
    #[serde(skip_serializing_if = "is_default_padding")]
    pub terminal_padding: i32,
    /// The terminal font size in the Rust app's points; 0 = the app's default. (The WPF
    /// `TerminalFontSize` is CSS pixels for xterm.js and stays as it was.)
    #[serde(skip_serializing_if = "is_zero")]
    pub terminal_font_points: i32,
    /// The Claude slash commands a Claude pane's menu offers ("Claude commands ▸", the WPF
    /// ⚡ Commands panel), sent typed plus Enter by `session_send_command`. Written only when
    /// changed from the defaults.
    #[serde(deserialize_with = "nd", skip_serializing_if = "SlashCommand::is_default_list")]
    pub claude_commands: Vec<SlashCommand>,
    /// Add-ons the person enabled, by id, with the digest of the `addon.toml` they approved
    /// (`csm-engine::addons`). Changed only by the add-on tools, never by `settings_set`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub addon_approvals: std::collections::BTreeMap<String, String>,
    /// Programs in a session may set the clipboard with OSC 52 (tmux, Claude Code's fullscreen
    /// mode, remote shells), as Windows Terminal allows by default. Reading the clipboard
    /// (an OSC 52 query) is never answered. Issue #156. Written only when turned off.
    #[serde(skip_serializing_if = "is_true")]
    pub allow_osc52_clipboard: bool,
    /// The whole UI's zoom in percent ([`UI_ZOOM_RANGE`]; 100 = the system scale). The terminal's
    /// font size is its own setting and does not follow it.
    #[serde(skip_serializing_if = "is_default_zoom")]
    pub ui_zoom_percent: i32,
    /// What closing the main window does where there is a tray: ask (the default), hide to the
    /// tray, or quit. Quit (the tray's, the title bar's, `app_quit`) always quits.
    #[serde(skip_serializing_if = "is_default_close")]
    pub close_action: CloseAction,
    /// The legacy `CloseToTray` switch (beta 1): read once and folded into `close_action` by
    /// [`AppSettings::migrate_legacy_fields`]; never written.
    #[serde(skip_serializing)]
    pub close_to_tray: bool,
    /// "Still running in the tray" was said once; it is not said again.
    #[serde(skip_serializing_if = "is_false")]
    pub tray_notice_shown: bool,
    /// Launch local Claude sessions with `--name <session name>`, so the conversation carries the
    /// session's name in Claude's own `/resume` list. Opt-in: it changes the `claude` command
    /// line, and a Claude CLI too old to know `--name` would refuse to start.
    #[serde(skip_serializing_if = "is_false")]
    pub pass_session_name_to_claude: bool,
    /// With the Claude usage add-on on: also read every past Claude transcript on this machine
    /// (the dashboard's history), not only the sessions CSM runs. On by default; written only
    /// when turned off. Never read under `--clean` or an isolated run.
    #[serde(skip_serializing_if = "is_true")]
    pub claude_usage_history: bool,
    /// The main window opens minimized (or, with [`Self::start_minimized_to_tray`] and a tray icon,
    /// hidden in the tray) on every start, including a start at sign-in; sessions restore as
    /// usual. Off by default. Starting at sign-in itself is the OS's entry, not a setting here
    /// (`app_autostart_set`).
    #[serde(skip_serializing_if = "is_false")]
    pub start_minimized: bool,
    /// With `StartMinimized`: hide the window in the tray (notification area) instead of
    /// minimizing it to the taskbar. Ignored where there is no tray (Linux).
    #[serde(skip_serializing_if = "is_false")]
    pub start_minimized_to_tray: bool,
    /// The theme drawn in dark mode: `mocha` or an add-on theme `addon-id.theme-id` (docs/addons.md,
    /// "Themes"). One that is not available falls back to Mocha.
    #[serde(skip_serializing_if = "is_mocha")]
    pub theme_dark: String,
    /// The theme drawn in light mode: `latte` or a light add-on theme; falls back to Latte.
    #[serde(skip_serializing_if = "is_latte")]
    pub theme_light: String,
    /// Ask GitHub once a day whether a newer CodeShellManager is out and show it in the title bar
    /// (`update_check`; nothing is downloaded). On by default, as in 0.x; written only when turned
    /// off. Never checked on its own under `--clean` or an isolated run.
    #[serde(skip_serializing_if = "is_true")]
    pub check_for_updates: bool,
    /// The search index keeps the output of all sessions together under this many MB (the oldest
    /// lines go first). Written only when changed from the default.
    #[serde(skip_serializing_if = "is_default_index_mb")]
    pub index_max_mb: i32,
    /// Claude usage rows older than this many days are removed (0 keeps them all). Written only
    /// when changed from the default.
    #[serde(skip_serializing_if = "is_default_usage_days")]
    pub claude_usage_retention_days: i32,
    /// A selection copied from a Claude Code pane is cleaned up: Claude's markers, frames and
    /// indentation go and the lines it wrapped are joined again (Alt while copying gives the raw
    /// text). On by default; written only when turned off.
    #[serde(skip_serializing_if = "is_true")]
    pub clean_claude_copies: bool,
}

/// The values `UiZoomPercent` may take.
pub const UI_ZOOM_RANGE: std::ops::RangeInclusive<i32> = 80..=200;
/// `UiZoomPercent`'s default.
pub const UI_ZOOM_DEFAULT: i32 = 100;

fn is_false(b: &bool) -> bool {
    !*b
}
/// `IndexMaxMb` by default: the index's output stays under 1 GB.
pub const DEFAULT_INDEX_MAX_MB: i32 = 1024;
/// The range `IndexMaxMb` accepts.
pub const INDEX_MAX_MB_RANGE: std::ops::RangeInclusive<i32> = 100..=102_400;
/// `ClaudeUsageRetentionDays` by default: about 13 months.
pub const DEFAULT_USAGE_RETENTION_DAYS: i32 = 400;
/// The range `ClaudeUsageRetentionDays` accepts (0 keeps everything).
pub const USAGE_RETENTION_DAYS_RANGE: std::ops::RangeInclusive<i32> = 0..=3650;

fn is_default_index_mb(v: &i32) -> bool {
    *v == DEFAULT_INDEX_MAX_MB
}

fn is_default_usage_days(v: &i32) -> bool {
    *v == DEFAULT_USAGE_RETENTION_DAYS
}

fn is_true(b: &bool) -> bool {
    *b
}
fn is_zero(n: &i32) -> bool {
    *n == 0
}
fn is_default_padding(n: &i32) -> bool {
    *n == default_padding()
}
fn is_mocha(s: &str) -> bool {
    s == "mocha"
}
fn is_latte(s: &str) -> bool {
    s == "latte"
}
fn is_default_zoom(n: &i32) -> bool {
    *n == UI_ZOOM_DEFAULT
}
fn is_default_close(a: &CloseAction) -> bool {
    *a == CloseAction::default()
}

impl AppSettings {
    /// The beta-1 `CloseToTray: true` becomes `CloseAction: Tray` (unless a choice was made since).
    pub fn migrate_legacy_fields(&mut self) {
        if std::mem::take(&mut self.close_to_tray) && self.close_action == CloseAction::Ask {
            self.close_action = CloseAction::Tray;
        }
    }
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            auto_restore_sessions: t(),
            auto_resume_claude: t(),
            claude_launch_stagger_ms: default_stagger(),
            auto_focus_terminal_on_select: t(),
            warn_on_close_when_busy: t(),
            show_toast_notifications: false,
            show_notification_sound: false,
            anthropic_api_key: String::new(),
            default_command: default_command(),
            default_working_folder: String::new(),
            show_git_branch: t(),
            group_display_mode: GroupDisplayMode::FilterStrip,
            filter_grid_by_active_group: t(),
            per_group_layout: t(),
            show_groups_tab: t(),
            show_worktree_clusters: t(),
            ungrouped_section_expanded: t(),
            legacy_default_group_cleared: false,
            search_collapse_after_navigate: t(),
            theme: default_theme(),
            theme_dark: "mocha".to_owned(),
            theme_light: "latte".to_owned(),
            max_search_results: default_max_search(),
            show_terminal_status_dot: t(),
            import_windows_terminal_profiles: false,
            sidebar_action_icons_mode: SidebarActionIconsMode::OnHover,
            index_terminal_output: t(),
            output_retention_days: default_retention(),
            debug_terminal_trace: false,
            use_bundled_con_pty: t(),
            program_status_probe: ProgramStatusProbeMode::Auto,
            terminal_font_family: default_font_family(),
            terminal_font_size: default_font_size(),
            terminal_font_ligatures: t(),
            terminal_font_weight: default_font_weight(),
            terminal_letter_spacing: 0.0,
            terminal_line_height: default_line_height(),
            open_with: Vec::new(),
            open_with_seeded: false,
            launch_commands: default_launch_commands(),
            offer_mcp_to_claude: false,
            terminal_padding: default_padding(),
            terminal_font_points: 0,
            claude_commands: SlashCommand::defaults(),
            addon_approvals: Default::default(),
            allow_osc52_clipboard: t(),
            ui_zoom_percent: UI_ZOOM_DEFAULT,
            close_action: CloseAction::Ask,
            close_to_tray: false,
            tray_notice_shown: false,
            pass_session_name_to_claude: false,
            claude_usage_history: t(),
            clean_claude_copies: t(),
            start_minimized: false,
            start_minimized_to_tray: false,
            check_for_updates: t(),
            index_max_mb: DEFAULT_INDEX_MAX_MB,
            claude_usage_retention_days: DEFAULT_USAGE_RETENTION_DAYS,
        }
    }
}

/// A window rectangle in WPF device-independent pixels (legacy; see [`WindowPlacementState`]).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct WindowBounds {
    #[serde(serialize_with = "ser_f64")]
    pub left: f64,
    #[serde(serialize_with = "ser_f64")]
    pub top: f64,
    #[serde(serialize_with = "ser_f64")]
    pub width: f64,
    #[serde(serialize_with = "ser_f64")]
    pub height: f64,
}

/// The window's normal rectangle in PHYSICAL screen pixels plus the DPI of the monitor it was
/// on when saved (#181).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct WindowPlacementState {
    pub left: i32,
    pub top: i32,
    pub width: i32,
    pub height: i32,
    pub dpi: i32,
}

/// An MCP client allowed to call one tool without the consent card ("Always for this client").
/// Revocable in Settings → MCP (`grant_revoke`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct McpGrant {
    pub client: String,
    pub tool: String,
}

fn default_layout() -> String {
    "Single".to_owned()
}

/// The JSON root of `state.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AppState {
    #[serde(deserialize_with = "nd")]
    pub sessions: Vec<ShellSession>,
    #[serde(deserialize_with = "nd")]
    pub groups: Vec<SessionGroup>,
    /// Newest first, capped by the UI layer (`MaxRecentlyClosed = 10`).
    #[serde(deserialize_with = "nd")]
    pub recently_closed: Vec<RecentlyClosedEntry>,
    pub last_layout: String,
    /// Per-group grid layouts. Key = group id, the Ungrouped marker, or `"__ALL__"`.
    #[serde(deserialize_with = "nd")]
    pub group_layouts: BTreeMap<String, String>,
    #[serde(deserialize_with = "nd_settings")]
    pub settings: AppSettings,

    /// Legacy normal bounds in DIPs; read only when `last_placement` is absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_normal_bounds: Option<WindowBounds>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_placement: Option<WindowPlacementState>,
    pub window_maximized: bool,

    /// "Always for this client" answers to MCP consent cards. Absent in WPF files.
    #[serde(deserialize_with = "nd", skip_serializing_if = "Vec::is_empty")]
    pub mcp_grants: Vec<McpGrant>,

    /// Open with entries imported from CodeShellManager 0.x, waiting for the person's yes or no
    /// (`open_with_pending` / `open_with_pending_answer`): they are programs to run, so they are
    /// never adopted silently. Absent in WPF files; read leniently (a bad entry is dropped).
    #[serde(deserialize_with = "lenient_entries", skip_serializing_if = "Vec::is_empty")]
    pub pending_open_with: Vec<OpenWithEntry>,

    /// The Rust app's own UI memory (rail tab, layouts per group, panels). Absent in WPF files;
    /// read leniently so a bad value never costs the sessions.
    #[serde(deserialize_with = "lenient", skip_serializing_if = "UiState::is_default")]
    pub ui: UiState,

    /// Version of the Rust app's file format; absent in files the WPF app wrote. The Rust app
    /// stamps it on every save.
    #[serde(rename = "CsmRsVersion", skip_serializing_if = "Option::is_none")]
    pub csm_rs_version: Option<u32>,
}

fn lenient_entries<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<OpenWithEntry>, D::Error> {
    let serde_json::Value::Array(items) = serde_json::Value::deserialize(d)? else { return Ok(Vec::new()) };
    Ok(items.into_iter().filter_map(|v| serde_json::from_value(v).ok()).collect())
}

fn nd_settings<'de, D: serde::Deserializer<'de>>(d: D) -> Result<AppSettings, D::Error> {
    // C# `Normalize`: `s.Settings ??= new()`.
    Ok(Option::<AppSettings>::deserialize(d)?.unwrap_or_default())
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            sessions: Vec::new(),
            groups: Vec::new(),
            recently_closed: Vec::new(),
            last_layout: default_layout(),
            group_layouts: BTreeMap::new(),
            settings: AppSettings::default(),
            last_normal_bounds: None,
            last_placement: None,
            window_maximized: false,
            mcp_grants: Vec::new(),
            pending_open_with: Vec::new(),
            ui: UiState::default(),
            csm_rs_version: None,
        }
    }
}

impl AppState {
    /// Serialises the state the way the C# app writes it: indented, nulls omitted, and the
    /// computed `IsRemote` compatibility key present on SSH sessions and entries so an older
    /// build still sees them as remote. Does not mutate `self`.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut copy = self.clone();
        for s in &mut copy.sessions {
            s.sync_legacy_is_remote();
        }
        for e in &mut copy.recently_closed {
            e.sync_legacy_is_remote();
        }
        serde_json::to_string_pretty(&copy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_copy_is_on_by_default_and_written_only_when_off() {
        let s = AppSettings::default();
        assert!(s.allow_osc52_clipboard);
        assert!(!serde_json::to_string(&s).unwrap().contains("AllowOsc52Clipboard"));
        let off = AppSettings { allow_osc52_clipboard: false, ..AppSettings::default() };
        let json = serde_json::to_string(&off).unwrap();
        assert!(json.contains("\"AllowOsc52Clipboard\":false"), "{json}");
        assert!(!serde_json::from_str::<AppSettings>(&json).unwrap().allow_osc52_clipboard);
        assert!(serde_json::from_str::<AppSettings>("{}").unwrap().allow_osc52_clipboard);
    }

    #[test]
    fn a_stale_dev_build_key_is_ignored_and_the_mode_defaults_to_auto() {
        // Earlier development builds saved "AnswerProgramStatusProbe": false meaning
        // "experimental, off"; the renamed setting must not read that as a deliberate "off".
        let settings: AppSettings = serde_json::from_str(r#"{"AnswerProgramStatusProbe": false, "UseBundledConPty": true}"#).unwrap();
        assert_eq!(settings.program_status_probe, ProgramStatusProbeMode::Auto);
        assert!(settings.use_bundled_con_pty);
    }

    #[test]
    fn check_for_updates_is_on_by_default_and_written_only_when_off() {
        let d = AppSettings::default();
        assert!(d.check_for_updates);
        assert!(!serde_json::to_string(&d).unwrap().contains("CheckForUpdates"));
        let off = AppSettings { check_for_updates: false, ..Default::default() };
        let json = serde_json::to_string(&off).unwrap();
        assert!(json.contains(r#""CheckForUpdates":false"#), "{json}");
        assert!(!serde_json::from_str::<AppSettings>(&json).unwrap().check_for_updates);
        assert!(serde_json::from_str::<AppSettings>("{}").unwrap().check_for_updates);
    }

    #[test]
    fn start_minimized_is_off_by_default_written_only_when_on_and_round_trips() {
        let d = AppSettings::default();
        assert!(!d.start_minimized && !d.start_minimized_to_tray);
        let json = serde_json::to_string(&d).unwrap();
        assert!(!json.contains("StartMinimized"), "a file the WPF app reads back stays as it wrote it: {json}");
        let on = AppSettings { start_minimized: true, start_minimized_to_tray: true, ..Default::default() };
        let json = serde_json::to_string(&on).unwrap();
        assert!(json.contains(r#""StartMinimized":true"#) && json.contains(r#""StartMinimizedToTray":true"#), "{json}");
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert!(back.start_minimized && back.start_minimized_to_tray);
    }

    #[test]
    fn the_probe_mode_round_trips_through_state_json_as_a_number() {
        let json = serde_json::to_string(&AppSettings { program_status_probe: ProgramStatusProbeMode::Off, ..Default::default() }).unwrap();
        assert!(json.contains(r#""ProgramStatusProbe":2"#), "{json}");
        let back: AppSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(back.program_status_probe, ProgramStatusProbeMode::Off);
    }

    #[test]
    fn missing_keys_take_the_csharp_defaults() {
        let s: AppSettings = serde_json::from_str("{}").unwrap();
        assert!(s.auto_restore_sessions && s.auto_resume_claude && s.warn_on_close_when_busy);
        assert_eq!(s.claude_launch_stagger_ms, 2000);
        assert_eq!(s.default_command, "claude");
        assert_eq!(s.group_display_mode, GroupDisplayMode::FilterStrip);
        assert_eq!(s.sidebar_action_icons_mode, SidebarActionIconsMode::OnHover);
        assert_eq!((s.terminal_font_size, s.terminal_line_height), (14, 1.0));
        assert_eq!(s.launch_commands.len(), 8);
        assert!(!s.open_with_seeded && s.use_bundled_con_pty);
        let st: AppState = serde_json::from_str("{}").unwrap();
        assert_eq!(st.last_layout, "Single");
        // A malformed Ui key never costs the rest of the file.
        let st: AppState = serde_json::from_str(r#"{"Ui": {"Layouts": 7}, "Sessions": [{"Id": "a"}]}"#).unwrap();
        assert_eq!(st.sessions.len(), 1);
        assert!(st.ui.layouts.is_empty());
    }
}
