//! Beta.1 small wins through the engine: broadcasting text to a group (#59), naming Claude
//! conversations after their session (#76), and the settings the window follows live (#193 UI
//! zoom, #149 close to tray).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::{H, osc7501};
use csm_engine::{Caller, Event, Status, ToolError};
use serde_json::json;

fn ext(session: Option<&str>) -> Caller {
    Caller::External { client: "test-mcp".into(), session_id: session.map(str::to_owned) }
}

/// A live session in `group`, its fake PTY and bracketed paste on.
fn member(h: &H, group: &str, command: &str) -> (String, std::sync::Arc<common::FakePty>) {
    let folder = h.dir.path().to_string_lossy().into_owned();
    let mut p = json!({"folder": folder, "command": command});
    if !group.is_empty() {
        p["group"] = json!(group);
    }
    let id = h.ok("session_new", p).data["id"].as_str().unwrap().to_owned();
    h.wait_status(&id, Status::Idle);
    let pty = h.pty_of(&id);
    pty.emit(b"\x1b[?2004h");
    (id, pty)
}

fn wait_for(what: &str, f: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn broadcast_pastes_into_every_live_member_and_skips_the_rest() {
    let h = H::new();
    let g = h.ok("group_new", json!({"name": "Agents"})).data["group_id"].as_str().unwrap().to_owned();
    let (idle, idle_pty) = member(&h, &g, "claude");
    let (busy, busy_pty) = member(&h, &g, "claude");
    let (asleep, _) = member(&h, &g, "pwsh");
    let (_outside, outside_pty) = member(&h, "", "pwsh");
    busy_pty.emit(&osc7501("state=working"));
    h.wait_status(&busy, Status::Working);
    h.ok("session_sleep", json!({"session_id": asleep}));

    let out = h.ok("session_send_text_many", json!({"group_id": g, "text": "git pull", "submit": true}));
    assert_eq!(out.data["sent"], json!([idle]));
    let skipped: Vec<&str> = out.data["skipped"].as_array().unwrap().iter().map(|s| s["session_id"].as_str().unwrap()).collect();
    assert_eq!(skipped, [busy.as_str(), asleep.as_str()]);
    assert!(out.message.contains("Sent to 1 sessions in Agents; skipped 2"), "{}", out.message);
    wait_for("Enter after the paste", || idle_pty.written_text().ends_with('\r'));
    assert!(idle_pty.written_text().contains("\x1b[200~git pull\x1b[201~\r"), "{:?}", idle_pty.written_text());
    assert!(!busy_pty.written_text().contains("git pull"), "mid-turn: skipped");
    assert!(!outside_pty.written_text().contains("git pull"), "not in the group");

    // force reaches the busy one too; session_ids works like a group.
    let out = h.ok("session_send_text_many", json!({"session_ids": [busy], "text": "stop", "force": true}));
    assert_eq!(out.data["sent"], json!([busy]));
    wait_for("the forced paste", || busy_pty.written_text().contains("stop"));

    // Exactly one of group_id / session_ids, and something to send.
    assert!(matches!(h.ui("session_send_text_many", json!({"text": "x"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("session_send_text_many", json!({"group_id": g, "session_ids": [idle], "text": "x"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("session_send_text_many", json!({"group_id": g, "text": ""})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("session_send_text_many", json!({"group_id": "nope", "text": "x"})), Err(ToolError::NotFound(_))));
}

#[test]
fn an_mcp_broadcast_asks_first_and_never_types_into_its_own_session() {
    let h = H::new();
    let g = h.ok("group_new", json!({"name": "Agents"})).data["group_id"].as_str().unwrap().to_owned();
    let (me, my_pty) = member(&h, &g, "claude");
    let (other, other_pty) = member(&h, &g, "claude");
    let events = h.engine.subscribe();
    let rx = h.engine.submit(ext(Some(&me)), "session_send_text_many", json!({"group_id": g, "text": "hello"}));
    let deadline = Instant::now() + Duration::from_secs(5);
    let request = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ConsentRequested { request }) => break request,
            Ok(_) if Instant::now() < deadline => continue,
            other => panic!("expected a consent request, got {other:?}"),
        }
    };
    assert_eq!(request["tool"], "session_send_text_many");
    assert!(rx.try_recv().is_err(), "must wait for the person");
    h.ok("consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"}));
    let out = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(out.data["sent"], json!([other]));
    assert_eq!(out.data["skipped"][0]["session_id"], me);
    wait_for("the paste", || other_pty.written_text().contains("hello"));
    assert!(!my_pty.written_text().contains("hello"));
}

#[test]
fn claude_sessions_are_named_after_their_session_when_the_setting_is_on() {
    let h = H::new();
    let folder = h.dir.path().to_string_lossy().into_owned();
    let start = |name: &str| {
        let id = h.ok("session_new", json!({"folder": folder, "command": "claude", "name": name})).data["id"].as_str().unwrap().to_owned();
        h.wait_status(&id, Status::Idle);
        h.pty_of(&id).argv.join(" ")
    };
    let off = start("Api work");
    assert!(!off.contains("--name"), "off by default: {off}");
    h.ok("settings_set", json!({"key": "PassSessionNameToClaude", "value": true}));
    let on = start("Api work; rm");
    assert!(on.contains("--name 'Api work rm'"), "{on}");
}

#[test]
fn settings_set_announces_changes_and_bounds_the_ui_zoom() {
    let h = H::new();
    let events = h.engine.subscribe();
    h.ok("settings_set", json!({"key": "UiZoomPercent", "value": 125}));
    assert_eq!(h.ok("settings_get", json!({"key": "ui_zoom_percent"})).data["value"], 125);
    for bad in [json!(79), json!(201), json!("big")] {
        assert!(matches!(h.ui("settings_set", json!({"key": "UiZoomPercent", "value": bad})), Err(ToolError::InvalidParams(_))), "{bad}");
    }
    assert_eq!(h.ok("settings_get", json!({"key": "UiZoomPercent"})).data["value"], 125, "a refused value changes nothing");
    h.ok("settings_set", json!({"key": "CloseToTray", "value": true}));
    h.ok("settings_set", json!({"key": "AnthropicApiKey", "value": "sk-secret"}));
    let seen: Vec<(String, serde_json::Value)> = events
        .try_iter()
        .filter_map(|e| match e {
            Event::SettingChanged { key, value } => Some((key, value)),
            _ => None,
        })
        .collect();
    assert_eq!(seen[0], ("UiZoomPercent".to_string(), json!(125)));
    assert_eq!(seen[1], ("CloseToTray".to_string(), json!(true)));
    assert_eq!(seen[2], ("AnthropicApiKey".to_string(), json!("(set)")), "a secret never goes out in an event");
}
