#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Process add-ons end to end through the engine: the handshake, a plugin's tool called from the
//! app, effects-based consent for an MCP caller, a `csm/call` from the plugin, events forwarded to
//! it, a crash and its restart, a manual restart, disabling, and the caps on a misbehaving plugin.
//! The plugin is `csm-echo-plugin` (tests/support/echo_plugin.rs), a real child process.

mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_engine::{Caller, Event};
use serde_json::{Value, json};

const EXE: &str = env!("CARGO_BIN_EXE_csm-echo-plugin");

/// The manifest of the test add-on. `mode` is the plugin's first argument.
fn manifest(mode: &str) -> String {
    let exe = serde_json::to_string(EXE).unwrap();
    format!(
        r#"
[addon]
id = "echo-test"
name = "Echo test"
version = "0.1.0"
description = "A process add-on for the tests"
api = 1
kind = "process"

[process]
argv = [{exe}, "{mode}"]
effects = ["writes_filesystem"]
"#
    )
}

/// A state folder whose `addons/echo-test/` holds the test add-on in `mode`.
fn fixture(dir: &Path, mode: &str) -> std::path::PathBuf {
    let state = dir.join("state.json");
    let folder = dir.join("addons").join("echo-test");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("addon.toml"), manifest(mode)).unwrap();
    state
}

fn start(state: &Path) -> H {
    let state = state.to_path_buf();
    H::start(test_app(), move |o| {
        o.save_path = Some(state);
        o.restore_on_start = false;
    })
}

/// Enables the add-on the way the consent dialog does: with the version and digest it shows.
fn enable(h: &H) {
    let list = h.ok("addon_list", json!({})).data["addons"].clone();
    let a = list.as_array().unwrap().iter().find(|a| a["id"] == "echo-test").cloned().unwrap_or_else(|| panic!("no echo-test in {list}"));
    h.ok("addon_enable", json!({"id": "echo-test", "version": a["version"], "digest": a["digest"]}));
}

fn processes(h: &H) -> Vec<Value> {
    h.ok("plugin_status", json!({})).data["processes"].as_array().cloned().unwrap_or_default()
}

fn process(h: &H) -> Option<Value> {
    processes(h).into_iter().find(|p| p["id"] == "echo-test")
}

/// Polls `plugin_status` until the add-on's row satisfies `f`. Panics with the last row.
fn wait_process(h: &H, what: &str, f: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let row = process(h).unwrap_or(Value::Null);
        if f(&row) {
            return row;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; last row: {row}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn running(h: &H) -> Value {
    wait_process(h, "the add-on to be running", |p| p["state"] == "running")
}

#[test]
fn handshake_registers_the_tools_and_a_tool_call_reaches_the_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    let row = running(&h);
    assert_eq!(row["name"], "Echo test plugin");
    assert!(row["pid"].as_u64().is_some(), "{row}");
    assert!(row["tools"].as_array().unwrap().iter().any(|t| t == "echo-test.echo"), "{row}");
    // The tool is in the registry as an add-on tool, and in the palette's list.
    let reg = h.engine.registry();
    let spec = reg.get("echo-test.echo").expect("registered");
    assert_eq!(spec.toolset, csm_engine::Toolset::Addons);
    assert!(reg.all().iter().any(|s| s.id == "echo-test.write"));
    // The tool runs in the plugin and its message and data come back.
    let out = h.ok("echo-test.echo", json!({"hello": 1}));
    assert_eq!(out.message, "echoed");
    assert_eq!(out.data["echo"]["hello"], 1);
    // The Settings row sees the same state.
    let row_in_list = h.engine.view().addons.iter().find(|a| a.id == "echo-test").and_then(|a| a.process.clone()).expect("process state in the view");
    assert_eq!(serde_json::to_value(row_in_list.state).unwrap(), "running");
    h.engine.shutdown();
}

#[test]
fn mcp_consent_follows_the_declared_effects() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    let ext = Caller::External { client: "test-client".into(), session_id: None };
    // An explicit empty `effects` list: an MCP caller runs it without a question.
    assert_eq!(h.engine.call(ext.clone(), "echo-test.echo", json!({})).unwrap().message, "echoed");
    // writes_filesystem declared: the person is asked first, and a denial stops it.
    let events = h.engine.subscribe();
    let rx = h.engine.submit(ext.clone(), "echo-test.write", json!({}));
    let request = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ConsentRequested { request }) if request["tool"] == "echo-test.write" => break request,
            Ok(_) => continue,
            Err(e) => panic!("no consent request: {e}"),
        }
    };
    assert!(rx.try_recv().is_err(), "must not run before the answer");
    h.ui("consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err(csm_engine::ToolError::Declined(_))));
    // A tool that omits `effects` inherits the add-on's declared ones, so it asks too.
    let rx = h.engine.submit(ext, "echo-test.inherit", json!({}));
    let request = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ConsentRequested { request }) if request["tool"] == "echo-test.inherit" => break request,
            Ok(_) => continue,
            Err(e) => panic!("no consent request for the inheriting tool: {e}"),
        }
    };
    assert!(rx.try_recv().is_err(), "must not run before the answer");
    h.ui("consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"})).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap().message, "inherited");
    h.engine.shutdown();
}

#[test]
fn a_plugin_calls_csm_through_csm_call_as_the_add_on() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    let out = h.ok("echo-test.callback", json!({}));
    assert_eq!(out.message, "called");
    assert!(out.data.is_object() || out.data.is_array(), "session_list's data came back: {}", out.data);
    h.engine.shutdown();
}

#[test]
fn subscribed_events_reach_the_plugin() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    h.session("sleep 1");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let seen = h.ok("echo-test.echo", json!({})).data["events_seen"].as_u64().unwrap_or(0);
        if seen >= 1 {
            break;
        }
        assert!(Instant::now() < deadline, "the plugin never saw a session event");
        std::thread::sleep(Duration::from_millis(50));
    }
    h.engine.shutdown();
}

#[test]
fn a_crashed_plugin_is_restarted_and_the_crash_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    let first = running(&h);
    let pid = first["pid"].clone();
    // The exit tool makes the process exit: the call fails, the crash is recorded, and it starts
    // again after its backoff.
    assert!(h.ui("echo-test.exit", json!({})).is_err());
    let row = wait_process(&h, "a restart", |p| p["state"] == "running" && p["restarts"].as_u64().unwrap_or(0) >= 1);
    assert_ne!(row["pid"], pid, "a new process");
    assert!(row["last_error"].as_str().unwrap_or_default().contains("exited"), "{row}");
    assert_eq!(h.ok("echo-test.echo", json!({"again": true})).data["echo"]["again"], true);
    h.engine.shutdown();
}

#[test]
fn plugin_restart_starts_it_again_and_refuses_what_is_not_a_process_add_on() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    let first = running(&h);
    h.ok("plugin_restart", json!({"id": "echo-test"}));
    let row = wait_process(&h, "the restart", |p| p["restarts"].as_u64().unwrap_or(0) >= 1 && p["state"] == "running");
    assert_ne!(row["pid"], first["pid"]);
    let missing = h.ui("plugin_restart", json!({"id": "nope"})).unwrap_err();
    assert!(missing.to_string().contains("no add-on"), "{missing}");
    h.ok("addon_disable", json!({"id": "echo-test"}));
    let refused = h.ui("plugin_restart", json!({"id": "echo-test"})).unwrap_err();
    assert!(refused.to_string().contains("not an enabled process add-on"), "{refused}");
    h.engine.shutdown();
}

#[test]
fn disabling_stops_the_process_and_removes_its_tools() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    h.ok("addon_disable", json!({"id": "echo-test"}));
    let deadline = Instant::now() + Duration::from_secs(10);
    while h.engine.registry().get("echo-test.echo").is_some() {
        assert!(Instant::now() < deadline, "the tools were never removed");
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(process(&h).is_none(), "a disabled add-on is not listed as a process");
    assert!(h.ui("echo-test.echo", json!({})).is_err());
    h.engine.shutdown();
}

#[test]
fn an_oversized_line_stops_the_plugin_and_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "oversize"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    let row = wait_process(&h, "the oversized line to be refused", |p| p["last_error"].is_string());
    assert!(row["last_error"].as_str().unwrap().contains("bytes"), "{row}");
    assert!(row["state"] != "running", "{row}");
    assert!(h.engine.registry().get("echo-test.echo").is_none(), "no tools from a plugin that was stopped");
    h.engine.shutdown();
}

#[test]
fn invalid_json_stops_the_plugin_and_is_recorded() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "badjson"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    let row = wait_process(&h, "the bad message to be refused", |p| p["last_error"].is_string());
    assert!(row["last_error"].as_str().unwrap().contains("bad message"), "{row}");
    h.engine.shutdown();
}

#[test]
fn an_undeclared_effect_drops_only_that_tool() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "undeclared"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    let row = running(&h);
    let tools: Vec<&str> = row["tools"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert!(tools.contains(&"echo-test.echo"), "{tools:?}");
    assert!(!tools.contains(&"echo-test.sneaky"), "{tools:?}");
    assert!(h.engine.registry().get("echo-test.sneaky").is_none());
    let problems = row["problems"].as_array().unwrap();
    assert!(problems.iter().any(|p| p.as_str().unwrap_or_default().contains("did not declare")), "{row}");
    h.engine.shutdown();
}

#[test]
fn nothing_is_started_before_the_person_enables_it() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    // Give a wrongly started process time to answer the handshake and register its tools.
    std::thread::sleep(Duration::from_millis(500));
    assert!(h.engine.registry().get("echo-test.echo").is_none(), "a disabled add-on has no tools");
    assert!(process(&h).is_none(), "a disabled add-on is not a running process");
    h.engine.shutdown();
}

#[test]
fn shutdown_stops_the_plugin_within_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    let started = Instant::now();
    h.engine.shutdown();
    assert!(started.elapsed() < Duration::from_secs(10), "shutdown took {:?}", started.elapsed());
}
