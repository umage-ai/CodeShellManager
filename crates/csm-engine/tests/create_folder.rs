//! `session_new { create_folder }` and `folder_probe`: a missing folder is made only when asked,
//! and the refusals hold.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::H;
use csm_engine::{Caller, Effects, ToolError};
use serde_json::json;

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn s(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn a_missing_folder_is_created_when_asked_and_the_session_starts_there() {
    let h = H::new();
    let root = dir();
    let target = root.path().join("a").join("b");
    h.ok("session_new", json!({"folder": s(&target), "command": "cmd", "create_folder": true}));
    assert!(target.is_dir(), "both levels were made");
}

#[test]
fn without_create_folder_nothing_is_made() {
    let h = H::new();
    let root = dir();
    let target = root.path().join("nope");
    let _ = h.ui("session_new", json!({"folder": s(&target), "command": "cmd"}));
    assert!(!target.exists(), "a plain session_new never creates a folder");
}

#[test]
fn create_folder_refusals_create_nothing() {
    let h = H::new();
    let root = dir();
    let file = root.path().join("f.txt");
    std::fs::write(&file, "x").unwrap();
    let base = s(root.path());
    let cases = [
        "relative\\folder".to_string(),
        format!("{base}\\..\\up"),
        format!("{base}\\q\"uote"),
        format!("{base}\\ctl\u{7}"),
        s(&file.join("inside")),
        s(&file),
        r"\\?\C:\nowhere\x".to_string(),
        r"\\.\pipe\x".to_string(),
    ];
    for c in cases {
        let r = h.ui("session_new", json!({"folder": c, "command": "cmd", "create_folder": true}));
        assert!(matches!(r, Err(ToolError::Refused(_) | ToolError::InvalidParams(_))), "{c}: {r:?}");
    }
    assert_eq!(h.pty_count(), 0, "no session started");
    assert!(!root.path().join("up").exists());
}

#[cfg(windows)]
#[test]
fn a_missing_drive_is_never_created() {
    let h = H::new();
    let letter = ('D'..='Z').find(|c| !std::path::Path::new(&format!("{c}:\\")).exists()).unwrap();
    let folder = format!("{letter}:\\x\\y");
    let r = h.ui("session_new", json!({"folder": folder, "command": "cmd", "create_folder": true}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
}

#[test]
fn create_folder_needs_a_local_session() {
    let h = H::new();
    let r = h.ui("session_new", json!({"kind": "wsl", "wsl": {"distro": "Ubuntu"}, "command": "bash", "create_folder": true}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{r:?}");
}

#[test]
fn an_mcp_caller_is_asked_before_a_folder_is_made() {
    let h = H::new();
    let reg = h.engine.registry();
    let new = reg.get("session_new").unwrap().effects;
    assert!(new.needs_consent() && new.contains(Effects::WRITES_FILESYSTEM), "declared honestly");
    assert!(!reg.get("folder_probe").unwrap().effects.needs_consent(), "a probe only looks");
}

#[test]
fn folder_probe_reports_the_levels_and_never_creates() {
    let h = H::new();
    let ext = Caller::External { client: "test-mcp".into(), session_id: None };
    let root = dir();
    let target = root.path().join("x").join("y");
    let out = h.engine.call(ext.clone(), "folder_probe", json!({"path": s(&target)})).expect("read-only: no consent");
    assert_eq!(out.data["exists"], false);
    assert_eq!(out.data["can_create"], true);
    assert_eq!(out.data["missing"], json!(["x", "y"]));
    assert!(!root.path().join("x").exists());
    let here = h.engine.call(ext.clone(), "folder_probe", json!({"path": s(root.path())})).unwrap();
    assert_eq!(here.data["exists"], true);
    let bad = h.engine.call(ext, "folder_probe", json!({"path": "relative"})).unwrap();
    assert_eq!(bad.data["can_create"], false);
    assert!(bad.data["reason"].is_string());
}
