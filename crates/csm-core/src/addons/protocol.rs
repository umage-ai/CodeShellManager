//! The process add-on protocol (docs/addons.md, "Process add-ons"): wire types, validation of
//! what a plugin declares, and JSON-RPC framing. Pure: no I/O, no process, no clock.
//!
//! Everything here reads hostile input (a plugin's stdout). Nothing panics; every size is capped
//! before it is looked at; an invalid message is an `Err`, and the engine decides what to do.

use serde::Serialize;
use serde_json::{Value, json};

use super::manifest::clean_text;
use super::manifest::valid_id;

/// The protocol version CSM speaks. Sent in `initialize`; a plugin that answers with another
/// `api` is refused.
pub const PROTOCOL_API: u64 = 1;
/// Longest line (one JSON-RPC message) either side may send. A longer line is a protocol error
/// and the process is stopped.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;
/// Most tools one process add-on may declare. Extra entries are reported and ignored.
pub const MAX_TOOLS: usize = 32;
/// Most event kinds one process add-on may subscribe to.
pub const MAX_SUBSCRIPTIONS: usize = 8;
/// Longest tool name after the `addon.` prefix, in bytes.
pub const MAX_TOOL_NAME: usize = 48;
/// Longest tool title and plugin name, in characters.
pub const MAX_TITLE: usize = 64;
/// Longest tool summary, in characters.
pub const MAX_SUMMARY: usize = 500;
/// Largest input schema a tool may declare, serialized.
pub const MAX_SCHEMA_BYTES: usize = 16 * 1024;

/// JSON-RPC error codes CSM uses. Tool errors carry `-32000` with the `ToolError` code in `data`.
pub const ERR_METHOD_NOT_FOUND: i64 = -32601;
pub const ERR_INVALID_PARAMS: i64 = -32602;
pub const ERR_TOOL: i64 = -32000;
pub const ERR_BUSY: i64 = -32001;

/// The event kinds a process add-on may subscribe to. Each is sent as a notification
/// `event` whose params are the engine event (`{"event": kind, ...}`), with no content beyond
/// what the event itself carries (session ids and the like).
pub const EVENT_KINDS: [&str; 3] = ["session_changed", "session_removed", "inbox_changed"];

/// An effect a process add-on may declare in `[process] effects` (and a tool may then use).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEffect {
    LaunchesProcess,
    SendsInput,
    ReadsSessionContent,
    WritesFilesystem,
    AccessesNetwork,
}

impl ProcessEffect {
    pub const ALL: [ProcessEffect; 5] = [
        ProcessEffect::LaunchesProcess,
        ProcessEffect::SendsInput,
        ProcessEffect::ReadsSessionContent,
        ProcessEffect::WritesFilesystem,
        ProcessEffect::AccessesNetwork,
    ];

    /// The manifest and wire name, as in `addon.toml`.
    pub fn name(self) -> &'static str {
        match self {
            ProcessEffect::LaunchesProcess => "launches_process",
            ProcessEffect::SendsInput => "sends_input",
            ProcessEffect::ReadsSessionContent => "reads_session_content",
            ProcessEffect::WritesFilesystem => "writes_filesystem",
            ProcessEffect::AccessesNetwork => "accesses_network",
        }
    }

    pub fn from_name(s: &str) -> Option<ProcessEffect> {
        ProcessEffect::ALL.into_iter().find(|e| e.name() == s)
    }
}

/// One tool as a plugin declared it, validated.
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredTool {
    /// `<addon id>.<name>`.
    pub id: String,
    pub title: String,
    pub summary: String,
    pub input_schema: Value,
    /// Effects the tool uses; each is one the add-on declared.
    pub effects: Vec<ProcessEffect>,
}

/// What a plugin answered to `initialize`, validated. Tool-level problems do not refuse the
/// plugin: the bad entry is dropped and named in `problems`.
#[derive(Debug, Clone, PartialEq)]
pub struct Handshake {
    pub name: String,
    pub tools: Vec<DeclaredTool>,
    /// Event kinds from [`EVENT_KINDS`], deduplicated.
    pub subscriptions: Vec<&'static str>,
    pub problems: Vec<String>,
}

/// Whether `name` is a legal tool name after the `addon.` prefix: lowercase letters, digits and
/// `_`, not empty, not starting or ending with `_`, at most [`MAX_TOOL_NAME`] bytes.
pub fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_TOOL_NAME
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !name.starts_with('_')
        && !name.ends_with('_')
}

/// Splits `addon.name` into its add-on id and name. `None` when it is not a namespaced id
/// (`addon` ids have no dots, so the first dot is the boundary).
pub fn split_tool_id(id: &str) -> Option<(&str, &str)> {
    let (addon, name) = id.split_once('.')?;
    (valid_id(addon) && valid_tool_name(name)).then_some((addon, name))
}

/// Validates the `initialize` result of a plugin.
///
/// Refused as a whole (`Err`): not an object, no `name`, an `api` other than
/// [`PROTOCOL_API`], or `tools`/`subscriptions` that are not arrays. Per entry (reported in
/// `problems`, the rest still loads): a tool whose id is not `addon.name` for this add-on, a
/// duplicate, a tool using an effect the add-on did not declare, a bad schema, an unknown event
/// kind.
pub fn parse_handshake(result: &Value, addon: &str, declared: &[ProcessEffect]) -> Result<Handshake, String> {
    let obj = result.as_object().ok_or("the answer to initialize is not an object")?;
    if let Some(api) = obj.get("api")
        && api.as_u64() != Some(PROTOCOL_API)
    {
        return Err(format!("the plugin speaks protocol {api}, and CodeShellManager speaks {PROTOCOL_API}"));
    }
    let name = obj
        .get("name")
        .and_then(Value::as_str)
        .map(|s| clean_text(s, MAX_TITLE))
        .filter(|s| !s.is_empty())
        .ok_or("the answer to initialize has no `name`")?;
    let mut problems = Vec::new();

    let mut tools = Vec::new();
    let list = match obj.get("tools") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(a)) => a.as_slice(),
        Some(_) => return Err("`tools` must be an array".into()),
    };
    if list.len() > MAX_TOOLS {
        problems.push(format!("more than {MAX_TOOLS} tools; {} ignored", list.len() - MAX_TOOLS));
    }
    for entry in list.iter().take(MAX_TOOLS) {
        match parse_tool(entry, addon, declared) {
            Ok(t) => {
                if tools.iter().any(|x: &DeclaredTool| x.id == t.id) {
                    problems.push(format!("tool `{}` is declared twice; the second is ignored", t.id));
                } else {
                    tools.push(t);
                }
            }
            Err(e) => problems.push(e),
        }
    }

    let mut subscriptions: Vec<&'static str> = Vec::new();
    let subs = match obj.get("subscriptions") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(a)) => a.as_slice(),
        Some(_) => return Err("`subscriptions` must be an array".into()),
    };
    if subs.len() > MAX_SUBSCRIPTIONS {
        problems.push(format!("more than {MAX_SUBSCRIPTIONS} subscriptions; {} ignored", subs.len() - MAX_SUBSCRIPTIONS));
    }
    for s in subs.iter().take(MAX_SUBSCRIPTIONS) {
        match s.as_str().and_then(|s| EVENT_KINDS.into_iter().find(|k| *k == s)) {
            Some(kind) => {
                if !subscriptions.contains(&kind) {
                    subscriptions.push(kind);
                }
            }
            None => problems.push(format!("`{}` is not an event kind (use one of {})", s, EVENT_KINDS.join(", "))),
        }
    }

    Ok(Handshake { name, tools, subscriptions, problems })
}

fn parse_tool(entry: &Value, addon: &str, declared: &[ProcessEffect]) -> Result<DeclaredTool, String> {
    let obj = entry.as_object().ok_or("a tool entry is not an object")?;
    let id = obj.get("id").and_then(Value::as_str).ok_or("a tool has no `id`")?;
    let prefix = format!("{addon}.");
    let name = id.strip_prefix(&prefix).ok_or_else(|| format!("tool `{}` must be named `{addon}.<name>`", clean_text(id, 80)))?;
    if !valid_tool_name(name) {
        return Err(format!("tool `{}` has a bad name: use lowercase letters, digits and `_`", clean_text(id, 80)));
    }
    let title = obj.get("title").and_then(Value::as_str).map(|s| clean_text(s, MAX_TITLE)).filter(|s| !s.is_empty()).unwrap_or_else(|| id.to_owned());
    let summary = obj.get("summary").and_then(Value::as_str).map(|s| clean_text(s, MAX_SUMMARY)).unwrap_or_default();
    let input_schema = match obj.get("input_schema") {
        None | Some(Value::Null) => json!({"type": "object", "properties": {}, "additionalProperties": false}),
        Some(v @ Value::Object(_)) => {
            if v.get("type").is_some_and(|t| t != "object") {
                return Err(format!("tool `{id}`: input_schema must describe an object"));
            }
            if serde_json::to_string(v).map_or(true, |s| s.len() > MAX_SCHEMA_BYTES) {
                return Err(format!("tool `{id}`: input_schema is larger than {MAX_SCHEMA_BYTES} bytes"));
            }
            v.clone()
        }
        Some(_) => return Err(format!("tool `{id}`: input_schema must be an object")),
    };
    let mut effects = Vec::new();
    match obj.get("effects") {
        None | Some(Value::Null) => {}
        Some(Value::Array(a)) => {
            for e in a {
                let word = e.as_str().ok_or_else(|| format!("tool `{id}`: effects must be strings"))?;
                let effect = ProcessEffect::from_name(word).ok_or_else(|| format!("tool `{id}`: `{}` is not an effect", clean_text(word, 40)))?;
                if !declared.contains(&effect) {
                    return Err(format!("tool `{id}` uses `{}`, which the add-on did not declare in its [process] effects; refused", effect.name()));
                }
                if !effects.contains(&effect) {
                    effects.push(effect);
                }
            }
        }
        Some(_) => return Err(format!("tool `{id}`: effects must be an array")),
    }
    Ok(DeclaredTool { id: id.to_owned(), title, summary, input_schema, effects })
}

/// A message read from a plugin's stdout, classified.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The answer to a request CSM sent (`id` is one of CSM's numbers).
    Response { id: u64, outcome: Result<Value, RpcError> },
    /// A request from the plugin (`csm/call`, or a method CSM does not know). `id` is echoed back.
    Request { id: Value, method: String, params: Value },
    /// A notification from the plugin (no `id`, no answer).
    Notification { method: String, params: Value },
}

/// The `error` object of a JSON-RPC response.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Value,
}

/// Classifies one line. Errors name what is wrong; the caller stops the process on any error.
/// The line must already be within [`MAX_LINE_BYTES`].
pub fn parse_line(line: &str) -> Result<Incoming, String> {
    if line.len() > MAX_LINE_BYTES {
        return Err(format!("a message is over {MAX_LINE_BYTES} bytes"));
    }
    let v: Value = serde_json::from_str(line).map_err(|_| "a line is not JSON".to_owned())?;
    let obj = v.as_object().ok_or("a message is not a JSON object")?;
    if obj.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err("a message does not say \"jsonrpc\": \"2.0\"".into());
    }
    if let Some(method) = obj.get("method") {
        let method = method.as_str().ok_or("`method` must be a string")?.to_owned();
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        return match obj.get("id") {
            None => Ok(Incoming::Notification { method, params }),
            Some(id @ (Value::Number(_) | Value::String(_))) => Ok(Incoming::Request { id: id.clone(), method, params }),
            Some(_) => Err("a request `id` must be a number or a string".into()),
        };
    }
    let id = obj.get("id").and_then(Value::as_u64).ok_or("a response has no numeric `id`")?;
    let has_result = obj.contains_key("result");
    let has_error = obj.contains_key("error");
    let outcome = match (has_result, has_error) {
        (true, false) => Ok(obj.get("result").cloned().unwrap_or(Value::Null)),
        (false, true) => {
            let e = obj.get("error").and_then(Value::as_object).ok_or("a response `error` must be an object")?;
            Err(RpcError {
                code: e.get("code").and_then(Value::as_i64).unwrap_or(ERR_TOOL),
                message: e.get("message").and_then(Value::as_str).map(|s| clean_text(s, 500)).unwrap_or_default(),
                data: e.get("data").cloned().unwrap_or(Value::Null),
            })
        }
        _ => return Err("a response has both or neither of `result` and `error`".into()),
    };
    Ok(Incoming::Response { id, outcome })
}

/// `{"jsonrpc":"2.0","id":..,"method":..,"params":..}` as one line (no newline).
pub fn request_line(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
}

/// A notification as one line.
pub fn notification_line(method: &str, params: Value) -> String {
    json!({"jsonrpc": "2.0", "method": method, "params": params}).to_string()
}

/// A successful answer to a plugin's request.
pub fn response_ok(id: &Value, result: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string()
}

/// An error answer to a plugin's request.
pub fn response_err(id: &Value, code: i64, message: &str, data: Value) -> String {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message, "data": data}}).to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn hs(v: Value, declared: &[ProcessEffect]) -> Result<Handshake, String> {
        parse_handshake(&v, "echo-test", declared)
    }

    #[test]
    fn a_good_handshake_keeps_its_tools_and_subscriptions() {
        let h = hs(
            json!({
                "api": 1,
                "name": "Echo",
                "tools": [
                    {"id": "echo-test.echo", "title": "Echo", "summary": "Echoes", "input_schema": {"type": "object"}},
                    {"id": "echo-test.write", "effects": ["writes_filesystem"]}
                ],
                "subscriptions": ["session_changed", "session_changed", "inbox_changed"]
            }),
            &[ProcessEffect::WritesFilesystem],
        )
        .unwrap();
        assert_eq!(h.name, "Echo");
        assert_eq!(h.tools.len(), 2);
        assert_eq!(h.tools[0].title, "Echo");
        assert_eq!(h.tools[1].title, "echo-test.write", "a missing title falls back to the id");
        assert_eq!(h.tools[1].effects, vec![ProcessEffect::WritesFilesystem]);
        assert_eq!(h.tools[1].input_schema["type"], "object");
        assert_eq!(h.subscriptions, vec!["session_changed", "inbox_changed"]);
        assert!(h.problems.is_empty(), "{:?}", h.problems);
    }

    #[test]
    fn an_undeclared_effect_drops_only_that_tool() {
        let h = hs(
            json!({"name": "E", "tools": [
                {"id": "echo-test.sneaky", "effects": ["sends_input"]},
                {"id": "echo-test.ok"}
            ]}),
            &[ProcessEffect::WritesFilesystem],
        )
        .unwrap();
        assert_eq!(h.tools.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["echo-test.ok"]);
        assert!(h.problems.iter().any(|p| p.contains("did not declare") && p.contains("sends_input")), "{:?}", h.problems);
    }

    #[test]
    fn tool_ids_must_carry_the_add_on_prefix_and_a_clean_name() {
        let h = hs(
            json!({"name": "E", "tools": [
                {"id": "other.echo"},
                {"id": "echo-test.Echo"},
                {"id": "echo-test._x"},
                {"id": "echo-test.a.b"},
                {"id": "echo-test.fine_one"}
            ]}),
            &[],
        )
        .unwrap();
        assert_eq!(h.tools.len(), 1);
        assert_eq!(h.problems.len(), 4, "{:?}", h.problems);
    }

    #[test]
    fn duplicates_bad_schemas_and_unknown_kinds_are_reported_not_fatal() {
        let h = hs(
            json!({"name": "E",
                "tools": [
                    {"id": "echo-test.a"}, {"id": "echo-test.a"},
                    {"id": "echo-test.b", "input_schema": {"type": "string"}},
                    {"id": "echo-test.c", "input_schema": 5}
                ],
                "subscriptions": ["session_created", 7]
            }),
            &[],
        )
        .unwrap();
        assert_eq!(h.tools.len(), 1);
        assert!(h.subscriptions.is_empty());
        assert!(h.problems.len() >= 5, "{:?}", h.problems);
    }

    #[test]
    fn whole_handshake_refusals() {
        assert!(hs(json!([1]), &[]).is_err());
        assert!(hs(json!({"tools": []}), &[]).is_err(), "no name");
        assert!(hs(json!({"name": "E", "api": 2}), &[]).is_err(), "other protocol");
        assert!(hs(json!({"name": "E", "tools": 3}), &[]).is_err());
        assert!(hs(json!({"name": "E", "subscriptions": {}}), &[]).is_err());
        assert!(hs(json!({"name": "  \u{7}  "}), &[]).is_err(), "a name of control characters only");
    }

    #[test]
    fn too_many_tools_are_cut_at_the_cap() {
        let tools: Vec<Value> = (0..MAX_TOOLS + 5).map(|i| json!({"id": format!("echo-test.t{i}")})).collect();
        let h = hs(json!({"name": "E", "tools": tools}), &[]).unwrap();
        assert_eq!(h.tools.len(), MAX_TOOLS);
        assert!(h.problems.iter().any(|p| p.contains("5 ignored")));
    }

    #[test]
    fn a_huge_schema_is_refused() {
        let big = "x".repeat(MAX_SCHEMA_BYTES + 10);
        let h = hs(json!({"name": "E", "tools": [{"id": "echo-test.big", "input_schema": {"type": "object", "description": big}}]}), &[]).unwrap();
        assert!(h.tools.is_empty());
        assert!(h.problems[0].contains("larger than"));
    }

    #[test]
    fn split_tool_id_needs_a_valid_add_on_and_name() {
        assert_eq!(split_tool_id("generic-agent.run_it"), Some(("generic-agent", "run_it")));
        assert_eq!(split_tool_id("generic_agent.run"), None);
        assert_eq!(split_tool_id("session_list"), None);
        assert_eq!(split_tool_id("a.B"), None);
    }

    #[test]
    fn classifies_requests_responses_and_notifications() {
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","id":1,"result":{"name":"x"}}"#).unwrap(),
            Incoming::Response { id: 1, outcome: Ok(json!({"name": "x"})) }
        );
        match parse_line(r#"{"jsonrpc":"2.0","id":3,"error":{"code":-32000,"message":"no","data":{"code":"refused"}}}"#).unwrap() {
            Incoming::Response { id: 3, outcome: Err(e) } => {
                assert_eq!(e.code, -32000);
                assert_eq!(e.message, "no");
                assert_eq!(e.data["code"], "refused");
            }
            other => panic!("{other:?}"),
        }
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","id":"a","method":"csm/call","params":{"tool":"session_list"}}"#).unwrap(),
            Incoming::Request { id: json!("a"), method: "csm/call".into(), params: json!({"tool": "session_list"}) }
        );
        assert_eq!(
            parse_line(r#"{"jsonrpc":"2.0","method":"log","params":{"m":"hi"}}"#).unwrap(),
            Incoming::Notification { method: "log".into(), params: json!({"m": "hi"}) }
        );
    }

    #[test]
    fn rejects_malformed_lines_without_panicking() {
        for bad in [
            "not json",
            "[1,2]",
            r#"{"id":1,"result":{}}"#,
            r#"{"jsonrpc":"1.0","id":1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":-1,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":1}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":1,"error":{}}"#,
            r#"{"jsonrpc":"2.0","id":{"x":1},"method":"m"}"#,
            r#"{"jsonrpc":"2.0","id":1,"error":"x"}"#,
            "",
        ] {
            assert!(parse_line(bad).is_err(), "{bad}");
        }
        let long = format!("{{\"jsonrpc\":\"2.0\",\"method\":\"{}\"}}", "a".repeat(MAX_LINE_BYTES));
        assert!(parse_line(&long).is_err());
    }

    #[test]
    fn framing_builds_single_lines() {
        let l = request_line(7, "tools/call", json!({"name": "echo-test.echo"}));
        assert!(!l.contains('\n'));
        assert_eq!(
            parse_line(&l).unwrap(),
            Incoming::Request { id: json!(7), method: "tools/call".into(), params: json!({"name": "echo-test.echo"}) }
        );
        let n = notification_line("event", json!({"event": "inbox_changed"}));
        assert_eq!(parse_line(&n).unwrap(), Incoming::Notification { method: "event".into(), params: json!({"event": "inbox_changed"}) });
    }
}
