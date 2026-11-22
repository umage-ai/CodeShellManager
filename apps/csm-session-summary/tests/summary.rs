#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The sample process add-on through the real plugin host: the shipped manifest, the handshake,
//! the install-samples tool, and a summary of live sessions (fake PTYs) over `csm/call`.

#[path = "../../../crates/csm-engine/tests/common/mod.rs"]
mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{H, osc7501, test_app};
use csm_engine::{Caller, Status};
use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_csm-session-summary");

fn samples_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../samples/addons")
}

/// A state folder holding the shipped manifest, with the program named by its full path (a test
/// binary is not on PATH; in the installer it sits beside codeshell.exe).
fn fixture(dir: &Path) -> PathBuf {
    let shipped = fs::read_to_string(samples_dir().join("session-summary").join("addon.toml")).unwrap();
    let wanted = "argv = [\"csm-session-summary\"]";
    assert!(shipped.contains(wanted), "the shipped manifest changed shape");
    let text = shipped.replace(wanted, &format!("argv = [{}]", serde_json::to_string(EXE).unwrap()));
    let folder = dir.join("addons").join("session-summary");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("addon.toml"), text).unwrap();
    dir.join("state.json")
}

fn start(state: &Path, samples: Option<PathBuf>) -> H {
    let state = state.to_path_buf();
    H::start(test_app(), move |o| {
        o.save_path = Some(state);
        o.restore_on_start = false;
        o.samples_dir = samples;
    })
}

fn enable(h: &H, id: &str) {
    let list = h.ok("addon_list", json!({})).data["addons"].clone();
    let a = list.as_array().unwrap().iter().find(|a| a["id"] == id).cloned().unwrap_or_else(|| panic!("no {id} in {list}"));
    h.ok("addon_enable", json!({"id": id, "version": a["version"], "digest": a["digest"]}));
}

fn wait_running(h: &H) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let rows = h.ok("plugin_status", json!({})).data["processes"].clone();
        let row = rows.as_array().unwrap().iter().find(|p| p["id"] == "session-summary").cloned().unwrap_or(Value::Null);
        if row["state"] == "running" {
            assert!(row["tools"].as_array().unwrap().iter().any(|t| t == "session-summary.summarise"), "{row}");
            return;
        }
        assert!(Instant::now() < deadline, "not running; last row: {row}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn it_summarises_the_live_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path()), None);
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    // Off until enabled: its tool does not exist yet.
    assert!(h.ui("session-summary.summarise", json!({})).is_err());
    enable(&h, "session-summary");
    wait_running(&h);

    let none = h.ok("session-summary.summarise", json!({}));
    assert_eq!(none.message, "No sessions.");

    let a = h.session("claude");
    let pty = h.pty_of(&a);
    // The program announces itself (OSC 7501 probe, then DA1), so its status is reported.
    pty.emit(b"\x1b]7501;?\x1b\\\x1b[c");
    let _b = h.session("sleep 2");
    pty.emit(&osc7501("state=blocked:kind=permission"));
    h.wait_status(&a, Status::NeedsYouApproval);

    let out = h.ok("session-summary.summarise", json!({}));
    assert!(out.message.starts_with("2 sessions in 1 repo:"), "{}", out.message);
    assert!(out.message.contains("1 needs you approval") && out.message.contains("1 idle"), "{}", out.message);
    assert!(out.message.contains("Needs you:"), "{}", out.message);
    assert_eq!(out.data["total"], 2);
    assert_eq!(out.data["needs_you"].as_array().unwrap().len(), 1);
    assert_eq!(out.data["by_repo"][0]["sessions"], 2);

    // Honest effects: it declares none, so an MCP client runs it without a consent card.
    let ext = Caller::External { client: "test-client".into(), session_id: None };
    assert!(h.engine.call(ext, "session-summary.summarise", json!({})).is_ok());
    h.engine.shutdown();
}

#[test]
fn installed_samples_are_all_disabled_and_the_packs_load_clean() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let h = start(&state, Some(samples_dir()));
    h.wait_until("scan", |s| s.addons.is_empty());
    let out = h.ok("addon_install_samples", json!({}));
    assert_eq!(out.data["installed"], json!(["codex-cli", "gemini-cli", "review", "session-summary", "themes"]), "{}", out.message);
    h.wait_until("five add-ons", |s| s.addons.len() == 5);
    let list = h.ok("addon_list", json!({})).data["addons"].clone();
    for a in list.as_array().unwrap() {
        assert_eq!(a["state"], "disabled", "{a}");
        assert_eq!(a["problems"].as_array().unwrap().len(), 0, "{a}");
    }
    // Enabling a pack loads its profiles and palette commands with nothing dropped.
    enable(&h, "codex-cli");
    let list = h.ok("addon_list", json!({})).data["addons"].clone();
    let codex = list.as_array().unwrap().iter().find(|a| a["id"] == "codex-cli").unwrap();
    assert_eq!(codex["state"], "loaded");
    assert_eq!(codex["problems"].as_array().unwrap().len(), 0, "{codex}");
    assert_eq!(codex["commands"].as_array().unwrap().len(), 2);
    enable(&h, "gemini-cli");
    let list = h.ok("addon_list", json!({})).data["addons"].clone();
    let gemini = list.as_array().unwrap().iter().find(|a| a["id"] == "gemini-cli").unwrap();
    assert_eq!(gemini["problems"].as_array().unwrap().len(), 0, "{gemini}");
    assert_eq!(gemini["commands"].as_array().unwrap().len(), 2);
    // The themes pack: three dark themes, offered by theme_list once it is enabled.
    enable(&h, "themes");
    let themes = h.ok("theme_list", json!({})).data["themes"].clone();
    let ids: Vec<&str> = themes.as_array().unwrap().iter().filter_map(|t| t["id"].as_str()).collect();
    assert_eq!(ids, vec!["mocha", "latte", "themes.frappe", "themes.macchiato", "themes.high-contrast"]);
    // A second install changes nothing.
    let again = h.ok("addon_install_samples", json!({}));
    assert_eq!(again.data["installed"], json!([]));
    assert_eq!(again.data["skipped"].as_array().unwrap().len(), 5);
    h.engine.shutdown();
}
