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
    // The save is debounced: wait for it (a fixed read raced it on a busy machine).
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while fs::read_to_string(&state).unwrap_or_default().contains("demo") {
        assert!(std::time::Instant::now() < deadline, "the approval was not forgotten on disk");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
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

#[test]
fn palette_commands_may_not_write_into_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    write_addon(
        &dir.path().join("addons"),
        "typer",
        "[addon]\nid = \"typer\"\nname = \"Typer\"\nversion = \"1.0.0\"\napi = 1\nkind = \"pack\"\n[[command]]\nid = \"x\"\ntitle = \"X\"\ntool = \"session_send_text\"\n",
    );
    let h = start(&state);
    h.wait_until("add-on", |s| s.addons.len() == 1);
    let l = list(&h);
    assert_eq!(find(&l, "typer")["commands"], json!([]));
    assert!(find(&l, "typer")["problems"].to_string().contains("may not do"), "{l}");
    h.engine.shutdown();
}

#[test]
fn install_samples_copies_missing_ones_disabled_and_never_overwrites() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let samples = dir.path().join("bundled");
    write_addon(&samples, "demo", DEMO);
    write_addon(&samples, "fresh", &DEMO.replace("id = \"demo\"", "id = \"fresh\""));
    // The person already has their own `demo`; it must survive.
    let mine = DEMO.replace("Demo pack", "My edited demo");
    write_addon(&dir.path().join("addons"), "demo", &mine);
    let s2 = state.clone();
    let h = H::start(test_app(), move |o| {
        o.save_path = Some(s2);
        o.restore_on_start = false;
        o.samples_dir = Some(samples);
    });
    h.wait_until("the one add-on", |s| s.addons.len() == 1);
    let out = h.ok("addon_install_samples", json!({}));
    assert!(out.message.contains("Installed 1"), "{}", out.message);
    assert_eq!(out.data["installed"], json!(["fresh"]));
    assert_eq!(out.data["skipped"], json!(["demo"]));
    h.wait_until("both add-ons", |s| s.addons.len() == 2);
    let l = list(&h);
    assert_eq!(find(&l, "fresh")["state"], "disabled", "installed samples are off until enabled");
    assert_eq!(find(&l, "demo")["name"], "My edited demo");
    assert_eq!(fs::read_to_string(dir.path().join("addons").join("demo").join("addon.toml")).unwrap(), mine);
    h.engine.shutdown();
}

#[test]
fn install_samples_is_refused_without_bundled_samples() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let h = start(&state);
    let e = h.ui("addon_install_samples", json!({})).unwrap_err();
    assert!(e.to_string().contains("no bundled samples"), "{e}");
    h.engine.shutdown();
}

const THEMES: &str = r##"
[addon]
id = "tones"
name = "Tones"
version = "0.1.0"
api = 1
kind = "pack"

[[theme]]
id = "night"
name = "Night"
base = "dark"
[theme.colors]
base = "#101010"

[[theme]]
id = "paper"
name = "Paper"
base = "light"
"##;

fn enable_id(h: &H, id: &str) {
    let l = list(h);
    let a = find(&l, id);
    let (version, digest) = (a["version"].clone(), a["digest"].clone());
    h.ok("addon_enable", json!({"id": id, "version": version, "digest": digest}));
}

#[test]
fn theme_list_and_theme_set_take_the_enabled_add_on_themes() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    write_addon(&dir.path().join("addons"), "tones", THEMES);
    let h = start(&state);
    h.wait_until("add-ons", |s| s.addons.len() == 1);
    // Off: only the built-ins, and an add-on id is refused.
    let ids = |h: &H| -> Vec<String> {
        h.ok("theme_list", json!({})).data["themes"].as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_owned()).collect()
    };
    assert_eq!(ids(&h), vec!["mocha", "latte"]);
    assert!(h.ui("theme_set", json!({"dark": "tones.night"})).unwrap_err().to_string().contains("not available"));
    enable_id(&h, "tones");
    assert_eq!(ids(&h), vec!["mocha", "latte", "tones.night", "tones.paper"]);
    let snap = h.engine.view();
    let tones = snap.addons.iter().find(|a| a.id == "tones").unwrap();
    assert_eq!(tones.themes.len(), 2, "the palettes reach the UI with the snapshot");
    // A dark theme only goes in `dark`; a bad mode is refused; nothing changes on a refusal.
    assert!(h.ui("theme_set", json!({"light": "tones.night"})).unwrap_err().to_string().contains("dark theme"));
    assert!(h.ui("theme_set", json!({"mode": "sepia"})).is_err());
    assert!(h.ui("theme_set", json!({})).is_err());
    let events = h.engine.subscribe();
    let out = h.ok("theme_set", json!({"mode": "system", "dark": "tones.night", "light": "tones.paper"}));
    assert_eq!(out.data, json!({"mode": "system", "dark": "tones.night", "light": "tones.paper"}));
    let get = |k: &str| h.ok("settings_get", json!({"key": k})).data["value"].clone();
    assert_eq!((get("Theme"), get("ThemeDark"), get("ThemeLight")), (json!("system"), json!("tones.night"), json!("tones.paper")));
    let changed: Vec<String> = events
        .try_iter()
        .filter_map(|e| match e {
            csm_engine::Event::SettingChanged { key, .. } => Some(key),
            _ => None,
        })
        .collect();
    assert_eq!(changed, vec!["Theme", "ThemeDark", "ThemeLight"], "the UI follows a change made elsewhere");
    // Turned off: the choice stays, and theme_list says what is drawn instead.
    h.ok("addon_disable", json!({"id": "tones"}));
    let l = h.ok("theme_list", json!({})).data;
    assert_eq!(l["dark"]["id"], "tones.night");
    assert_eq!(l["dark"]["drawn"], "mocha");
    assert!(l["dark"]["fallback"].as_str().unwrap().contains("not available"), "{l}");
    // MCP effects are honest: the view only, so a client is not asked for consent.
    let spec = h.engine.registry().get("theme_set").map(|t| t.effects).unwrap();
    assert!(!spec.needs_consent(), "{spec:?}");
    h.engine.shutdown();
}

#[test]
fn an_enabled_theme_reloads_when_its_file_changes_and_stays_enabled() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let root = dir.path().join("addons");
    write_addon(&root, "tones", THEMES);
    let h = start(&state);
    h.wait_until("add-ons", |s| s.addons.len() == 1);
    enable_id(&h, "tones");
    let base_of = |s: &csm_engine::Snapshot| s.addons.first().and_then(|a| a.themes.first()).and_then(|t| t.colors.get("base").cloned());
    h.wait_until("the first palette", |s| base_of(s).as_deref() == Some("#101010"));
    // Let the watcher take its first look, then edit the theme (only the theme).
    std::thread::sleep(std::time::Duration::from_millis(1500));
    write_addon(&root, "tones", &THEMES.replace("#101010", "#202020"));
    h.wait_until("the edited palette", |s| base_of(s).as_deref() == Some("#202020"));
    let l = list(&h);
    assert_eq!(find(&l, "tones")["state"], "loaded", "a theme-only edit keeps the approval: {l}");
    // Anything else in the file still turns the add-on off until it is approved again.
    std::thread::sleep(std::time::Duration::from_millis(1500));
    write_addon(&root, "tones", &format!("{}\n[[slash]]\ncommand = \"/x\"\n", THEMES.replace("#101010", "#202020")));
    h.wait_until("off again", |s| s.addons.first().is_some_and(|a| a.state == csm_engine::addons::AddonState::Disabled));
    h.engine.shutdown();
}

const FONT_THEMES: &str = r##"
[addon]
id = "fonts"
name = "Fonts"
version = "0.1.0"
api = 1
kind = "pack"

[[theme]]
id = "good"
name = "Good"
base = "light"
[theme.style]
ui_font = "fonts/good.otf"

[[theme]]
id = "broken"
name = "Broken"
base = "light"
[theme.style]
ui_font = "fonts/broken.ttf"
terminal_font = "fonts/missing.ttf"

[[theme]]
id = "escape"
name = "Escape"
base = "light"
[theme.style]
ui_font = "../outside.ttf"
"##;

#[test]
fn theme_fonts_load_on_the_scan_and_bad_ones_are_reported_not_fatal() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let root = dir.path().join("addons");
    write_addon(&root, "fonts", FONT_THEMES);
    fs::create_dir_all(root.join("fonts").join("fonts")).unwrap();
    let font = include_bytes!("../../../samples/addons/theme-fun/fonts/W95FA.otf");
    fs::write(root.join("fonts").join("fonts").join("good.otf"), font).unwrap();
    fs::write(root.join("fonts").join("fonts").join("broken.ttf"), b"not a font").unwrap();
    let h = start(&state);
    h.wait_until("add-ons", |s| s.addons.len() == 1);
    let snap = h.engine.view();
    let a = snap.addons.iter().find(|a| a.id == "fonts").unwrap();
    assert_eq!(a.theme_fonts.len(), 1, "only the good font loaded: {:?}", a.theme_fonts);
    assert_eq!((a.theme_fonts[0].0.as_str(), a.theme_fonts[0].1.0.len()), ("fonts/good.otf", font.len()));
    let problems = a.problems.join("\n");
    assert!(problems.contains("broken") && problems.contains("not a TrueType or OpenType font"), "{problems}");
    assert!(problems.contains("fonts/missing.ttf") && problems.contains("no such file"), "{problems}");
    assert!(problems.contains("style.ui_font") && problems.contains("`..`"), "the escaping path is refused at parse: {problems}");
    assert!(problems.contains("the default font is used"), "{problems}");
    // The themes themselves still load.
    assert_eq!(a.themes.len(), 3);
    h.engine.shutdown();
}
