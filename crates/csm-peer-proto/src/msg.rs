//! The control messages of the peer link (JSON inside Noise). Bulk terminal bytes are not here:
//! they travel as binary frames ([`crate::codec::Frame`]) so they cost no base64.
//!
//! Direction is by variant: the client sends `Hello`, `Call`, `Term*` (except `TermResized` and
//! `TermClosed`), `ConsentAnswer`, `SnapshotRequest`, `Ping`, `Pong` and `Bye`; the server sends the rest. A
//! receiver refuses a message that does not belong to its peer's direction ([`Msg::from_client`]).
//!
//! Streams: the client picks a `stream` id (any `u32`) in `TermSubscribe`; every later frame of
//! that terminal carries it, so terminal frames stay small. Offsets are absolute byte counts in
//! the server's per-session output, so a client can resume with `from_offset`.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::codec::CodecError;
use crate::delta::Delta;
use crate::limits::{MAX_ID_BYTES, MAX_NAME_CHARS, MAX_TERM_DIM, MAX_UPLOAD_FILE, MAX_UPLOAD_IMAGE, MAX_UPLOAD_LABEL_BYTES};
use crate::{Event, Snapshot};

/// The version of the message set. A `Hello` or `Welcome` of another value is refused at decode.
/// (The Noise prologue carries the wire generation; this one allows a friendly `Bye`.)
pub const PROTOCOL_VERSION: u32 = 3;

/// What a device may do. The server enforces it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// Snapshot and terminal output only (later).
    Viewer,
    /// Sessions, input and runs, but no settings, add-ons or grants (later).
    Operator,
    /// Everything the office UI can do, including answering consent cards.
    Full,
}

impl Level {
    pub fn allows(self, required: Level) -> bool {
        self >= required
    }
}

/// Why a tool call failed. Mirrors the engine's error kinds, plus `Forbidden` (the device's level
/// or the caller kind does not allow the tool).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    UnknownTool,
    InvalidParams,
    NotFound,
    Refused,
    Declined,
    Failed,
    TimedOut,
    Forbidden,
}

/// The result of a call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Ok { message: String, data: Value },
    Err { kind: ErrorKind, message: String },
}

/// Why a terminal stream ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TermClosedReason {
    /// The session is gone (closed or removed).
    SessionGone,
    /// The session was put to sleep or exited: subscribe again after it wakes.
    NotRunning,
    /// No session of that id.
    UnknownSession,
    /// The device's level does not allow terminal access.
    Forbidden,
    /// The client sent `TermUnsubscribe`.
    Unsubscribed,
}

/// What became of a consent answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConsentOutcome {
    /// This answer decided the card (the first answer wins).
    Applied,
    /// Someone else answered first, or the card timed out.
    AlreadyResolved,
    /// The device is not at the Full level.
    Forbidden,
}

/// Why a side says goodbye.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    /// Server mode was turned off.
    ServerOff,
    /// The device was revoked.
    Revoked,
    /// The server is quitting.
    ServerQuit,
    /// The `Hello` names another protocol version.
    ProtocolMismatch,
    /// The terminal emulators differ (another `csm-term` minor version).
    TermMismatch,
    /// A message could not be decoded or broke the rules.
    BadMessage,
    /// No traffic within the dead-peer timeout.
    Timeout,
    /// The client disconnected on purpose.
    ClientQuit,
    /// The same device connected again; this connection is replaced.
    Replaced,
}

/// What an upload is for. An image has its own limit and its own name rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadKind {
    File,
    Image,
}

impl UploadKind {
    /// The largest file of this kind, in bytes.
    pub fn limit(self) -> u64 {
        match self {
            Self::File => MAX_UPLOAD_FILE,
            Self::Image => MAX_UPLOAD_IMAGE,
        }
    }
}

/// Why the office refused an upload. Refusing never drops the link.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadRefused {
    /// Over the size limit for the kind.
    TooLarge,
    /// Too many uploads are active.
    TooMany,
    /// Server mode is off, the device is revoked, or its level is below Full.
    Forbidden,
    /// No session of that id, or the id is not a usable folder name.
    NoSession,
    /// The name cannot be used (the office never trusts it).
    BadName,
    /// An image upload that does not start with the magic bytes of a supported image.
    NotAnImage,
    /// A chunk beyond the credit, out of order, or of an upload that is not open.
    Overrun,
    /// The disk is full.
    DiskFull,
    /// The bytes do not match the declared size or hash.
    Corrupt,
    /// No chunk arrived for too long.
    Stalled,
    /// Any other write or file error.
    Failed,
    /// The session cannot take uploads (an SSH session: the file would sit on the office, where
    /// the remote host cannot read it).
    NotSupported,
    /// The office's uploads folder is full (its quota) and nothing old enough could be removed.
    FolderFull,
}

/// A control message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Msg {
    /// C to S, the first message. `term_version` is the `csm-term` crate version.
    Hello { protocol: u32, client: String, app_version: String, term_version: String },
    /// S to C, the answer to `Hello`.
    Welcome { protocol: u32, host_name: String, server_version: String, term_version: String, level: Level, device_id: String },
    /// S to C: the whole read model (at attach and after every reconnect).
    Snapshot { snapshot: Box<Snapshot> },
    /// S to C: what changed since `delta.from`.
    Delta { delta: Delta },
    /// C to S: send the whole snapshot again. The client sends it when a delta does not apply to its
    /// mirror; the server answers with a `Snapshot` and the deltas go on from that version.
    SnapshotRequest,
    /// S to C. `None` is an event this client's version does not know: ignore it.
    Event {
        #[serde(default, deserialize_with = "lenient_event")]
        event: Option<Event>,
    },
    /// C to S: run a tool. `id` is the client's own counter.
    Call {
        id: u64,
        tool: String,
        #[serde(default)]
        params: Value,
    },
    /// S to C: the answer to `Call { id }`. Long calls are fine: there is no timeout here.
    Reply { id: u64, outcome: Outcome },
    /// C to S: show me this session's terminal. `instance` and `from_offset` ask to resume.
    TermSubscribe {
        stream: u32,
        session: String,
        #[serde(default)]
        instance: Option<u64>,
        #[serde(default)]
        from_offset: Option<u64>,
    },
    /// C to S: the pane is hidden. The server answers `TermClosed { Unsubscribed }`.
    TermUnsubscribe { stream: u32 },
    /// C to S: the client lost sync (or was told to): send a fresh attach.
    TermResync { stream: u32 },
    /// C to S: this client's pane wants this size. `driver` says it is the one typing (it decides
    /// the PTY size; see the design, "who owns the PTY size").
    TermResize { stream: u32, cols: u16, rows: u16, driver: bool },
    /// S to C: the PTY now has this size from `offset` on, in stream order.
    TermResized { stream: u32, offset: u64, cols: u16, rows: u16 },
    /// S to C: the stream ended.
    TermClosed { stream: u32, reason: TermClosedReason },
    /// S to C: a consent card, to Full clients only (pending ones are sent at attach).
    ConsentCard { request: Value },
    /// C to S: answer a card. The first answer wins, across the office UI and every client.
    ConsentAnswer { request_id: u64, allow: bool },
    /// S to C: what became of `ConsentAnswer`.
    ConsentAck { request_id: u64, outcome: ConsentOutcome },
    /// C to S: start an upload into the folder of `session`. `name` is a label only; the office
    /// sanitises it. Chunks follow only after `UploadAccepted`.
    UploadBegin { id: u64, session: String, name: String, size: u64, kind: UploadKind },
    /// C to S: all chunks are sent. `sha256` is the hex digest of the whole file.
    UploadEnd { id: u64, sha256: String },
    /// C to S: stop this upload and delete its partial file. The office sends no reply.
    UploadCancel { id: u64 },
    /// S to C: `UploadBegin` passed the checks; the laptop may send chunks.
    UploadAccepted { id: u64 },
    /// S to C: `received` bytes are on disk. The only credit for the chunk window.
    UploadAck { id: u64, received: u64 },
    /// S to C: the file is in place at `path` (the office's absolute path).
    UploadDone { id: u64, path: String },
    /// S to C: the upload is over and nothing was kept.
    UploadRefused { id: u64, reason: UploadRefused },
    Ping { n: u64 },
    Pong { n: u64 },
    Bye { reason: ByeReason },
}

fn lenient_event<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Event>, D::Error> {
    let v = Value::deserialize(d)?;
    Ok(serde_json::from_value::<Event>(v).ok())
}

fn no_control(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

fn valid_text(s: &str, max_bytes: usize) -> bool {
    !s.is_empty() && s.len() <= max_bytes && no_control(s)
}

/// Unicode format characters (category Cf) and line/paragraph separators: invisible, and able to
/// reorder or hide the text around them (bidi overrides, zero-width joiners, BOM).
fn is_format_char(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
        | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0xFEFF | 0xFFF9..=0xFFFB
        | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3 | 0x1D173..=0x1D17A
        | 0xE0001 | 0xE0020..=0xE007F)
}

/// The longest absolute path the office reports back.
const MAX_UPLOAD_PATH_BYTES: usize = 4096;

/// 1 to 64 characters, no control or invisible format characters, not only whitespace: a device
/// or host name (the person reads it on the Allow screen, so it must not be able to spoof).
pub fn valid_name(name: &str) -> bool {
    let n = name.trim().chars().count();
    (1..=MAX_NAME_CHARS).contains(&n) && name.chars().count() <= MAX_NAME_CHARS && no_control(name) && !name.chars().any(is_format_char)
}

/// A tool id: lowercase letters, digits, `_`, `.` and `-`, at most 128 bytes.
pub fn valid_tool_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_ID_BYTES && id.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
}

fn valid_dim(cols: u16, rows: u16) -> bool {
    (1..=MAX_TERM_DIM).contains(&cols) && (1..=MAX_TERM_DIM).contains(&rows)
}

/// The `csm-term` minor version: `major.minor` of a semver string. Emulators agree byte for byte
/// only within one minor version.
pub fn term_minor(version: &str) -> Option<(u32, u32)> {
    let mut it = version.split('.');
    let major = it.next()?.parse().ok()?;
    let minor = it.next()?.parse().ok()?;
    Some((major, minor))
}

impl Msg {
    /// Whether the client may send it (the server refuses the rest from a client, and the client
    /// refuses a client-only message from the server).
    pub fn from_client(&self) -> bool {
        matches!(
            self,
            Self::Hello { .. }
                | Self::Call { .. }
                | Self::TermSubscribe { .. }
                | Self::TermUnsubscribe { .. }
                | Self::TermResync { .. }
                | Self::TermResize { .. }
                | Self::ConsentAnswer { .. }
                | Self::SnapshotRequest
                | Self::UploadBegin { .. }
                | Self::UploadEnd { .. }
                | Self::UploadCancel { .. }
                | Self::Ping { .. }
                | Self::Pong { .. }
                | Self::Bye { .. }
        )
    }

    /// Whether the server may send it.
    pub fn from_server(&self) -> bool {
        !matches!(
            self,
            Self::Hello { .. }
                | Self::Call { .. }
                | Self::TermSubscribe { .. }
                | Self::TermUnsubscribe { .. }
                | Self::TermResync { .. }
                | Self::TermResize { .. }
                | Self::ConsentAnswer { .. }
                | Self::SnapshotRequest
                | Self::UploadBegin { .. }
                | Self::UploadEnd { .. }
                | Self::UploadCancel { .. }
        )
    }

    /// Checks the fields the JSON shape does not: protocol versions, lengths and characters.
    pub fn validate(&self) -> Result<(), CodecError> {
        let bad = |what: &'static str| Err(CodecError::Invalid(what));
        match self {
            Self::Hello { protocol, client, app_version, term_version } => {
                if *protocol != PROTOCOL_VERSION {
                    return Err(CodecError::Version(*protocol));
                }
                if !valid_name(client) {
                    return bad("client name");
                }
                if !valid_text(app_version, MAX_ID_BYTES) || !valid_text(term_version, MAX_ID_BYTES) {
                    return bad("version string");
                }
            }
            Self::Welcome { protocol, host_name, server_version, term_version, level: _, device_id } => {
                if *protocol != PROTOCOL_VERSION {
                    return Err(CodecError::Version(*protocol));
                }
                if !valid_name(host_name) {
                    return bad("host name");
                }
                if !valid_text(server_version, MAX_ID_BYTES) || !valid_text(term_version, MAX_ID_BYTES) {
                    return bad("version string");
                }
                if !crate::ids::is_valid_id(device_id) {
                    return bad("device id");
                }
            }
            Self::Call { tool, .. } => {
                if !valid_tool_id(tool) {
                    return bad("tool id");
                }
            }
            Self::TermSubscribe { session, .. } => {
                if !valid_text(session, MAX_ID_BYTES) {
                    return bad("session id");
                }
            }
            Self::TermResize { cols, rows, .. } | Self::TermResized { cols, rows, .. } => {
                if !valid_dim(*cols, *rows) {
                    return bad("terminal size");
                }
            }
            Self::Reply { outcome: Outcome::Err { message, .. }, .. } if !no_control_text(message) => return bad("error message"),
            Self::UploadBegin { session, name, .. } => {
                if !valid_text(session, MAX_ID_BYTES) {
                    return bad("session id");
                }
                if !valid_text(name, MAX_UPLOAD_LABEL_BYTES) {
                    return bad("upload name");
                }
                // The size limit is the office's to enforce (`UploadRefused::TooLarge`): a refusal
                // must not drop the link, and a codec error does.
            }
            Self::UploadEnd { sha256, .. } if !is_hex_digest(sha256) => return bad("upload digest"),
            Self::UploadDone { path, .. } if !valid_text(path, MAX_UPLOAD_PATH_BYTES) => return bad("upload path"),
            _ => {}
        }
        Ok(())
    }
}

/// A sha256 digest as 64 hex digits.
pub fn is_hex_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Error and message texts may hold newlines and tabs, but no other control characters.
fn no_control_text(s: &str) -> bool {
    !s.chars().any(|c| c.is_control() && c != '\n' && c != '\t')
}

/// Whether a client with `term_version` can attach to a server with `server_term`: the same
/// `major.minor`. Returns the `Bye` reason when not.
pub fn check_term_versions(server_term: &str, client_term: &str) -> Result<(), ByeReason> {
    match (term_minor(server_term), term_minor(client_term)) {
        (Some(a), Some(b)) if a == b => Ok(()),
        _ => Err(ByeReason::TermMismatch),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_are_ordered() {
        assert!(Level::Full.allows(Level::Operator));
        assert!(Level::Full.allows(Level::Full));
        assert!(!Level::Operator.allows(Level::Full));
        assert!(!Level::Viewer.allows(Level::Operator));
    }

    #[test]
    fn names_and_tool_ids() {
        assert!(valid_name("Laptop"));
        assert!(!valid_name(""));
        assert!(!valid_name("   "));
        assert!(!valid_name("a\nb"));
        for bad in ["a\u{202E}b", "a\u{200B}b", "\u{FEFF}x", "x\u{2066}", "a\u{200F}", "a\u{2028}b", "a\u{00AD}b", "a\u{E0041}b"] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name("Laptop \u{e9}\u{4e2d}"));
        assert!(!valid_name(&"x".repeat(65)));
        assert!(valid_name(&"x".repeat(64)));
        assert!(valid_tool_id("session_send_text"));
        assert!(valid_tool_id("addon.tool-1"));
        assert!(!valid_tool_id(""));
        assert!(!valid_tool_id("Bad"));
        assert!(!valid_tool_id("a b"));
        assert!(!valid_tool_id(&"a".repeat(129)));
    }

    #[test]
    fn term_versions_must_share_a_minor() {
        assert_eq!(check_term_versions("2.1.4", "2.1.9"), Ok(()));
        assert_eq!(check_term_versions("2.1.4", "2.2.0"), Err(ByeReason::TermMismatch));
        assert_eq!(check_term_versions("2.1.4", "junk"), Err(ByeReason::TermMismatch));
        assert_eq!(term_minor("0.26.1-beta.2"), Some((0, 26)), "the patch part may be anything");
        assert_eq!(term_minor("2"), None);
        assert_eq!(term_minor("a.b.c"), None);
    }

    #[test]
    fn direction_is_by_variant() {
        let hello = Msg::Hello { protocol: PROTOCOL_VERSION, client: "c".into(), app_version: "2".into(), term_version: "2.0.0".into() };
        assert!(hello.from_client() && !hello.from_server());
        let ping = Msg::Ping { n: 1 };
        assert!(ping.from_client() && ping.from_server());
        let snap = Msg::Delta { delta: Delta { from: 1, to: 2, ops: vec![] } };
        assert!(!snap.from_client() && snap.from_server());
    }
}
