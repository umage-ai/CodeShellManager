//! Loads `state.json` files written by the real C# app (System.Text.Json, .NET 10 models) and
//! checks that csm-core reads them and writes them back equivalently.
//!
//! `fixtures/state_realistic.json` was produced by serialising an `AppState` built from the C#
//! `Models/*.cs` classes themselves (copied into a scratch console project) with the exact
//! options `StateService` uses (`WriteIndented`, `WhenWritingNull`). It covers a Local session
//! with every appearance override and two run commands, an SSH session (which writes the
//! legacy `IsRemote: true` compatibility key), a dormant WSL session, groups, per-group
//! layouts, both recently-closed shapes, window placement in both generations and non-default
//! settings. `fixtures/state_legacy.json` is what an older build (before `Kind`) wrote.

#![allow(clippy::unwrap_used, clippy::expect_used)] // tests may unwrap

use csm_core::models::{AppState, GroupDisplayMode, ProgramStatusProbeMode, RunMode, SessionKind, SessionStatus, SidebarActionIconsMode};
use csm_core::session_manager::SessionManager;
use csm_core::state::{self, LoadSource};
use csm_core::time::Timestamp;

const REALISTIC: &str = include_str!("fixtures/state_realistic.json");
const LEGACY: &str = include_str!("fixtures/state_legacy.json");

fn load_realistic() -> AppState {
    let loaded = state::load(Some(REALISTIC), None);
    assert_eq!(loaded.source, LoadSource::Primary);
    loaded.state
}

#[test]
fn reads_a_realistic_csharp_state_file() {
    let s = load_realistic();

    assert_eq!(s.sessions.len(), 3);
    assert_eq!(s.groups.len(), 2);
    assert_eq!(s.last_layout, "TwoByTwo");
    assert_eq!(s.group_layouts.get("g-claude").map(String::as_str), Some("TwoByTwo"));
    assert_eq!(s.group_layouts.get("__ALL__").map(String::as_str), Some("Single"));
    assert!(!s.window_maximized);

    // Local session with overrides and run commands.
    let local = &s.sessions[0];
    assert_eq!(local.id, "11111111-1111-1111-1111-111111111111");
    assert_eq!(local.name, "CodeShellManager");
    assert_eq!(local.working_folder, r"C:\Users\Morten\source\repos\umage-ai\CodeShellManager");
    assert_eq!((local.command.as_str(), local.args.as_str()), ("claude", "--model claude-opus-4-6"));
    assert_eq!(local.group_id, "g-claude");
    assert_eq!(local.color_override.as_deref(), Some("#a6e3a1"));
    assert_eq!(local.status, SessionStatus::Running);
    assert_eq!(local.kind, SessionKind::Local);
    assert!(!local.is_dormant);
    assert_eq!(local.created_at.epoch_ms(), Timestamp::from_text("2026-11-01T08:30:15.123Z").epoch_ms());
    assert_eq!(local.created_at.as_str(), "2026-11-01T08:30:15.123Z");
    assert_eq!(local.profile_font_family.as_deref(), Some("Cascadia Mono"));
    assert_eq!(local.profile_font_size, Some(15));
    assert_eq!(local.profile_font_ligatures, Some(false));
    assert_eq!(local.profile_cursor_shape.as_deref(), Some("bar"));
    assert_eq!(local.profile_background_opacity, Some(0.85));
    assert_eq!(local.profile_color_scheme_json.as_deref(), Some(r##"{"background":"#1e1e2e","foreground":"#cdd6f4"}"##));
    assert_eq!(local.run_commands.len(), 2);
    assert!(local.run_commands[0].is_default);
    assert_eq!(local.run_commands[0].mode, RunMode::Process);
    assert_eq!(local.run_commands[1].mode, RunMode::PowerShell);
    assert_eq!(local.run_commands[1].post_run_url.as_deref(), Some("http://localhost:5173"));

    // SSH session: Kind 1, plus the legacy key, which must not change the answer.
    let ssh = &s.sessions[1];
    assert_eq!(ssh.kind, SessionKind::Ssh);
    assert!(ssh.is_remote());
    assert_eq!((ssh.ssh_user.as_str(), ssh.ssh_host.as_str(), ssh.ssh_port), ("alice", "dev.example.com", 2222));
    assert_eq!(ssh.ssh_remote_folder, "/srv/app");
    assert_eq!(ssh.folder_short(), "dev.example.com");
    assert_eq!(ssh.accent_key(), "alice@dev.example.com");

    // Dormant WSL session.
    let wsl = &s.sessions[2];
    assert_eq!(wsl.kind, SessionKind::Wsl);
    assert!(wsl.is_dormant);
    assert_eq!(wsl.status, SessionStatus::Exited);
    assert_eq!(wsl.working_folder, r"\\wsl$\Ubuntu\home\alice\proj");
    assert_eq!(wsl.default_display_name(), "Ubuntu: proj");

    // Groups (including a non-ASCII name and a collapsed one).
    assert_eq!(s.groups[1].name, "Ops \u{e9}");
    assert!(!s.groups[1].is_expanded);

    // Recently closed: an SSH entry carrying the legacy key, and a local one.
    assert_eq!(s.recently_closed.len(), 2);
    assert_eq!(s.recently_closed[0].kind, SessionKind::Ssh);
    assert_eq!(s.recently_closed[0].subtitle(), "bob@old.example.com");
    assert_eq!(s.recently_closed[0].run_commands.len(), 1);
    assert_eq!(s.recently_closed[1].subtitle(), r"C:\src\thing");

    // Window placement in both generations.
    let placement = s.last_placement.unwrap();
    assert_eq!((placement.left, placement.top, placement.width, placement.height, placement.dpi), (2200, 300, 2400, 1500, 144));
    let bounds = s.last_normal_bounds.unwrap();
    assert!((bounds.left - 1466.6666666666667).abs() < 1e-9 && bounds.width == 1600.0);

    // Settings: non-defaults survive, everything unmentioned keeps its default.
    let st = &s.settings;
    assert_eq!(st.program_status_probe, ProgramStatusProbeMode::On);
    assert_eq!(st.group_display_mode, GroupDisplayMode::InlineHeaders);
    assert_eq!(st.sidebar_action_icons_mode, SidebarActionIconsMode::Always);
    assert_eq!((st.terminal_letter_spacing, st.claude_launch_stagger_ms), (0.5, 1500));
    assert!(st.open_with_seeded && st.auto_restore_sessions && st.warn_on_close_when_busy);
    assert_eq!(st.open_with.len(), 1);
    assert!(st.open_with[0].is_default);
    assert_eq!(st.open_with[0].local_args.as_deref(), Some("{folder}"));
    assert!(st.open_with[0].wsl_args.as_deref().unwrap().contains("{distro:uri}"));
    assert_eq!(st.open_with[0].ssh_args, None);
    assert_eq!(st.launch_commands.len(), 8);
}

#[test]
fn rewriting_the_realistic_file_is_equivalent_to_what_csharp_wrote() {
    let s = load_realistic();
    let ours = s.to_json().unwrap();

    // Same values (number-for-number, including the IsRemote compatibility key on SSH)...
    let theirs: serde_json::Value = serde_json::from_str(REALISTIC).unwrap();
    let ours_value: serde_json::Value = serde_json::from_str(&ours).unwrap();
    assert_eq!(ours_value, theirs);

    // ...and the same key order, line by line (System.Text.Json writes properties in
    // declaration order; an older build or a human diffing the file would notice a shuffle).
    assert_eq!(key_sequence(&ours), key_sequence(REALISTIC));

    // Writing is a fixed point: loading what we wrote and writing again changes nothing.
    let again = state::parse(&ours).unwrap().to_json().unwrap();
    assert_eq!(again, ours);
}

/// The object keys in file order: lines of the form `   "Key": ...` (pretty-printed, one
/// property per line, so string values containing `":` cannot be mistaken for keys).
fn key_sequence(json: &str) -> Vec<String> {
    json.lines()
        .filter_map(|l| {
            let t = l.trim_start();
            let rest = t.strip_prefix('"')?;
            let end = rest.find("\":")?;
            let key = &rest[..end];
            key.chars().all(|c| c.is_ascii_alphanumeric()).then(|| key.to_owned())
        })
        .collect()
}

#[test]
fn reads_a_legacy_file_written_before_kind_existed() {
    let s = state::parse(LEGACY).unwrap();
    assert_eq!(s.sessions.len(), 2);
    assert_eq!(s.sessions[0].kind, SessionKind::Ssh); // IsRemote: true folded into Kind
    assert_eq!(s.sessions[1].kind, SessionKind::Local); // IsRemote: false
    assert_eq!(s.sessions[0].ssh_host, "old.host");
    assert_eq!(s.sessions[1].working_folder, r"C:\proj");
    assert_eq!(s.last_layout, "SixColumn");

    // Rewriting it emits Kind and keeps the compatibility key for the SSH session only.
    let out = s.to_json().unwrap();
    assert_eq!(out.matches("\"IsRemote\": true").count(), 1);
    assert!(!out.contains("\"IsRemote\": false"));
    assert!(out.contains("\"Kind\": 1"));

    // The session manager then backfills LastActivityAt and drops the old auto-created group.
    let mut s = s;
    let mut m = SessionManager::new();
    m.load_from_state(&mut s);
    assert_eq!(m.sessions()[0].last_activity_at, m.sessions()[0].created_at);
    assert!(m.groups().is_empty());
    assert!(s.settings.legacy_default_group_cleared);
}

#[test]
fn a_torn_realistic_file_falls_back_to_a_good_backup() {
    let torn = &REALISTIC[..REALISTIC.len() / 2];
    let loaded = state::load(Some(torn), Some(REALISTIC));
    assert_eq!(loaded.source, LoadSource::Backup);
    assert_eq!(loaded.state.sessions.len(), 3);
}

#[test]
fn every_truncation_of_the_realistic_file_is_survivable() {
    // A write interrupted at any byte must never panic the loader.
    for cut in (0..REALISTIC.len()).step_by(37) {
        if let Some(prefix) = REALISTIC.get(..cut) {
            let _ = state::load(Some(prefix), None);
            let _ = state::read_window_state(Some(prefix), None);
        }
    }
}
