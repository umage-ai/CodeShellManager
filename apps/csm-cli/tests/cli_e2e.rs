//! The `csm` binary against a real bridge (a test engine with fake tools on a temp runtime dir),
//! and against nothing at all.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::process::{Command, Output};
use std::sync::mpsc;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use csm_engine::{Caller, Ctx, Effects, Engine, Event, State, ToolError, ToolOutput, ToolSpec, Toolset, parse_params};
use csm_mcp::{IpcOptions, RuntimeDir, serve_ipc_with};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Echo {
    text: String,
    #[serde(default)]
    loud: bool,
}

fn echo(_: &mut Ctx<'_>, p: Value) -> Result<ToolOutput, ToolError> {
    let p: Echo = parse_params(p)?;
    let text = if p.loud { p.text.to_uppercase() } else { p.text };
    Ok(ToolOutput::new(format!("said {text}"), json!({"text": text})))
}

fn fail(_: &mut Ctx<'_>, _: Value) -> Result<ToolOutput, ToolError> {
    Err(ToolError::NotFound("no such thing".into()))
}

fn send(_: &mut Ctx<'_>, _: Value) -> Result<ToolOutput, ToolError> {
    Ok(ToolOutput::message("sent"))
}

fn echo_schema() -> Value {
    json!({"type": "object", "required": ["text"], "properties": {"text": {"type": "string"}, "loud": {"type": "boolean"}}})
}

fn specs() -> Vec<ToolSpec> {
    let s = |id, toolset, effects, schema: fn() -> Value, run| ToolSpec { id, title: "T", summary: "A fake tool.", toolset, effects, input_schema: schema, run };
    vec![
        s("fake_echo", Toolset::Core, Effects::READ_ONLY, echo_schema, echo),
        s("fake_fail", Toolset::Git, Effects::READ_ONLY, csm_engine::empty_schema, fail),
        s("fake_send", Toolset::Core, Effects::SENDS_INPUT, echo_schema, send),
    ]
}

fn csm(args: &[&str], runtime: Option<&std::path::Path>) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_csm"));
    c.args(args).env_remove("CSM_APP");
    match runtime {
        Some(p) => c.env("CSM_RUNTIME_DIR", p),
        None => c.env_remove("CSM_RUNTIME_DIR"),
    };
    c.output().unwrap()
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// One bridge per process (instances are named by pid), so the tests that serve one take turns.
static TURN: Mutex<()> = Mutex::new(());

fn bridge(rt: &std::path::Path) -> (Engine, csm_mcp::Handle, MutexGuard<'static, ()>) {
    let turn = TURN.lock().unwrap_or_else(PoisonError::into_inner);
    let engine = Engine::start(State::default(), specs()).unwrap();
    let handle = serve_ipc_with(engine.clone(), IpcOptions { runtime: Some(RuntimeDir::at(rt).unwrap()), ..IpcOptions::default() }).unwrap();
    (engine, handle, turn)
}

#[test]
fn against_the_running_app() {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("rt");
    let (engine, handle, _turn) = bridge(&rt);

    // Generated from the app's registry: no list in the CLI knows `fake-echo`.
    let o = csm(&["--help"], Some(&rt));
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("fake-echo") && out(&o).contains("Git (git)") && !out(&o).contains("Not connected"), "{}", out(&o));

    let o = csm(&["tools", "--toolset", "git"], Some(&rt));
    assert!(out(&o).contains("fake-fail") && !out(&o).contains("fake-echo"), "{}", out(&o));
    let o = csm(&["commands", "--json"], Some(&rt));
    let list: Value = serde_json::from_str(&out(&o)).unwrap();
    assert!(list.as_array().unwrap().iter().any(|t| t["id"] == "fake_echo" && t["command"] == "fake-echo"));

    // Message by default, flags typed from the schema.
    let o = csm(&["fake-echo", "--text", "hi", "--loud"], Some(&rt));
    assert_eq!((o.status.code(), out(&o).trim()), (Some(0), "said HI"));
    // --json on its own: the full result.
    let o = csm(&["fake-echo", "--text", "hi", "--json"], Some(&rt));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert_eq!(v, json!({"message": "said hi", "data": {"text": "hi"}}));
    // --json with params.
    let o = csm(&["fake-echo", "--json", r#"{"text":"p"}"#], Some(&rt));
    assert_eq!(out(&o).trim(), "said p");

    // Exit codes: tool error 1, usage 2.
    let o = csm(&["fake-fail"], Some(&rt));
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no such thing"));
    let o = csm(&["fake-fail", "--json"], Some(&rt));
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(serde_json::from_str::<Value>(&out(&o)).unwrap()["error"]["code"], "not_found");
    assert_eq!(csm(&["fake-echo"], Some(&rt)).status.code(), Some(2));
    assert_eq!(csm(&["fake-echo", "--text", "x", "--nope"], Some(&rt)).status.code(), Some(2));
    assert_eq!(csm(&["no-such-tool"], Some(&rt)).status.code(), Some(2));
    assert_eq!(csm(&["tools", "--toolset", "zzz"], Some(&rt)).status.code(), Some(2));

    // The CLI is the person: a consent-needing tool runs without a card.
    let o = csm(&["fake-send", "--text", "x"], Some(&rt));
    assert_eq!((o.status.code(), out(&o).trim()), (Some(0), "sent"));

    handle.shutdown().unwrap();
    engine.shutdown();
}

#[test]
fn without_the_app_the_exit_code_is_3_and_the_message_says_what_to_do() {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("rt");
    let o = csm(&["fake-echo", "--text", "x"], Some(&rt));
    assert_eq!(o.status.code(), Some(3), "{}", err(&o));
    assert!(err(&o).contains("not running") && err(&o).contains("--start"), "{}", err(&o));
    // Help still works, from the tools this build knows.
    let o = csm(&["--help"], Some(&rt));
    assert_eq!(o.status.code(), Some(0));
    assert!(out(&o).contains("tool-list") && out(&o).contains("Not connected"));
    let o = csm(&["tools"], Some(&rt));
    assert_eq!(o.status.code(), Some(0));
    // --start with an app that is not there fails with 3 and names it.
    let o = csm(&["--start", "--app", "no-such-app.exe", "tool-list"], Some(&rt));
    assert_eq!(o.status.code(), Some(3));
    assert!(err(&o).contains("no-such-app"), "{}", err(&o));
}

#[test]
fn in_process_runs_an_engine_of_its_own() {
    let o = csm(&["--in-process", "tool-list", "--json"], None);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let v: Value = serde_json::from_str(&out(&o)).unwrap();
    assert!(v["data"].as_array().unwrap().iter().any(|t| t["id"] == "tool_list"));
    let o = csm(&["--in-process", "tool-list", "--toolset", "bogus"], None);
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn mcp_in_process_speaks_json_rpc_on_stdio() {
    use std::io::{BufRead, BufReader, Write};
    use std::process::Stdio;
    let mut child = Command::new(env!("CARGO_BIN_EXE_csm"))
        .args(["--in-process", "mcp", "--session", "odulodo"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut ask = |msg: Value| -> Value {
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    };
    let init = ask(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "t"}}}));
    assert_eq!(init["result"]["serverInfo"]["name"], "codeshell");
    let list = ask(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let names: Vec<&str> = list["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["name"].as_str()).collect();
    assert!(names.contains(&"tool_list") && names.contains(&"toolset_enable"), "{names:?}");
    let call = ask(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "tool_list", "arguments": {}}}));
    assert!(call["result"]["structuredContent"]["result"].is_array(), "{call}");
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn auto_allow_needs_in_process() {
    let dir = tempfile::tempdir().unwrap();
    let o = csm(&["mcp", "--auto-allow"], Some(&dir.path().join("rt")));
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn events_stream_from_the_app() {
    let dir = tempfile::tempdir().unwrap();
    let rt = dir.path().join("rt");
    let (engine, handle, _turn) = bridge(&rt);
    let mut child = Command::new(env!("CARGO_BIN_EXE_csm"))
        .arg("events")
        .env("CSM_RUNTIME_DIR", &rt)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        use std::io::BufRead;
        for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    // Nothing says when the subscription is live, so park and deny a consent request (which raises
    // events) until a line shows up.
    let events = engine.subscribe();
    let mut got = None;
    for _ in 0..100 {
        let parked = engine.submit(Caller::External { client: "c".into(), session_id: None }, "fake_send", json!({"text": "t"}));
        let request = loop {
            if let Event::ConsentRequested { request } = events.recv_timeout(Duration::from_secs(5)).unwrap() {
                break request;
            }
        };
        engine.call(Caller::Ui, "consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
        let _ = parked.recv_timeout(Duration::from_secs(5));
        if let Ok(line) = rx.recv_timeout(Duration::from_millis(200)) {
            got = Some(line);
            break;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let line: Value = serde_json::from_str(&got.expect("an event line")).unwrap();
    assert!(line["event"] == "consent_requested" || line["event"] == "consent_resolved", "{line}");
    handle.shutdown().unwrap();
    engine.shutdown();
}
