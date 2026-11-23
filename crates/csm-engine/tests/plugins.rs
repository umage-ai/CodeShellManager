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
    manifest_with(mode, r#"["writes_filesystem", "stores_secrets"]"#, "")
}

/// The same with its `[process]` `effects` list (TOML) and extra lines for the `[process]` table.
fn manifest_with(mode: &str, effects: &str, extra: &str) -> String {
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
effects = {effects}
{extra}
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
fn an_oversized_argument_is_refused_before_it_is_sent_and_the_plugin_keeps_running() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    let first = running(&h);
    // 1.5 MiB of arguments: the plugin would stop reading at 1 MiB, so CSM refuses the call.
    let big = "x".repeat(1536 * 1024);
    let err = h.ui("echo-test.echo", json!({"big": big})).unwrap_err();
    assert!(err.to_string().contains("1 MiB"), "{err}");
    let row = running(&h);
    assert_eq!(row["pid"], first["pid"], "the same process, not restarted");
    assert_eq!(h.ok("echo-test.echo", json!({"small": 1})).data["echo"]["small"], 1);
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

// ── What plugins contribute to sessions: remotes, status notes, secrets ─────────────────────

/// A plugin tool that runs a CSM tool through `csm/call`: the answer (`data` or the error).
fn plugin_csm(h: &H, tool: &str, params: Value) -> Value {
    // The `csm/call` answer: `{message, data}`, or an error whose `data` is the tool error.
    h.ok("echo-test.csm", json!({"tool": tool, "params": params})).data["data"].clone()
}

fn offer(h: &H, remotes: Value) {
    h.ok("echo-test.offer", json!({"remotes": remotes}));
    // A notification: wait for the snapshot to carry it.
    let want = remotes.as_array().map_or(0, |a| a.iter().filter(|r| r["id"] != "bad id").count());
    h.wait_until("the remotes", |s| s.remotes.len() == want);
}

fn ssh_remote() -> Value {
    json!({"id": "agent-1", "label": "Agent 1", "detail": "idle", "session": {
        "name": "Agent 1",
        "ssh": {"host": "agent-alias", "config_file": "cfg/ssh_config", "remote_argv": ["tmux", "new-session", "-A", "-s", "main"]},
        "resume": "rerun"
    }})
}

fn view_of(h: &H, id: &str) -> csm_engine::SessionView {
    h.engine.view().sessions.iter().find(|s| s.id == id).cloned().unwrap_or_else(|| panic!("no session {id}"))
}

fn has<'a>(s: &'a csm_engine::Snapshot, id: &str) -> Option<&'a csm_engine::SessionView> {
    s.sessions.iter().find(|v| v.id == id)
}

#[test]
fn a_plugin_offers_remotes_and_opening_one_starts_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    offer(
        &h,
        json!([
            ssh_remote(),
            {"id": "agent-2", "label": "Agent 2", "tool": "echo-test.open_agent", "params": {"name": "Second"}},
            {"id": "bad id", "label": "x", "session": {}}
        ]),
    );
    let listed = h.ok("remote_list", json!({})).data["remotes"].clone();
    assert_eq!(listed.as_array().unwrap().len(), 2, "the bad entry was dropped: {listed}");
    assert_eq!(listed[0]["addon"], "echo-test");
    assert_eq!(listed[0]["addon_name"], "Echo test plugin");
    assert_eq!(listed[0]["opens"], "session");
    assert_eq!(listed[1]["opens"], "tool");

    // A session entry opens as given: ssh with the config file, the alias, the remote command as
    // one quoted argument, and the session is the add-on's, re-run on resume.
    let before = h.pty_count();
    let out = h.ok("remote_open", json!({"addon": "echo-test", "remote_id": "agent-1"}));
    let id = out.data["id"].as_str().unwrap().to_owned();
    let pty = h.pty(before);
    assert_eq!(&pty.argv[..2], ["ssh", "-F"], "{:?}", pty.argv);
    assert_eq!(pty.argv[2], "cfg/ssh_config");
    assert!(pty.argv.contains(&"agent-alias".to_owned()), "{:?}", pty.argv);
    assert_eq!(pty.argv.last().map(String::as_str), Some("tmux new-session -A -s main"));
    let v = view_of(&h, &id);
    assert_eq!(v.owner_addon.as_deref(), Some("echo-test"));
    assert_eq!(v.name, "Agent 1");

    // A tool entry asks the plugin's own tool what to open.
    let out = h.ok("remote_open", json!({"addon": "echo-test", "remote_id": "agent-2"}));
    let id2 = out.data["id"].as_str().unwrap().to_owned();
    assert_eq!(view_of(&h, &id2).name, "Second");
    assert_eq!(view_of(&h, &id2).owner_addon.as_deref(), Some("echo-test"));

    // Unknown entries and add-ons are refused plainly.
    assert!(h.ui("remote_open", json!({"addon": "echo-test", "remote_id": "nope"})).unwrap_err().to_string().contains("no remote"));
    assert!(h.ui("remote_open", json!({"addon": "other", "remote_id": "agent-1"})).is_err());

    // Opening reaches the network and starts a process: an MCP client is asked first.
    assert!(h.engine.registry().get("remote_open").unwrap().effects.needs_consent());
    assert!(!h.engine.registry().get("remote_list").unwrap().effects.needs_consent());

    // Withdrawing the list, and disabling the add-on, take the entries away.
    offer(&h, json!([]));
    offer(&h, json!([ssh_remote()]));
    h.ok("addon_disable", json!({"id": "echo-test"}));
    h.wait_until("the remotes to go with the add-on", |s| s.remotes.is_empty());
    h.engine.shutdown();
}

#[test]
fn a_plugin_sets_a_status_and_note_on_its_own_sessions_only() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    offer(&h, json!([ssh_remote()]));
    let mine = h.ok("remote_open", json!({"addon": "echo-test", "remote_id": "agent-1"})).data["id"].as_str().unwrap().to_owned();
    let theirs = h.session("sleep 1");

    let set = plugin_csm(&h, "session_annotate", json!({"session_id": mine, "status": "working", "annotation": "running remotely"}));
    assert_eq!(set["session_id"], mine.as_str(), "{set}");
    // The note and the status can land a step apart while the session is still starting (the
    // status waits for it to leave Starting), so wait for both; under load the gap was seen.
    let snap = h.wait_until("the note and the status", |s| {
        has(s, &mine).is_some_and(|v| v.annotation.is_some() && v.status == csm_engine::Status::Working)
    });
    let v = has(&snap, &mine).unwrap();
    assert_eq!(v.annotation.as_deref(), Some("running remotely"));
    assert_eq!(v.status, csm_engine::Status::Working);
    assert!(v.folder_short.ends_with("running remotely"), "{}", v.folder_short);

    // Someone else's session, a made-up status, the person, and an MCP client are all refused.
    let other = plugin_csm(&h, "session_annotate", json!({"session_id": theirs, "annotation": "mine now"}));
    assert_eq!(other["code"], "refused", "{other}");
    let bad = plugin_csm(&h, "session_annotate", json!({"session_id": mine, "status": "exited"}));
    assert_eq!(bad["code"], "invalid_params", "{bad}");
    assert!(h.ui("session_annotate", json!({"session_id": mine, "annotation": "x"})).is_err());
    let ext = Caller::External { client: "test-client".into(), session_id: None };
    assert!(h.engine.call(ext, "session_annotate", json!({"session_id": mine, "annotation": "x"})).is_err());
    assert!(view_of(&h, &theirs).annotation.is_none());

    // Clearing: an empty string.
    plugin_csm(&h, "session_annotate", json!({"session_id": mine, "status": "", "annotation": ""}));
    h.wait_until("the note to clear", |s| has(s, &mine).is_some_and(|v| v.annotation.is_none() && v.status != csm_engine::Status::Working));

    // It goes with the plugin's process: a restart clears what the last one set.
    plugin_csm(&h, "session_annotate", json!({"session_id": mine, "annotation": "again"}));
    h.wait_until("the note", |s| has(s, &mine).is_some_and(|v| v.annotation.is_some()));
    assert!(h.ui("echo-test.exit", json!({})).is_err());
    h.wait_until("the note to go with the process", |s| has(s, &mine).is_some_and(|v| v.annotation.is_none()));
    // Ownership outlives the process: the session still names its add-on.
    assert_eq!(view_of(&h, &mine).owner_addon.as_deref(), Some("echo-test"));
    h.engine.shutdown();
}

#[test]
fn a_plugin_hears_when_its_own_sessions_sleep_and_wake_and_only_those() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    offer(&h, json!([ssh_remote()]));
    let mine = h.ok("remote_open", json!({"addon": "echo-test", "remote_id": "agent-1"})).data["id"].as_str().unwrap().to_owned();
    let theirs = h.session("sleep 1");
    h.ok("session_sleep", json!({"session_id": theirs}));
    h.ok("session_sleep", json!({"session_id": mine}));
    let count_of = |kind: &str| {
        let kinds = h.ok("echo-test.echo", json!({})).data["event_kinds"].clone();
        kinds.as_array().unwrap().iter().filter(|k| *k == kind).count()
    };
    let wait_for_kind = |kind: &str| {
        let deadline = Instant::now() + Duration::from_secs(10);
        while count_of(kind) == 0 {
            assert!(Instant::now() < deadline, "never saw {kind}");
            std::thread::sleep(Duration::from_millis(50));
        }
    };
    wait_for_kind("session_asleep");
    assert_eq!(count_of("session_asleep"), 1, "only the add-on's own session");
    h.ok("session_wake", json!({"session_id": mine}));
    wait_for_kind("session_awake");
    h.ok("session_wake", json!({"session_id": theirs}));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(count_of("session_awake"), 1);
    h.engine.shutdown();
}

#[test]
fn a_plugin_keeps_secrets_in_its_own_namespace_and_nobody_else_can() {
    use csm_services::secrets::{MemoryStore, SecretStore, service_for};
    let dir = tempfile::tempdir().unwrap();
    let store = std::sync::Arc::new(MemoryStore::default());
    let state = fixture(dir.path(), "ok");
    let h = {
        let (state, store) = (state.clone(), store.clone());
        H::start(test_app(), move |o| {
            o.save_path = Some(state);
            o.restore_on_start = false;
            o.secrets = store;
        })
    };
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);

    let set = plugin_csm(&h, "addon_secret_set", json!({"key": "ssh-key.path", "value": "/keys/id"}));
    assert_eq!(set["key"], "ssh-key.path", "{set}");
    assert_eq!(store.get(&service_for("echo-test"), "ssh-key.path").unwrap().as_deref(), Some("/keys/id"));
    assert_eq!(store.get(&service_for("someone-else"), "ssh-key.path").unwrap(), None, "the namespace is the caller's");
    let got = plugin_csm(&h, "addon_secret_get", json!({"key": "ssh-key.path"}));
    assert_eq!(got["value"], "/keys/id");
    assert_eq!(plugin_csm(&h, "addon_secret_get", json!({"key": "missing"}))["value"], Value::Null);

    // The secret is in no state file.
    h.ok("session_new", json!({"command": "sleep 1", "folder": "."}));
    std::thread::sleep(Duration::from_millis(900));
    let saved = fs::read_to_string(dir.path().join("state.json")).unwrap_or_default();
    assert!(!saved.is_empty() && !saved.contains("/keys/id"), "a secret reached state.json");

    // Bad keys and oversize values are refused; the person, the CLI and MCP cannot touch secrets.
    assert_eq!(plugin_csm(&h, "addon_secret_get", json!({"key": "Bad Key"}))["code"], "invalid_params");
    let big = "x".repeat(csm_services::secrets::MAX_VALUE_BYTES + 1);
    assert_eq!(plugin_csm(&h, "addon_secret_set", json!({"key": "k", "value": big}))["code"], "invalid_params");
    for caller in [Caller::Ui, Caller::Cli, Caller::External { client: "test-client".into(), session_id: None }] {
        let e = h.engine.call(caller, "addon_secret_get", json!({"key": "ssh-key.path"})).unwrap_err();
        assert!(e.to_string().contains("belong to an add-on"), "{e}");
    }
    assert!(!h.engine.registry().get("addon_secret_get").unwrap().effects.needs_consent());

    let gone = plugin_csm(&h, "addon_secret_delete", json!({"key": "ssh-key.path"}));
    assert_eq!(gone["removed"], true);
    assert_eq!(store.get(&service_for("echo-test"), "ssh-key.path").unwrap(), None);
    h.engine.shutdown();
}

#[test]
fn secrets_need_the_declared_effect() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture(dir.path(), "ok");
    let toml = dir.path().join("addons").join("echo-test").join("addon.toml");
    fs::write(&toml, manifest("ok").replace(r#"effects = ["writes_filesystem", "stores_secrets"]"#, r#"effects = ["writes_filesystem"]"#)).unwrap();
    let h = start(&state);
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    let refused = plugin_csm(&h, "addon_secret_set", json!({"key": "k", "value": "v"}));
    assert_eq!(refused["code"], "refused", "{refused}");
    assert!(refused["message"].as_str().unwrap().contains("stores_secrets"), "{refused}");
    h.engine.shutdown();
}

#[test]
fn a_session_a_plugin_opens_with_csm_call_is_its_own() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    // session_new starts a process, so the plugin's call asks the person first.
    let events = h.engine.subscribe();
    let caller = h.engine.submit(Caller::Ui, "echo-test.csm", json!({"tool": "session_new", "params": {"command": "sleep 1", "folder": "."}}));
    let request = loop {
        match events.recv_timeout(Duration::from_secs(10)) {
            Ok(Event::ConsentRequested { request }) if request["tool"] == "session_new" => break request,
            Ok(_) => continue,
            Err(e) => panic!("no consent request: {e}"),
        }
    };
    h.ui("consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"})).unwrap();
    let out = caller.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    let id = out.data["data"]["id"].as_str().unwrap_or_default().to_owned();
    assert_eq!(view_of(&h, &id).owner_addon.as_deref(), Some("echo-test"), "{}", out.data);
    h.engine.shutdown();
}

// ── Standing consent, caller identity and the data folder ──────────────────────────────

/// A state folder whose add-on has `text` as its manifest.
fn fixture_text(dir: &Path, text: &str) -> std::path::PathBuf {
    let state = dir.join("state.json");
    let folder = dir.join("addons").join("echo-test");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("addon.toml"), text).unwrap();
    state
}

fn addon_row(h: &H) -> Value {
    h.ok("addon_list", json!({})).data["addons"].as_array().unwrap().iter().find(|a| a["id"] == "echo-test").cloned().unwrap()
}

/// Waits for a consent card for `tool` and returns it. Panics when none comes.
fn consent_for(events: &std::sync::mpsc::Receiver<Event>, tool: &str) -> Value {
    loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ConsentRequested { request }) if request["tool"] == tool => return request,
            Ok(_) => continue,
            Err(e) => panic!("no consent card for {tool}: {e}"),
        }
    }
}

/// Whether a consent card for `tool` shows up in the next moments.
fn card_appears(events: &std::sync::mpsc::Receiver<Event>, tool: &str) -> bool {
    let deadline = Instant::now() + Duration::from_millis(400);
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        match events.recv_timeout(left) {
            Ok(Event::ConsentRequested { request }) if request["tool"] == tool => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

const CALLS: &str = r#"calls = ["session_read_screen", "addon_enable", "settings_set", "not_a_tool"]"#;

#[test]
fn standing_calls_run_without_a_card_and_every_other_tool_still_asks() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture_text(dir.path(), &manifest_with("ok", r#"["stores_data"]"#, CALLS)));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    // What the consent dialog shows: the tools that survived, with their effects; the rest are
    // reported and not in force.
    let row = addon_row(&h);
    let shown: Vec<&str> = row["process_calls"].as_array().unwrap().iter().filter_map(|c| c["tool"].as_str()).collect();
    assert_eq!(shown, ["session_read_screen"], "{row}");
    assert_eq!(row["process_calls"][0]["asks"], true, "the tool would ask an MCP caller: {row}");
    assert!(row["process_calls"][0]["effects"].as_str().unwrap().contains("session content"), "{row}");
    let problems = row["problems"].to_string();
    for name in ["addon_enable", "settings_set", "not_a_tool"] {
        assert!(problems.contains(name), "{name} should be reported: {problems}");
    }
    enable(&h);
    running(&h);
    let events = h.engine.subscribe();

    // A listed tool: the plugin's csm/call runs it with no card (the session is missing, so the
    // answer is the tool's own error, which proves it ran).
    let out = h.ok("echo-test.csm", json!({"tool": "session_read_screen", "params": {"session_id": "nope"}}));
    assert!(out.data["data"].to_string().contains("no session"), "{}", out.data);
    assert!(!card_appears(&events, "session_read_screen"), "a listed tool must not ask");

    // A tool outside the list asks as before, and a denial stops it.
    let rx = h.engine.submit(Caller::Ui, "echo-test.csm", json!({"tool": "session_read_output", "params": {"session_id": "nope"}}));
    let request = consent_for(&events, "session_read_output");
    assert_eq!(request["client"], "add-on echo-test");
    h.ui("consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
    let done = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert!(done.data["data"].to_string().contains("was not allowed"), "{}", done.data);

    // The standing consent is the add-on's: an MCP client calling the same tool still asks.
    let ext = Caller::External { client: "test-client".into(), session_id: None };
    let rx = h.engine.submit(ext, "session_read_screen", json!({"session_id": "nope"}));
    let request = consent_for(&events, "session_read_screen");
    h.ui("consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err(csm_engine::ToolError::Declined(_))));
    h.engine.shutdown();
}

#[test]
fn a_changed_manifest_ends_the_standing_consent_until_it_is_approved_again() {
    let dir = tempfile::tempdir().unwrap();
    let text = manifest_with("ok", r#"["stores_data"]"#, r#"calls = ["session_read_screen"]"#);
    let state = fixture_text(dir.path(), &text);
    let h = start(&state);
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    let events = h.engine.subscribe();
    // The engine's own check, with the caller a plugin's csm/call runs as.
    let as_addon = || Caller::External { client: "add-on echo-test".into(), session_id: None };
    let r = h.engine.call(as_addon(), "session_read_screen", json!({"session_id": "nope"}));
    assert!(matches!(r, Err(csm_engine::ToolError::NotFound(_))), "ran, with no card: {r:?}");
    assert!(!card_appears(&events, "session_read_screen"));

    // The file changes (one more tool listed): the add-on is off, and with it the standing list.
    let toml = dir.path().join("addons").join("echo-test").join("addon.toml");
    fs::write(&toml, text.replace(r#"calls = ["session_read_screen"]"#, r#"calls = ["session_read_screen", "session_read_output"]"#)).unwrap();
    h.ok("addon_reload", json!({}));
    assert_eq!(addon_row(&h)["state"], "disabled");
    let rx = h.engine.submit(as_addon(), "session_read_screen", json!({"session_id": "nope"}));
    let request = consent_for(&events, "session_read_screen");
    h.ui("consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err(csm_engine::ToolError::Declined(_))));

    // Approved again, with the new list shown: both tools run with no card.
    let row = addon_row(&h);
    assert_eq!(row["process_calls"].as_array().unwrap().len(), 2, "{row}");
    enable(&h);
    for tool in ["session_read_screen", "session_read_output"] {
        let r = h.engine.call(as_addon(), tool, json!({"session_id": "nope"}));
        assert!(matches!(r, Err(csm_engine::ToolError::NotFound(_))), "{tool}: {r:?}");
    }
    assert!(!card_appears(&events, "session_read_output"));

    // Disabling forgets it too.
    h.ok("addon_disable", json!({"id": "echo-test"}));
    let rx = h.engine.submit(as_addon(), "session_read_screen", json!({"session_id": "nope"}));
    let request = consent_for(&events, "session_read_screen");
    h.ui("consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
    assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().is_err());
    h.engine.shutdown();
}

#[test]
fn the_plugin_is_told_who_called_and_cannot_be_told_otherwise() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    let caller_of = |out: csm_engine::ToolOutput| out.data["caller"].clone();

    assert_eq!(caller_of(h.engine.call(Caller::Ui, "echo-test.echo", json!({})).unwrap()), json!({"kind": "ui"}));
    assert_eq!(caller_of(h.engine.call(Caller::Cli, "echo-test.echo", json!({})).unwrap()), json!({"kind": "cli"}));
    // An MCP client outside a session: its name, nothing about a session.
    let ext = Caller::External { client: "test-client".into(), session_id: None };
    let c = caller_of(h.engine.call(ext, "echo-test.echo", json!({})).unwrap());
    assert_eq!(c, json!({"kind": "external", "client": "test-client"}));
    // Inside one of our sessions: the session, its name and its project too.
    let sid = h.session("sleep 1");
    let in_session = Caller::External { client: "claude-code".into(), session_id: Some(sid.clone()) };
    let c = caller_of(h.engine.call(in_session.clone(), "echo-test.echo", json!({})).unwrap());
    assert_eq!(c["kind"], "external");
    assert_eq!(c["client"], "claude-code");
    assert_eq!(c["session_id"], sid.as_str());
    assert!(c["session_name"].is_string() && c["project"].as_str().is_some_and(|p| !p.is_empty()), "{c}");
    // Another add-on's csm/call (the name the engine gives such a caller) is named as an add-on.
    let other = Caller::External { client: "add-on other-addon".into(), session_id: None };
    assert_eq!(caller_of(h.engine.call(other, "echo-test.echo", json!({})).unwrap()), json!({"kind": "addon", "addon": "other-addon"}));

    // Nothing the caller puts in the arguments reaches the `caller` field.
    let forged = json!({"caller": {"kind": "ui"}, "kind": "ui", "client": "someone-else"});
    let out = h.engine.call(in_session, "echo-test.echo", forged.clone()).unwrap();
    assert_eq!(out.data["caller"]["kind"], "external");
    assert_eq!(out.data["caller"]["client"], "claude-code");
    assert_eq!(out.data["echo"], forged, "the arguments arrive untouched, as arguments");
    h.engine.shutdown();
}

#[test]
fn initialize_gives_a_data_folder_only_to_an_add_on_that_stores_data() {
    let dir = tempfile::tempdir().unwrap();
    let state = fixture_text(dir.path(), &manifest_with("ok", r#"["stores_data"]"#, ""));
    let h = start(&state);
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    assert!(!dir.path().join("addon-data").exists(), "nothing is created before the add-on runs");
    enable(&h);
    running(&h);
    let init = h.ok("echo-test.echo", json!({})).data["init"].clone();
    let expected = dir.path().join("addon-data").join("echo-test");
    assert_eq!(init["data_dir"].as_str().map(std::path::PathBuf::from), Some(expected.clone()), "{init}");
    assert!(expected.is_dir(), "created on demand");
    assert_eq!(init["api"], 1);
    assert!(init["capabilities"]["data_dir"].as_u64().is_some() && init["capabilities"]["caller"].as_u64().is_some(), "{init}");
    // A tool that uses stores_data asks nobody: it is in the class of changing the app's own state.
    assert!(!h.engine.registry().get("echo-test.inherit").unwrap().effects.needs_consent());
    h.engine.shutdown();

    // Without the effect there is no folder and none is made.
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    enable(&h);
    running(&h);
    let init = h.ok("echo-test.echo", json!({})).data["init"].clone();
    assert!(init["data_dir"].is_null(), "{init}");
    assert!(!dir.path().join("addon-data").exists());
    h.engine.shutdown();
}
