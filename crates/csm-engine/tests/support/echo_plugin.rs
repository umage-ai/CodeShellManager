//! A test process add-on for the engine's plugin host (`crates/csm-engine/tests/plugins.rs`).
//! Not shipped: a `[[bin]]` of csm-engine, built for the tests only.
//!
//! The first argument is the mode: `ok` (default), `oversize` (writes a 2 MiB line at
//! initialize), `badjson` (writes a line that is not JSON at initialize), `undeclared` (declares a
//! tool with an effect its add-on did not declare). Its tools are `echo-test.echo`, `.write`,
//! `.exit` (crashes the process) and `.callback` (calls `session_list` through `csm/call`).
//! `.csm` runs any CSM tool through `csm/call` (`tool`, `params`) and returns the answer or error;
//! `.offer` sends `csm/remotes_changed` with the `remotes` it is given; `.open_agent` answers with
//! a `session` to open (an ssh config/alias session). It counts the events it was sent, keeps their
//! kinds, and reports both from `echo`, with the `caller` of that call and the `initialize` params.
//!
//! **Screens.** When CSM says `capabilities.screens`, it declares `echo-test.main` (global) and
//! `echo-test.proj` (project scope); mode `noscreens` declares none. `screen/open` answers with a
//! view (a title, a `status` text, buttons, a text input, a file list, a list, markdown). Each
//! `screen/event` is remembered and answered with a new `screen/render` whose `status` shows it,
//! except a few special keys: `patch` answers with a `screen/patch` of `status`, `badpatch` with a
//! patch on the wrong base (CSM must answer `screen/refresh`, which gets a full render), `bad` with
//! an over-cap tree (CSM must refuse it and answer `screen/error`), `flood` with 200 renders.
//! `screen/closed`, `screen/refresh` and `screen/error` are remembered; `echo-test.screen_state`
//! reports everything it has seen. Mode `badopen` answers `screen/open` with an over-cap tree.
//! Code and diff: an event with the key `showcode` is answered with a view that has a `code` node
//! (key `code`, a range action, one open and one closed thread) and a `diff` node (key `diff`);
//! from then on every event on that instance re-renders that view with the event in `status`.
//! The key `bigcode` answers with a code node whose text is over the 512 KiB cap.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::HashMap;
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
fn wait_for(lines: &mut Lines<StdinLock<'static>>, want: u64, events: &mut u64, kinds: &mut Vec<String>) -> Value {
    while let Some(line) = next_line(lines) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if msg.get("method").and_then(Value::as_str) == Some("event") {
            *events += 1;
            kinds.push(msg["params"]["event"].as_str().unwrap_or_default().to_owned());
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
    let mut kinds: Vec<String> = Vec::new();
    let mut next_csm_id: u64 = 1000;
    let mut init_params = Value::Null;
    let mut screen_rev: HashMap<String, u64> = HashMap::new();
    let mut screen_log: Vec<Value> = Vec::new();
    let mut screen_closed: Vec<String> = Vec::new();
    let mut screen_errors: Vec<String> = Vec::new();
    let mut code_mode: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut refreshes: u64 = 0;
    while let Some(line) = next_line(&mut lines) {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default().to_owned();
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match (method.as_str(), id) {
            ("initialize", Some(id)) => {
                init_params = params.clone();
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
                    json!({"id": "echo-test.echo", "title": "Echo", "summary": "Returns its arguments and how many events it has seen", "effects": []}),
                    json!({"id": "echo-test.inherit", "title": "Inherit", "summary": "Declares no effects, so it gets the add-on's"}),
                    json!({"id": "echo-test.write", "title": "Write", "summary": "Pretends to write a file", "effects": ["writes_filesystem"]}),
                    json!({"id": "echo-test.exit", "title": "Exit", "summary": "Exits the process (a crash)"}),
                    json!({"id": "echo-test.callback", "title": "Callback", "summary": "Calls session_list through csm/call"}),
                    json!({"id": "echo-test.status", "title": "Status", "summary": "Sends the csm/status params it is given", "effects": []}),
                    json!({"id": "echo-test.csm", "title": "Csm", "summary": "Runs a CSM tool through csm/call", "effects": []}),
                    json!({"id": "echo-test.offer", "title": "Offer", "summary": "Sends csm/remotes_changed", "effects": []}),
                    json!({"id": "echo-test.open_agent", "title": "Open agent", "summary": "Says which session to open", "effects": []}),
                ];
                if mode == "undeclared" {
                    tools.push(json!({"id": "echo-test.sneaky", "title": "Sneaky", "effects": ["sends_input"]}));
                }
                tools.push(json!({"id": "echo-test.screen_state", "title": "Screen state", "summary": "What the screens have seen", "effects": []}));
                let screens_ok = init_params["capabilities"]["screens"].as_u64() >= Some(1) && mode != "noscreens";
                let mut answer = json!({"api": 1, "name": "Echo test plugin", "tools": tools, "subscriptions": ["session_changed", "inbox_changed", "session_asleep", "session_awake", "not_an_event"]});
                if screens_ok {
                    answer["screens"] = json!([
                        {"id": "echo-test.main", "title": "Echo screen", "icon": "layers", "scope": "global", "summary": "A screen for the tests"},
                        {"id": "echo-test.proj", "title": "Echo project", "icon": "branch", "scope": "project"}
                    ]);
                }
                send(&mut out, &reply(&id, answer));
            }
            ("screen/open", Some(id)) => {
                let instance = params["instance"].as_str().unwrap_or_default().to_owned();
                screen_log.push(json!({"open": instance, "screen": params["screen"], "context": params["context"], "theme": params["theme"]}));
                screen_rev.insert(instance.clone(), 1);
                let root = if mode == "badopen" { too_big() } else { view(&instance, "opened") };
                send(&mut out, &reply(&id, json!({"rev": 1, "root": root})));
            }
            ("screen/event", None) => {
                let instance = params["instance"].as_str().unwrap_or_default().to_owned();
                let key = params["key"].as_str().unwrap_or_default().to_owned();
                screen_log.push(json!({"event": params["key"], "kind": params["kind"], "value": params["value"], "rev": params["rev"], "instance": instance}));
                let rev = screen_rev.get(&instance).copied().unwrap_or(0);
                let note = format!("{} {} {}", params["kind"].as_str().unwrap_or_default(), key, params["value"]);
                let line = |m: &str, p: Value| json!({"jsonrpc": "2.0", "method": m, "params": p});
                match key.as_str() {
                    "showcode" => {
                        code_mode.insert(instance.clone());
                        screen_rev.insert(instance.clone(), rev + 1);
                        send(&mut out, &line("screen/render", json!({"instance": instance, "rev": rev + 1, "root": code_view(&instance, "code shown", false)})));
                    }
                    "bigcode" => {
                        code_mode.insert(instance.clone());
                        screen_rev.insert(instance.clone(), rev + 1);
                        send(&mut out, &line("screen/render", json!({"instance": instance, "rev": rev + 1, "root": code_view(&instance, "code too big", true)})));
                    }
                    _ if code_mode.contains(&instance) => {
                        screen_rev.insert(instance.clone(), rev + 1);
                        send(&mut out, &line("screen/render", json!({"instance": instance, "rev": rev + 1, "root": code_view(&instance, &note, false)})));
                    }
                    "patch" => {
                        screen_rev.insert(instance.clone(), rev + 1);
                        send(&mut out, &line("screen/patch", json!({"instance": instance, "rev": rev + 1, "base": rev, "ops": [{"op": "replace", "key": "status", "node": {"type": "text", "key": "status", "text": format!("patched {}", rev + 1)}}]})));
                    }
                    "badpatch" => {
                        send(&mut out, &line("screen/patch", json!({"instance": instance, "rev": rev + 10, "base": rev + 5, "ops": []})));
                    }
                    "bad" => {
                        send(&mut out, &line("screen/render", json!({"instance": instance, "rev": rev + 1, "root": too_big()})));
                    }
                    "flood" => {
                        for n in 1..=200u64 {
                            send(&mut out, &line("screen/render", json!({"instance": instance, "rev": rev + n, "root": view(&instance, &format!("flood {n}"))})));
                        }
                        screen_rev.insert(instance.clone(), rev + 200);
                    }
                    _ => {
                        screen_rev.insert(instance.clone(), rev + 1);
                        send(&mut out, &line("screen/render", json!({"instance": instance, "rev": rev + 1, "root": view(&instance, &note)})));
                    }
                }
            }
            ("screen/refresh", None) => {
                refreshes += 1;
                let instance = params["instance"].as_str().unwrap_or_default().to_owned();
                let rev = screen_rev.get(&instance).copied().unwrap_or(0) + 20;
                screen_rev.insert(instance.clone(), rev);
                send(&mut out, &json!({"jsonrpc": "2.0", "method": "screen/render", "params": {"instance": instance, "rev": rev, "root": view(&instance, "refreshed")}}));
            }
            ("screen/closed", None) => screen_closed.push(params["instance"].as_str().unwrap_or_default().to_owned()),
            ("screen/error", None) => screen_errors.push(params["message"].as_str().unwrap_or_default().to_owned()),
            ("event", None) => {
                events += 1;
                let kind = params.get("event").and_then(Value::as_str).unwrap_or_default();
                kinds.push(kind.to_owned());
                eprintln!("event {kind}");
            }
            ("tools/call", Some(id)) => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                match name.as_str() {
                    "echo-test.echo" => send(
                        &mut out,
                        &reply(&id, json!({"message": "echoed", "data": {"echo": args, "events_seen": events, "event_kinds": kinds, "caller": params.get("caller"), "init": init_params}})),
                    ),
                    "echo-test.screen_state" => send(&mut out, &reply(&id, json!({"message": "state", "data": {"log": screen_log, "closed": screen_closed, "errors": screen_errors, "refreshes": refreshes}}))),
                    "echo-test.inherit" => send(&mut out, &reply(&id, json!({"message": "inherited"}))),
                    "echo-test.write" => send(&mut out, &reply(&id, json!({"message": "wrote (pretend)"}))),
                    "echo-test.exit" => std::process::exit(3),
                    "echo-test.csm" => {
                        next_csm_id += 1;
                        let cid = next_csm_id;
                        let call = json!({"tool": args["tool"], "params": args.get("params").cloned().unwrap_or(json!({}))});
                        send(&mut out, &json!({"jsonrpc": "2.0", "id": cid, "method": "csm/call", "params": call}));
                        let answer = wait_for(&mut lines, cid, &mut events, &mut kinds);
                        send(&mut out, &reply(&id, json!({"message": "csm", "data": answer})));
                    }
                    "echo-test.offer" => {
                        send(&mut out, &json!({"jsonrpc": "2.0", "method": "csm/remotes_changed", "params": {"remotes": args["remotes"]}}));
                        send(&mut out, &reply(&id, json!({"message": "offered"})));
                    }
                    "echo-test.open_agent" => {
                        let name = args["name"].as_str().unwrap_or("agent").to_owned();
                        let session = json!({"name": name, "ssh": {"host": "agent-alias", "config_file": "cfg/ssh_config", "remote_argv": ["tmux", "new-session", "-A", "-s", "main"]}, "resume": "rerun"});
                        send(&mut out, &reply(&id, json!({"message": "open this", "data": {"session": session}})));
                    }
                    "echo-test.status" => {
                        // Sends each `csm/status` it is given, as the params it is given (a test of CSM's checks).
                        for s in args["statuses"].as_array().cloned().unwrap_or_default() {
                            send(&mut out, &json!({"jsonrpc": "2.0", "method": "csm/status", "params": s}));
                        }
                        send(&mut out, &reply(&id, json!({"message": "status sent"})));
                    }
                    "echo-test.callback" => {
                        next_csm_id += 1;
                        let cid = next_csm_id;
                        send(&mut out, &json!({"jsonrpc": "2.0", "id": cid, "method": "csm/call", "params": {"tool": "session_list", "params": {}}}));
                        let answer = wait_for(&mut lines, cid, &mut events, &mut kinds);
                        send(&mut out, &reply(&id, json!({"message": "called", "data": answer})));
                    }
                    _ => send(&mut out, &json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "no such tool"}})),
                }
            }
            _ => {}
        }
    }
}

/// The view every screen shows. `status` is the node tests watch.
fn view(instance: &str, status: &str) -> Value {
    json!({"type": "column", "key": "root", "gap": "md", "children": [
        {"type": "text", "key": "title", "style": "title", "text": format!("Echo screen {instance}")},
        {"type": "text", "key": "status", "text": status},
        {"type": "toolbar", "children": [
            {"type": "button", "key": "btn", "label": "Ping", "kind": "primary", "event": true},
            {"type": "button", "label": "Echo tool", "action": {"tool": "echo-test.echo", "params": {"from": "screen"}}},
            {"type": "button", "label": "Send hi", "action": {"tool": "session_send_text", "params": {"session_id": "s1", "text": "hello from the screen"}}},
            {"type": "button", "label": "Quit app", "action": {"tool": "app_quit"}}
        ]},
        {"type": "text_input", "key": "name", "value": "", "placeholder": "Name", "submit_label": "Go"},
        {"type": "file_list", "key": "files", "view": "tree", "files": [
            {"path": "src/lib.rs", "status": "M"}, {"path": "src/ui/a.rs", "status": "A"}, {"path": "README.md", "status": "?"}]},
        {"type": "list", "key": "items", "items": [{"key": "one", "title": "One", "subtitle": "first"}, {"key": "two", "title": "Two"}]},
        {"type": "markdown", "source": "Hello **world**"}
    ]})
}

/// A tree over the node cap.
fn too_big() -> Value {
    let kids: Vec<Value> = (0..6000).map(|_| json!({"type": "spacer"})).collect();
    json!({"type": "column", "children": kids})
}

/// A view with a code node and a diff node (`big`: the code text is over the 512 KiB cap).
fn code_view(instance: &str, status: &str, big: bool) -> Value {
    let text = if big { "x".repeat(600 * 1024) } else { "fn a() {}\nfn b() {}\nfn c() {}\nlet s = \"hi\";\n".to_owned() };
    let thread = |key: &str, open: bool| {
        json!({"key": key, "anchor": {"start": 2, "end": 3}, "open": open, "count": 2, "child": {"type": "comment", "key": format!("{key}-c"), "author": {"name": "Allan", "kind": "person"}, "body": "check this"}})
    };
    json!({"type": "column", "key": "root", "children": [
        {"type": "text", "key": "title", "text": format!("Code {instance}")},
        {"type": "text", "key": "status", "text": status},
        {"type": "code", "key": "code", "text": text, "language": "rust", "path": "src/lib.rs",
            "range_actions": [{"id": "comment", "label": "+ Comment"}],
            "threads": [thread("t-open", true), thread("t-closed", false)],
            "marks": [{"line": 1, "tone": "danger"}]},
        {"type": "diff", "key": "diff", "view": "split", "patch": "--- a/f\n+++ b/f\n@@ -1 +1 @@\n-old\n+new\n",
            "range_actions": [{"id": "comment", "label": "+ Comment"}]}
    ]})
}
