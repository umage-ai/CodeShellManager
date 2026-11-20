//! Windows Terminal profiles and a session's own look, through `Engine::call`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{H, test_app};
use csm_engine::{Caller, ToolError};
use csm_term::alacritty_terminal::vte::ansi::CursorShape;
use serde_json::json;

const SETTINGS: &str = r##"// Windows Terminal settings (JSONC)
{
  "profiles": {
    "defaults": { "font": { "face": "Cascadia Mono", "size": 11 } },
    "list": [
      { "name": "PowerShell", "commandline": "pwsh.exe -NoLogo", "colorScheme": "Demo",
        "cursorShape": "bar", "padding": "8, 12", "font": { "size": 14 }, },
      { "name": "Ubuntu", "commandline": "wsl.exe -d Ubuntu", "colorScheme": "One Half Dark", "cursorShape": "underscore" },
      { "name": "Hidden", "hidden": true },
    ]
  },
  "schemes": [ { "name": "Demo", "background": "#012456", "foreground": "#CCCCCC", "purple": "#881798" } ],
}"##;

fn with_profiles(import: bool) -> H {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.json");
    std::fs::write(&path, SETTINGS).unwrap();
    let mut app = test_app();
    app.settings.import_windows_terminal_profiles = import;
    let h = H::start(app, |o| o.terminal_profile_files = Some(vec![("Stable".into(), path)]));
    // Keep the file alive for the test: leak the temp dir into the harness's lifetime.
    std::mem::forget(dir);
    h
}

#[test]
fn profile_list_reads_the_file_off_the_engine_thread_and_needs_no_consent() {
    let h = with_profiles(true);
    let ext = Caller::External { client: "test-mcp".into(), session_id: None };
    let out = h.engine.call(ext, "profile_list", json!({})).expect("read-only: no consent card");
    let list = out.data.as_array().unwrap();
    let names: Vec<&str> = list.iter().map(|p| p["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["PowerShell", "Ubuntu"]);
    let ps = &list[0];
    assert_eq!(ps["source"], "Stable");
    assert_eq!(ps["commandline"], "pwsh.exe -NoLogo");
    assert_eq!(ps["look"]["font_size"], 14);
    assert_eq!(ps["look"]["font_family"], "Cascadia Mono");
    assert_eq!(ps["look"]["cursor_shape"], "bar");
    assert_eq!(ps["look"]["padding"], json!([8, 12, 8, 12]));
    assert_eq!(ps["look"]["colors"]["background"], "#012456");
    assert_eq!(ps["look"]["colors"]["magenta"], "#881798");
    // A built-in scheme fills in what the file does not have.
    assert_eq!(list[1]["look"]["colors"]["background"], "#282c34");
}

#[test]
fn profile_import_is_opt_in() {
    let h = with_profiles(false);
    assert!(matches!(h.ui("profile_list", json!({})), Err(ToolError::Refused(m)) if m.contains("ImportWindowsTerminalProfiles")));
    let id = h.session("pwsh");
    let r = h.ui("session_look_set", json!({"session_id": id, "profile": "PowerShell"}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    // An explicit look needs no import.
    h.ok("session_look_set", json!({"session_id": id, "look": {"font_size": 15}}));
    assert_eq!(h.engine.view().session(&id).unwrap().look.as_ref().unwrap().font_size, Some(15));
}

#[test]
fn a_profile_look_is_stamped_persisted_and_reaches_the_running_terminal() {
    let h = with_profiles(true);
    let id = h.session("pwsh");
    let out = h.ok("session_look_set", json!({"session_id": id, "profile": "powershell"}));
    assert_eq!(out.data["changed"], true);
    assert!(out.message.contains("PowerShell"), "{}", out.message);

    // In the snapshot (what the renderer draws from)…
    let v = h.engine.view();
    let look = v.session(&id).unwrap().look.clone().unwrap();
    assert_eq!(look.font_size, Some(14));
    assert_eq!(look.colors.as_ref().unwrap().background.as_deref(), Some("#012456"));
    // …in the emulator: the default cursor and the colours OSC queries answer with…
    {
        let t = v.terminal(&id).unwrap().terminal().lock().unwrap();
        assert_eq!(t.term().cursor_style().shape, CursorShape::Beam);
        assert_eq!((t.palette().background.r, t.palette().background.g, t.palette().background.b), (0x01, 0x24, 0x56));
    }
    // …and in the persisted session, in the WPF app's formats.
    let cfg = h.ok("session_get", json!({"session_id": id})).data["config"]["look"].clone();
    assert_eq!(cfg["cursor_shape"], "bar");

    // A restart keeps it (the look is applied to the new emulator at launch).
    h.ok("session_restart", json!({"session_id": id}));
    let v = h.wait_until("the restarted terminal", |v| {
        v.terminal(&id).is_some_and(|t| t.terminal().lock().unwrap().term().cursor_style().shape == CursorShape::Beam)
    });
    assert!(v.session(&id).unwrap().look.is_some());

    // Clearing goes back to the app's look.
    let out = h.ok("session_look_set", json!({"session_id": id, "clear": true}));
    assert_eq!(out.data["changed"], true);
    let v = h.engine.view();
    assert!(v.session(&id).unwrap().look.is_none());
    assert_eq!(v.terminal(&id).unwrap().terminal().lock().unwrap().term().cursor_style().shape, CursorShape::Block);
}

#[test]
fn session_new_takes_a_look_and_bad_looks_are_refused() {
    let h = with_profiles(true);
    let folder = h.dir.path().to_string_lossy().into_owned();
    let out = h.ok(
        "session_new",
        json!({"folder": folder, "command": "pwsh", "look": {"colors": {"background": "#102030"}, "cursor_shape": "underline", "padding": [4]}}),
    );
    let id = out.data["id"].as_str().unwrap().to_owned();
    let look = h.engine.view().session(&id).unwrap().look.clone().unwrap();
    assert_eq!(look.padding, Some(vec![4, 4, 4, 4]));
    assert_eq!(look.colors.unwrap().background.as_deref(), Some("#102030"));

    for bad in [
        json!({"session_id": id, "look": {"colors": {"background": "blue"}}}),
        json!({"session_id": id, "look": {"font_size": 500}}),
        json!({"session_id": id, "look": {"padding": [1, 2, 3]}}),
        json!({"session_id": id, "look": {"cursor_shape": "triangle"}}),
        json!({"session_id": id}),
        json!({"session_id": id, "clear": true, "look": {}}),
    ] {
        assert!(matches!(h.ui("session_look_set", bad.clone()), Err(ToolError::InvalidParams(_))), "{bad}");
    }
    assert!(matches!(h.ui("session_look_set", json!({"session_id": id, "profile": "Nope"})), Err(ToolError::NotFound(_))));
    assert!(matches!(h.ui("session_look_set", json!({"session_id": "missing", "clear": true})), Err(ToolError::NotFound(_))));
    let r = h.ui("session_new", json!({"folder": folder, "command": "pwsh", "look": {"opacity": 3.0}}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{r:?}");
}
