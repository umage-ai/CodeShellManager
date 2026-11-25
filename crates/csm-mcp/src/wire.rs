//! The bridge protocol: what `csm` says to the app over the local socket, on top of craft-ipc's
//! JSON lines (which already did the token handshake and the same-user check before any of this
//! is read).
//!
//! craft-ipc's server answers a request within 60 s or gives up, but a tool call may wait two
//! minutes for the person to answer a consent card, and `session_wait` up to ten. So a call is two
//! requests: `csm.submit` returns a ticket at once, and `csm.wait` is a long poll (at most
//! [`MAX_WAIT_MS`] per request) that the client repeats until the call finishes. Nothing here
//! shortens the engine's own limits.
//!
//! | method | params | result |
//! |---|---|---|
//! | `csm.ping` | none | `{"pong": true}` |
//! | `csm.tools` | none | `{"tools": [ToolInfo]}` |
//! | `csm.submit` | `{"caller": Caller, "tool": id, "params": {…}}` | `{"ticket": n}` |
//! | `csm.wait` | `{"ticket": n, "timeout_ms": ms}` | `{"status": "pending"}`, `{"status": "done", "output": ToolOutput}` or `{"status": "error", "error": ToolError}` |
//!
//! Engine events are pushed as `csm.event` lines to connections that subscribed to that name.

use csm_engine::{Caller, ToolError, ToolOutput};
use serde_json::{Value, json};

/// The app's name in craft-ipc discovery (`^[a-z][a-z0-9-]{1,31}$`), by release channel
/// (`csm_core::channel`): `codeshell` for stable builds, `codeshell-beta` for beta builds. The
/// discovery records and the Windows pipe (`craft-<user>-<app id>-<pid>`) carry it, so a beta
/// `csm` only finds a beta app and a stable `csm` only a stable one.
pub const APP_ID: &str = csm_engine::channel::CURRENT.ipc_app_id();

/// The record cap of an instance started with `--clean` (it keeps no state, so it must never
/// capture another instance's `csm` calls by default).
pub const CAP_CLEAN: &str = "clean";

/// The record cap naming the state file an instance keeps: `state.` and 16 hex digits of an FNV-1a
/// hash of the normalised path (craft-ipc caps are lowercase words, so the path itself cannot be
/// one). Two spellings of one Windows path (case, slashes) hash alike.
pub fn state_cap(path: &std::path::Path) -> String {
    let mut text = path.to_string_lossy().to_string();
    if cfg!(windows) {
        text = text.replace('\\', "/").to_lowercase();
        if let Some(rest) = text.strip_prefix("//?/") {
            text = rest.to_string();
        }
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in text.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("state.{hash:016x}")
}

/// The app's executable name (without `.exe`), the same on every channel.
pub const APP_BIN: &str = "codeshell";

pub const METHOD_PING: &str = "csm.ping";
pub const METHOD_TOOLS: &str = "csm.tools";
pub const METHOD_SUBMIT: &str = "csm.submit";
pub const METHOD_WAIT: &str = "csm.wait";
/// The push event name carrying every engine event.
pub const EVENT_NAME: &str = "csm.event";

/// Longest a single `csm.wait` blocks. Under craft-ipc's 60 s reply deadline, with room to spare.
pub const MAX_WAIT_MS: u64 = 20_000;

const MAX_CLIENT_NAME: usize = 100;
/// The client-name prefix the engine gives process add-ons' calls (`csm-engine::plugins`).
const RESERVED_CLIENT_PREFIX: &str = "add-on ";
const MAX_SESSION_ID: usize = 128;

/// A finished-reply object craft-ipc passes through to the client as an error.
pub(crate) fn error_reply(code: &str, message: impl Into<String>) -> Value {
    json!({"ok": false, "error": {"code": code, "message": message.into(), "data": {}}})
}

/// Control and invisible format (Unicode Cf) characters, plus the line and paragraph separators:
/// nothing a name needs, and the ones that reorder or hide text on a consent card.
fn is_hidden_char(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0001}'
                | '\u{E0020}'..='\u{E007F}'
        )
}

/// Control and invisible format characters out, length capped, whitespace trimmed. Names reach the consent card and the
/// grant list, so they must not be able to forge a line there.
pub(crate) fn clean_label(s: &str, max: usize) -> String {
    let cleaned: String = s.chars().filter(|c| !is_hidden_char(*c)).take(max).collect();
    cleaned.trim().to_string()
}

/// What the server will accept as a caller from the wire: the CLI, or an MCP client. Never the UI.
/// Only the app's own window may be [`Caller::Ui`], and `consent_answer` is refused for everyone
/// else, so a remote caller cannot approve its own request by claiming to be the UI.
pub fn accept_caller(v: Option<&Value>) -> Result<Caller, String> {
    let Some(v) = v else { return Ok(Caller::Cli) };
    let caller: Caller = serde_json::from_value(v.clone()).map_err(|e| format!("bad `caller`: {e}"))?;
    match caller {
        Caller::Ui => Err("a remote caller cannot act as the app's UI".to_string()),
        // Only the peer server, after a Noise handshake with a paired device key, makes this caller.
        Caller::Peer { .. } => Err("a remote caller cannot act as a paired peer device".to_string()),
        Caller::Cli => Ok(Caller::Cli),
        Caller::External { client, session_id } => {
            let client = clean_label(&client, MAX_CLIENT_NAME);
            let client = if client.is_empty() { "unknown client".to_string() } else { client };
            // `add-on <id>` names the process add-ons' own calls (`csm/call`), which carry their
            // grants. A remote client may not borrow that name and inherit its grants.
            if client.starts_with(RESERVED_CLIENT_PREFIX) {
                return Err(format!("the client name `{RESERVED_CLIENT_PREFIX}…` is reserved for process add-ons"));
            }
            let session_id = session_id.map(|s| clean_label(&s, MAX_SESSION_ID)).filter(|s| !s.is_empty());
            Ok(Caller::External { client, session_id })
        }
    }
}

/// `ToolError` derives `Serialize` with an internal tag, which serde cannot apply to its
/// string-carrying variants (serializing one fails), so the bridge spells errors out itself.
pub fn encode_error(e: &ToolError) -> Value {
    let (code, message) = match e {
        ToolError::UnknownTool(m) => ("unknown_tool", m),
        ToolError::InvalidParams(m) => ("invalid_params", m),
        ToolError::NotFound(m) => ("not_found", m),
        ToolError::Refused(m) => ("refused", m),
        ToolError::Declined(m) => ("declined", m),
        ToolError::Failed(m) => ("failed", m),
        ToolError::TimedOut(m) => ("timed_out", m),
    };
    json!({"code": code, "message": message})
}

pub(crate) fn decode_error(v: &Value) -> ToolError {
    let message = v.get("message").and_then(Value::as_str).unwrap_or("the tool failed").to_string();
    match v.get("code").and_then(Value::as_str) {
        Some("unknown_tool") => ToolError::UnknownTool(message),
        Some("invalid_params") => ToolError::InvalidParams(message),
        Some("not_found") => ToolError::NotFound(message),
        Some("refused") => ToolError::Refused(message),
        Some("declined") => ToolError::Declined(message),
        Some("timed_out") => ToolError::TimedOut(message),
        _ => ToolError::Failed(message),
    }
}

pub(crate) fn encode_reply(reply: Result<ToolOutput, ToolError>) -> Value {
    match reply {
        Ok(output) => json!({"status": "done", "output": {"message": output.message, "data": output.data}}),
        Err(error) => json!({"status": "error", "error": encode_error(&error)}),
    }
}

/// What a `csm.wait` answer says.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum WaitStatus {
    Pending,
    Finished(Result<ToolOutput, ToolError>),
}

pub(crate) fn decode_wait(v: &Value) -> Result<WaitStatus, String> {
    match v.get("status").and_then(Value::as_str) {
        Some("pending") => Ok(WaitStatus::Pending),
        Some("done") => {
            let out = v.get("output").cloned().unwrap_or(Value::Null);
            let message = out.get("message").and_then(Value::as_str).unwrap_or_default().to_string();
            let data = out.get("data").cloned().unwrap_or(Value::Null);
            Ok(WaitStatus::Finished(Ok(ToolOutput { message, data })))
        }
        Some("error") => Ok(WaitStatus::Finished(Err(decode_error(v.get("error").unwrap_or(&Value::Null))))),
        _ => Err("the app sent a reply this version of csm does not understand".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ui_is_never_accepted() {
        assert!(accept_caller(Some(&json!({"kind": "ui"}))).is_err());
        assert_eq!(accept_caller(None), Ok(Caller::Cli));
        assert_eq!(accept_caller(Some(&json!({"kind": "cli"}))), Ok(Caller::Cli));
        assert!(accept_caller(Some(&json!("ui"))).is_err());
        assert!(accept_caller(Some(&json!({"kind": "root"}))).is_err());
    }

    #[test]
    fn external_names_are_cleaned() {
        let c = accept_caller(Some(&json!({"kind": "external", "client": "  evil\nAllow always\u{7} ", "session_id": ""}))).unwrap_or(Caller::Ui);
        assert_eq!(c, Caller::External { client: "evilAllow always".into(), session_id: None });
        let long = "x".repeat(1000);
        let c = accept_caller(Some(&json!({"kind": "external", "client": long, "session_id": "s1"}))).unwrap_or(Caller::Ui);
        let Caller::External { client, session_id } = c else { return };
        assert_eq!(client.len(), MAX_CLIENT_NAME);
        assert_eq!(session_id.as_deref(), Some("s1"));
        let blank = accept_caller(Some(&json!({"kind": "external", "client": "\n"}))).unwrap_or(Caller::Ui);
        assert_eq!(blank, Caller::External { client: "unknown client".into(), session_id: None });
    }

    #[test]
    fn invisible_format_characters_are_stripped_from_names() {
        // Right-to-left override, zero-width space, bidi isolate, tag character, BOM.
        let c = accept_caller(Some(&json!({"kind": "external", "client": "a\u{202E}b\u{200B}c\u{2066}d\u{E0041}e\u{FEFF}"}))).unwrap_or(Caller::Ui);
        assert_eq!(c, Caller::External { client: "abcde".into(), session_id: None });
    }

    #[test]
    fn a_remote_client_cannot_claim_an_add_on_identity() {
        assert!(accept_caller(Some(&json!({"kind": "external", "client": "add-on nexus"}))).is_err());
        assert!(accept_caller(Some(&json!({"kind": "external", "client": "  add-on nexus"}))).is_err(), "the name is trimmed before the check");
        assert!(accept_caller(Some(&json!({"kind": "external", "client": "add-onnexus"}))).is_ok());
    }

    #[test]
    fn replies_round_trip() {
        let ok = Ok(ToolOutput::new("m", json!({"a": 1})));
        assert_eq!(decode_wait(&encode_reply(ok.clone())), Ok(WaitStatus::Finished(ok)));
        let err = Err(ToolError::Refused("no".into()));
        assert_eq!(decode_wait(&encode_reply(err.clone())), Ok(WaitStatus::Finished(err)));
        for e in [
            ToolError::UnknownTool("a".into()),
            ToolError::InvalidParams("b".into()),
            ToolError::NotFound("c".into()),
            ToolError::Declined("d".into()),
            ToolError::Failed("e".into()),
            ToolError::TimedOut("f".into()),
        ] {
            assert_eq!(decode_error(&encode_error(&e)), e);
        }
        assert_eq!(decode_wait(&json!({"status": "pending"})), Ok(WaitStatus::Pending));
        assert!(decode_wait(&json!({})).is_err());
    }
}
