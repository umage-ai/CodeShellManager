//! Upgrading from CodeShellManager 0.x: `state_import_wpf` takes the 0.x settings that map onto
//! ours, and offers its Open with list for consent (`open_with_pending` /
//! `open_with_pending_answer`) instead of applying it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::Duration;

use common::{H, test_app};
use csm_core::models::{AppState, OpenWithEntry};
use csm_engine::{Caller, Event, Status, ToolError};
use serde_json::{Value, json};

fn ext() -> Caller {
    Caller::External { client: "test-mcp".into(), session_id: None }
}

fn abs(dir: &tempfile::TempDir, name: &str) -> String {
    dir.path().join(name).to_string_lossy().into_owned()
}

/// A 0.x state.json: two sessions, a group, settings, and an Open with list of a real program
/// (this test binary), a batch file and a program that isn't there.
fn wpf_state(dir: &tempfile::TempDir) -> String {
    let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    let (bat, body) = if cfg!(windows) { (abs(dir, "evil.cmd"), "@echo off\r\n") } else { (abs(dir, "evil.sh"), "#!/bin/sh\n") };
    std::fs::write(&bat, body).unwrap();
    let state = json!({
        "Sessions": [
            {"Id": "w1", "Name": "one", "Command": "claude", "WorkingFolder": "C:\\src\\one", "GroupId": "g1"},
            {"Id": "w2", "Name": "two", "Command": "pwsh", "WorkingFolder": "C:\\src\\two"}
        ],
        "Groups": [{"Id": "g1", "Name": "Client"}],
        "LastLayout": "TwoByTwo",
        "Settings": {
            "Theme": "light",
            "DefaultCommand": "codex",
            "LaunchCommands": ["codex", "claude", "pwsh"],
            "TerminalFontSize": 18,
            "ShowGitBranch": false,
            "AnthropicApiKey": "sk-0x",
            "OpenWithSeeded": true,
            "OpenWith": [
                {"Id": "code", "Label": "Real editor", "Executable": exe, "LocalArgs": "{folder}", "IsDefault": true},
                {"Id": "bat", "Label": "Batch", "Executable": bat, "LocalArgs": "{folder}"},
                {"Id": "gone", "Label": "Gone", "Executable": abs(dir, "gone.exe"), "LocalArgs": "{folder}"}
            ]
        }
    });
    let path = abs(dir, "wpf-state.json");
    std::fs::write(&path, serde_json::to_string_pretty(&state).unwrap()).unwrap();
    path
}

fn labels(v: &Value) -> Vec<String> {
    v["entries"].as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap().to_owned()).collect()
}

#[test]
fn a_0x_import_takes_its_settings_and_holds_its_editors_for_consent() {
    let mut app = test_app();
    app.settings.open_with_seeded = true;
    app.settings.open_with = vec![OpenWithEntry {
        id: "mine".into(),
        label: "Mine".into(),
        executable: if cfg!(windows) { r"C:\Mine\mine.exe".into() } else { "/opt/mine".into() },
        local_args: Some("{folder}".into()),
        is_default: true,
        ..OpenWithEntry::default()
    }];
    let h = H::start(app, |_| {});
    let path = wpf_state(&h.dir);
    let events = h.engine.subscribe();

    let out = h.ok("state_import_wpf", json!({"path": path}));
    assert_eq!(out.data["sessions"], 2);
    let changed: Vec<&str> = out.data["settings"]["changed"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    for key in ["Theme", "DefaultCommand", "LaunchCommands", "TerminalFontPoints", "ShowGitBranch"] {
        assert!(changed.contains(&key), "{key} not in {changed:?}");
    }
    assert_eq!(out.data["open_with_pending"], 2, "the batch file is dropped: {}", out.data);
    assert!(out.message.contains("waiting for your OK"), "{}", out.message);

    // The settings, as the app reads them.
    let s = h.ok("settings_get", json!({})).data;
    assert_eq!(s["Theme"], "light");
    assert_eq!(s["DefaultCommand"], "codex");
    assert_eq!(s["LaunchCommands"], json!(["codex", "claude", "pwsh"]));
    assert_eq!(s["TerminalFontPoints"], 18);
    assert_eq!(s["ShowGitBranch"], false);
    assert_eq!(s["AnthropicApiKey"], "", "the 0.x API key is not copied");
    // The UI follows them through events (the first-run import is not the UI's own call).
    let mut seen = Vec::new();
    while let Ok(e) = events.recv_timeout(Duration::from_millis(200)) {
        if let Event::SettingChanged { key, .. } = e {
            seen.push(key);
        }
    }
    assert!(seen.contains(&"Theme".to_owned()) && seen.contains(&"LaunchCommands".to_owned()), "{seen:?}");

    // Sessions arrive asleep; nothing runs, and the editors are NOT in the list yet.
    let v = h.engine.view();
    assert!(v.sessions.iter().all(|s| s.status == Status::Asleep));
    assert_eq!(h.pty_count(), 0);
    assert_eq!(labels(&h.ok("open_with_list", json!({})).data), ["Mine"]);
    assert_eq!(v.pending_open_with.len(), 2, "the snapshot carries the offer for the card");
    let pending = h.ok("open_with_pending", json!({}));
    assert_eq!(labels(&pending.data), ["Real editor", "Gone"]);
    assert!(pending.data["review"].as_str().unwrap().contains("Real editor"), "the commands are spelled out");

    // MCP clients may neither answer nor import (both decide what runs).
    let r = h.engine.call(ext(), "open_with_pending_answer", json!({"adopt_open_with": true}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    let r = h.engine.call(ext(), "state_import_wpf", json!({"path": path}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    assert!(h.engine.call(ext(), "open_with_pending", json!({})).is_ok(), "reading the offer is fine");

    // Don't allow: discarded, the list as it was.
    let no = h.ok("open_with_pending_answer", json!({"adopt_open_with": false}));
    assert_eq!(no.data["discarded"], 2);
    assert_eq!(labels(&h.ok("open_with_list", json!({})).data), ["Mine"]);
    assert!(h.engine.view().pending_open_with.is_empty(), "asked once");

    // The manual import offers them again (sessions merge by id: none new). Allow: added,
    // and 0.x's default becomes the default.
    let again = h.ok("state_import_wpf", json!({"path": path}));
    assert_eq!(again.data["sessions"], 0);
    assert_eq!(again.data["open_with_pending"], 2);
    let yes = h.ok("open_with_pending_answer", json!({"adopt_open_with": true}));
    assert_eq!(yes.data["added"], 2);
    let list = h.ok("open_with_list", json!({})).data;
    assert_eq!(labels(&list), ["Mine", "Real editor", "Gone"]);
    let defaults: Vec<&str> =
        list["entries"].as_array().unwrap().iter().filter(|e| e["is_default"] == true).map(|e| e["label"].as_str().unwrap()).collect();
    assert_eq!(defaults, ["Real editor"]);
    // Nothing left to ask; a third import offers only what isn't listed (nothing).
    assert_eq!(h.ok("open_with_pending_answer", json!({"adopt_open_with": true})).data["added"], 0);
    assert_eq!(h.ok("state_import_wpf", json!({"path": path})).data["open_with_pending"], 0);
    assert!(h.engine.view().pending_open_with.is_empty());
}

#[test]
fn garbage_settings_never_cost_the_sessions() {
    let h = H::new();
    let path = abs(&h.dir, "odd.json");
    let odd = json!({
        "Sessions": [{"Id": "w1", "Name": "one"}],
        "Settings": {"Theme": 7, "LaunchCommands": "claude", "TerminalFontSize": "big", "OpenWith": {"x": 1}, "OpenWithSeeded": true, "Brand": "new"}
    });
    std::fs::write(&path, odd.to_string()).unwrap();
    let before = h.ok("settings_get", json!({})).data;
    let out = h.ok("state_import_wpf", json!({"path": path}));
    assert_eq!(out.data["sessions"], 1);
    assert!(out.data["settings"]["changed"].as_array().unwrap().is_empty());
    assert!(out.data["settings"]["skipped"].as_array().unwrap().len() >= 5, "{}", out.data);
    assert_eq!(out.data["open_with_pending"], 0);
    assert_eq!(h.ok("settings_get", json!({})).data, before);
    // A Settings value that is not an object, and a file with no Settings at all.
    for text in [r#"{"Sessions": [], "Settings": [1, 2]}"#, r#"{"Sessions": []}"#, "\u{feff}{}"] {
        std::fs::write(&path, text).unwrap();
        h.ok("state_import_wpf", json!({"path": path}));
    }
    for text in ["not json", "[1]", r#"{"Sessions": 5}"#] {
        std::fs::write(&path, text).unwrap();
        assert!(matches!(h.ui("state_import_wpf", json!({"path": path})), Err(ToolError::Failed(_))), "{text}");
    }
}

#[test]
fn a_pending_offer_survives_a_restart_and_a_bad_entry_is_dropped() {
    let app =
        AppState { pending_open_with: vec![OpenWithEntry { id: "a".into(), label: "A".into(), ..OpenWithEntry::default() }], ..AppState::default() };
    let text = app.to_json().unwrap();
    assert!(text.contains("PendingOpenWith"));
    let back = csm_core::state::parse(&text).unwrap();
    assert_eq!(back.pending_open_with, app.pending_open_with);
    let odd = csm_core::state::parse(r#"{"PendingOpenWith": [{"Label": 5}, {"Label": "ok"}], "Sessions": [{"Id": "s"}]}"#).unwrap();
    assert_eq!(odd.pending_open_with.len(), 1);
    assert_eq!(odd.sessions.len(), 1);
    assert!(csm_core::state::parse(r#"{"PendingOpenWith": "x"}"#).unwrap().pending_open_with.is_empty());
    assert!(!AppState::default().to_json().unwrap().contains("PendingOpenWith"), "absent while empty");
}
