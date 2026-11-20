//! `ShellSession` and its enums (C# `Models/ShellSession.cs`).
//!
//! The command-line builders of the C# class (`BuildSshArgs`, `BuildWslArgs`, `QuoteForCmd`)
//! are argv construction and live in the git/argv crate, not here.

use serde::{Deserialize, Serialize};

use super::run_command::RunCommandItem;
use super::serde_util::{null_default as nd, numeric_enum, ser_opt_f64};
use crate::time::Timestamp;

numeric_enum! {
    /// Lifecycle status shown on a session.
    pub enum SessionStatus { Idle = 0, Running = 1, NeedsAttention = 2, Exited = 3 }
    default = Idle;
}

numeric_enum! {
    /// Kind of pseudo-terminal session. `Local` runs a process directly, `Ssh` tunnels
    /// through the system ssh client, `Wsl` launches a shell in a WSL distro via `wsl.exe`.
    pub enum SessionKind { Local = 0, Ssh = 1, Wsl = 2 }
    default = Local;
}

fn default_command() -> String {
    "claude".to_owned()
}

fn default_ssh_port() -> i32 {
    22
}

/// One terminal session, as persisted in `state.json`. Keys are PascalCase, like the C#
/// model's property names.
///
/// Read `state.json` through [`crate::state`], which folds the legacy `IsRemote` key into
/// [`ShellSession::kind`]. Write it through [`ShellSession::to_json`] (or
/// [`crate::models::AppState::to_json`]): those emit the computed `IsRemote` compatibility
/// key that a plain `serde_json::to_string` would leave out.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ShellSession {
    #[serde(deserialize_with = "nd")]
    pub id: String,
    #[serde(deserialize_with = "nd")]
    pub name: String,
    #[serde(deserialize_with = "nd")]
    pub working_folder: String,
    pub command: String,
    #[serde(deserialize_with = "nd")]
    pub args: String,
    #[serde(deserialize_with = "nd")]
    pub group_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color_override: Option<String>,
    pub status: SessionStatus,
    pub created_at: Timestamp,
    /// Last time the user gave this session focus. `Timestamp::never()` for a legacy file;
    /// [`crate::session_manager::SessionManager::load_from_state`] backfills it from
    /// `created_at`.
    pub last_activity_at: Timestamp,
    /// No live PTY: a placeholder in the sidebar that can be woken later.
    pub is_dormant: bool,
    /// Authoritative kind. Legacy files only carried `IsRemote`.
    pub kind: SessionKind,
    /// The legacy `"IsRemote"` JSON key as read from a file; `None` otherwise. It is only a
    /// transport for [`ShellSession::migrate_legacy_fields`]. The value written back is
    /// computed from `kind` (see [`ShellSession::sync_legacy_is_remote`]).
    #[serde(rename = "IsRemote", skip_serializing_if = "Option::is_none")]
    pub legacy_is_remote: Option<bool>,

    #[serde(deserialize_with = "nd")]
    pub ssh_user: String,
    #[serde(deserialize_with = "nd")]
    pub ssh_host: String,
    #[serde(default = "default_ssh_port")]
    pub ssh_port: i32,
    #[serde(deserialize_with = "nd")]
    pub ssh_remote_folder: String,

    /// WSL distro name (matches `wsl -l -q`), e.g. "Ubuntu".
    #[serde(deserialize_with = "nd")]
    pub wsl_distro: String,
    /// Optional `wsl -u <user>` override. Empty = the distro's default user.
    #[serde(deserialize_with = "nd")]
    pub wsl_user: String,
    /// Linux-style working folder inside the distro. Empty = the user's home.
    #[serde(deserialize_with = "nd")]
    pub wsl_working_folder: String,

    // Per-session appearance overrides; `None` = use the global terminal settings.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_font_family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_font_size: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_font_weight: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_font_ligatures: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_cursor_shape: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_cursor_blink: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_padding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_f64")]
    pub profile_background_opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_retro_effect: Option<bool>,
    /// Pre-baked xterm theme object (JSON text), or `None` for the xterm default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_color_scheme_json: Option<String>,

    #[serde(deserialize_with = "nd")]
    pub run_commands: Vec<RunCommandItem>,

    /// The Claude Code conversation this session runs (`--session-id` when CSM started it,
    /// the id passed to `--resume` otherwise). Untrusted (it came from a state file): checked
    /// against a strict alphabet before it goes on a command line. Not in the WPF format.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
}

impl Default for ShellSession {
    /// Mirrors the C# property initialisers. C# also mints a GUID `Id` and stamps `CreatedAt`
    /// with the current time; this crate has neither a random source nor a clock, so both are
    /// left for the caller (`id` empty, `created_at` the "never" sentinel).
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            working_folder: String::new(),
            command: default_command(),
            args: String::new(),
            group_id: String::new(),
            color_override: None,
            status: SessionStatus::Idle,
            created_at: Timestamp::never(),
            last_activity_at: Timestamp::never(),
            is_dormant: false,
            kind: SessionKind::Local,
            legacy_is_remote: None,
            ssh_user: String::new(),
            ssh_host: String::new(),
            ssh_port: default_ssh_port(),
            ssh_remote_folder: String::new(),
            wsl_distro: String::new(),
            wsl_user: String::new(),
            wsl_working_folder: String::new(),
            profile_font_family: None,
            profile_font_size: None,
            profile_font_weight: None,
            profile_font_ligatures: None,
            profile_cursor_shape: None,
            profile_cursor_blink: None,
            profile_padding: None,
            profile_background_opacity: None,
            profile_retro_effect: None,
            profile_color_scheme_json: None,
            run_commands: Vec::new(),
            claude_session_id: None,
        }
    }
}

impl ShellSession {
    /// True iff the session tunnels through ssh (`IsRemote` in C#).
    pub fn is_remote(&self) -> bool {
        self.kind == SessionKind::Ssh
    }

    /// C# `IsRemote` setter: `true` makes the session SSH; `false` on an SSH session makes it
    /// Local; `false` on a Local or WSL session changes nothing.
    pub fn set_is_remote(&mut self, remote: bool) {
        if remote {
            self.kind = SessionKind::Ssh;
        } else if self.kind == SessionKind::Ssh {
            self.kind = SessionKind::Local;
        }
    }

    /// True iff this session runs inside a WSL distro.
    pub fn is_wsl(&self) -> bool {
        self.kind == SessionKind::Wsl
    }

    /// Folds the legacy `IsRemote` JSON key into `kind`. Idempotent. `kind` stays
    /// authoritative: a stale `IsRemote: true` never overrides Wsl, only promotes Local.
    pub fn migrate_legacy_fields(&mut self) {
        if self.legacy_is_remote == Some(true) && self.kind == SessionKind::Local {
            self.kind = SessionKind::Ssh;
        }
        self.legacy_is_remote = None;
    }

    /// Sets the compatibility key to what an older build expects to read: `true` for SSH,
    /// absent for Local and WSL. (C# computes this in the getter; a derived `Serialize`
    /// cannot, so [`ShellSession::to_json`] calls this on a copy.)
    pub fn sync_legacy_is_remote(&mut self) {
        self.legacy_is_remote = (self.kind == SessionKind::Ssh).then_some(true);
    }

    /// Serialises this session the way the C# app does, `IsRemote` compatibility key included.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut copy = self.clone();
        copy.sync_legacy_is_remote();
        serde_json::to_string(&copy)
    }

    /// Full command line for display, where it does not need argv quoting: `Some` for a Local
    /// session and for an incomplete SSH/WSL session (just the executable, never failing,
    /// because it is used in error dialogs on exactly those paths). `None` for a complete
    /// SSH/WSL session, whose line comes from the argv builders in the git/argv crate.
    pub fn full_command_line(&self) -> Option<String> {
        match self.kind {
            SessionKind::Ssh if self.ssh_host.trim().is_empty() => Some("ssh".to_owned()),
            SessionKind::Wsl if self.wsl_distro.trim().is_empty() => Some("wsl.exe".to_owned()),
            SessionKind::Ssh | SessionKind::Wsl => None,
            SessionKind::Local if self.args.trim().is_empty() => Some(self.command.clone()),
            SessionKind::Local => Some(format!("{} {}", self.command, self.args)),
        }
    }

    /// `None` when the session has everything it needs to launch; otherwise a sentence for the
    /// user. Checked before any terminal or PTY is created, because `state.json` and imports
    /// are untrusted and a bad entry used to leak a pane.
    pub fn launch_validation_error(&self) -> Option<&'static str> {
        match self.kind {
            SessionKind::Ssh if self.ssh_host.trim().is_empty() => Some("This SSH session has no host. Edit the session and set one."),
            SessionKind::Wsl if self.wsl_distro.trim().is_empty() => Some("This WSL session has no distro. Edit the session and pick one."),
            _ => None,
        }
    }

    /// Subtitle-line text for the sidebar: Local → working folder leaf; Ssh → host;
    /// Wsl → `distro: linux-leaf`.
    pub fn folder_short(&self) -> String {
        match self.kind {
            SessionKind::Ssh => {
                if self.ssh_host.trim().is_empty() {
                    String::new()
                } else {
                    self.ssh_host.clone()
                }
            }
            SessionKind::Wsl => self.wsl_folder_short(),
            SessionKind::Local => {
                if self.working_folder.is_empty() {
                    String::new()
                } else {
                    get_file_name(self.working_folder.trim_end_matches(['/', '\\'])).to_owned()
                }
            }
        }
    }

    /// The session's label when `name` is blank.
    ///
    /// C# parity: for a Local session this is `Path.GetFileName(folder)`, which returns `""`
    /// (not null) for an empty or root folder, so the `?? Command` fallback in the C# source
    /// is dead code and a Local session with no folder is labelled with an empty string.
    pub fn default_display_name(&self) -> String {
        match self.kind {
            SessionKind::Ssh => {
                if self.ssh_host.trim().is_empty() {
                    self.command.clone()
                } else {
                    self.ssh_host.clone()
                }
            }
            SessionKind::Wsl => {
                if self.wsl_distro.trim().is_empty() {
                    self.command.clone()
                } else if self.wsl_working_folder.is_empty() {
                    self.wsl_distro.clone()
                } else {
                    format!("{}: {}", self.wsl_distro, get_file_name(self.wsl_working_folder.trim_end_matches('/')))
                }
            }
            SessionKind::Local => get_file_name(self.working_folder.trim_end_matches(['/', '\\'])).to_owned(),
        }
    }

    /// Key `crate::color::hex_color` hashes to pick the accent: `user@host` for SSH,
    /// `wsl://<distro><linux-folder>` for WSL, the working folder otherwise.
    pub fn accent_key(&self) -> String {
        match self.kind {
            SessionKind::Ssh => {
                if self.ssh_user.trim().is_empty() {
                    self.ssh_host.clone()
                } else {
                    format!("{}@{}", self.ssh_user, self.ssh_host)
                }
            }
            SessionKind::Wsl => format!("wsl://{}{}", self.wsl_distro, self.wsl_working_folder),
            SessionKind::Local => self.working_folder.clone(),
        }
    }

    fn wsl_folder_short(&self) -> String {
        if self.wsl_distro.trim().is_empty() {
            return String::new();
        }
        let leaf = if self.wsl_working_folder.trim().is_empty() { "" } else { get_file_name(self.wsl_working_folder.trim_end_matches('/')) };
        if leaf.is_empty() { self.wsl_distro.clone() } else { format!("{}: {leaf}", self.wsl_distro) }
    }
}

/// .NET `Path.GetFileName` on Windows: the text after the last `\`, `/` or `:`. Total — an
/// embedded NUL or any other odd character is just text (the C# `DirectoryInfo.Name` it
/// replaced threw on a NUL, reachable from `state.json`).
pub fn get_file_name(path: &str) -> &str {
    match path.rfind(['\\', '/', ':']) {
        Some(i) => path.get(i + 1..).unwrap_or(""),
        None => path,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_command_line_local_returns_command_and_args() {
        let s = ShellSession { command: "claude".into(), args: "--continue".into(), ..Default::default() };
        assert_eq!(s.full_command_line().as_deref(), Some("claude --continue"));
        let bare = ShellSession { command: "pwsh".into(), ..Default::default() };
        assert_eq!(bare.full_command_line().as_deref(), Some("pwsh"));
    }

    #[test]
    fn incomplete_ssh_session_shows_just_the_executable() {
        // Regression carried over: a blank host must never fail the display string.
        let s = ShellSession { kind: SessionKind::Ssh, ssh_host: String::new(), ..Default::default() };
        assert_eq!(s.full_command_line().as_deref(), Some("ssh"));
        let w = ShellSession { kind: SessionKind::Wsl, ..Default::default() };
        assert_eq!(w.full_command_line().as_deref(), Some("wsl.exe"));
    }

    #[test]
    fn is_remote_set_true_sets_kind_ssh() {
        let mut s = ShellSession::default();
        s.set_is_remote(true);
        assert_eq!(s.kind, SessionKind::Ssh);
        assert!(s.is_remote());
    }

    #[test]
    fn is_remote_getter_true_only_for_ssh() {
        let of = |kind| ShellSession { kind, ..Default::default() }.is_remote();
        assert!(!of(SessionKind::Local));
        assert!(of(SessionKind::Ssh));
        assert!(!of(SessionKind::Wsl));
    }

    #[test]
    fn is_remote_setter_false_on_ssh_demotes_to_local() {
        let mut s = ShellSession { kind: SessionKind::Ssh, ..Default::default() };
        s.set_is_remote(false);
        assert_eq!(s.kind, SessionKind::Local);
    }

    #[test]
    fn is_remote_setter_false_on_wsl_leaves_wsl() {
        let mut s = ShellSession { kind: SessionKind::Wsl, ..Default::default() };
        s.set_is_remote(false);
        assert_eq!(s.kind, SessionKind::Wsl);
    }

    #[test]
    fn default_display_name_wsl_with_folder_is_distro_and_leaf() {
        let s =
            ShellSession { kind: SessionKind::Wsl, wsl_distro: "Ubuntu".into(), wsl_working_folder: "/home/alice/proj".into(), ..Default::default() };
        assert_eq!(s.default_display_name(), "Ubuntu: proj");
    }

    #[test]
    fn accent_key_wsl_is_distinct_from_local() {
        let wsl = ShellSession { kind: SessionKind::Wsl, wsl_distro: "Ubuntu".into(), wsl_working_folder: "/proj".into(), ..Default::default() };
        let local = ShellSession { working_folder: "/proj".into(), ..Default::default() };
        assert_ne!(wsl.accent_key(), local.accent_key());
        assert_eq!(wsl.accent_key(), "wsl://Ubuntu/proj");
    }

    #[test]
    fn accent_key_ssh_uses_user_at_host() {
        let s = ShellSession { kind: SessionKind::Ssh, ssh_user: "alice".into(), ssh_host: "dev".into(), ..Default::default() };
        assert_eq!(s.accent_key(), "alice@dev");
        let no_user = ShellSession { kind: SessionKind::Ssh, ssh_host: "dev".into(), ..Default::default() };
        assert_eq!(no_user.accent_key(), "dev");
    }

    #[test]
    fn launch_validation_error_cases() {
        let none = |s: ShellSession| s.launch_validation_error().is_none();
        assert!(none(ShellSession { kind: SessionKind::Local, command: "claude".into(), ..Default::default() }));
        let ssh = ShellSession { kind: SessionKind::Ssh, ..Default::default() };
        assert!(ssh.launch_validation_error().is_some_and(|m| m.to_lowercase().contains("host")));
        let wsl = ShellSession { kind: SessionKind::Wsl, ..Default::default() };
        assert!(wsl.launch_validation_error().is_some_and(|m| m.to_lowercase().contains("distro")));
        assert!(none(ShellSession { kind: SessionKind::Wsl, wsl_distro: "Ubuntu".into(), ..Default::default() }));
    }

    #[test]
    fn folder_short_local_working_folder_with_embedded_nul_does_not_panic() {
        // C# regression: DirectoryInfo(...).Name threw on a NUL, reachable from state.json.
        let s = ShellSession { working_folder: "C:\\src\\web\u{0}oops".into(), ..Default::default() };
        assert_eq!(s.folder_short(), "web\u{0}oops");
    }

    #[test]
    fn folder_short_per_kind() {
        let local = ShellSession { working_folder: "C:\\src\\app\\".into(), ..Default::default() };
        assert_eq!(local.folder_short(), "app");
        let ssh = ShellSession { kind: SessionKind::Ssh, ssh_host: "dev".into(), ..Default::default() };
        assert_eq!(ssh.folder_short(), "dev");
        let wsl =
            ShellSession { kind: SessionKind::Wsl, wsl_distro: "Ubuntu".into(), wsl_working_folder: "/home/a/proj/".into(), ..Default::default() };
        assert_eq!(wsl.folder_short(), "Ubuntu: proj");
        let wsl_home = ShellSession { kind: SessionKind::Wsl, wsl_distro: "Ubuntu".into(), ..Default::default() };
        assert_eq!(wsl_home.folder_short(), "Ubuntu");
        assert_eq!(ShellSession::default().folder_short(), "");
    }

    #[test]
    fn default_display_name_falls_back_per_kind() {
        let ssh = ShellSession { kind: SessionKind::Ssh, command: "bash".into(), ..Default::default() };
        assert_eq!(ssh.default_display_name(), "bash");
        // C# parity: an empty local folder yields "" (GetFileName never returns null).
        assert_eq!(ShellSession::default().default_display_name(), "");
        let local = ShellSession { working_folder: "C:\\repos\\csm".into(), ..Default::default() };
        assert_eq!(local.default_display_name(), "csm");
    }

    #[test]
    fn the_claude_session_id_round_trips_and_is_absent_by_default() {
        let s = ShellSession { id: "a".into(), claude_session_id: Some("0b1c-22".into()), ..Default::default() };
        let json = s.to_json().unwrap();
        assert!(json.contains("\"ClaudeSessionId\":\"0b1c-22\""), "{json}");
        let back: ShellSession = serde_json::from_str(&json).unwrap();
        assert_eq!(back.claude_session_id.as_deref(), Some("0b1c-22"));
        let plain = ShellSession { id: "a".into(), ..Default::default() }.to_json().unwrap();
        assert!(!plain.contains("ClaudeSessionId"), "the WPF format is unchanged for sessions without one");
        let legacy: ShellSession = serde_json::from_str(r#"{"Id":"x","Command":"claude"}"#).unwrap();
        assert_eq!(legacy.claude_session_id, None);
    }

    #[test]
    fn get_file_name_matches_dotnet_on_windows() {
        assert_eq!(get_file_name("C:\\a\\b"), "b");
        assert_eq!(get_file_name("a/b/c"), "c");
        assert_eq!(get_file_name("C:"), "");
        assert_eq!(get_file_name("plain"), "plain");
        assert_eq!(get_file_name(""), "");
    }

    #[test]
    fn enum_numbers_match_the_csharp_declaration_order() {
        assert_eq!(SessionKind::Local.number(), 0);
        assert_eq!(SessionKind::Ssh.number(), 1);
        assert_eq!(SessionKind::Wsl.number(), 2);
        assert_eq!(SessionStatus::Exited.number(), 3);
    }
}
