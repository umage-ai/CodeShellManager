//! The stdio shell, driven as CSM drives it: the real `csm-phone` binary, JSON-RPC lines on stdin
//! and stdout, and the manifest checked against the program's own declarations.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use csm_phone::csm::CALLS;
use csm_phone::shell::tool_specs;
use serde_json::{Value, json};

const MANIFEST: &str = include_str!("../../../samples/addons/phone-remote/addon.toml");

/// A running program: lines in, answers out (read on a thread, so a hang is a test failure).
struct Program {
    child: std::process::Child,
    stdin: Option<std::process::ChildStdin>,
    lines: mpsc::Receiver<Value>,
}

impl Program {
    fn start() -> Program {
        let mut child = Command::new(env!("CARGO_BIN_EXE_csm-phone")).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().expect("the program starts");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send(v).is_err()
                {
                    break;
                }
            }
        });
        Program { child, stdin, lines: rx }
    }

    fn send(&mut self, v: &Value) {
        self.send_raw(&format!("{v}"));
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{line}").expect("written");
        stdin.flush().expect("flushed");
    }

    /// The next message from the program that is an answer to `id`. Other messages are skipped.
    fn answer(&mut self, id: u64) -> Value {
        loop {
            let v = self.lines.recv_timeout(Duration::from_secs(10)).expect("an answer in time");
            if v.get("id").and_then(Value::as_u64) == Some(id) && v.get("method").is_none() {
                return v;
            }
        }
    }

    fn initialize(&mut self, data_dir: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"api": 1, "csm_version": "test", "capabilities": {"caller": 1, "data_dir": 1}, "data_dir": data_dir}}));
        self.answer(1)
    }

    fn call(&mut self, id: u64, name: &str, caller_kind: &str, arguments: Value) -> Value {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": arguments, "caller": {"kind": caller_kind, "client": "test"}}}));
        self.answer(id)
    }
}

impl Drop for Program {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn initialize_declares_the_tools_and_api_one() {
    let mut p = Program::start();
    let r = p.initialize(Value::Null);
    assert_eq!(r["result"]["api"], 1);
    assert_eq!(r["result"]["name"], "Phone remote");
    let ids: Vec<&str> = r["result"]["tools"].as_array().unwrap().iter().filter_map(|t| t["id"].as_str()).collect();
    let expected: Vec<&str> = tool_specs().iter().map(|t| t.id).collect();
    assert_eq!(ids, expected);
    assert!(r["result"]["subscriptions"].as_array().unwrap().is_empty());
}

#[test]
fn without_a_data_folder_status_says_why_and_nothing_is_kept() {
    let mut p = Program::start();
    p.initialize(Value::Null);
    let r = p.call(2, "phone-remote.status", "ui", json!({}));
    assert_eq!(r["result"]["data"]["enabled"], false);
    assert!(r["result"]["data"]["problem"].as_str().unwrap().contains("no data folder"));
}

#[test]
fn an_agent_cannot_pair_decide_or_revoke_and_nothing_is_started() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Program::start();
    p.initialize(json!(dir.path().to_string_lossy()));
    for (i, (name, args)) in [
        ("phone-remote.pair_start", json!({})),
        ("phone-remote.pair_decide", json!({"allow": true, "level": "viewer"})),
        ("phone-remote.device_revoke", json!({"device_id": "d_x"})),
        ("phone-remote.device_list", json!({})),
    ]
    .into_iter()
    .enumerate()
    {
        let r = p.call(10 + i as u64, name, "external", args);
        assert!(r.get("error").is_some(), "{name} must be refused for an agent: {r}");
        assert!(r["error"]["message"].as_str().unwrap().contains("only the person"));
    }
}

#[test]
fn the_person_gets_no_link_without_a_relay_and_an_empty_device_list() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Program::start();
    p.initialize(json!(dir.path().to_string_lossy()));
    let r = p.call(20, "phone-remote.pair_start", "ui", json!({}));
    assert!(r["error"]["message"].as_str().unwrap().contains("no relay"), "{r}");
    let r = p.call(21, "phone-remote.device_list", "cli", json!({}));
    assert_eq!(r["result"]["data"]["devices"], json!([]));
    let r = p.call(22, "phone-remote.pair_decide", "ui", json!({"allow": true}));
    assert!(r["error"]["message"].as_str().unwrap().contains("level"), "an allow needs a level: {r}");
}

#[test]
fn a_bad_line_or_an_unknown_method_is_answered_and_the_program_stays_up() {
    let mut p = Program::start();
    p.initialize(Value::Null);
    p.send_raw("this is not json");
    p.send_raw("{\"jsonrpc\":\"2.0\",\"id\":5,\"method\":\"nope\"}");
    let r = p.answer(5);
    assert_eq!(r["error"]["code"], -32601);
    let r = p.call(6, "phone-remote.status", "ui", json!({}));
    assert!(r.get("result").is_some(), "still answering: {r}");
}

#[test]
fn the_program_exits_when_csm_closes_stdin() {
    let mut p = Program::start();
    p.initialize(Value::Null);
    p.stdin.take();

    let status = p.child.wait().expect("exits");
    assert!(status.success() || status.code().is_some(), "exited: {status:?}");
}

// ---- the manifest against the program --------------------------------------------------------

fn manifest() -> toml::Table {
    MANIFEST.parse::<toml::Table>().expect("the manifest is TOML")
}

#[test]
fn the_manifest_declares_a_process_with_the_program_name_and_no_shell() {
    let m = manifest();
    assert_eq!(m["addon"]["id"].as_str(), Some("phone-remote"));
    assert_eq!(m["addon"]["kind"].as_str(), Some("process"));
    let argv: Vec<&str> = m["process"]["argv"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert_eq!(argv, vec!["csm-phone"], "plain tokens, the program only");
}

#[test]
fn the_manifest_calls_are_exactly_the_calls_the_program_declares() {
    let m = manifest();
    let mut declared: Vec<&str> = m["process"]["calls"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    let mut known: Vec<&str> = CALLS.to_vec();
    declared.sort_unstable();
    known.sort_unstable();
    assert_eq!(declared, known);
}

#[test]
fn every_tool_effect_is_declared_by_the_process_and_every_tool_is_in_the_addon_namespace() {
    let m = manifest();
    let process_effects: Vec<&str> = m["process"]["effects"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    for t in tool_specs() {
        for e in &t.effects {
            assert!(process_effects.contains(e), "{} uses {e}, which the [process] table does not declare", t.id);
        }
        assert!(t.id.starts_with("phone-remote."));
    }
}

/// The first object under `v` with this `key` (a node's key in a screen tree).
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => {
            if map.get("key").and_then(Value::as_str) == Some(key) {
                return Some(v);
            }
            map.values().find_map(|c| find_key(c, key))
        }
        Value::Array(items) => items.iter().find_map(|c| find_key(c, key)),
        _ => None,
    }
}

impl Program {
    /// The next message with this method (other messages are skipped), within the usual wait.
    fn next_method(&mut self, method: &str) -> Value {
        loop {
            let v = self.lines.recv_timeout(Duration::from_secs(10)).expect("a message in time");
            if v.get("method").and_then(Value::as_str) == Some(method) {
                return v;
            }
        }
    }
}

#[test]
fn the_screens_open_render_and_take_their_events_and_the_status_is_sent() {
    let dir = tempfile::tempdir().unwrap();
    let mut p = Program::start();
    // CSM says it draws screens, so the add-on declares its three.
    p.send(&json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"api": 1, "csm_version": "test", "capabilities": {"caller": 1, "data_dir": 1, "screens": 1}, "data_dir": dir.path().to_string_lossy()}}));
    let init = p.answer(1);
    let screen_ids: Vec<&str> = init["result"]["screens"].as_array().unwrap().iter().filter_map(|s| s["id"].as_str()).collect();
    assert_eq!(screen_ids, vec!["phone-remote.pair", "phone-remote.devices", "phone-remote.settings"]);

    // The status line comes without anyone asking: the add-on is off until connect is on.
    let status = p.next_method("csm/status");
    assert_eq!(status["params"]["text"], "Phone remote is off");
    assert_eq!(status["params"]["tone"], "idle");

    p.send(&json!({"jsonrpc": "2.0", "id": 30, "method": "screen/open", "params": {"screen": "phone-remote.settings", "instance": "i1", "theme": "dark"}}));
    let opened = p.answer(30);
    assert_eq!(opened["result"]["rev"], 1);
    assert!(find_key(&opened["result"]["root"], "push").is_some(), "the settings screen has its push switch");

    p.send(&json!({"jsonrpc": "2.0", "id": 31, "method": "screen/open", "params": {"screen": "phone-remote.nope", "instance": "i2"}}));
    assert_eq!(p.answer(31)["error"]["code"], -32602, "an undeclared screen is refused");

    // A press of the push switch: the add-on saves the setting and renders the screen again.
    p.send(&json!({"jsonrpc": "2.0", "method": "screen/event", "params": {"instance": "i1", "rev": 1, "key": "push", "kind": "toggle", "value": {"checked": true}}}));
    // The host applies the change on its own thread, so the first render may still show the old
    // value: the render that shows the new one is the one to check.
    let render = loop {
        let r = p.next_method("screen/render");
        if find_key(&r["params"]["root"], "push").is_some_and(|n| n["checked"] == true) {
            break r;
        }
    };
    assert_eq!(render["params"]["instance"], "i1");
    assert!(render["params"]["rev"].as_u64().unwrap() > 1, "a newer revision");
    let saved: Value = {
        let text = std::fs::read_to_string(dir.path().join("config.json")).expect("the setting is written");
        serde_json::from_str(&text).expect("config.json is JSON")
    };
    assert_eq!(saved["push"], true);

    // A closed screen is forgotten: the add-on no longer renders it.
    p.send(&json!({"jsonrpc": "2.0", "method": "screen/closed", "params": {"instance": "i1"}}));
    p.send(&json!({"jsonrpc": "2.0", "method": "screen/event", "params": {"instance": "i1", "rev": 2, "key": "push", "kind": "toggle", "value": {"checked": false}}}));
    p.send(&json!({"jsonrpc": "2.0", "id": 32, "method": "tools/call", "params": {"name": "phone-remote.status", "arguments": {}, "caller": {"kind": "ui", "client": "test"}}}));
    let after = p.answer(32);
    assert_eq!(after["result"]["data"]["enabled"], false);
}
