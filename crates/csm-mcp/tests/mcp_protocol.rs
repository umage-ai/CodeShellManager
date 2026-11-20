//! The MCP protocol end to end, in process: initialize, toolsets, calls, errors and consent.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use csm_engine::{Caller, Engine, Event, ToolError, ToolOutput};
use csm_mcp::{Backend, BridgeError, LocalBackend, McpOptions, McpService, ToolInfo};
use serde_json::{Value, json};

/// A client of the service: runs it on its own thread so a call that waits for consent does not
/// block the test, and returns every line the service wrote.
struct Client {
    tx: mpsc::Sender<String>,
    rx: mpsc::Receiver<Vec<String>>,
    next: u64,
}

impl Client {
    fn start<B: Backend + 'static>(backend: B, options: McpOptions) -> Client {
        let (tx, lines) = mpsc::channel::<String>();
        let (out, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut service = McpService::new(backend, options);
            while let Ok(line) = lines.recv() {
                if out.send(service.handle_line(&line)).is_err() {
                    break;
                }
            }
        });
        Client { tx, rx, next: 1 }
    }

    fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next;
        self.next += 1;
        self.tx.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()).unwrap();
        id
    }

    fn lines(&mut self, wait: Duration) -> Option<Vec<Value>> {
        let batch = self.rx.recv_timeout(wait).ok()?;
        Some(batch.iter().map(|l| serde_json::from_str(l).unwrap()).collect())
    }

    /// Send a request and return its reply, plus any notifications that came with it.
    fn request_all(&mut self, method: &str, params: Value) -> (Value, Vec<Value>) {
        let id = self.send(method, params);
        let mut all = self.lines(Duration::from_secs(10)).expect("no reply");
        let pos = all.iter().position(|m| m["id"] == id).expect("reply with our id");
        let reply = all.remove(pos);
        (reply, all)
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.request_all(method, params).0
    }

    fn initialize(&mut self, client: &str) -> Value {
        let r =
            self.request("initialize", json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": client, "version": "1"}}));
        self.tx.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string()).unwrap();
        assert!(self.lines(Duration::from_millis(300)).is_some_and(|l| l.is_empty()), "a notification gets no reply");
        r
    }

    fn tool_names(&mut self) -> Vec<String> {
        let r = self.request("tools/list", json!({}));
        r["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
    }

    fn call(&mut self, name: &str, args: Value) -> Value {
        self.request("tools/call", json!({"name": name, "arguments": args}))
    }
}

fn next_consent(events: &mpsc::Receiver<Event>) -> Value {
    loop {
        if let Event::ConsentRequested { request } = events.recv_timeout(Duration::from_secs(5)).expect("a consent request") {
            return request;
        }
    }
}

fn start() -> (Engine, Client) {
    let engine = common::engine();
    let client = Client::start(LocalBackend::new(engine.clone()), McpOptions::default());
    (engine, client)
}

#[test]
fn initialize_reports_server_instructions_and_list_changed() {
    let (_e, mut c) = start();
    let r = c.initialize("claude-code");
    assert_eq!(r["result"]["serverInfo"]["name"], "codeshell");
    assert_eq!(r["result"]["capabilities"]["tools"]["listChanged"], true);
    let instructions = r["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("approve") && instructions.contains("toolset_enable"));
}

#[test]
fn default_list_is_core_plus_the_toolset_tools() {
    let (_e, mut c) = start();
    c.initialize("claude-code");
    let names = c.tool_names();
    for expected in ["fake_echo", "fake_send", "tool_list", "toolset_list", "toolset_enable", "toolset_disable"] {
        assert!(names.contains(&expected.to_string()), "{expected} missing from {names:?}");
    }
    for hidden in ["fake_branch", "fake_layout", "consent_list", "consent_answer"] {
        assert!(!names.contains(&hidden.to_string()), "{hidden} must not be listed by default: {names:?}");
    }
}

#[test]
fn definitions_carry_schema_and_annotations_from_effects() {
    let (_e, mut c) = start();
    c.initialize("claude-code");
    let r = c.request("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let find = |n: &str| tools.iter().find(|t| t["name"] == n).unwrap().clone();

    let echo = find("fake_echo");
    assert_eq!(echo["inputSchema"]["required"], json!(["text"]));
    assert!(echo["inputSchema"].get("$schema").is_none() && echo["inputSchema"].get("title").is_none());
    assert_eq!(echo["annotations"]["readOnlyHint"], true);
    assert_eq!(echo["annotations"]["idempotentHint"], true);
    assert_eq!(echo["annotations"]["destructiveHint"], false);
    assert_eq!(echo["title"], "Fake");

    let send = find("fake_send");
    assert_eq!(send["annotations"]["readOnlyHint"], false);
    assert_eq!(send["annotations"]["destructiveHint"], true);
    assert!(send["description"].as_str().unwrap().contains("approval"));
    assert!(!echo["description"].as_str().unwrap().contains("approval"));
}

#[test]
fn enabling_a_toolset_lists_its_tools_and_notifies() {
    let (e, mut c) = start();
    c.initialize("claude-code");
    // The engine's own git tools plus the fake one.
    let git_tools: Vec<String> = e.registry().all().iter().filter(|s| s.toolset == csm_engine::Toolset::Git).map(|s| s.id.to_string()).collect();
    assert!(git_tools.contains(&"fake_branch".to_string()));

    let list = c.call("toolset_list", json!({}));
    let sets = list["result"]["structuredContent"]["toolsets"].as_array().unwrap().clone();
    let git = sets.iter().find(|s| s["id"] == "git").unwrap();
    assert_eq!(git["enabled"], false);
    assert_eq!(git["tools"], git_tools.len());

    let (reply, extra) = c.request_all("tools/call", json!({"name": "toolset_enable", "arguments": {"toolset": "git"}}));
    assert!(reply["result"].get("isError").is_none(), "{reply}");
    let enabled: Vec<String> = serde_json::from_value(reply["result"]["structuredContent"]["tools"].clone()).unwrap();
    assert_eq!(enabled.len(), git_tools.len(), "{enabled:?}");
    assert!(enabled.contains(&"fake_branch".to_string()));
    assert!(extra.iter().any(|m| m["method"] == "notifications/tools/list_changed"), "{extra:?}");
    assert!(c.tool_names().contains(&"fake_branch".to_string()));

    // Enabling again changes nothing, so no notification.
    let (_, extra) = c.request_all("tools/call", json!({"name": "toolset_enable", "arguments": {"toolset": "git"}}));
    assert!(extra.is_empty());

    let (_, extra) = c.request_all("tools/call", json!({"name": "toolset_disable", "arguments": {"toolset": "git"}}));
    assert_eq!(extra.len(), 1);
    assert!(!c.tool_names().contains(&"fake_branch".to_string()));

    // Core cannot be switched off by name, `all` enables everything.
    c.call("toolset_disable", json!({"toolset": "core"}));
    assert!(c.tool_names().contains(&"fake_echo".to_string()));
    c.call("toolset_enable", json!({"toolset": "all"}));
    let names = c.tool_names();
    assert!(names.contains(&"fake_layout".to_string()) && names.contains(&"fake_branch".to_string()));
    assert!(!names.contains(&"consent_answer".to_string()), "never listed, even with everything on");
}

#[test]
fn unknown_toolset_is_an_error_result() {
    let (_e, mut c) = start();
    c.initialize("claude-code");
    let r = c.call("toolset_enable", json!({"toolset": "all"}));
    assert!(r["result"].get("isError").is_none());
    // The schema's enum rejects a made-up name before it reaches the handler.
    let r = c.call("toolset_enable", json!({"toolset": "bogus"}));
    assert_eq!(r["error"]["code"], -32602);
}

#[test]
fn a_tool_in_a_disabled_toolset_says_how_to_enable_it() {
    let (_e, mut c) = start();
    c.initialize("claude-code");
    let r = c.call("fake_branch", json!({}));
    assert_eq!(r["result"]["isError"], true);
    let text = r["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("toolset_enable") && text.contains("\"git\""), "{text}");
    // A tool that does not exist at all is still unknown.
    let r = c.call("nope", json!({}));
    assert_eq!(r["error"]["code"], -32602);
}

#[test]
fn call_returns_message_and_structured_data() {
    let (_e, mut c) = start();
    c.initialize("claude-code");
    let r = c.call("fake_echo", json!({"text": "ab", "times": 2}));
    let result = &r["result"];
    assert!(result.get("isError").is_none());
    assert_eq!(result["content"][0]["text"], "Echoed abab.");
    assert_eq!(result["structuredContent"], json!({"text": "abab"}));
    assert_eq!(result["content"][1]["text"], json!({"text": "abab"}).to_string());

    // Arrays are wrapped: structuredContent must be an object.
    let r = c.call("fake_list", json!({}));
    assert_eq!(r["result"]["structuredContent"], json!({"result": [{"id": "a"}, {"id": "b"}]}));
}

#[test]
fn tool_errors_are_is_error_results_with_the_message() {
    let (_e, mut c) = start();
    c.initialize("claude-code");
    let r = c.call("fake_missing", json!({}));
    assert_eq!(r["result"]["isError"], true);
    assert_eq!(r["result"]["content"][0]["text"], "No session called nothing. Call session_list to see them.");
    assert!(r["result"].get("structuredContent").is_none());
}

#[test]
fn bad_arguments_are_rejected_before_the_engine() {
    let (_e, mut c) = start();
    c.initialize("claude-code");
    for args in [json!({}), json!({"text": 5}), json!({"text": "x", "extra": 1})] {
        let r = c.call("fake_echo", args.clone());
        assert_eq!(r["error"]["code"], -32602, "{args}: {r}");
    }
}

#[test]
fn calls_carry_the_client_name_and_session() {
    let engine = common::engine();
    let options = McpOptions { session_id: Some("odulodo".into()), all_tools: false };
    let mut c = Client::start(LocalBackend::new(engine), options);
    c.initialize("claude-code");
    let r = c.call("fake_whoami", json!({}));
    assert_eq!(r["result"]["structuredContent"], json!({"kind": "external", "client": "claude-code", "session_id": "odulodo"}));
}

#[test]
fn a_consent_call_waits_until_the_app_answers() {
    let (engine, mut c) = start();
    let events = engine.subscribe();
    c.initialize("claude-code");
    let id = c.send("tools/call", json!({"name": "fake_send", "arguments": {"text": "hi"}}));

    let request = next_consent(&events);
    assert_eq!(request["client"], "claude-code");
    assert_eq!(request["tool"], "fake_send");
    assert!(c.lines(Duration::from_millis(400)).is_none(), "the call must wait for the person");

    // Not even through the toolset tools: consent_answer is not on offer to an MCP client at all.
    c.call_expect_pending_error(&engine, &request);

    engine.call(Caller::Ui, "consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"})).unwrap();
    let reply = c.lines(Duration::from_secs(5)).expect("the answer releases the call").remove(0);
    assert_eq!(reply["id"], id);
    assert!(reply["result"].get("isError").is_none(), "{reply}");
    assert_eq!(reply["result"]["content"][0]["text"], "Sent.");
}

#[test]
fn a_denied_consent_call_is_an_error_result() {
    let (engine, mut c) = start();
    let events = engine.subscribe();
    c.initialize("claude-code");
    let id = c.send("tools/call", json!({"name": "fake_send", "arguments": {"text": "hi"}}));
    let request = next_consent(&events);
    engine.call(Caller::Ui, "consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
    let reply = c.lines(Duration::from_secs(5)).unwrap().remove(0);
    assert_eq!(reply["id"], id);
    assert_eq!(reply["result"]["isError"], true);
    assert!(reply["result"]["content"][0]["text"].as_str().unwrap().contains("not allowed"));
}

#[test]
fn a_client_cannot_answer_its_own_consent() {
    // Through the backend directly (as `csm mcp --all-tools` or a bug could): the engine refuses.
    let engine = common::engine();
    let events = engine.subscribe();
    let rx = engine.submit(Caller::External { client: "x".into(), session_id: None }, "fake_send", json!({"text": "t"}));
    let request = next_consent(&events);
    let mut backend = LocalBackend::new(engine.clone());
    let me = Caller::External { client: "x".into(), session_id: None };
    let r = backend.call(&me, "consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"}));
    assert!(matches!(r, Err(BridgeError::Tool(ToolError::Refused(_)))), "{r:?}");
    assert!(rx.try_recv().is_err(), "still waiting");
}

impl Client {
    /// While a request is parked: `consent_answer` is neither listed nor callable over MCP.
    fn call_expect_pending_error(&mut self, _engine: &Engine, _request: &Value) {
        // The service is busy with the parked call (calls are sequential), so ask through a second
        // service on the same engine: that is what a second MCP client would get.
        let second_engine = _engine.clone();
        let mut other = Client::start(LocalBackend::new(second_engine), McpOptions { session_id: None, all_tools: true });
        other.initialize("rogue-client");
        assert!(!other.tool_names().contains(&"consent_answer".to_string()));
        let r = other.call("consent_answer", json!({"request_id": _request["request_id"], "answer": "allow_once"}));
        assert!(r["error"].is_object(), "{r}");
    }
}

/// A backend that cannot reach the app.
struct Down;

impl Backend for Down {
    fn tools(&mut self) -> Result<Vec<ToolInfo>, BridgeError> {
        Err(BridgeError::Unreachable("CodeShellManager is not running.".into()))
    }
    fn call(&mut self, _: &Caller, _: &str, _: Value) -> Result<ToolOutput, BridgeError> {
        Err(BridgeError::Unreachable("CodeShellManager is not running.".into()))
    }
}

#[test]
fn an_unreachable_app_is_reported_in_plain_words() {
    let mut c = Client::start(Down, McpOptions::default());
    c.initialize("claude-code");
    let names = c.tool_names();
    assert_eq!(names, ["toolset_list", "toolset_enable", "toolset_disable"]);
    let r = c.call("toolset_list", json!({}));
    assert!(r["result"]["content"][0]["text"].as_str().unwrap().contains("not reachable"));
}

#[test]
fn junk_input_gets_a_parse_error_and_the_server_carries_on() {
    let (_e, mut c) = start();
    c.tx.send("{not json".to_string()).unwrap();
    let lines = c.lines(Duration::from_secs(5)).unwrap();
    assert_eq!(lines[0]["error"]["code"], -32700);
    assert!(c.initialize("claude-code")["result"].is_object());
}
