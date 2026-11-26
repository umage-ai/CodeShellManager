//! Explorer's "Open in CodeShellManager": `session_new_dialog` (the tool a second launch of
//! `codeshell --open-folder` ends in) and the per-user off switch (`app_explorer_menu_get/set`).
//! The registry is in memory here: no test touches the real one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_core::channel::Channel;
use csm_engine::{Caller, Effects, Event, ExplorerMenu, ToolError};
use csm_peer_proto::msg::Level;
use csm_services::explorer_menu::{DISABLE_VALUE, MemoryStore, verb_keys};
use serde_json::json;

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn s(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Events seen so far, after the engine has had a moment to post the worker's answer.
fn requested(rx: &std::sync::mpsc::Receiver<Event>) -> Vec<Option<String>> {
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    rx.try_iter()
        .filter_map(|e| match e {
            Event::NewSessionRequested { folder } => Some(folder),
            _ => None,
        })
        .collect()
}

fn toasts(events: &[Event]) -> usize {
    events.iter().filter(|e| matches!(e, Event::Toast { .. })).count()
}

#[test]
fn the_dialog_opens_on_an_existing_folder_and_nothing_starts_or_is_made() {
    let h = H::new();
    let rx = h.engine.subscribe();
    let root = dir();
    let before = std::fs::read_dir(root.path()).unwrap().count();
    let out = h.ok("session_new_dialog", json!({"folder": s(root.path())}));
    assert!(out.message.contains("New session dialog"), "{}", out.message);
    let got = requested(&rx);
    assert_eq!(got.len(), 1, "{got:?}");
    let folder = got[0].clone().unwrap();
    assert!(std::path::Path::new(&folder).is_dir());
    // Nothing started, nothing created, no session in the snapshot.
    assert_eq!(h.pty_count(), 0);
    assert!(h.engine.view().sessions.is_empty());
    assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), before);
}

#[test]
fn without_a_folder_the_dialog_opens_empty() {
    let h = H::new();
    let rx = h.engine.subscribe();
    h.ok("session_new_dialog", json!({}));
    assert_eq!(requested(&rx), vec![None]);
    assert_eq!(h.pty_count(), 0);
}

#[cfg(windows)]
#[test]
fn a_drive_root_as_argv_parsing_leaves_it_opens_on_the_root() {
    let h = H::new();
    let rx = h.engine.subscribe();
    h.ok("session_new_dialog", json!({"folder": "C:\""}));
    h.ok("session_new_dialog", json!({"folder": "C:"}));
    assert_eq!(requested(&rx), vec![Some(r"C:\".to_string()), Some(r"C:\".to_string())]);
}

#[test]
fn hostile_missing_and_file_paths_are_refused_with_a_toast_for_a_second_process() {
    let h = H::new();
    let root = dir();
    let file = root.path().join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let missing = root.path().join("gone");
    let base = s(root.path());
    let cases = [
        "relative\\folder".to_string(),
        format!("{base}\\..\\up"),
        "shell:Downloads".to_string(),
        "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}".to_string(),
        r"\\?\C:\Windows".to_string(),
        r"\\.\pipe\x".to_string(),
        "https://example.com".to_string(),
        format!("{base}\\ctl\u{7}"),
        s(&file),
        s(&missing),
        String::new(),
    ];
    for c in cases {
        let rx = h.engine.subscribe();
        let r = h.engine.call(Caller::Cli, "session_new_dialog", json!({"folder": c}));
        assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{c:?}: {r:?}");
        let deadline = Instant::now() + Duration::from_millis(200);
        let mut events = Vec::new();
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
            events.extend(rx.try_iter());
        }
        assert!(!events.iter().any(|e| matches!(e, Event::NewSessionRequested { .. })), "{c:?} opened the dialog");
        assert_eq!(toasts(&events), 1, "{c:?}: the window says why");
        assert!(events.iter().any(|e| matches!(e, Event::WindowShowRequested)), "{c:?}: and comes forward");
    }
    assert_eq!(h.pty_count(), 0, "no session started");
    assert!(!missing.exists() && !root.path().join("up").exists(), "nothing created");
}

#[test]
fn the_apps_own_window_gets_the_error_on_the_reply_not_a_second_toast() {
    let h = H::new();
    let rx = h.engine.subscribe();
    let r = h.ui("session_new_dialog", json!({"folder": "shell:Downloads"}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{r:?}");
    assert_eq!(toasts(&rx.try_iter().collect::<Vec<_>>()), 0);
}

#[test]
fn the_tool_only_shows_a_dialog_so_an_mcp_agent_needs_no_card_and_a_paired_device_is_refused() {
    let h = H::new();
    let eff = h.engine.registry().get("session_new_dialog").unwrap().effects;
    assert_eq!(eff, Effects::CHANGES_UI_STATE, "declared honestly: it changes what the window shows, nothing else");
    assert!(!eff.needs_consent());
    let rx = h.engine.subscribe();
    let agent = Caller::External { client: "an-agent".into(), session_id: None };
    h.engine.call(agent, "session_new_dialog", json!({})).unwrap();
    assert_eq!(requested(&rx), vec![None]);
    let peer = Caller::Peer { device_id: "dev1".into(), name: "Laptop".into(), level: Level::Full };
    let r = h.engine.call(peer, "session_new_dialog", json!({"folder": s(dir().path())}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    assert!(requested(&rx).is_empty());
    // Unknown fields and a wrong type are refused, not ignored.
    assert!(h.ui("session_new_dialog", json!({"folder": 5})).is_err());
    assert!(h.ui("session_new_dialog", json!({"command": "evil"})).is_err());
}

// ── The off switch ─────────────────────────────────────────────────────────────────────────────

fn with_store() -> (H, Arc<MemoryStore>) {
    let store = Arc::new(MemoryStore::default());
    store.install(Channel::current());
    let backend: Arc<dyn csm_services::explorer_menu::Store> = store.clone();
    let h = H::start(test_app(), |o| o.explorer_menu = ExplorerMenu { store: Some(backend) });
    (h, store)
}

#[test]
fn hiding_the_entry_writes_legacy_disable_for_this_user_and_showing_removes_it() {
    let (h, store) = with_store();
    let events = h.engine.subscribe();
    let got = h.ok("app_explorer_menu_get", json!({}));
    assert_eq!((got.data["supported"].clone(), got.data["installed"].clone(), got.data["shown"].clone()), (json!(true), json!(true), json!(true)));
    assert!(store.user_keys().is_empty(), "reading writes nothing");

    let off = h.ok("app_explorer_menu_set", json!({"shown": false}));
    assert_eq!(off.data["shown"], false);
    assert!(off.message.contains("hidden"), "{}", off.message);
    for k in verb_keys(Channel::current()) {
        assert_eq!(store.user_value(&k, DISABLE_VALUE).as_deref(), Some(""), "{k}");
    }
    // The UI follows a change made by the CLI or an MCP client.
    let changed = events.try_iter().find_map(|e| match e {
        Event::SettingChanged { key, value } if key == "ExplorerMenu" => Some(value),
        _ => None,
    });
    assert_eq!(changed.unwrap()["shown"], false);

    let on = h.ok("app_explorer_menu_set", json!({"shown": true}));
    assert_eq!(on.data["shown"], true);
    assert!(store.user_keys().is_empty(), "no trace left: {:?}", store.user_keys());
    assert!(h.ui("app_explorer_menu_set", json!({})).is_err(), "shown is required");
}

#[test]
fn the_switch_is_declared_honestly_and_is_not_a_setting_an_agent_can_set() {
    let (h, store) = with_store();
    let reg = h.engine.registry();
    let set = reg.get("app_explorer_menu_set").unwrap().effects;
    assert!(set.needs_consent() && set.contains(Effects::WRITES_FILESYSTEM));
    assert!(!reg.get("app_explorer_menu_get").unwrap().effects.needs_consent());
    let agent = Caller::External { client: "an-agent".into(), session_id: None };
    let r = h.engine.call(agent.clone(), "settings_set", json!({"key": "ExplorerMenu", "value": false}));
    assert!(r.is_err(), "not a settings key: {r:?}");
    assert!(store.user_keys().is_empty());
    let all = h.ok("settings_get", json!({})).data;
    assert!(all.get("ExplorerMenu").is_none());
}

#[test]
fn a_run_that_must_not_touch_the_registry_says_it_is_not_available() {
    let h = H::start(test_app(), |o| o.explorer_menu = ExplorerMenu::none());
    let got = h.ok("app_explorer_menu_get", json!({}));
    assert_eq!(got.data["supported"], false);
    let r = h.ui("app_explorer_menu_set", json!({"shown": false}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
}

#[test]
fn the_default_engine_never_touches_the_real_registry() {
    let h = H::new();
    let off = h.ok("app_explorer_menu_set", json!({"shown": false}));
    // An in-memory store: the real keys are untouched (the memory store has no machine entry).
    assert_eq!(off.data["installed"], false);
}
