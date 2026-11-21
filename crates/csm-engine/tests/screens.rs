#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Add-on screens end to end through the engine with the echo test plugin (a real child process):
//! declaring screens, opening one, the first render, an event round trip, a keyed patch, a patch
//! on the wrong base (CSM asks for a full render), an over-cap tree (refused, the old tree stays),
//! a render flood, the process stopping and coming back, closing, and the tools' honest effects.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_core::addons::screen::{EventKind, Kind, ScreenEvent};
use csm_engine::{Caller, Event, ScreenPhase};
use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_csm-echo-plugin");

fn fixture(dir: &Path, mode: &str) -> std::path::PathBuf {
    let state = dir.join("state.json");
    let folder = dir.join("addons").join("echo-test");
    fs::create_dir_all(&folder).unwrap();
    let exe = serde_json::to_string(EXE).unwrap();
    fs::write(
        folder.join("addon.toml"),
        format!(
            r#"
[addon]
id = "echo-test"
name = "Echo test"
version = "0.1.0"
api = 1
kind = "process"

[process]
argv = [{exe}, "{mode}"]
effects = ["writes_filesystem"]
"#
        ),
    )
    .unwrap();
    state
}

fn start(state: &Path) -> H {
    let state = state.to_path_buf();
    H::start(test_app(), move |o| {
        o.save_path = Some(state);
        o.restore_on_start = false;
    })
}

fn enable(h: &H) {
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    let list = h.ok("addon_list", json!({})).data["addons"].clone();
    let a = list.as_array().unwrap().iter().find(|a| a["id"] == "echo-test").cloned().unwrap();
    h.ok("addon_enable", json!({"id": "echo-test", "version": a["version"], "digest": a["digest"]}));
}

/// Polls `f` until it gives a value, for up to 15 s.
fn wait<T>(what: &str, f: impl Fn() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// An enabled, running add-on with its screens declared.
fn started(mode: &str) -> (H, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), mode));
    enable(&h);
    if mode != "noscreens" {
        wait("the screens to be declared", || (!h.engine.screens().declared().is_empty()).then_some(()));
    }
    (h, dir)
}

fn open(h: &H) -> String {
    let out = h.ok("screen_open", json!({"screen": "echo-test.main"}));
    assert_eq!(out.data["created"], true);
    out.data["instance"].as_str().unwrap().to_owned()
}

fn status_text(h: &H, instance: &str) -> Option<(u64, String)> {
    let t = h.engine.screens().tree(instance)?;
    match &t.find("status")?.kind {
        Kind::Text { text, .. } => Some((t.rev, text.clone())),
        _ => None,
    }
}

fn wait_status(h: &H, instance: &str, want: &str) -> u64 {
    wait(&format!("status `{want}`"), || status_text(h, instance).filter(|(_, t)| t.contains(want)).map(|(r, _)| r))
}

fn plugin_state(h: &H) -> Value {
    h.ok("echo-test.screen_state", json!({})).data
}

#[test]
fn screens_are_declared_and_a_screen_opens_with_its_first_render() {
    let (h, _dir) = started("ok");
    let declared = h.engine.screens().declared();
    assert_eq!(declared.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), vec!["echo-test.main", "echo-test.proj"]);
    assert_eq!(declared[0].addon_name, "Echo test plugin");
    let list = h.ok("screen_list", json!({}));
    assert_eq!(list.data["screens"].as_array().unwrap().len(), 2);
    assert!(list.data["instances"].as_array().unwrap().is_empty());
    let i = open(&h);
    wait_status(&h, &i, "opened");
    let info = h.engine.screens().instance(&i).unwrap();
    assert_eq!((info.phase, info.rev, info.addon.as_str()), (ScreenPhase::Ready, 1, "echo-test"));
    assert!(info.error.is_none());
    let tree = h.engine.screens().tree(&i).unwrap();
    assert!(tree.find("files").is_some() && tree.find("name").is_some());
    assert_eq!(tree.nodes, 12);
    // The open request carried the screen id, the context and (when given) the theme.
    let st = plugin_state(&h);
    assert_eq!(st["log"][0]["open"], json!(i));
    assert_eq!(st["log"][0]["screen"], "echo-test.main");
    // The capability and no handshake problems.
    let status = h.ok("plugin_status", json!({})).data;
    assert!(!status["processes"][0]["problems"].to_string().contains("screen"), "{status}");
    h.engine.shutdown();
}

#[test]
fn opening_the_same_screen_twice_focuses_it_and_the_context_comes_from_the_session() {
    let (h, _dir) = started("ok");
    let events = h.engine.subscribe();
    let i = open(&h);
    let again = h.ok("screen_open", json!({"screen": "echo-test.main"}));
    assert_eq!((again.data["instance"].as_str().unwrap(), &again.data["created"]), (i.as_str(), &json!(false)));
    // The tab is brought to the front, both times.
    let mut focus = 0;
    while let Ok(e) = events.recv_timeout(Duration::from_millis(300)) {
        if matches!(&e, Event::ScreenFocusRequested { instance } if *instance == i) {
            focus += 1;
        }
    }
    assert_eq!(focus, 2);
    // A project screen needs a project; a session gives it one, with its folder.
    let err = h.ui("screen_open", json!({"screen": "echo-test.proj"})).unwrap_err();
    assert!(err.to_string().contains("project screen"), "{err}");
    let s = h.session("claude");
    let out = h.ok("screen_open", json!({"screen": "echo-test.proj", "context": {"session_id": s, "args": {"a": 1}}, "theme": "dark"}));
    let p = out.data["instance"].as_str().unwrap().to_owned();
    wait_status(&h, &p, "opened");
    let ctx = h.engine.screens().instance(&p).unwrap().context;
    assert_eq!(ctx.session_id.as_deref(), Some(s.as_str()));
    assert!(ctx.project.is_some());
    let st = plugin_state(&h);
    let open = st["log"].as_array().unwrap().iter().find(|l| l["open"] == json!(p)).unwrap();
    assert_eq!(open["theme"], "dark");
    assert_eq!(open["context"]["args"]["a"], 1);
    assert_eq!(open["context"]["session_id"], json!(s));
    // Unknown screens and sessions are refused with a hint.
    assert!(h.ui("screen_open", json!({"screen": "echo-test.nope"})).unwrap_err().to_string().contains("screen_list"));
    assert!(h.ui("screen_open", json!({"screen": "echo-test.main", "context": {"session_id": "ghost"}})).is_err());
    h.engine.shutdown();
}

#[test]
fn an_event_round_trips_and_a_rev_is_attached() {
    let (h, _dir) = started("ok");
    let i = open(&h);
    wait_status(&h, &i, "opened");
    h.engine.screen_event(&i, ScreenEvent::new("btn", EventKind::Click, Value::Null)).unwrap();
    let rev = wait_status(&h, &i, "click btn");
    assert_eq!(rev, 2);
    h.engine.screen_event(&i, ScreenEvent::new("name", EventKind::Submit, json!("Ada"))).unwrap();
    wait_status(&h, &i, "submit name \"Ada\"");
    let st = plugin_state(&h);
    let evs: Vec<&Value> = st["log"].as_array().unwrap().iter().filter(|l| l.get("event").is_some()).collect();
    assert_eq!(evs.len(), 2);
    assert_eq!((evs[0]["event"].as_str(), evs[0]["kind"].as_str(), evs[0]["rev"].as_u64()), (Some("btn"), Some("click"), Some(1)), "the rev the person saw");
    assert_eq!(evs[1]["value"], "Ada");
    // An unknown instance is refused, and so is an event for a screen whose add-on is gone.
    assert!(h.engine.screen_event("nope", ScreenEvent::new("btn", EventKind::Click, Value::Null)).is_err());
    h.engine.shutdown();
}

#[test]
fn a_keyed_patch_is_applied_and_a_wrong_base_makes_csm_ask_for_a_full_render() {
    let (h, _dir) = started("ok");
    let i = open(&h);
    wait_status(&h, &i, "opened");
    h.engine.screen_event(&i, ScreenEvent::new("patch", EventKind::Click, Value::Null)).unwrap();
    let rev = wait_status(&h, &i, "patched 2");
    assert_eq!(rev, 2, "the patch replaced only `status`; the rest of the tree is the first render's");
    assert!(h.engine.screens().tree(&i).unwrap().find("files").is_some());
    // A patch on the wrong base is dropped and CSM sends screen/refresh; the plugin answers with a full render.
    h.engine.screen_event(&i, ScreenEvent::new("badpatch", EventKind::Click, Value::Null)).unwrap();
    wait_status(&h, &i, "refreshed");
    assert_eq!(plugin_state(&h)["refreshes"], 1);
    h.engine.shutdown();
}

#[test]
fn an_over_cap_tree_is_refused_keeps_the_old_one_and_the_plugin_is_told() {
    let (h, _dir) = started("ok");
    let i = open(&h);
    wait_status(&h, &i, "opened");
    h.engine.screen_event(&i, ScreenEvent::new("bad", EventKind::Click, Value::Null)).unwrap();
    wait("the refusal", || h.engine.screens().instance(&i).and_then(|x| x.error));
    let info = h.engine.screens().instance(&i).unwrap();
    assert!(info.error.unwrap().contains("5000"), "the reason names the cap");
    assert_eq!(info.rev, 1, "the previous tree stays");
    assert_eq!(info.phase, ScreenPhase::Ready);
    assert_eq!(status_text(&h, &i).unwrap().1, "opened");
    let errors = wait("screen/error to reach the plugin", || {
        let e = plugin_state(&h)["errors"].clone();
        e.as_array().filter(|a| !a.is_empty()).cloned()
    });
    assert!(errors[0].as_str().unwrap().contains("5000"));
    // The next good render clears the error.
    h.engine.screen_event(&i, ScreenEvent::new("btn", EventKind::Click, Value::Null)).unwrap();
    wait_status(&h, &i, "click btn");
    assert!(h.engine.screens().instance(&i).unwrap().error.is_none());
    h.engine.shutdown();
}

#[test]
fn an_over_cap_answer_to_open_leaves_a_retry_state() {
    let (h, _dir) = started("badopen");
    let i = open(&h);
    wait("the refusal", || h.engine.screens().instance(&i).filter(|x| x.error.is_some()));
    let info = h.engine.screens().instance(&i).unwrap();
    assert_eq!(info.phase, ScreenPhase::NotAnswering);
    assert!(h.engine.screens().tree(&i).is_none());
    // Asking again retries the open (a new screen/open request) on the same instance.
    let again = h.ok("screen_open", json!({"screen": "echo-test.main"}));
    assert_eq!(again.data["instance"], json!(i));
    wait("the second open request", || (plugin_state(&h)["log"].as_array().unwrap().iter().filter(|l| l.get("open").is_some()).count() == 2).then_some(()));
    h.engine.shutdown();
}

#[test]
fn a_render_flood_ends_on_the_latest_render() {
    let (h, _dir) = started("ok");
    let i = open(&h);
    wait_status(&h, &i, "opened");
    let events = h.engine.subscribe();
    h.engine.screen_event(&i, ScreenEvent::new("flood", EventKind::Click, Value::Null)).unwrap();
    let rev = wait_status(&h, &i, "flood 200");
    assert_eq!(rev, 201);
    let changes = std::iter::from_fn(|| events.try_recv().ok()).filter(|e| matches!(e, Event::ScreenChanged { .. })).count();
    assert!(changes >= 1, "UI repaints are driven by ScreenChanged ({changes})");
    h.engine.shutdown();
}

#[test]
fn the_screen_survives_the_process_stopping_and_opens_again_when_it_is_back() {
    let (h, _dir) = started("ok");
    let i = open(&h);
    wait_status(&h, &i, "opened");
    // The process crashes: the tab stays, with its last tree, and the add-on offers nothing meanwhile.
    let _ = h.ui("echo-test.exit", json!({}));
    wait("the stopped placeholder", || h.engine.screens().instance(&i).filter(|x| x.phase == ScreenPhase::Stopped));
    assert!(h.engine.screens().tree(&i).is_some(), "the last tree stays on screen");
    assert!(h.engine.screens().declared().is_empty());
    assert!(h.engine.screen_event(&i, ScreenEvent::new("btn", EventKind::Click, Value::Null)).is_err(), "no event goes to a stopped add-on");
    // It restarts by itself, and the instance is opened again on the new process.
    wait("the screen to be opened again", || h.engine.screens().instance(&i).filter(|x| x.phase == ScreenPhase::Ready && x.rev >= 1));
    let rev = wait_status(&h, &i, "opened");
    assert_eq!(rev, 1, "a new process starts its revisions again");
    assert_eq!(h.engine.screens().declared().len(), 2);
    h.engine.shutdown();
}

#[test]
fn closing_tells_the_plugin_and_disabling_the_add_on_closes_its_screens() {
    let (h, _dir) = started("ok");
    let i = open(&h);
    wait_status(&h, &i, "opened");
    h.ok("screen_close", json!({"instance": i}));
    assert!(h.engine.screens().instance(&i).is_none());
    wait("screen/closed to reach the plugin", || (plugin_state(&h)["closed"] == json!([i])).then_some(()));
    assert!(h.ui("screen_close", json!({"instance": i})).is_err());
    // Pop out and dock are state of the instance.
    let j = open(&h);
    h.ok("screen_pop_out", json!({"instance": j}));
    assert!(h.engine.screens().instance(&j).unwrap().popped_out);
    h.ok("screen_dock", json!({"instance": j}));
    assert!(!h.engine.screens().instance(&j).unwrap().popped_out);
    // Disabling removes the add-on's screens and instances.
    h.ok("addon_disable", json!({"id": "echo-test"}));
    wait("the screens to go", || (h.engine.screens().instances().is_empty() && h.engine.screens().declared().is_empty()).then_some(()));
    assert!(h.ui("screen_open", json!({"screen": "echo-test.main"})).is_err());
    h.engine.shutdown();
}

#[test]
fn a_plugin_that_does_not_declare_screens_has_none() {
    let (h, _dir) = started("noscreens");
    wait("the add-on to run", || (h.ok("plugin_status", json!({})).data["processes"][0]["state"] == "running").then_some(()));
    assert!(h.engine.screens().declared().is_empty());
    assert!(h.ok("screen_list", json!({})).data["screens"].as_array().unwrap().is_empty());
    h.engine.shutdown();
}

#[test]
fn the_screen_tools_declare_honest_effects() {
    let (h, _dir) = started("ok");
    let reg = h.engine.registry();
    for id in ["screen_list", "screen_open", "screen_close", "screen_pop_out", "screen_dock"] {
        let spec = reg.get(id).unwrap_or_else(|| panic!("{id} is not registered"));
        assert_eq!(spec.toolset, csm_engine::Toolset::Addons, "{id}");
        assert!(!spec.effects.needs_consent(), "{id} only changes what the app shows");
        assert!(!spec.effects.contains(csm_engine::Effects::SENDS_INPUT | csm_engine::Effects::LAUNCHES_PROCESS), "{id}");
    }
    assert!(reg.get("screen_list").unwrap().effects.read_only_hint());
    assert!(!reg.get("screen_open").unwrap().effects.read_only_hint(), "opening changes what the app shows");
    // So an MCP client runs them without a card: it can say "I opened the review for you".
    let ext = Caller::External { client: "claude-code".into(), session_id: None };
    let out = h.engine.call(ext.clone(), "screen_open", json!({"screen": "echo-test.main"})).unwrap();
    assert_eq!(out.data["created"], true);
    assert!(h.engine.call(ext, "screen_list", json!({})).is_ok());
    h.engine.shutdown();
}
