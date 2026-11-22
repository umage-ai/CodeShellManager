//! JSON round trips of the wire types, and the wire shape a peer reads.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use csm_peer_proto::{
    ClosedView, Event, GroupInfo, GroupView, OpenWithEntry, OpenWithTarget, RunPhase, RunView, SessionView, Snapshot, Status, WIRE_VERSION,
};
use serde_json::{Value, json};

fn session(id: &str, name: &str, status: Status, label: &str) -> SessionView {
    SessionView {
        id: id.into(),
        name: name.into(),
        kind: "local".into(),
        folder: format!("C:\\Github\\{name}"),
        folder_short: name.into(),
        annotation: None,
        owner_addon: None,
        command: "claude".into(),
        group_id: "g1".into(),
        accent: "#3b82f6".into(),
        accent_override: false,
        status,
        status_label: label.into(),
        progress: Some(40),
        status_message: Some("Editing parser.rs".into()),
        live: true,
        asleep: false,
        exit_code: None,
        branch: Some("main".into()),
        git_dirty: true,
        git_upstream: Some("origin/main".into()),
        git_ahead: Some(2),
        git_behind: Some(0),
        git_changes: 3,
        git_summary: Some("main → origin/main · ↑2 · 3 changed".into()),
        worktree_of: None,
        repo_root: Some("C:\\Github\\repo".into()),
        git_busy: false,
        title: Some("claude: parser".into()),
        is_claude: true,
        claude_session_id: Some("0f7c2a9e-1111-2222-3333-444455556666".into()),
        reports_status: true,
        remote_control: true,
        remote_control_url: Some("https://claude.ai/code/session_example".into()),
        last_activity_ms: Some(1_790_000_000_000),
        inbox_since_ms: Some(1_790_000_100_000),
        status_since_ms: Some(1_790_000_050_000),
        project: "repo:C:\\Github\\repo".into(),
        project_name: "repo".into(),
        backend: Some("bundled ConPTY".into()),
        runs: vec![
            RunView {
                id: "r1".into(),
                label: "Test".into(),
                command_line: "cargo test --workspace".into(),
                is_default: true,
                mode: "process".into(),
                post_run_url: Some("http://localhost:3000".into()),
                phase: RunPhase::Failed,
                exit_code: Some(101),
                started_ms: Some(1_790_000_000_000),
                ended_ms: Some(1_790_000_030_000),
            },
            RunView {
                id: "r2".into(),
                label: "Build".into(),
                command_line: "cargo build".into(),
                is_default: false,
                mode: "powershell".into(),
                post_run_url: None,
                phase: RunPhase::Idle,
                exit_code: None,
                started_ms: None,
                ended_ms: None,
            },
        ],
        open_with: vec![OpenWithTarget {
            entry_id: "vscode".into(),
            label: "VS Code".into(),
            program: "code.cmd".into(),
            is_default: true,
            command: "code.cmd \"C:\\Github\\repo\"".into(),
        }],
        subagents_running: 1,
        subagents_total: 4,
    }
}

/// A snapshot with every kind of row a client draws: a live Claude session, an idle shell, an
/// asleep session, one group, an inbox entry, a closed session and an Open with entry.
fn realistic() -> Snapshot {
    let mut asleep = session("s3", "docs", Status::Asleep, "Asleep");
    asleep.live = false;
    asleep.asleep = true;
    asleep.progress = None;
    asleep.status_message = None;
    asleep.runs.clear();
    Snapshot {
        version: 42,
        sessions: vec![session("s1", "repo", Status::NeedsYouApproval, "Needs you: approval"), session("s2", "shell", Status::Idle, "Idle"), asleep],
        groups: vec![GroupInfo { id: "g1".into(), name: "Work".into(), expanded: true, torn_off: false, session_count: 3, needs_you: 1, unseen: 0 }],
        view: GroupView::Rail,
        rail_tab: Some("g1".into()),
        layout_key: "g1".into(),
        inbox: vec!["s1".into()],
        focused: Some("s1".into()),
        inbox_open: true,
        inbox_height: Some(180.5),
        recently_closed: vec![ClosedView { name: "old".into(), subtitle: "Ubuntu: src".into(), command: "wsl.exe -d Ubuntu".into() }],
        open_with: vec![OpenWithEntry {
            id: "vscode".into(),
            label: "VS Code".into(),
            executable: "code.cmd".into(),
            program: "code.cmd".into(),
            local_args: Some("--reuse-window".into()),
            wsl_args: None,
            ssh_args: None,
            is_default: true,
        }],
    }
}

#[test]
fn a_realistic_snapshot_round_trips_through_json() {
    let snap = realistic();
    let text = serde_json::to_string_pretty(&snap).unwrap();
    let back: Snapshot = serde_json::from_str(&text).unwrap();
    assert_eq!(back, snap);
    assert_eq!(back.session("s1").map(|s| s.status), Some(Status::NeedsYouApproval));
    assert!(back.session("nope").is_none());
}

#[test]
fn the_snapshot_json_has_the_engine_field_names_and_values() {
    let v: Value = serde_json::to_value(realistic()).unwrap();
    assert_eq!(v["version"], json!(42));
    assert_eq!(v["view"], json!("rail"));
    assert_eq!(v["inbox_height"], json!(180.5));
    let s1 = &v["sessions"][0];
    assert_eq!(s1["status"], json!("needs_you_approval"));
    assert_eq!(s1["status_label"], json!("Needs you: approval"));
    assert_eq!(s1["runs"][0]["phase"], json!("failed"));
    assert_eq!(s1["runs"][1]["phase"], json!("idle"));
    assert_eq!(s1["backend"], json!("bundled ConPTY"));
    assert_eq!(s1["open_with"][0]["command"], json!("code.cmd \"C:\\Github\\repo\""));
    assert_eq!(v["sessions"][2]["status"], json!("asleep"));
}

#[test]
fn every_status_and_group_view_has_its_engine_name() {
    let cases = [
        (Status::Starting, "starting"),
        (Status::Working, "working"),
        (Status::NeedsYouApproval, "needs_you_approval"),
        (Status::NeedsYouQuestion, "needs_you_question"),
        (Status::DoneUnseen, "done_unseen"),
        (Status::Error, "error"),
        (Status::Idle, "idle"),
        (Status::Asleep, "asleep"),
        (Status::Exited, "exited"),
    ];
    for (status, name) in cases {
        assert_eq!(serde_json::to_value(status).unwrap(), json!(name));
        assert_eq!(serde_json::from_value::<Status>(json!(name)).unwrap(), status);
    }
    for (view, name) in [(GroupView::Rail, "rail"), (GroupView::Headers, "headers"), (GroupView::Flat, "flat")] {
        assert_eq!(serde_json::from_value::<GroupView>(json!(name)).unwrap(), view);
    }
}

#[test]
fn every_event_reads_the_engine_tag_and_fields() {
    let cases = vec![
        (json!({"event": "session_changed", "session_id": "s1"}), Event::SessionChanged { session_id: "s1".into() }),
        (json!({"event": "layout_changed"}), Event::LayoutChanged),
        (json!({"event": "inbox_changed"}), Event::InboxChanged),
        (json!({"event": "toast", "message": "Started claude."}), Event::Toast { message: "Started claude.".into() }),
        (json!({"event": "consent_resolved", "request_id": 9}), Event::ConsentResolved { request_id: 9 }),
        (
            json!({"event": "quit_progress", "session_id": "s1", "phase": "clean", "elapsed_ms": 800}),
            Event::QuitProgress { session_id: "s1".into(), phase: "clean".into(), elapsed_ms: 800 },
        ),
        (json!({"event": "quit_ready"}), Event::QuitReady),
        (json!({"event": "setting_changed", "key": "ui_zoom", "value": 1.1}), Event::SettingChanged { key: "ui_zoom".into(), value: json!(1.1) }),
        (json!({"event": "screen_focus_requested", "instance": "i-1"}), Event::ScreenFocusRequested { instance: "i-1".into() }),
    ];
    for (text, expected) in cases {
        let got: Event = serde_json::from_value(text.clone()).unwrap();
        assert_eq!(got, expected, "{text}");
        assert_eq!(serde_json::to_value(&got).unwrap(), text, "{text} writes back");
    }
}

#[test]
fn a_newer_peer_may_add_fields_that_this_one_ignores() {
    let mut v = serde_json::to_value(realistic()).unwrap();
    v["a_future_field"] = json!([1, 2, 3]);
    v["sessions"][0]["a_future_session_field"] = json!({"x": true});
    let back: Snapshot = serde_json::from_value(v).unwrap();
    assert_eq!(back, realistic());
}

#[test]
fn the_wire_version_is_one() {
    assert_eq!(WIRE_VERSION, 1);
}
