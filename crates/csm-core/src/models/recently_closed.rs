//! `RecentlyClosedEntry` (C# `Models/RecentlyClosedEntry.cs`): a snapshot of a closed
//! session, kept so it can be reopened. Deliberately separate from `ShellSession` so runtime
//! fields (dormancy, status, last activity) never leak into the ring buffer.

use serde::{Deserialize, Serialize};

use super::run_command::RunCommandItem;
use super::serde_util::{null_default as nd, ser_opt_f64};
use super::session::{SessionKind, ShellSession};
use crate::time::Timestamp;

fn default_command() -> String {
    "claude".to_owned()
}

fn default_ssh_port() -> i32 {
    22
}

/// A recently-closed session. Keys are PascalCase; `IsRemote` is the legacy compatibility key
/// (see [`ShellSession`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct RecentlyClosedEntry {
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
    pub kind: SessionKind,
    /// Legacy `"IsRemote"` key, as read from a file. See [`ShellSession::legacy_is_remote`].
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

    #[serde(deserialize_with = "nd")]
    pub wsl_distro: String,
    #[serde(deserialize_with = "nd")]
    pub wsl_user: String,
    #[serde(deserialize_with = "nd")]
    pub wsl_working_folder: String,

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
    #[serde(
        skip_serializing_if = "Option::is_none",
        serialize_with = "ser_opt_f64"
    )]
    pub profile_background_opacity: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_retro_effect: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_color_scheme_json: Option<String>,

    #[serde(deserialize_with = "nd")]
    pub run_commands: Vec<RunCommandItem>,

    pub closed_at: Timestamp,
}

impl Default for RecentlyClosedEntry {
    fn default() -> Self {
        Self {
            name: String::new(),
            working_folder: String::new(),
            command: default_command(),
            args: String::new(),
            group_id: String::new(),
            color_override: None,
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
            closed_at: Timestamp::never(),
        }
    }
}

impl RecentlyClosedEntry {
    /// True iff the closed session was SSH.
    pub fn is_remote(&self) -> bool {
        self.kind == SessionKind::Ssh
    }

    /// See [`ShellSession::migrate_legacy_fields`].
    pub fn migrate_legacy_fields(&mut self) {
        if self.legacy_is_remote == Some(true) && self.kind == SessionKind::Local {
            self.kind = SessionKind::Ssh;
        }
        self.legacy_is_remote = None;
    }

    /// See [`ShellSession::sync_legacy_is_remote`].
    pub fn sync_legacy_is_remote(&mut self) {
        self.legacy_is_remote = (self.kind == SessionKind::Ssh).then_some(true);
    }

    /// Copies the persistable fields of a session. Run commands are deep-copied with fresh
    /// ids (from `new_id`) so editing the reopened session cannot mutate the entry still in
    /// the ring buffer. `now` stamps `closed_at` (C# read the clock here).
    pub fn from_session(
        s: &ShellSession,
        now: Timestamp,
        mut new_id: impl FnMut() -> String,
    ) -> Self {
        Self {
            name: s.name.clone(),
            working_folder: s.working_folder.clone(),
            command: s.command.clone(),
            args: s.args.clone(),
            group_id: s.group_id.clone(),
            color_override: s.color_override.clone(),
            kind: s.kind,
            legacy_is_remote: None,
            ssh_user: s.ssh_user.clone(),
            ssh_host: s.ssh_host.clone(),
            ssh_port: s.ssh_port,
            ssh_remote_folder: s.ssh_remote_folder.clone(),
            wsl_distro: s.wsl_distro.clone(),
            wsl_user: s.wsl_user.clone(),
            wsl_working_folder: s.wsl_working_folder.clone(),
            profile_font_family: s.profile_font_family.clone(),
            profile_font_size: s.profile_font_size,
            profile_font_weight: s.profile_font_weight.clone(),
            profile_font_ligatures: s.profile_font_ligatures,
            profile_cursor_shape: s.profile_cursor_shape.clone(),
            profile_cursor_blink: s.profile_cursor_blink,
            profile_padding: s.profile_padding.clone(),
            profile_background_opacity: s.profile_background_opacity,
            profile_retro_effect: s.profile_retro_effect,
            profile_color_scheme_json: s.profile_color_scheme_json.clone(),
            run_commands: s
                .run_commands
                .iter()
                .map(|r| RunCommandItem {
                    id: new_id(),
                    label: r.label.clone(),
                    command_line: r.command_line.clone(),
                    is_default: r.is_default,
                    mode: r.mode,
                    post_run_url: r.post_run_url.clone(),
                })
                .collect(),
            closed_at: now,
        }
    }

    /// Friendly subtitle for the recents UI: a kind-specific locator.
    pub fn subtitle(&self) -> String {
        match self.kind {
            SessionKind::Ssh => {
                if self.ssh_user.trim().is_empty() {
                    self.ssh_host.clone()
                } else {
                    format!("{}@{}", self.ssh_user, self.ssh_host)
                }
            }
            SessionKind::Wsl => {
                if self.wsl_working_folder.is_empty() {
                    self.wsl_distro.clone()
                } else {
                    format!("{}: {}", self.wsl_distro, self.wsl_working_folder)
                }
            }
            SessionKind::Local => self.working_folder.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::RunMode;

    fn fresh_ids() -> impl FnMut() -> String {
        let mut n = 0;
        move || {
            n += 1;
            format!("fresh-{n}")
        }
    }

    #[test]
    fn from_session_copies_scalar_and_ssh_fields() {
        let mut s = ShellSession {
            name: "alpha".into(),
            working_folder: r"C:\proj".into(),
            command: "claude".into(),
            args: "--resume foo".into(),
            group_id: "g1".into(),
            color_override: Some("#abcdef".into()),
            ssh_user: "alice".into(),
            ssh_host: "dev.example.com".into(),
            ssh_port: 2222,
            ssh_remote_folder: "/srv".into(),
            ..Default::default()
        };
        s.set_is_remote(true);

        let e = RecentlyClosedEntry::from_session(&s, Timestamp::never(), fresh_ids());

        assert_eq!(e.name, "alpha");
        assert_eq!(e.working_folder, r"C:\proj");
        assert_eq!(e.command, "claude");
        assert_eq!(e.args, "--resume foo");
        assert_eq!(e.group_id, "g1");
        assert_eq!(e.color_override.as_deref(), Some("#abcdef"));
        assert!(e.is_remote());
        assert_eq!(e.ssh_user, "alice");
        assert_eq!(e.ssh_host, "dev.example.com");
        assert_eq!(e.ssh_port, 2222);
        assert_eq!(e.ssh_remote_folder, "/srv");
    }

    #[test]
    fn from_session_copies_wsl_fields() {
        let s = ShellSession {
            kind: SessionKind::Wsl,
            wsl_distro: "Ubuntu".into(),
            wsl_user: "bob".into(),
            wsl_working_folder: "/srv/app".into(),
            ..Default::default()
        };
        let e = RecentlyClosedEntry::from_session(&s, Timestamp::never(), fresh_ids());
        assert_eq!(e.kind, SessionKind::Wsl);
        assert_eq!(
            (
                e.wsl_distro.as_str(),
                e.wsl_user.as_str(),
                e.wsl_working_folder.as_str()
            ),
            ("Ubuntu", "bob", "/srv/app")
        );
    }

    #[test]
    fn from_session_copies_profile_overrides() {
        let s = ShellSession {
            profile_font_family: Some("Cascadia".into()),
            profile_font_size: Some(16),
            profile_font_weight: Some("bold".into()),
            profile_font_ligatures: Some(true),
            profile_cursor_shape: Some("underline".into()),
            profile_cursor_blink: Some(false),
            profile_padding: Some("8 8 8 8".into()),
            profile_background_opacity: Some(0.85),
            profile_retro_effect: Some(true),
            profile_color_scheme_json: Some("{\"foreground\":\"#fff\"}".into()),
            ..Default::default()
        };

        let e = RecentlyClosedEntry::from_session(&s, Timestamp::never(), fresh_ids());

        assert_eq!(e.profile_font_family.as_deref(), Some("Cascadia"));
        assert_eq!(e.profile_font_size, Some(16));
        assert_eq!(e.profile_font_weight.as_deref(), Some("bold"));
        assert_eq!(e.profile_font_ligatures, Some(true));
        assert_eq!(e.profile_cursor_shape.as_deref(), Some("underline"));
        assert_eq!(e.profile_cursor_blink, Some(false));
        assert_eq!(e.profile_padding.as_deref(), Some("8 8 8 8"));
        assert_eq!(e.profile_background_opacity, Some(0.85));
        assert_eq!(e.profile_retro_effect, Some(true));
        assert_eq!(
            e.profile_color_scheme_json.as_deref(),
            Some("{\"foreground\":\"#fff\"}")
        );
    }

    #[test]
    fn from_session_deep_copies_run_commands_with_fresh_ids() {
        let mut s = ShellSession::default();
        s.run_commands.push(RunCommandItem {
            id: "original-id".into(),
            label: "Test".into(),
            command_line: "dotnet test".into(),
            is_default: true,
            mode: RunMode::PowerShell,
            post_run_url: Some("http://localhost:5173".into()),
        });

        let mut e = RecentlyClosedEntry::from_session(&s, Timestamp::never(), fresh_ids());

        assert_eq!(e.run_commands.len(), 1);
        let copy = &mut e.run_commands[0];
        assert_ne!(copy.id, "original-id"); // fresh id
        assert_eq!(copy.label, "Test");
        assert_eq!(copy.command_line, "dotnet test");
        assert!(copy.is_default);
        assert_eq!(copy.mode, RunMode::PowerShell);
        assert_eq!(copy.post_run_url.as_deref(), Some("http://localhost:5173"));
        // Mutating the copy must NOT touch the source.
        copy.label = "MUTATED".into();
        assert_eq!(s.run_commands[0].label, "Test");
    }

    #[test]
    fn subtitle_remote_session_returns_user_at_host() {
        let e = RecentlyClosedEntry {
            kind: SessionKind::Ssh,
            ssh_user: "bob".into(),
            ssh_host: "dev.local".into(),
            working_folder: r"C:\should-be-ignored".into(),
            ..Default::default()
        };
        assert_eq!(e.subtitle(), "bob@dev.local");
    }

    #[test]
    fn subtitle_remote_session_without_user_returns_host_only() {
        let e = RecentlyClosedEntry {
            kind: SessionKind::Ssh,
            ssh_host: "dev.local".into(),
            ..Default::default()
        };
        assert_eq!(e.subtitle(), "dev.local");
    }

    #[test]
    fn subtitle_local_session_returns_working_folder() {
        let e = RecentlyClosedEntry {
            working_folder: r"C:\proj".into(),
            ..Default::default()
        };
        assert_eq!(e.subtitle(), r"C:\proj");
    }

    #[test]
    fn subtitle_wsl_session() {
        let e = RecentlyClosedEntry {
            kind: SessionKind::Wsl,
            wsl_distro: "Ubuntu".into(),
            wsl_working_folder: "/srv".into(),
            ..Default::default()
        };
        assert_eq!(e.subtitle(), "Ubuntu: /srv");
        let home = RecentlyClosedEntry {
            kind: SessionKind::Wsl,
            wsl_distro: "Ubuntu".into(),
            ..Default::default()
        };
        assert_eq!(home.subtitle(), "Ubuntu");
    }
}
