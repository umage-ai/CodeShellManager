//! The add-on's view of CSM: a trait with one method, `call(tool, params)`, that every CSM tool
//! goes through. Production wraps `csm/call` over stdio; tests use a fake that counts calls.
//!
//! The tool names used here are the only ones the add-on may call. They are listed in the
//! manifest's `[process] calls` and checked by a test against `CALLS` there.

use serde_json::Value;

/// The CSM tools the add-on maps phone ops onto (design note, section 4). Nothing else.
pub const CALLS: &[&str] = &[
    "session_list",
    "session_get",
    "session_recent",
    "inbox_list",
    "task_list",
    "task_get",
    "settings_get",
    "session_new",
    "session_wake",
    "session_sleep",
    "session_restart",
    "session_close",
    "session_status_clear",
    "session_send_command",
];

/// Why a CSM call failed, in the terms the phone's error codes need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CsmError {
    UnknownSession,
    Busy,
    CommandRefused,
    /// CSM answered with this (plain text) reason.
    Failed(String),
    /// CSM did not answer in time, or is not running.
    Unavailable,
}

/// `Send`, so the host (which owns one) can move to the thread that runs it.
pub trait Csm: Send {
    fn call(&self, tool: &str, params: Value) -> Result<Value, CsmError>;
}

/// Turns CSM's failure text into a [`CsmError`]. The match is on fixed phrases only; anything
/// else is `Failed` with the text, which is cut to a safe length for the phone.
pub fn classify(message: &str) -> CsmError {
    let m = message.to_lowercase();
    if m.contains("unknown session") || m.contains("no such session") {
        CsmError::UnknownSession
    } else if m.contains("busy") || m.contains("mid-turn") || m.contains("is working") {
        CsmError::Busy
    } else if m.contains("command") && (m.contains("refus") || m.contains("not allowed")) {
        CsmError::CommandRefused
    } else {
        CsmError::Failed(crate::policy::cut(message, 300))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_uses_fixed_phrases() {
        assert_eq!(classify("Unknown session abc"), CsmError::UnknownSession);
        assert_eq!(classify("session is busy; use force"), CsmError::Busy);
        assert_eq!(classify("command refused: mid-turn"), CsmError::Busy);
        assert_eq!(classify("the command is not allowed"), CsmError::CommandRefused);
        assert_eq!(classify("disk full"), CsmError::Failed("disk full".into()));
    }

    #[test]
    fn every_call_is_in_the_known_list() {
        assert!(CALLS.contains(&"session_send_command"));
        assert!(!CALLS.contains(&"addon_secret_get"), "secrets are the add-on's own tools, not CSM calls");
    }
}
