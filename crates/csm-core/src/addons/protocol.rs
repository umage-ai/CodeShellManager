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
pub const EVENT_KINDS: [&str; 5] = ["session_changed", "session_removed", "inbox_changed", "session_asleep", "session_awake"];

/// An effect a process add-on may declare in `[process] effects` (and a tool may then use).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessEffect {
    LaunchesProcess,
    SendsInput,
    ReadsSessionContent,
    WritesFilesystem,
    AccessesNetwork,
    /// Keeps secrets of its own in the OS keychain (`addon_secret_set` / `_get` / `_delete`).
    StoresSecrets,
    /// Writes only inside its own data folder (`data_dir` in `initialize`). No consent card.
    StoresData,
}

impl ProcessEffect {
    pub const ALL: [ProcessEffect; 7] = [
        ProcessEffect::LaunchesProcess,
        ProcessEffect::SendsInput,
        ProcessEffect::ReadsSessionContent,
        ProcessEffect::WritesFilesystem,
        ProcessEffect::AccessesNetwork,
        ProcessEffect::StoresSecrets,
        ProcessEffect::StoresData,
    ];

    /// The manifest and wire name, as in `addon.toml`.
    pub fn name(self) -> &'static str {
        match self {
            ProcessEffect::LaunchesProcess => "launches_process",
            ProcessEffect::SendsInput => "sends_input",
            ProcessEffect::ReadsSessionContent => "reads_session_content",
            ProcessEffect::WritesFilesystem => "writes_filesystem",
            ProcessEffect::AccessesNetwork => "accesses_network",
            ProcessEffect::StoresSecrets => "stores_secrets",
            ProcessEffect::StoresData => "stores_data",
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
    /// The screens it declared (at most [`screen::MAX_SCREENS`]), validated.
    pub screens: Vec<super::screen::ScreenDecl>,
}

/// What CSM tells a plugin it supports, in `initialize` (`capabilities`): feature name to the
/// version of that feature. Additive: a plugin ignores names it does not know, and a CSM that
/// predates one leaves it out.
pub fn capabilities() -> Value {
    json!({"caller": 1, "data_dir": 1, "standing_calls": 1, "screens": 1})
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
/// kind. A tool that omits `effects` gets all of the add-on's declared effects (the most it may
/// use), so it asks for consent like any tool that uses them.
pub fn parse_handshake(result: &Value, addon: &str, declared: &[ProcessEffect]) -> Result<Handshake, String> {
    let obj = result.as_object().ok_or("the answer to initialize is not an object")?;
    if let Some(api) = obj.get("api")
        && api.as_u64() != Some(PROTOCOL_API)
    {
        return Err(format!("the plugin speaks protocol {}, and CodeShellManager speaks {PROTOCOL_API}", clean_text(&api.to_string(), 40)));
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
            None => problems.push(format!("`{}` is not an event kind (use one of {})", clean_text(&s.to_string(), 40), EVENT_KINDS.join(", "))),
        }
    }

    let (screens, screen_problems) = super::screen::parse_screens(obj.get("screens"), addon);
    problems.extend(screen_problems);
    Ok(Handshake { name, tools, subscriptions, problems, screens })
}

fn parse_tool(entry: &Value, addon: &str, declared: &[ProcessEffect]) -> Result<DeclaredTool, String> {
    let obj = entry.as_object().ok_or("a tool entry is not an object")?;
    let id = obj.get("id").and_then(Value::as_str).ok_or("a tool has no `id`")?;
    let shown = clean_text(id, 80);
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
                return Err(format!("tool `{shown}`: input_schema must describe an object"));
            }
            if serde_json::to_string(v).map_or(true, |s| s.len() > MAX_SCHEMA_BYTES) {
                return Err(format!("tool `{shown}`: input_schema is larger than {MAX_SCHEMA_BYTES} bytes"));
            }
            v.clone()
        }
        Some(_) => return Err(format!("tool `{shown}`: input_schema must be an object")),
    };
    let mut effects = Vec::new();
    match obj.get("effects") {
        // Omitted: the tool may use everything the add-on declared, so consent for an MCP caller
        // follows the add-on's ceiling and a tool cannot skip consent by leaving `effects` out.
        // An explicit `[]` says the tool uses none of them.
        None | Some(Value::Null) => {
            for e in declared {
                if !effects.contains(e) {
                    effects.push(*e);
                }
            }
        }
        Some(Value::Array(a)) => {
            for e in a {
                let word = e.as_str().ok_or_else(|| format!("tool `{shown}`: effects must be strings"))?;
                let effect = ProcessEffect::from_name(word).ok_or_else(|| format!("tool `{shown}`: `{}` is not an effect", clean_text(word, 40)))?;
                if !declared.contains(&effect) {
                    return Err(format!("tool `{shown}` uses `{}`, which the add-on did not declare in its [process] effects; refused", effect.name()));
                }
                if !effects.contains(&effect) {
                    effects.push(effect);
                }
            }
        }
        Some(_) => return Err(format!("tool `{shown}`: effects must be an array")),
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

/// Most entries a plugin may offer in one `csm/remotes_changed`. Extra entries are reported and
/// ignored.
pub const MAX_REMOTES: usize = 200;
/// Longest remote label and detail, in characters.
pub const MAX_REMOTE_LABEL: usize = 80;
pub const MAX_REMOTE_DETAIL: usize = 160;
/// Largest session spec or tool-call params of one remote, serialized.
pub const MAX_REMOTE_SPEC_BYTES: usize = 8 * 1024;
/// Longest remote id, in bytes.
pub const MAX_REMOTE_ID: usize = 64;

/// What opening a remote does.
#[derive(Debug, Clone, PartialEq)]
pub enum RemoteOpen {
    /// The parameters of `session_new` (an ssh config/alias session, usually), opened as given.
    Session(Value),
    /// A tool of the same add-on, called with `params`; its `data.session` is then opened as in
    /// [`RemoteOpen::Session`].
    Tool { name: String, params: Value },
}

/// One entry a process add-on offers under "Remote…" in quick-open and the palette.
#[derive(Debug, Clone, PartialEq)]
pub struct Remote {
    /// Unique within the add-on.
    pub id: String,
    pub label: String,
    pub detail: String,
    pub open: RemoteOpen,
}

/// Whether `id` is a legal remote id: letters, digits and `. _ : -`, up to [`MAX_REMOTE_ID`].
pub fn valid_remote_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_REMOTE_ID && id.bytes().all(|b| b.is_ascii_alphanumeric() || b".:_-".contains(&b))
}

/// Validates the params of `csm/remotes_changed`: `{"remotes": [..]}`, the plugin's complete
/// current list (an empty list withdraws all of them).
///
/// `Err` when the params are not that shape. Per entry (reported, the rest still load): a bad id,
/// a duplicate, a missing label, neither or both of `session` and `tool`, a `tool` that is not
/// `<addon>.<name>`, a spec over [`MAX_REMOTE_SPEC_BYTES`]. At most [`MAX_REMOTES`] are kept.
pub fn parse_remotes(params: &Value, addon: &str) -> Result<(Vec<Remote>, Vec<String>), String> {
    let list = params.get("remotes").and_then(Value::as_array).ok_or("`remotes` must be an array")?;
    let mut problems = Vec::new();
    if list.len() > MAX_REMOTES {
        problems.push(format!("more than {MAX_REMOTES} remotes; {} ignored", list.len() - MAX_REMOTES));
    }
    let mut out: Vec<Remote> = Vec::new();
    for entry in list.iter().take(MAX_REMOTES) {
        match parse_remote(entry, addon) {
            Ok(r) if out.iter().any(|x| x.id == r.id) => problems.push(format!("remote `{}` is listed twice; the second is ignored", r.id)),
            Ok(r) => out.push(r),
            Err(e) => problems.push(e),
        }
    }
    Ok((out, problems))
}

fn parse_remote(entry: &Value, addon: &str) -> Result<Remote, String> {
    let obj = entry.as_object().ok_or("a remote entry is not an object")?;
    let id = obj.get("id").and_then(Value::as_str).ok_or("a remote has no `id`")?;
    if !valid_remote_id(id) {
        return Err(format!("remote id `{}` is not valid: use letters, digits and . _ : - (up to {MAX_REMOTE_ID})", clean_text(id, 40)));
    }
    let label = obj.get("label").and_then(Value::as_str).map(|s| clean_text(s, MAX_REMOTE_LABEL)).filter(|s| !s.is_empty()).ok_or_else(|| format!("remote `{id}` has no `label`"))?;
    let detail = obj.get("detail").and_then(Value::as_str).map(|s| clean_text(s, MAX_REMOTE_DETAIL)).unwrap_or_default();
    let too_big = |v: &Value| serde_json::to_string(v).map_or(true, |s| s.len() > MAX_REMOTE_SPEC_BYTES);
    let open = match (obj.get("session"), obj.get("tool")) {
        (Some(s @ Value::Object(_)), None) => {
            if too_big(s) {
                return Err(format!("remote `{id}`: the session spec is over {MAX_REMOTE_SPEC_BYTES} bytes"));
            }
            RemoteOpen::Session(s.clone())
        }
        (None, Some(Value::String(t))) => {
            let ok = t.strip_prefix(addon).and_then(|r| r.strip_prefix('.')).is_some_and(valid_tool_name);
            if !ok {
                return Err(format!("remote `{id}`: `tool` must be one of this add-on's tools, `{addon}.<name>`"));
            }
            let params = match obj.get("params") {
                None | Some(Value::Null) => json!({}),
                Some(p @ Value::Object(_)) => p.clone(),
                Some(_) => return Err(format!("remote `{id}`: `params` must be an object")),
            };
            if too_big(&params) {
                return Err(format!("remote `{id}`: `params` is over {MAX_REMOTE_SPEC_BYTES} bytes"));
            }
            RemoteOpen::Tool { name: t.clone(), params }
        }
        _ => return Err(format!("remote `{id}` needs exactly one of `session` (an object) and `tool` (a string)")),
    };
    Ok(Remote { id: id.to_owned(), label, detail, open })
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
    fn a_tool_that_omits_effects_gets_the_add_ons_ceiling() {
        let h = hs(
            json!({"name": "E", "tools": [
                {"id": "echo-test.omitted"},
                {"id": "echo-test.none", "effects": []},
                {"id": "echo-test.one", "effects": ["writes_filesystem"]}
            ]}),
            &[ProcessEffect::WritesFilesystem, ProcessEffect::SendsInput],
        )
        .unwrap();
        assert_eq!(h.tools[0].effects, vec![ProcessEffect::WritesFilesystem, ProcessEffect::SendsInput]);
        assert!(h.tools[1].effects.is_empty(), "an explicit empty list declares none");
        assert_eq!(h.tools[2].effects, vec![ProcessEffect::WritesFilesystem]);
        let none = hs(json!({"name": "E", "tools": [{"id": "echo-test.omitted"}]}), &[]).unwrap();
        assert!(none.tools[0].effects.is_empty(), "with no declared effects there is nothing to inherit");
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
    fn problem_text_from_the_plugin_is_capped() {
        // A refused entry's id and a bad subscription are echoed into `problems`; a 1 MiB id must not be.
        let long = format!("echo-test.{}", "x".repeat(200_000));
        let h = hs(json!({"name": "E", "tools": [{"id": long, "effects": ["no_such_effect"]}], "subscriptions": ["y".repeat(100_000)]}), &[]).unwrap();
        assert!(h.tools.is_empty());
        assert_eq!(h.problems.len(), 2);
        assert!(h.problems.iter().all(|p| p.chars().count() < 400), "{:?}", h.problems.iter().map(|p| p.len()).collect::<Vec<_>>());
        let bad_api = hs(json!({"name": "E", "api": "z".repeat(100_000)}), &[]);
        assert!(bad_api.is_err_and(|e| e.chars().count() < 200));
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

    #[test]
    fn remotes_are_validated_entry_by_entry() {
        let big = "x".repeat(MAX_REMOTE_SPEC_BYTES);
        let (list, problems) = parse_remotes(
            &json!({"remotes": [
                {"id": "a", "label": "Agent A", "detail": "idle", "session": {"ssh": {"host": "h", "config_file": "/c"}}},
                {"id": "b", "label": "Agent B", "tool": "echo-test.open", "params": {"name": "b"}},
                {"id": "a", "label": "dup", "session": {}},
                {"id": "bad id", "label": "x", "session": {}},
                {"id": "c", "session": {}},
                {"id": "d", "label": "both", "session": {}, "tool": "echo-test.open"},
                {"id": "e", "label": "neither"},
                {"id": "f", "label": "other add-on", "tool": "other.open"},
                {"id": "g", "label": "big", "session": {"x": big}},
                {"id": "h", "label": "bad params", "tool": "echo-test.open", "params": [1]},
                "nope"
            ]}),
            "echo-test",
        )
        .unwrap();
        assert_eq!(list.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(list[0].open, RemoteOpen::Session(json!({"ssh": {"host": "h", "config_file": "/c"}})));
        assert_eq!(list[1].open, RemoteOpen::Tool { name: "echo-test.open".into(), params: json!({"name": "b"}) });
        assert_eq!(problems.len(), 9, "{problems:?}");
        assert!(parse_remotes(&json!({"remotes": "x"}), "echo-test").is_err());
        assert!(parse_remotes(&json!({}), "echo-test").is_err());
        assert!(parse_remotes(&json!({"remotes": []}), "echo-test").unwrap().0.is_empty());
    }

    #[test]
    fn remotes_are_capped() {
        let many: Vec<Value> = (0..MAX_REMOTES + 5).map(|i| json!({"id": format!("r{i}"), "label": "l", "session": {}})).collect();
        let (list, problems) = parse_remotes(&json!({"remotes": many}), "echo-test").unwrap();
        assert_eq!(list.len(), MAX_REMOTES);
        assert_eq!(problems.len(), 1);
    }

    #[test]
    fn the_secrets_effect_and_sleep_events_are_known() {
        assert_eq!(ProcessEffect::from_name("stores_secrets"), Some(ProcessEffect::StoresSecrets));
        assert_eq!(ProcessEffect::from_name("stores_data"), Some(ProcessEffect::StoresData));
        assert!(ProcessEffect::ALL.iter().all(|e| ProcessEffect::from_name(e.name()) == Some(*e)));
        assert!(EVENT_KINDS.contains(&"session_asleep") && EVENT_KINDS.contains(&"session_awake"));
    }
}
