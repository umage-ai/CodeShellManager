//! Start at sign-in (`app_autostart_get` / `app_autostart_set`) and the start-minimized settings,
//! through the engine. The OS entry is an in-memory registry here: no test touches the real one.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{H, test_app};
use csm_engine::{Autostart, Caller, Effects, Event};
use csm_services::autostart::{APPROVED_KEY, ENTRY_NAME, MemValue, MemoryRegistry, RUN_KEY, WindowsRun};
use serde_json::json;

fn exe() -> PathBuf {
    if cfg!(windows) { PathBuf::from(r"C:\Program Files\umage.ai\CodeShellManager\codeshell.exe") } else { PathBuf::from("/opt/Code Shell/codeshell") }
}

/// An engine whose autostart entry is a Run key in memory; the registry is returned for checks.
fn with_run_key() -> (H, Arc<WindowsRun<MemoryRegistry>>) {
    let run = Arc::new(WindowsRun::new(MemoryRegistry::default()));
    let backend: Arc<dyn csm_services::autostart::Backend> = run.clone();
    let h = H::start(test_app(), |o| o.autostart = Autostart { backend: Some(backend), program: Some(exe()) });
    (h, run)
}

#[test]
fn turning_start_at_sign_in_on_and_off_writes_the_os_entry_and_reads_it_back() {
    let (h, run) = with_run_key();
    let events = h.engine.subscribe();
    let got = h.ok("app_autostart_get", json!({}));
    assert_eq!(got.data["enabled"], false);
    assert_eq!(got.data["supported"], true);

    let on = h.ok("app_autostart_set", json!({"enabled": true}));
    assert_eq!(on.data["enabled"], true);
    assert_eq!(on.data["current"], true);
    assert_eq!(on.message, "CodeShellManager starts when you sign in.");
    let Some(MemValue::String(line)) = run.registry().value(RUN_KEY, ENTRY_NAME) else { panic!("no Run value") };
    assert!(line.ends_with("\" --autostart") && line.starts_with('"'), "{line}");
    // The UI follows a change made by the CLI or an MCP client.
    let changed = events.try_iter().find_map(|e| match e {
        Event::SettingChanged { key, value } if key == "StartAtSignIn" => Some(value),
        _ => None,
    });
    assert_eq!(changed.unwrap()["enabled"], true);

    // Task Manager turns it off: read back from the OS, not from what was set.
    run.registry().put(APPROVED_KEY, ENTRY_NAME, MemValue::Binary(vec![3, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]));
    let got = h.ok("app_autostart_get", json!({}));
    assert_eq!((got.data["enabled"].clone(), got.data["disabled_by_os"].clone()), (json!(false), json!(true)));
    assert!(got.message.contains("turned off"), "{}", got.message);
    // Turning it on here turns it back on.
    assert_eq!(h.ok("app_autostart_set", json!({"enabled": true})).data["enabled"], true);
    assert!(run.registry().value(APPROVED_KEY, ENTRY_NAME).is_none());

    let off = h.ok("app_autostart_set", json!({"enabled": false}));
    assert_eq!(off.data["registered"], false);
    assert!(run.registry().value(RUN_KEY, ENTRY_NAME).is_none());
    assert!(h.ui("app_autostart_set", json!({})).is_err(), "enabled is required");
}

#[test]
fn an_mcp_caller_is_asked_before_changing_what_runs_at_sign_in() {
    let h = H::new();
    let reg = h.engine.registry();
    let set = reg.get("app_autostart_set").unwrap().effects;
    assert!(set.needs_consent(), "it decides what runs at the next sign-in");
    assert!(set.contains(Effects::LAUNCHES_PROCESS | Effects::WRITES_FILESYSTEM));
    assert!(!reg.get("app_autostart_get").unwrap().effects.needs_consent());
    let _ = Caller::Cli;
}

#[test]
fn without_an_os_entry_the_tools_say_so() {
    let h = H::start(test_app(), |o| o.autostart = Autostart { backend: None, program: None });
    let got = h.ok("app_autostart_get", json!({}));
    assert_eq!(got.data["supported"], false);
    assert!(h.ui("app_autostart_set", json!({"enabled": true})).is_err());
}

#[test]
fn the_default_engine_never_touches_the_real_entry() {
    // Tests and the demo get an entry in memory.
    let h = H::new();
    let on = h.ok("app_autostart_set", json!({"enabled": true}));
    assert_eq!(on.data["location"], "(memory)");
}

#[test]
fn start_minimized_settings_are_listed_off_by_default_and_saved() {
    let h = H::new();
    let all = h.ok("settings_get", json!({})).data;
    assert_eq!((all["StartMinimized"].clone(), all["StartMinimizedToTray"].clone()), (json!(false), json!(false)));
    h.ok("settings_set", json!({"key": "StartMinimized", "value": true}));
    h.ok("settings_set", json!({"key": "start_minimized_to_tray", "value": true}));
    let all = h.ok("settings_get", json!({})).data;
    assert_eq!((all["StartMinimized"].clone(), all["StartMinimizedToTray"].clone()), (json!(true), json!(true)));
    // Not cosmetic: an MCP client may not change it (SEC-3).
    let ext = Caller::External { client: "t".into(), session_id: None };
    assert!(matches!(h.engine.call(ext, "settings_set", json!({"key": "StartMinimized", "value": false})), Err(csm_engine::ToolError::Refused(_))));
}
