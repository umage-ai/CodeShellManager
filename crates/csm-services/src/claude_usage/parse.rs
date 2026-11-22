//! One transcript line → what it means for usage (ContextCost `parseLine`, numbers only).
//!
//! Tolerant by construction: every field is optional and read leniently (a number given as a
//! string, a float, `null` or a missing key all work or fall back to nothing), unknown keys are
//! ignored, and a line that is not JSON is a [`Line::Bad`] to count, never an error.
//! Only numbers, ids, model names, timestamps and the working folder are kept: no content.
//! (Remote Control lines are classified from their text and link; the text is not kept.)

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use csm_core::claude_usage::Tokens;
use csm_core::remote_control::{self, RemoteControlMark};
use csm_core::time::Timestamp;

/// What one line is.
#[derive(Debug, Clone, PartialEq)]
pub enum Line {
    /// An assistant message with usage.
    Request(ParsedRequest),
    /// `system` / `compact_boundary`.
    Compaction(ParsedCompaction),
    /// A main-thread `system` line about Remote Control (`csm_core::remote_control`).
    RemoteControl { mark: RemoteControlMark, cwd: Option<String> },
    /// Anything else that is valid (user lines, titles, snapshots...): carries the `cwd` when the
    /// line has one.
    Other { cwd: Option<String> },
    /// Not usable: not JSON, not an object, an assistant line without a message.
    Bad(&'static str),
    /// Valid JSON without a `type`: a format we do not know.
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRequest {
    pub id: String,
    pub ts_ms: Option<i64>,
    pub session_id: Option<String>,
    /// The line's `agentId`, or `sidechain` for an `isSidechain` line without one.
    pub agent_id: Option<String>,
    pub cwd: Option<String>,
    pub model: String,
    pub fast: bool,
    pub tokens: Tokens,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCompaction {
    pub ts_ms: Option<i64>,
    pub agent_id: Option<String>,
    pub trigger: Option<String>,
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
}

/// A lenient field: whatever JSON value is there, or `None`. Never fails.
fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Ok(Option::<Value>::deserialize(d).unwrap_or(None).filter(|v| !v.is_null()))
}

fn as_u64(v: &Option<Value>) -> Option<u64> {
    match v.as_ref()? {
        Value::Number(n) => n.as_u64().or_else(|| n.as_f64().filter(|f| f.is_finite() && *f >= 0.0).map(|f| f as u64)),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn as_string(v: &Option<Value>) -> Option<String> {
    match v.as_ref()? {
        Value::String(s) if !s.is_empty() => Some(s.chars().filter(|c| !c.is_control()).take(512).collect()),
        _ => None,
    }
}

fn as_bool(v: &Option<Value>) -> bool {
    matches!(v, Some(Value::Bool(true)))
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawLine {
    #[serde(rename = "type", deserialize_with = "lenient")]
    kind: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    subtype: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    timestamp: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    session_id: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    agent_id: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    is_sidechain: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    cwd: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    request_id: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    message: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    compact_metadata: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    content: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    url: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawMessage {
    #[serde(deserialize_with = "lenient")]
    id: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    model: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    usage: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawUsage {
    #[serde(deserialize_with = "lenient")]
    input_tokens: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    output_tokens: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    cache_read_input_tokens: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    cache_creation_input_tokens: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    cache_creation: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    speed: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct RawCacheCreation {
    #[serde(deserialize_with = "lenient")]
    ephemeral_5m_input_tokens: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    ephemeral_1h_input_tokens: Option<Value>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct RawCompact {
    #[serde(deserialize_with = "lenient")]
    trigger: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    pre_tokens: Option<Value>,
    #[serde(deserialize_with = "lenient")]
    post_tokens: Option<Value>,
}

fn from_value<T: for<'de> Deserialize<'de> + Default>(v: Option<Value>) -> T {
    v.and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default()
}

/// Epoch ms of an ISO-8601 timestamp; `None` for anything else.
pub fn parse_ts(v: &str) -> Option<i64> {
    Timestamp::from_text(v.to_owned()).epoch_ms()
}

/// The cache write split (ContextCost): when the 5m/1h split is missing or short of the total,
/// the rest counts as 5-minute writes.
fn cache_split(total: u64, w5: u64, w1: u64) -> (u64, u64) {
    let split = w5.saturating_add(w1);
    if split < total { (w5.saturating_add(total - split), w1) } else { (w5, w1) }
}

/// Parses one line. Never fails.
pub fn parse_line(line: &str) -> Line {
    let line = line.trim();
    if line.is_empty() {
        return Line::Other { cwd: None };
    }
    let value: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return Line::Bad("invalid_json"),
    };
    if !value.is_object() {
        return Line::Bad("not_object");
    }
    let raw: RawLine = serde_json::from_value(value).unwrap_or_default();
    let cwd = as_string(&raw.cwd);
    let Some(kind) = as_string(&raw.kind) else { return Line::Unknown };
    let ts_ms = as_string(&raw.timestamp).and_then(|t| parse_ts(&t));
    let agent_id = as_string(&raw.agent_id).or_else(|| as_bool(&raw.is_sidechain).then(|| "sidechain".to_owned()));
    match kind.as_str() {
        "assistant" => {
            let Some(msg) = raw.message.filter(Value::is_object) else { return Line::Bad("assistant_without_message") };
            let m: RawMessage = from_value(Some(msg));
            let model = as_string(&m.model).unwrap_or_else(|| "unknown".to_owned());
            if model == "<synthetic>" {
                return Line::Other { cwd };
            }
            let id = as_string(&m.id).or_else(|| as_string(&raw.request_id));
            let (Some(id), Some(usage)) = (id, m.usage.filter(Value::is_object)) else { return Line::Other { cwd } };
            let u: RawUsage = from_value(Some(usage));
            let cc: RawCacheCreation = from_value(u.cache_creation.clone());
            let total = as_u64(&u.cache_creation_input_tokens).unwrap_or(0);
            let (w5, w1) = cache_split(total, as_u64(&cc.ephemeral_5m_input_tokens).unwrap_or(0), as_u64(&cc.ephemeral_1h_input_tokens).unwrap_or(0));
            Line::Request(ParsedRequest {
                id,
                ts_ms,
                session_id: as_string(&raw.session_id),
                agent_id,
                cwd,
                model,
                fast: as_string(&u.speed).is_some_and(|s| s.eq_ignore_ascii_case("fast")),
                tokens: Tokens {
                    input: as_u64(&u.input_tokens).unwrap_or(0),
                    output: as_u64(&u.output_tokens).unwrap_or(0),
                    cache_read: as_u64(&u.cache_read_input_tokens).unwrap_or(0),
                    cache_write_5m: w5,
                    cache_write_1h: w1,
                },
            })
        }
        "system" if as_string(&raw.subtype).as_deref() == Some("compact_boundary") => {
            let md: RawCompact = from_value(raw.compact_metadata);
            Line::Compaction(ParsedCompaction {
                ts_ms,
                agent_id,
                trigger: as_string(&md.trigger),
                pre_tokens: as_u64(&md.pre_tokens),
                post_tokens: as_u64(&md.post_tokens),
            })
        }
        // Remote Control: only the conversation's own lines (a subagent's say nothing about it).
        // The text is read to classify the line and not kept.
        "system" if agent_id.is_none() => {
            let text = |v: &Option<Value>| match v {
                Some(Value::String(s)) => Some(s.chars().take(4096).collect::<String>()),
                _ => None,
            };
            let subtype = as_string(&raw.subtype);
            match remote_control::classify(subtype.as_deref(), text(&raw.content).as_deref(), text(&raw.url).as_deref()) {
                Some(event) => Line::RemoteControl { mark: RemoteControlMark { event, ts_ms }, cwd },
                None => Line::Other { cwd },
            }
        }
        _ => Line::Other { cwd },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// ContextCost's `asst()` helper.
    fn asst(id: &str, usage: Value, extra: Value) -> String {
        let mut usage_full = json!({"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0});
        if let (Some(base), Some(add)) = (usage_full.as_object_mut(), usage.as_object()) {
            for (k, v) in add {
                base.insert(k.clone(), v.clone());
            }
        }
        let mut line = json!({
            "type": "assistant", "sessionId": "s1", "isSidechain": false, "timestamp": "2026-10-01T10:00:00.000Z",
            "cwd": "C:\\Projects\\Demo", "gitBranch": "main", "requestId": format!("req_{id}"),
            "message": {"id": id, "model": "claude-opus-5-5", "role": "assistant", "content": [{"type": "text", "text": "hi"}], "usage": usage_full}
        });
        if let (Some(base), Some(add)) = (line.as_object_mut(), extra.as_object()) {
            for (k, v) in add {
                base.insert(k.clone(), v.clone());
            }
        }
        line.to_string()
    }

    fn request(l: Line) -> ParsedRequest {
        match l {
            Line::Request(r) => r,
            other => panic!("not a request: {other:?}"),
        }
    }

    #[test]
    fn assistant_line_with_a_split_cache_write() {
        let r = request(parse_line(&asst(
            "msg_1",
            json!({"input_tokens": 10, "output_tokens": 20, "cache_read_input_tokens": 30, "cache_creation_input_tokens": 40,
                   "cache_creation": {"ephemeral_5m_input_tokens": 15, "ephemeral_1h_input_tokens": 25}, "speed": "fast"}),
            json!({}),
        )));
        assert_eq!(r.id, "msg_1");
        assert_eq!(r.model, "claude-opus-5-5");
        assert_eq!(r.tokens, Tokens { input: 10, output: 20, cache_read: 30, cache_write_5m: 15, cache_write_1h: 25 });
        assert!(r.fast);
        assert_eq!(r.cwd.as_deref(), Some("C:\\Projects\\Demo"));
        assert_eq!(r.ts_ms, Some(1_790_848_800_000));
        assert_eq!(r.agent_id, None);
    }

    #[test]
    fn a_zero_split_or_no_split_counts_all_writes_as_5m() {
        let r = request(parse_line(&asst(
            "m",
            json!({"cache_creation_input_tokens": 26880, "cache_creation": {"ephemeral_5m_input_tokens": 0, "ephemeral_1h_input_tokens": 0}}),
            json!({}),
        )));
        assert_eq!((r.tokens.cache_write_5m, r.tokens.cache_write_1h), (26880, 0));
        let r = request(parse_line(&asst("m", json!({"cache_creation_input_tokens": 500}), json!({}))));
        assert_eq!(r.tokens.cache_write_5m, 500);
    }

    #[test]
    fn synthetic_messages_are_skipped_and_request_id_is_the_fallback_id() {
        let mut v: Value = serde_json::from_str(&asst("m", json!({}), json!({}))).unwrap();
        v["message"]["model"] = json!("<synthetic>");
        assert_eq!(parse_line(&v.to_string()), Line::Other { cwd: Some("C:\\Projects\\Demo".into()) });
        let mut v: Value = serde_json::from_str(&asst("msg_1", json!({}), json!({}))).unwrap();
        v["message"].as_object_mut().unwrap().remove("id");
        assert_eq!(request(parse_line(&v.to_string())).id, "req_msg_1");
    }

    #[test]
    fn sidechain_and_agent_lines_are_subagent_requests() {
        let r = request(parse_line(&asst("m", json!({}), json!({"agentId": "a1", "isSidechain": true}))));
        assert_eq!(r.agent_id.as_deref(), Some("a1"));
        let r = request(parse_line(&asst("m", json!({}), json!({"isSidechain": true}))));
        assert_eq!(r.agent_id.as_deref(), Some("sidechain"));
    }

    #[test]
    fn compaction_and_other_lines() {
        let l = json!({"type": "system", "subtype": "compact_boundary", "sessionId": "s1", "timestamp": "2026-10-01T10:05:00.000Z",
                       "compactMetadata": {"trigger": "auto", "preTokens": 968516, "postTokens": 22679}});
        match parse_line(&l.to_string()) {
            Line::Compaction(c) => {
                assert_eq!((c.trigger.as_deref(), c.pre_tokens, c.post_tokens), (Some("auto"), Some(968516), Some(22679)));
            }
            other => panic!("{other:?}"),
        }
        let title = json!({"type": "ai-title", "aiTitle": "My session", "sessionId": "s1"});
        assert_eq!(parse_line(&title.to_string()), Line::Other { cwd: None });
        let user = json!({"type": "user", "cwd": "/srv/x", "message": {"role": "user", "content": "secret prompt"}});
        assert_eq!(parse_line(&user.to_string()), Line::Other { cwd: Some("/srv/x".into()) });
    }

    #[test]
    fn malformed_lines_are_counted_never_errors() {
        assert_eq!(parse_line("{not json"), Line::Bad("invalid_json"));
        assert_eq!(parse_line("42"), Line::Bad("not_object"));
        assert_eq!(parse_line("[1,2]"), Line::Bad("not_object"));
        assert_eq!(parse_line(""), Line::Other { cwd: None });
        assert_eq!(parse_line(r#"{"type":"assistant"}"#), Line::Bad("assistant_without_message"));
        assert_eq!(parse_line(r#"{"type":"assistant","message":"x"}"#), Line::Bad("assistant_without_message"));
        assert_eq!(parse_line(r#"{"no":"type"}"#), Line::Unknown);
        assert_eq!(parse_line(r#"{"type":5}"#), Line::Unknown);
    }

    #[test]
    fn hostile_field_types_fall_back_instead_of_failing() {
        let l = json!({"type": "assistant", "timestamp": 12, "cwd": ["x"], "sessionId": {"a": 1},
            "message": {"id": "m", "model": 7, "usage": {"input_tokens": "12", "output_tokens": -4, "cache_read_input_tokens": 1.9e3,
                "cache_creation_input_tokens": null, "cache_creation": "nope", "speed": 1}}});
        let r = request(parse_line(&l.to_string()));
        assert_eq!(r.model, "unknown");
        assert_eq!(r.ts_ms, None);
        assert_eq!(r.cwd, None);
        assert_eq!(r.tokens, Tokens { input: 12, output: 0, cache_read: 1900, cache_write_5m: 0, cache_write_1h: 0 });
        assert!(!r.fast);
        // Huge numbers saturate, they do not wrap or panic.
        let l = json!({"type": "assistant", "message": {"id": "m", "usage": {"input_tokens": 1e300, "cache_creation_input_tokens": u64::MAX,
            "cache_creation": {"ephemeral_5m_input_tokens": u64::MAX, "ephemeral_1h_input_tokens": u64::MAX}}}});
        let r = request(parse_line(&l.to_string()));
        assert_eq!(r.tokens.input, u64::MAX);
        assert_eq!(r.tokens.prompt(), u64::MAX);
    }

    #[test]
    fn an_assistant_line_without_usage_or_id_is_not_a_request() {
        let l = json!({"type": "assistant", "message": {"model": "claude-x", "usage": {"input_tokens": 1}}});
        assert_eq!(parse_line(&l.to_string()), Line::Other { cwd: None });
        let l = json!({"type": "assistant", "message": {"id": "m", "model": "claude-x"}});
        assert_eq!(parse_line(&l.to_string()), Line::Other { cwd: None });
    }

    #[test]
    fn remote_control_lines_from_the_main_thread_only() {
        use csm_core::remote_control::RemoteControlEvent;
        let url = "https://claude.ai/code/session_01W2nwy51AsMgseViHRgKABa";
        let on = json!({"type": "system", "subtype": "bridge_status", "content": format!("/remote-control is active \u{b7} Continue here, on your phone, or at {url}"),
            "url": url, "isMeta": false, "timestamp": "2026-05-29T10:30:01.718Z", "cwd": "C:/x"});
        match parse_line(&on.to_string()) {
            Line::RemoteControl { mark, cwd } => {
                assert_eq!(mark.event, RemoteControlEvent::On { url: Some(url.to_owned()) });
                assert!(mark.ts_ms.is_some());
                assert_eq!(cwd.as_deref(), Some("C:/x"));
            }
            other => panic!("not remote control: {other:?}"),
        }
        let off = json!({"type": "system", "subtype": "local_command", "content": "<local-command-stdout>Remote Control disconnected.</local-command-stdout>", "level": "info"});
        assert!(matches!(parse_line(&off.to_string()), Line::RemoteControl { mark, .. } if mark.event == RemoteControlEvent::Off));
        // A subagent's line, and a content that is not text, say nothing.
        let side = json!({"type": "system", "subtype": "bridge_status", "url": url, "isSidechain": true});
        assert_eq!(parse_line(&side.to_string()), Line::Other { cwd: None });
        let odd = json!({"type": "system", "subtype": "local_command", "content": {"x": 1}, "url": 7});
        assert_eq!(parse_line(&odd.to_string()), Line::Other { cwd: None });
        let mut b = super::super::Batch::default();
        b.push(&on.to_string());
        b.push(&off.to_string());
        assert_eq!(b.remote_control.map(|m| m.event), Some(RemoteControlEvent::Off));
    }
}
