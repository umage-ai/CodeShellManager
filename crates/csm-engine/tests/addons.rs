#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Add-ons end to end through the tools: discovery on start, enabling with the consent version,
//! changed files going off again, contributions, profiles and run templates, and --clean.

mod common;

use std::fs;
use std::path::Path;
use std::sync::{Arc, Mutex};

use common::{H, test_app};
use csm_engine::{Engine, State};
use serde_json::{Value, json};

const DEMO: &str = r#"
[addon]
id = "demo"
name = "Demo pack"
version = "0.1.0"
description = "A demo pack"
api = 1
kind = "pack"

[[profile]]
name = "agent"
argv = ["demo-agent"]
args = ["--plain"]
resume = "re-run"
status = [{ match = "Allow\\?", status = "needs_you_approval" }]

[[profile]]
name = "quiet"
argv = ["quiet-agent"]
resume = "none"

[[profile]]
name = "bad"
argv = ["rm", "-rf", "x;y"]

[[run_template]]
name = "test"
argv = ["cargo", "test"]

[[slash]]
command = "/review"
description = "Review the diff"

[[command]]
id = "list"
title = "List sessions"
tool = "session_list"
params = {}

[[command]]
id = "ghost"
title = "Ghost"
tool = "no_such_tool"
"#;

fn write_addon(root: &Path, folder: &str, text: &str) {
    let d = root.join(folder);
    fs::create_dir_all(&d).unwrap();
    fs::write(d.join("addon.toml"), text).unwrap();
}

/// A state folder with `addons/` already holding the fixtures; returns (state path, root).
fn fixture(dir: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let state = dir.join("state.json");
    let root = dir.join("addons");
    write_addon(&root, "demo", DEMO);
    write_addon(&root, "broken", "[addon\nid = ");
    write_addon(&root, "newer", &DEMO.replace("id = \"demo\"", "id = \"newer\"").replace("api = 1", "api = 2"));
    (state, root)
}

fn start(state: &Path) -> H {
    let state = state.to_path_buf();
    H::start(test_app(), move |o| {
        o.save_path = Some(state);
        o.restore_on_start = false;
    })
}

fn find<'a>(list: &'a Value, id: &str) -> &'a Value {
    list.as_array().unwrap().iter().find(|a| a["id"] == id).unwrap_or_else(|| panic!("no add-on {id} in {list}"))
}

fn list(h: &H) -> Value {
    h.ok("addon_list", json!({})).data["addons"].clone()
}

/// Enables `demo` the way the consent dialog does: with the version and digest `addon_list` shows.
fn enable_demo(h: &H) {
    let l = list(h);
    let demo = find(&l, "demo");
    let (version, digest) = (demo["version"].clone(), demo["digest"].clone());
    h.ok("addon_enable", json!({"id": "demo", "version": version, "digest": digest}));
}

#[test]
fn discovery_reports_each_state_and_nothing_is_enabled_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = fixture(dir.path());
    let h = start(&state);
    h.wait_until("three add-on folders", |s| s.addons.len() == 3);
    let l = list(&h);
    assert_eq!(find(&l, "demo")["state"], "disabled");
    assert_eq!(find(&l, "demo")["message"], "not enabled yet");
    assert_eq!(find(&l, "broken")["state"], "error");
    assert!(find(&l, "broken")["message"].as_str().unwrap().contains("not valid TOML"));
    assert!(find(&l, "newer")["message"].as_str().unwrap().contains("Update CodeShellManager"));
    // The bad profile and the unknown tool are reported, the good ones still listed.
    let demo = find(&l, "demo");
    let profiles = demo["profiles"].as_array().unwrap();
    assert_eq!(profiles.len(), 2, "{profiles:?}");
    assert_eq!(profiles[0]["id"], "demo.agent");
    assert_eq!(profiles[0]["command_line"], "demo-agent --plain", "the consent dialog shows the exact command");
    assert_eq!(profiles[1]["resume"], "none");
    assert_eq!(demo["commands"].as_array().unwrap().len(), 1, "a command naming an unknown tool is dropped");
    let problems: Vec<&str> = demo["problems"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(problems.iter().any(|p| p.contains("not a plain token")), "{problems:?}");
    assert!(problems.iter().any(|p| p.contains("no_such_tool")), "{problems:?}");
    // Disabled add-ons contribute nothing to the slash menu.
    assert!(h.engine.view().slash_presets.iter().all(|c| c.command != "/review"));
    h.engine.shutdown();
}

#[test]
fn enable_needs_the_version_shown_and_contributions_appear() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = fixture(dir.path());
    let h = start(&state);
    h.wait_until("add-ons", |s| s.addons.len() == 3);
    let digest = find(&list(&h), "demo")["digest"].clone();
    let refused = h.ui("addon_enable", json!({"id": "demo", "version": "9.9.9", "digest": digest})).unwrap_err();
    assert!(refused.to_string().contains("review it again"), "{refused}");
    // The same version with other bytes (a file changed after the dialog showed it) is refused too.
    let stale = h.ui("addon_enable", json!({"id": "demo", "version": "0.1.0", "digest": "0".repeat(64)})).unwrap_err();
    assert!(stale.to_string().contains("changed since it was shown"), "{stale}");
    assert_eq!(find(&list(&h), "demo")["state"], "disabled", "a refused enable approves nothing");
    enable_demo(&h);
    let l = list(&h);
    assert_eq!(find(&l, "demo")["state"], "loaded");
    let v = h.engine.view();
    let review = v.slash_presets.iter().find(|c| c.command == "/review").expect("the add-on's preset");
    assert!(review.description.contains("Demo pack"), "{}", review.description);
    assert_eq!(v.addons.iter().find(|a| a.id == "demo").unwrap().commands[0].id, "demo.list");
    // A run template is offered by id.
    assert_eq!(find(&l, "demo")["run_templates"][0]["id"], "demo.test");
    h.engine.shutdown();
    // The approval is saved in state.json settings (not writable through settings_set).
    let saved = fs::read_to_string(&state).unwrap();
    assert!(saved.contains("AddonApprovals"), "{saved}");
}

#[test]
fn a_changed_file_is_off_until_enabled_again_and_disable_forgets_it() {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = fixture(dir.path());
    let h = start(&state);
    h.wait_until("add-ons", |s| s.addons.len() == 3);
    enable_demo(&h);
    write_addon(&root, "demo", &DEMO.replace("\"/review\"", "\"/audit\""));
    h.ok("addon_reload", json!({}));
    h.wait_until("reloaded", |s| s.addons.iter().any(|a| a.id == "demo" && a.state == csm_engine::addons::AddonState::Disabled));
    let l = list(&h);
    assert!(find(&l, "demo")["message"].as_str().unwrap().contains("changed since you enabled it"));
    h.ok("addon_disable", json!({"id": "demo"}));
    let l = list(&h);
    assert_eq!(find(&l, "demo")["message"], "not enabled yet");
    assert!(!fs::read_to_string(&state).unwrap_or_default().contains("demo"));
    h.engine.shutdown();
}

#[test]
fn a_profile_launches_its_argv_and_a_run_template_joins_its_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = fixture(dir.path());
    let folder = dir.path().join("work");
    fs::create_dir_all(&folder).unwrap();
    let h = start(&state);
    h.wait_until("add-ons", |s| s.addons.len() == 3);
    enable_demo(&h);
    // A profile sets the command: passing command or args with it is refused.
    let both = h.ui("session_new", json!({"folder": folder.display().to_string(), "profile": "demo.agent", "command": "claude"}));
    assert!(both.is_err());
    let s = h.ok("session_new", json!({"folder": folder.display().to_string(), "profile": "demo.agent", "focus": false}));
    let id = s.data["id"].as_str().unwrap().to_owned();
    let sessions = h.engine.view();
    assert!(sessions.sessions.iter().any(|x| x.id == id));
    h.wait_status(&id, csm_engine::Status::Idle);
    let argv = h.pty_of(&id).argv.join(" ");
    assert!(argv.contains("demo-agent --plain"), "{argv}");
    // A run template is added to the session's runs, as its tokens joined by spaces.
    h.ok("addon_run_apply", json!({"session_id": id, "template": "demo.test"}));
    let runs = h.ok("run_list", json!({"session_id": id}));
    let text = runs.data.to_string();
    assert!(text.contains("cargo test"), "{text}");
    // A profile of a disabled add-on is refused.
    let off = h.ui("session_new", json!({"folder": folder.display().to_string(), "profile": "broken.agent"}));
    assert!(off.is_err());
    h.engine.shutdown();
}

#[test]
fn a_restart_restores_resuming_profiles_only_and_waits_for_the_scan() {
    let dir = tempfile::tempdir().unwrap();
    let (state, _) = fixture(dir.path());
    let folder = dir.path().join("work").display().to_string();
    fs::create_dir_all(&folder).unwrap();
    let h = start(&state);
    h.wait_until("add-ons", |s| s.addons.len() == 3);
    enable_demo(&h);
    let agent = h.ok("session_new", json!({"folder": folder, "profile": "demo.agent", "focus": false})).data["id"].as_str().unwrap().to_owned();
    let quiet = h.ok("session_new", json!({"folder": folder, "profile": "demo.quiet", "focus": false})).data["id"].as_str().unwrap().to_owned();
    h.wait_status(&agent, csm_engine::Status::Idle);
    h.wait_status(&quiet, csm_engine::Status::Idle);
    h.engine.shutdown();
    // Second run: the add-on scan is still on its worker when the restore begins, so the
    // profile sessions wait for it. The re-run profile starts again; the `none` one stays asleep.
    // Loaded from the state file, as a real start does (`H::start` builds a fresh state).
    let ptys = Arc::new(Mutex::new(Vec::new()));
    let mut o = common::fast_options(&ptys);
    o.save_path = Some(state);
    o.restore_on_start = true;
    let (st, _) = State::load(o);
    let h2 = H { engine: Engine::start(st, vec![]).unwrap(), ptys, dir: tempfile::tempdir().unwrap() };
    h2.wait_until("both sessions", |s| s.session(&agent).is_some() && s.session(&quiet).is_some());
    h2.wait_status(&agent, csm_engine::Status::Idle);
    let v = h2.engine.view();
    assert!(!v.session(&agent).unwrap().asleep, "the re-run profile restores");
    assert!(v.session(&quiet).unwrap().asleep, "a resume `none` profile waits for the person");
    h2.engine.shutdown();
}

#[test]
fn clean_runs_read_no_add_ons_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (state, root) = fixture(dir.path());
    let before = fs::read_dir(&root).unwrap().count();
    let h = H::start(test_app(), move |o| {
        o.save_path = Some(state);
        o.clean = true;
        o.restore_on_start = false;
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    let v = h.ok("addon_list", json!({}));
    assert!(v.message.contains("off in this run"), "{}", v.message);
    assert_eq!(v.data["addons"], json!([]));
    assert!(h.ui("addon_open_folder", json!({})).is_err());
    assert_eq!(fs::read_dir(&root).unwrap().count(), before);
    h.engine.shutdown();
}

#[test]
fn add_on_tools_are_not_callable_as_palette_commands() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    write_addon(&dir.path().join("addons"), "self", "[addon]\nid = \"self\"\nname = \"Self\"\nversion = \"1.0.0\"\napi = 1\nkind = \"pack\"\n[[command]]\nid = \"x\"\ntitle = \"X\"\ntool = \"addon_enable\"\n");
    let h = start(&state);
    h.wait_until("add-on", |s| s.addons.len() == 1);
    let l = list(&h);
    assert_eq!(find(&l, "self")["commands"], json!([]));
    assert!(find(&l, "self")["problems"].to_string().contains("add-on tools"), "{l}");
    h.engine.shutdown();
}
