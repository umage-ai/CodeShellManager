//! The pure half of C# `StateService`: turning `state.json` text into an [`AppState`].
//!
//! No file is read or written here. The caller reads the primary file and its `.bak` and hands
//! over the text; the atomic write (`.tmp` + replace, one backup generation, serialised saves)
//! stays with the I/O layer. What lives here is everything that must behave identically to the
//! C# loader: the primary-then-backup fallback, "unreadable means empty, never an error", and
//! `Normalize` (legacy `IsRemote` folded into `Kind`).

use serde::Deserialize;

use crate::models::{AppState, WindowBounds, WindowPlacementState};

/// Where a loaded state came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadSource {
    /// The primary `state.json`.
    Primary,
    /// The primary was unreadable and the `.bak` was used (C# logs "recovered state from backup").
    Backup,
    /// Nothing on disk at all: an ordinary first run.
    FirstRun,
    /// Something was on disk but none of it was usable; starting empty (C# logs this loudly,
    /// because it is indistinguishable from a first run for the user).
    Unreadable,
}

/// The result of [`load`]: the state and where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded {
    pub state: AppState,
    pub source: LoadSource,
}

/// Why a state file could not be used. Only ever logged by callers; [`load`] never fails.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("state file is not valid state JSON: {0}")]
    Json(#[from] serde_json::Error),
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Parses one state file's text and normalises it. Fails on malformed JSON and on JSON of the
/// wrong shape (an array, a string), like `JsonSerializer.Deserialize<AppState>` does.
pub fn parse(text: &str) -> Result<AppState, StateError> {
    let state: AppState = serde_json::from_str(strip_bom(text))?;
    Ok(normalize(state))
}

/// C# `StateService.Normalize`. Explicit `null` collections were already read as empty by the
/// model's deserialisers (`null_default`); what remains is the legacy-field migration, which
/// lives here, in the loader, so the models stay free of deserialisation-order tricks.
///
/// Not done here: C# also runs `OpenWithService.Sanitize` over the Open-with list. That needs
/// the open-with rules (programs on disk) and belongs to the crate that ports them; callers
/// must apply it after loading.
pub fn normalize(mut state: AppState) -> AppState {
    for session in &mut state.sessions {
        session.migrate_legacy_fields();
    }
    for entry in &mut state.recently_closed {
        entry.migrate_legacy_fields();
    }
    state
}

/// C# `StateService.LoadAsync` over already-read text: primary first, then the backup; a first
/// run (neither exists) is not a failure; otherwise an empty state, never an error.
///
/// `None` means the file does not exist; `Some(text)` means it exists (and may be torn).
pub fn load(primary: Option<&str>, backup: Option<&str>) -> Loaded {
    if let Some(Ok(state)) = primary.map(parse) {
        return Loaded {
            state,
            source: LoadSource::Primary,
        };
    }
    if primary.is_none() && backup.is_none() {
        return Loaded {
            state: AppState::default(),
            source: LoadSource::FirstRun,
        };
    }
    if let Some(Ok(state)) = backup.map(parse) {
        return Loaded {
            state,
            source: LoadSource::Backup,
        };
    }
    Loaded {
        state: AppState::default(),
        source: LoadSource::Unreadable,
    }
}

/// The window fields of the state file, and nothing else (C# `StateService.SavedWindowState`).
#[derive(Debug, Clone, PartialEq, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct SavedWindowState {
    pub last_placement: Option<WindowPlacementState>,
    pub last_normal_bounds: Option<WindowBounds>,
    pub window_maximized: bool,
}

/// C# `StateService.TryReadWindowState` over already-read text: primary, then backup, first
/// one that parses; `None` on a first run or when neither parses. Never fails.
pub fn read_window_state(primary: Option<&str>, backup: Option<&str>) -> Option<SavedWindowState> {
    [primary, backup]
        .into_iter()
        .flatten()
        .find_map(|text| serde_json::from_str::<SavedWindowState>(strip_bom(text)).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{RunMode, SessionKind, ShellSession};

    const TORN: &str = "{\"Sessions\":[{\"Id\":\"tru";

    fn with_sessions(ids: &[&str]) -> AppState {
        AppState {
            sessions: ids
                .iter()
                .map(|id| ShellSession {
                    id: (*id).into(),
                    working_folder: format!("C:\\{id}"),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    fn load_state(json: &str) -> AppState {
        parse(json).unwrap()
    }

    // ---- StateServiceTests: window state ----

    #[test]
    fn read_window_state_first_run_is_none() {
        assert_eq!(read_window_state(None, None), None);
    }

    #[test]
    fn read_window_state_round_trips_placement_and_legacy_bounds() {
        let mut s = with_sessions(&["a"]);
        s.window_maximized = true;
        s.last_normal_bounds = Some(WindowBounds {
            left: 10.0,
            top: 20.0,
            width: 800.0,
            height: 600.0,
        });
        s.last_placement = Some(WindowPlacementState {
            left: 2000,
            top: 100,
            width: 1500,
            height: 900,
            dpi: 144,
        });
        let json = s.to_json().unwrap();

        let w = read_window_state(Some(&json), None).unwrap();
        assert!(w.window_maximized);
        assert_eq!(w.last_normal_bounds.unwrap().width, 800.0);
        let p = w.last_placement.unwrap();
        assert_eq!(
            (p.left, p.top, p.width, p.height, p.dpi),
            (2000, 100, 1500, 900, 144)
        );
    }

    #[test]
    fn read_window_state_old_file_without_placement_still_reads_legacy_bounds() {
        let json = r#"{ "Sessions": [], "LastNormalBounds": { "Left": 1, "Top": 2, "Width": 1280, "Height": 800 }, "WindowMaximized": false }"#;
        let w = read_window_state(Some(json), None).unwrap();
        assert_eq!(w.last_placement, None);
        assert_eq!(w.last_normal_bounds.unwrap().width, 1280.0);
    }

    #[test]
    fn read_window_state_falls_back_to_backup_when_primary_is_torn() {
        let w = read_window_state(
            Some("{ \"Sessions\": [ { \"Id\": "),
            Some(r#"{ "WindowMaximized": true }"#),
        );
        assert!(w.unwrap().window_maximized);
    }

    // ---- StateServiceTests: save round trip, recovery, null tolerance, back-compat ----

    #[test]
    fn save_round_trips_sessions() {
        let original = with_sessions(&["a", "b"]);
        let back = parse(&original.to_json().unwrap()).unwrap();
        assert_eq!(
            back.sessions
                .iter()
                .map(|s| s.id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(back.sessions[0].working_folder, r"C:\a");
        assert_eq!(back, original);
    }

    #[test]
    fn load_torn_primary_recovers_from_backup() {
        let good = with_sessions(&["good"]).to_json().unwrap();
        let loaded = load(Some(TORN), Some(&good));
        assert_eq!(loaded.source, LoadSource::Backup);
        assert_eq!(loaded.state.sessions.len(), 1);
        assert_eq!(loaded.state.sessions[0].id, "good");
    }

    #[test]
    fn load_torn_primary_and_no_backup_returns_empty_rather_than_failing() {
        let loaded = load(Some(TORN), None);
        assert_eq!(loaded.source, LoadSource::Unreadable);
        assert!(loaded.state.sessions.is_empty());
    }

    #[test]
    fn load_missing_file_returns_empty_first_run() {
        let loaded = load(None, None);
        assert_eq!(loaded.source, LoadSource::FirstRun);
        assert!(loaded.state.sessions.is_empty());
        assert!(loaded.state.groups.is_empty());
    }

    #[test]
    fn load_good_primary_wins_over_backup() {
        let newer = with_sessions(&["newer"]).to_json().unwrap();
        let older = with_sessions(&["older"]).to_json().unwrap();
        let loaded = load(Some(&newer), Some(&older));
        assert_eq!(
            (loaded.source, loaded.state.sessions[0].id.as_str()),
            (LoadSource::Primary, "newer")
        );
    }

    #[test]
    fn load_missing_primary_but_present_backup_uses_backup() {
        let backup = with_sessions(&["b"]).to_json().unwrap();
        assert_eq!(load(None, Some(&backup)).source, LoadSource::Backup);
    }

    #[test]
    fn load_explicit_null_lists_are_coerced_to_empty() {
        // Property initialisers only apply when a key is absent, not when it is explicitly
        // null; an import can hand the loader any JSON file.
        let loaded = load(
            Some(
                r#"{"Sessions":null,"Groups":null,"RecentlyClosed":null,"GroupLayouts":null,"Settings":null,"LastLayout":"Single"}"#,
            ),
            None,
        );
        assert_eq!(loaded.source, LoadSource::Primary);
        assert!(loaded.state.sessions.is_empty());
        assert!(loaded.state.groups.is_empty());
        assert!(loaded.state.recently_closed.is_empty());
        assert!(loaded.state.group_layouts.is_empty());
        assert_eq!(loaded.state.settings, crate::models::AppSettings::default());
    }

    #[test]
    fn load_valid_json_of_the_wrong_shape_returns_empty() {
        for json in [r#"["not","an","appstate"]"#, "null", "42", r#""str""#] {
            let loaded = load(Some(json), None);
            assert_eq!(loaded.source, LoadSource::Unreadable, "{json}");
            assert!(loaded.state.sessions.is_empty());
        }
    }

    #[test]
    fn load_pre_v060_file_defaults_new_fields_safely() {
        let loaded = load(
            Some(
                r#"{"Sessions":[{"Id":"a","WorkingFolder":"C:\\p","Command":"claude",
              "RunCommands":[{"Id":"r","Label":"Run","CommandLine":"npm run dev","IsDefault":true}]}],
             "Groups":[],"LastLayout":"SixColumn"}"#,
            ),
            None,
        );
        let state = loaded.state;
        assert_eq!(state.sessions.len(), 1);
        assert!(state.recently_closed.is_empty()); // key absent pre-0.6.0
        assert_eq!(state.sessions[0].run_commands.len(), 1);
        let rc = &state.sessions[0].run_commands[0];
        assert_eq!(rc.mode, RunMode::Process); // historical behaviour
        assert_eq!(rc.post_run_url, None);
        assert_eq!(state.last_layout, "SixColumn");
    }

    #[test]
    fn a_utf8_bom_is_ignored() {
        let json = format!("\u{feff}{}", with_sessions(&["a"]).to_json().unwrap());
        assert_eq!(parse(&json).unwrap().sessions.len(), 1);
    }

    // ---- ShellSessionMigrationTests ----

    #[test]
    fn normalize_legacy_is_remote_true_promotes_kind_to_ssh() {
        let legacy = r#"{ "Sessions": [ { "IsRemote": true, "SshUser": "alice", "SshHost": "dev.example.com" } ] }"#;
        let s = &load_state(legacy).sessions[0];
        assert_eq!(s.kind, SessionKind::Ssh);
        assert!(s.is_remote());
        // The compatibility key is computed from Kind on write, so it reads true again.
        let mut copy = s.clone();
        copy.sync_legacy_is_remote();
        assert_eq!(copy.legacy_is_remote, Some(true));
    }

    #[test]
    fn normalize_legacy_is_remote_false_keeps_kind_local() {
        let legacy = r#"{ "Sessions": [ { "IsRemote": false, "WorkingFolder": "C:\\proj" } ] }"#;
        let s = &load_state(legacy).sessions[0];
        assert_eq!(s.kind, SessionKind::Local);
        assert!(!s.is_remote());
    }

    #[test]
    fn normalize_kind_wsl_with_stray_legacy_false_stays_wsl() {
        let mixed =
            r#"{ "Sessions": [ { "Kind": 2, "IsRemote": false, "WslDistro": "Ubuntu" } ] }"#;
        assert_eq!(load_state(mixed).sessions[0].kind, SessionKind::Wsl);
    }

    #[test]
    fn normalize_kind_wsl_with_stray_legacy_true_kind_wins() {
        // Kind is authoritative once present; a stale IsRemote must not clobber it.
        let mixed = r#"{ "Sessions": [ { "Kind": 2, "IsRemote": true, "WslDistro": "Ubuntu" } ] }"#;
        assert_eq!(load_state(mixed).sessions[0].kind, SessionKind::Wsl);
    }

    #[test]
    fn normalize_recently_closed_legacy_is_remote_promotes_to_ssh() {
        let legacy = r#"{ "RecentlyClosed": [ { "IsRemote": true, "SshHost": "h" } ] }"#;
        let e = &load_state(legacy).recently_closed[0];
        assert_eq!(e.kind, SessionKind::Ssh);
        assert!(e.is_remote());
    }

    #[test]
    fn serialize_writes_legacy_is_remote_only_for_ssh_and_no_computed_properties() {
        // origin/main persists only "IsRemote" and has no Kind at all, so an older build
        // reading this file must still see an SSH session as remote: "IsRemote":true IS
        // written for Ssh. Local/Wsl omit the key entirely.
        let ssh = ShellSession {
            kind: SessionKind::Ssh,
            ssh_host: "h".into(),
            ..Default::default()
        };
        let json = ssh.to_json().unwrap();
        assert!(json.contains("\"IsRemote\":true"), "{json}");
        for computed in ["FullCommandLine", "FolderShort", "AccentKey"] {
            assert!(
                !json.contains(computed),
                "{computed} must not be serialised"
            );
        }
        assert!(json.contains("\"Kind\":1"), "{json}");

        let local = ShellSession {
            kind: SessionKind::Local,
            ..Default::default()
        };
        assert!(!local.to_json().unwrap().contains("\"IsRemote\""));
        let wsl = ShellSession {
            kind: SessionKind::Wsl,
            wsl_distro: "Ubuntu".into(),
            ..Default::default()
        };
        assert!(!wsl.to_json().unwrap().contains("\"IsRemote\""));
    }

    #[test]
    fn serialize_incomplete_ssh_session_does_not_fail() {
        // Regression: FullCommandLine used to be serialised and threw on a blank host, which
        // made every state save fail after a bad edit.
        let s = ShellSession {
            kind: SessionKind::Ssh,
            ssh_host: String::new(),
            ..Default::default()
        };
        assert!(s.to_json().is_ok());
        assert_eq!(s.full_command_line().as_deref(), Some("ssh"));
    }

    #[test]
    fn roundtrip_ssh_session_through_computed_legacy_is_remote_keeps_kind_ssh() {
        let original = ShellSession {
            kind: SessionKind::Ssh,
            ssh_host: "dev.example.com".into(),
            ..Default::default()
        };
        let wrapped = format!("{{ \"Sessions\": [ {} ] }}", original.to_json().unwrap());
        let revived = &load_state(&wrapped).sessions[0];
        assert_eq!(revived.kind, SessionKind::Ssh);
        assert!(revived.is_remote());
        assert_eq!(revived.ssh_host, "dev.example.com");
    }

    #[test]
    fn roundtrip_new_format_preserves_kind() {
        let original = ShellSession {
            kind: SessionKind::Wsl,
            wsl_distro: "Debian".into(),
            wsl_user: "bob".into(),
            wsl_working_folder: "/srv/app".into(),
            ..Default::default()
        };
        let revived: ShellSession = serde_json::from_str(&original.to_json().unwrap()).unwrap();
        assert_eq!(revived.kind, SessionKind::Wsl);
        assert_eq!(
            (
                revived.wsl_distro.as_str(),
                revived.wsl_user.as_str(),
                revived.wsl_working_folder.as_str()
            ),
            ("Debian", "bob", "/srv/app")
        );
    }

    // ---- ShellSessionRunCommandsTests ----

    #[test]
    fn run_commands_default_to_an_empty_list() {
        assert!(ShellSession::default().run_commands.is_empty());
    }

    #[test]
    fn run_commands_round_trip_through_json() {
        use crate::models::RunCommandItem;
        let s = ShellSession {
            run_commands: vec![
                RunCommandItem {
                    id: "a".into(),
                    label: "run".into(),
                    command_line: "dotnet run".into(),
                    is_default: true,
                    ..Default::default()
                },
                RunCommandItem {
                    id: "b".into(),
                    label: "test".into(),
                    command_line: "dotnet test".into(),
                    is_default: false,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let back: ShellSession = serde_json::from_str(&s.to_json().unwrap()).unwrap();
        assert_eq!(back.run_commands.len(), 2);
        assert_eq!(back.run_commands[0].id, "a");
        assert!(back.run_commands[0].is_default);
        assert_eq!(back.run_commands[1].command_line, "dotnet test");
    }

    // ---- hostile input ----

    #[test]
    fn hostile_state_files_never_panic() {
        let blobs = [
            "",
            "{",
            "\0\0\0",
            r#"{"Sessions":[null]}"#,
            r#"{"Sessions":[{"Kind":"banana","Status":9999999999,"SshPort":"x"}]}"#,
            r#"{"Sessions":[{"Kind":1e400}]}"#,
            r#"{"LastPlacement":{"Left":2147483648}}"#,
            r#"{"Settings":{"Theme":null,"LaunchCommands":null,"OpenWith":[null]}}"#,
        ];
        for b in blobs {
            let _ = load(Some(b), Some(b));
            let _ = read_window_state(Some(b), Some(b));
        }
    }

    #[test]
    fn unknown_kind_number_or_name_degrades_to_the_default_variant() {
        let s = &load_state(r#"{"Sessions":[{"Kind":7},{"Kind":"Wsl"},{"Kind":"nope"}]}"#).sessions;
        assert_eq!(
            s.iter().map(|x| x.kind).collect::<Vec<_>>(),
            [SessionKind::Local, SessionKind::Wsl, SessionKind::Local]
        );
    }
}
