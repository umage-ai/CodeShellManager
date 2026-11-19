//! The bridge end to end over a real local socket (a named pipe on Windows) in a temp directory.

mod common;

use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use craft_ipc::client::{Client, HelloClient};
use craft_ipc::{Discovery, LinkError, RuntimeDir};
use csm_engine::{Caller, Engine, Event, ToolError};
use csm_mcp::{Backend, BridgeError, Handle, IpcOptions, McpOptions, McpService, RemoteBackend, RemoteOptions, serve_ipc_with, wire};
use serde_json::{Value, json};

/// An instance is named by app and pid (the pipe too, on Windows), so a process can serve one
/// bridge at a time: tests that start one take turns.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

struct Bridge {
    engine: Engine,
    handle: Handle,
    dir: tempfile::TempDir,
    _turn: MutexGuard<'static, ()>,
}

impl Bridge {
    fn start() -> Bridge {
        let turn = ONE_AT_A_TIME.lock().unwrap_or_else(PoisonError::into_inner);
        let dir = tempfile::tempdir().unwrap();
        let engine = common::engine();
        let runtime = RuntimeDir::at(dir.path().join("rt")).unwrap();
        let handle = serve_ipc_with(engine.clone(), IpcOptions { runtime: Some(runtime), ..IpcOptions::default() }).unwrap();
        Bridge { engine, handle, dir, _turn: turn }
    }

    fn runtime(&self) -> RuntimeDir {
        RuntimeDir::at(self.dir.path().join("rt")).unwrap()
    }

    fn remote(&self) -> RemoteBackend {
        RemoteBackend::new(RemoteOptions { runtime: Some(self.runtime()), ..RemoteOptions::default() })
    }

    /// A raw craft-ipc client with the real token, for speaking the wire protocol directly.
    fn raw(&self) -> Client {
        let record = self.handle.record().unwrap().clone();
        let mut c = if csm_mcp::USE_TCP_BY_DEFAULT { Client::connect_tcp(&record) } else { Client::connect(&record) }.unwrap();
        let hello = HelloClient {
            app: "test".into(),
            app_version: "0".into(),
            pid: std::process::id(),
            instance: craft_ipc::random_uuid().unwrap(),
            protocol_min: craft_ipc::wire::PROTOCOL_MIN,
            protocol_max: craft_ipc::wire::PROTOCOL_MAX,
        };
        c.hello(&hello).unwrap();
        c
    }
}

fn next_consent(events: &mpsc::Receiver<Event>) -> Value {
    loop {
        if let Event::ConsentRequested { request } = events.recv_timeout(Duration::from_secs(10)).expect("a consent request") {
            return request;
        }
    }
}

fn ext(name: &str) -> Caller {
    Caller::External { client: name.into(), session_id: Some("s1".into()) }
}

#[test]
fn the_cli_lists_the_registry_over_the_socket() {
    let b = Bridge::start();
    let mut remote = b.remote();
    let tools = remote.tools().unwrap();
    let local = csm_mcp::LocalBackend::new(b.engine.clone()).tools().unwrap();
    assert_eq!(tools, local, "what the app registered is exactly what the CLI sees");
    assert!(tools.iter().any(|t| t.id == "fake_echo" && t.input_schema["required"] == json!(["text"])));
}

#[test]
fn cli_and_mcp_callers_are_told_apart() {
    let b = Bridge::start();
    let mut remote = b.remote();
    let cli = remote.call(&Caller::Cli, "fake_whoami", json!({})).unwrap();
    assert_eq!(cli.data, json!({"kind": "cli"}));
    let mcp = remote.call(&ext("claude-code"), "fake_whoami", json!({})).unwrap();
    assert_eq!(mcp.data, json!({"kind": "external", "client": "claude-code", "session_id": "s1"}));
    let echo = remote.call(&Caller::Cli, "fake_echo", json!({"text": "x"})).unwrap();
    assert_eq!(echo.message, "Echoed x.");
}

#[test]
fn tool_errors_cross_the_socket_with_their_kind() {
    let b = Bridge::start();
    let mut remote = b.remote();
    let e = remote.call(&Caller::Cli, "fake_missing", json!({})).unwrap_err();
    assert!(matches!(e, BridgeError::Tool(ToolError::NotFound(m)) if m.contains("session_list")));
    let e = remote.call(&Caller::Cli, "nope", json!({})).unwrap_err();
    assert!(matches!(e, BridgeError::Tool(ToolError::UnknownTool(_))), "{e:?}");
    let e = remote.call(&Caller::Cli, "fake_echo", json!({"wrong": 1})).unwrap_err();
    assert!(matches!(e, BridgeError::Tool(ToolError::InvalidParams(_))), "{e:?}");
}

#[test]
fn a_remote_caller_can_never_claim_to_be_the_ui() {
    let b = Bridge::start();
    let mut raw = b.raw();
    let err = raw.call(wire::METHOD_SUBMIT, json!({"caller": {"kind": "ui"}, "tool": "fake_echo", "params": {"text": "x"}})).unwrap_err();
    assert!(err.to_string().contains("UI"), "{err}");

    // Even as the CLI, consent cannot be answered: the engine reserves it for the app's window.
    let events = b.engine.subscribe();
    let mut remote = b.remote();
    let rx = b.engine.submit(ext("claude-code"), "fake_send", json!({"text": "t"}));
    let request = next_consent(&events);
    let e = remote.call(&Caller::Cli, "consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"})).unwrap_err();
    assert!(matches!(e, BridgeError::Tool(ToolError::Refused(_))), "{e:?}");
    let e = remote.call(&ext("claude-code"), "consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"})).unwrap_err();
    assert!(matches!(e, BridgeError::Tool(ToolError::Refused(_))), "{e:?}");
    assert!(rx.recv_timeout(Duration::from_millis(300)).is_err(), "still parked");
    b.engine.call(Caller::Ui, "consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
}

#[test]
fn a_consent_call_over_the_socket_waits_for_the_app() {
    let b = Bridge::start();
    let events = b.engine.subscribe();
    let mut remote = b.remote();
    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let r = remote.call(&ext("claude-code"), "fake_send", json!({"text": "hi"}));
        let _ = tx.send(r);
    });
    let request = next_consent(&events);
    assert_eq!(request["client"], "claude-code");
    assert_eq!(request["from_session_id"], "s1");
    assert!(rx.recv_timeout(Duration::from_millis(500)).is_err(), "waits for the person");
    b.engine.call(Caller::Ui, "consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"})).unwrap();
    let out = rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    assert_eq!(out.message, "Sent.");
    worker.join().unwrap();
}

#[test]
fn a_long_wait_is_a_series_of_polls() {
    let b = Bridge::start();
    let events = b.engine.subscribe();
    let mut raw = b.raw();
    let t = raw.call(wire::METHOD_SUBMIT, json!({"caller": {"kind": "external", "client": "c"}, "tool": "fake_send", "params": {"text": "x"}})).unwrap();
    let ticket = t["ticket"].as_u64().unwrap();
    let request = next_consent(&events);
    let started = Instant::now();
    for _ in 0..3 {
        let r = raw.call(wire::METHOD_WAIT, json!({"ticket": ticket, "timeout_ms": 100})).unwrap();
        assert_eq!(r["status"], "pending");
    }
    assert!(started.elapsed() >= Duration::from_millis(300));
    b.engine.call(Caller::Ui, "consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"})).unwrap();
    let r = raw.call(wire::METHOD_WAIT, json!({"ticket": ticket, "timeout_ms": 5000})).unwrap();
    assert_eq!(r["status"], "done");
    // A finished ticket is gone.
    assert!(raw.call(wire::METHOD_WAIT, json!({"ticket": ticket, "timeout_ms": 10})).is_err());
    assert!(raw.call(wire::METHOD_WAIT, json!({"ticket": "x"})).is_err());
    assert!(raw.call("csm.nonsense", json!({})).is_err());
}

#[test]
fn a_wrong_token_is_rejected() {
    let b = Bridge::start();
    let mut record = b.handle.record().unwrap().clone();
    record.token = "0".repeat(64);
    // Every transport the app offers (the socket or pipe, and loopback TCP where it is on).
    let mut outcomes = vec![Client::connect(&record).and_then(|mut c| c.call(wire::METHOD_PING, json!({})))];
    if record.tcp_port().is_some() {
        outcomes.push(Client::connect_tcp(&record).and_then(|mut c| c.call(wire::METHOD_PING, json!({}))));
    }
    for outcome in outcomes {
        assert!(outcome.is_err(), "a client without the token must not get an answer: {outcome:?}");
        if let Err(e) = outcome {
            assert!(matches!(e, LinkError::Auth | LinkError::Closed | LinkError::Io(_)), "{e:?}");
        }
    }
    // The real token still works.
    assert_eq!(b.raw().call(wire::METHOD_PING, json!({})).unwrap()["pong"], true);
}

#[test]
fn engine_events_reach_a_watching_client() {
    let b = Bridge::start();
    let mut remote = b.remote();
    let (tx, rx) = mpsc::channel();
    let watcher = thread::spawn(move || {
        let _ = remote.watch(|ev| {
            let done = ev["event"] == "toast";
            let _ = tx.send(ev);
            !done
        });
    });
    // Subscribing takes a moment, and there is no "ready" signal to wait on: poke until it lands.
    let deadline = Instant::now() + Duration::from_secs(10);
    let event = loop {
        b.engine.call(Caller::Ui, "fake_toast", json!({})).unwrap();
        if let Ok(ev) = rx.recv_timeout(Duration::from_millis(200)) {
            break ev;
        }
        assert!(Instant::now() < deadline, "no event arrived");
    };
    assert_eq!(event, json!({"event": "toast", "message": "hello from a tool"}));
    watcher.join().unwrap();
}

#[test]
fn csm_mcp_through_the_bridge_labels_calls_as_the_mcp_client() {
    let b = Bridge::start();
    let mut service = McpService::new(b.remote(), McpOptions { session_id: Some("odulodo".into()), all_tools: false });
    let init = service.handle_line(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "claude-code"}}}).to_string());
    assert!(init[0].contains("codeshell"));
    let call = service.handle_line(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "fake_whoami", "arguments": {}}}).to_string());
    let reply: Value = serde_json::from_str(&call[0]).unwrap();
    assert_eq!(reply["result"]["structuredContent"], json!({"kind": "external", "client": "claude-code", "session_id": "odulodo"}));
}

#[test]
fn no_app_running_is_said_plainly() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimeDir::at(dir.path().join("rt")).unwrap();
    let mut remote = RemoteBackend::new(RemoteOptions { runtime: Some(runtime), ..RemoteOptions::default() });
    let Err(BridgeError::Unreachable(m)) = remote.tools() else { panic!("expected unreachable") };
    assert!(m.contains("not running") && m.contains("--start") && m.contains("CSM_APP"), "{m}");
    let Err(BridgeError::Unreachable(_)) = remote.call(&Caller::Cli, "fake_echo", json!({"text": "x"})) else { panic!("expected unreachable") };
}

#[test]
fn start_with_a_bad_app_path_fails_with_the_path_in_the_message() {
    let dir = tempfile::tempdir().unwrap();
    let runtime = RuntimeDir::at(dir.path().join("rt")).unwrap();
    let missing = dir.path().join("no-such-app.exe");
    let mut remote = RemoteBackend::new(RemoteOptions { runtime: Some(runtime), start: true, app_path: Some(missing), app_args: vec![], tcp: None });
    let Err(BridgeError::Unreachable(m)) = remote.tools() else { panic!("expected unreachable") };
    assert!(m.contains("no-such-app"), "{m}");
}

#[test]
fn shutdown_removes_the_instance_record() {
    let b = Bridge::start();
    let runtime = b.runtime();
    assert_eq!(Discovery::scan(&runtime).unwrap().len(), 1);
    let Bridge { handle, dir, engine, _turn } = b;
    handle.shutdown().unwrap();
    assert!(Discovery::scan(&RuntimeDir::at(dir.path().join("rt")).unwrap()).unwrap().is_empty());
    drop(engine);
}

#[test]
fn a_reconnect_after_the_app_restarts() {
    let b = Bridge::start();
    let mut remote = b.remote();
    assert!(remote.tools().is_ok());
    let Bridge { handle, dir, engine, _turn } = b;
    handle.shutdown().unwrap();
    let t = Instant::now();
    let r = remote.tools();
    eprintln!("after shutdown: {r:?} in {:?}", t.elapsed());
    assert!(matches!(r, Err(BridgeError::Unreachable(_))));
    let runtime = RuntimeDir::at(dir.path().join("rt")).unwrap();
    let _again = serve_ipc_with(engine, IpcOptions { runtime: Some(runtime), ..IpcOptions::default() }).unwrap();
    let t = Instant::now();
    let r = remote.tools();
    eprintln!("after restart: {:?} in {:?}", r.as_ref().map(|v| v.len()), t.elapsed());
    assert!(r.is_ok(), "the next call finds the new instance: {r:?}");
}
