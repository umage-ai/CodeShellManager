//! A test process add-on for the engine's plugin host (`crates/csm-engine/tests/plugins.rs`).
//! Not shipped: a `[[bin]]` of csm-engine, built for the tests only.
//!
//! The first argument is the mode: `ok` (default), `oversize` (writes a 2 MiB line at
//! initialize), `badjson` (writes a line that is not JSON at initialize), `undeclared` (declares a
//! tool with an effect its add-on did not declare). Its tools are `echo-test.echo`, `.write`,
//! `.exit` (crashes the process) and `.callback` (calls `session_list` through `csm/call`).
//! It counts the events it was sent and reports the count from `echo`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{self, BufRead, Lines, StdinLock, Write};

use serde_json::{Value, json};

fn send(out: &mut impl Write, v: &Value) {
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn next_line(lines: &mut Lines<StdinLock<'static>>) -> Option<String> {
    lines.next()?.ok()
}

/// Reads until the answer to `want` arrives. Events that come in the meantime are counted.
fn wait_for(lines: &mut Lines<StdinLock<'static>>, want: u64, events: &mut u64) -> Value {
    while let Some(line) = next_line(lines) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if msg.get("method").and_then(Value::as_str) == Some("event") {
            *events += 1;
            continue;
        }
        if msg.get("id").and_then(Value::as_u64) == Some(want) {
            return msg.get("result").cloned().or_else(|| msg.get("error").cloned()).unwrap_or(Value::Null);
        }
    }
    Value::Null
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "ok".into());
    let stdin = io::stdin();
    let mut lines: Lines<StdinLock<'static>> = stdin.lock().lines();
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let mut events: u64 = 0;
    let mut next_csm_id: u64 = 1000;
    while let Some(line) = next_line(&mut lines) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default().to_owned();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match (method.as_str(), id) {
            ("initialize", Some(id)) => {
                match mode.as_str() {
                    "oversize" => {
                        let _ = writeln!(out, "{}", "x".repeat(2 * 1024 * 1024));
                        let _ = out.flush();
                        continue;
                    }
                    "badjson" => {
                        let _ = writeln!(out, "this is not json");
                        let _ = out.flush();
                        continue;
                    }
                    _ => {}
                }
                let mut tools = vec![
                    json!({"id": "echo-test.echo", "title": "Echo", "summary": "Returns its arguments and how many events it has seen"}),
                    json!({"id": "echo-test.write", "title": "Write", "summary": "Pretends to write a file", "effects": ["writes_filesystem"]}),
                    json!({"id": "echo-test.exit", "title": "Exit", "summary": "Exits the process (a crash)"}),
                    json!({"id": "echo-test.callback", "title": "Callback", "summary": "Calls session_list through csm/call"}),
                ];
                if mode == "undeclared" {
                    tools.push(json!({"id": "echo-test.sneaky", "title": "Sneaky", "effects": ["sends_input"]}));
                }
                send(
                    &mut out,
                    &reply(
                        &id,
                        json!({"api": 1, "name": "Echo test plugin", "tools": tools, "subscriptions": ["session_changed", "inbox_changed", "not_an_event"]}),
                    ),
                );
            }
            ("event", None) => {
                events += 1;
                let kind = params.get("event").and_then(Value::as_str).unwrap_or_default();
                eprintln!("event {kind}");
            }
            ("tools/call", Some(id)) => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                match name.as_str() {
                    "echo-test.echo" => send(&mut out, &reply(&id, json!({"message": "echoed", "data": {"echo": args, "events_seen": events}}))),
                    "echo-test.write" => send(&mut out, &reply(&id, json!({"message": "wrote (pretend)"}))),
                    "echo-test.exit" => std::process::exit(3),
                    "echo-test.callback" => {
                        next_csm_id += 1;
                        let cid = next_csm_id;
                        send(&mut out, &json!({"jsonrpc": "2.0", "id": cid, "method": "csm/call", "params": {"tool": "session_list", "params": {}}}));
                        let answer = wait_for(&mut lines, cid, &mut events);
                        send(&mut out, &reply(&id, json!({"message": "called", "data": answer})));
                    }
                    _ => send(&mut out, &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such tool"}})),
                }
            }
            _ => {}
        }
    }
}
