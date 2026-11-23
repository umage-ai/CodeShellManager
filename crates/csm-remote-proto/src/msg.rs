//! Messages between the phone and the add-on, carried as JSON inside Noise transport messages.
//!
//! ```text
//! phone -> host   {"id":7,"op":"session.wake","args":{"session_id":"s1"}}
//! host  -> phone  {"id":7,"ok":true,"data":{...}}        or
//!                 {"id":7,"ok":false,"error":{"code":"forbidden","message":"..."}}
//! host  -> phone  {"ev":"delta","seq":12,"upserts":[...],"removed":[...]}
//! ```
//!
//! The phone sends a **fixed op set** ([`Op`]); never a tool id or raw parameters. The add-on
//! checks the device's [`Level`] against [`Op::required_level`], validates the arguments
//! ([`Op::validate`]) and maps the op onto its fixed list of CSM tools. v1 has no free text and no
//! terminal reading: [`Op::SessionCommand`] only names a command from the PC's configured list.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::limits::MAX_APP_MESSAGE_BYTES;

/// What a paired device may do. `Operator` includes everything `Viewer` may.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    Viewer,
    Operator,
}

impl Level {
    pub fn allows(self, required: Level) -> bool {
        self >= required
    }
}

// ---- requests ----------------------------------------------------------------------------

/// The ops. Wire names are the `rename`s; `args` is an object (absent or `{}` for no-argument ops).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", content = "args")]
pub enum Op {
    /// First message on a channel: the phone says who it is; the reply carries [`HelloData`].
    #[serde(rename = "hello")]
    Hello { client: String, protocol: u8 },
    #[serde(rename = "ping")]
    Ping,
    /// Ask for a full snapshot (also sent by the host unprompted after `hello`).
    #[serde(rename = "snapshot")]
    Snapshot,
    #[serde(rename = "session.list")]
    SessionList,
    #[serde(rename = "session.get")]
    SessionGet { session_id: String },
    #[serde(rename = "inbox.list")]
    InboxList,
    #[serde(rename = "task.list")]
    TaskList,
    #[serde(rename = "task.get")]
    TaskGet { task_id: String },
    /// Folders a new session may open in (sessions' folders, recent, pinned) and the profiles.
    #[serde(rename = "folder.list")]
    FolderList,
    #[serde(rename = "profile.list")]
    ProfileList,
    /// The slash commands the PC lets a phone send (a subset of the configured Claude commands).
    #[serde(rename = "command.list")]
    CommandList,
    #[serde(rename = "session.wake")]
    SessionWake { session_id: String },
    #[serde(rename = "session.sleep")]
    SessionSleep { session_id: String },
    #[serde(rename = "session.restart")]
    SessionRestart { session_id: String },
    /// `force` after the phone's own "this session is busy" confirmation.
    #[serde(rename = "session.close")]
    SessionClose {
        session_id: String,
        #[serde(default)]
        force: bool,
    },
    /// `folder` must be on the add-on's allow list and `profile` a known profile; there is no
    /// `command` or `args`. `remote_control`: turn Remote Control on once the session is idle.
    #[serde(rename = "session.new")]
    SessionNew {
        folder: String,
        profile: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        #[serde(default)]
        remote_control: bool,
    },
    /// Send a slash command from the PC's configured list (above all `/remote-control`).
    #[serde(rename = "session.command")]
    SessionCommand {
        session_id: String,
        command: String,
        #[serde(default)]
        force: bool,
    },
    #[serde(rename = "session.status_clear")]
    SessionStatusClear { session_id: String },
    /// Register or replace this device's Web Push subscription (keys stay inside the E2E channel).
    #[serde(rename = "push.subscribe")]
    PushSubscribe { endpoint: String, p256dh: String, auth: String },
    #[serde(rename = "push.unsubscribe")]
    PushUnsubscribe,
    /// This device's preferences. `hide_names`: pushes do not show session names.
    #[serde(rename = "settings.set")]
    SettingsSet { hide_names: bool, notify: bool },
}

pub const MAX_ID_CHARS: usize = 64;
pub const MAX_FOLDER_CHARS: usize = 1024;
pub const MAX_NAME_CHARS: usize = 128;
pub const MAX_COMMAND_CHARS: usize = 64;
pub const MAX_ENDPOINT_CHARS: usize = 1024;

fn clean(s: &str, max: usize) -> bool {
    let n = s.chars().count();
    (1..=max).contains(&n) && !s.chars().any(char::is_control)
}

impl Op {
    /// Every wire name, for "unknown op" detection and docs.
    pub const NAMES: &'static [&'static str] = &[
        "hello",
        "ping",
        "snapshot",
        "session.list",
        "session.get",
        "inbox.list",
        "task.list",
        "task.get",
        "folder.list",
        "profile.list",
        "command.list",
        "session.wake",
        "session.sleep",
        "session.restart",
        "session.close",
        "session.new",
        "session.command",
        "session.status_clear",
        "push.subscribe",
        "push.unsubscribe",
        "settings.set",
    ];

    pub fn name(&self) -> &'static str {
        match self {
            Self::Hello { .. } => "hello",
            Self::Ping => "ping",
            Self::Snapshot => "snapshot",
            Self::SessionList => "session.list",
            Self::SessionGet { .. } => "session.get",
            Self::InboxList => "inbox.list",
            Self::TaskList => "task.list",
            Self::TaskGet { .. } => "task.get",
            Self::FolderList => "folder.list",
            Self::ProfileList => "profile.list",
            Self::CommandList => "command.list",
            Self::SessionWake { .. } => "session.wake",
            Self::SessionSleep { .. } => "session.sleep",
            Self::SessionRestart { .. } => "session.restart",
            Self::SessionClose { .. } => "session.close",
            Self::SessionNew { .. } => "session.new",
            Self::SessionCommand { .. } => "session.command",
            Self::SessionStatusClear { .. } => "session.status_clear",
            Self::PushSubscribe { .. } => "push.subscribe",
            Self::PushUnsubscribe => "push.unsubscribe",
            Self::SettingsSet { .. } => "settings.set",
        }
    }

    /// The least level that may run this op. Everything that changes a session, or opens one,
    /// needs `Operator`; reading and the device's own preferences need `Viewer`.
    pub fn required_level(&self) -> Level {
        match self {
            Self::SessionWake { .. }
            | Self::SessionSleep { .. }
            | Self::SessionRestart { .. }
            | Self::SessionClose { .. }
            | Self::SessionNew { .. }
            | Self::SessionCommand { .. }
            | Self::SessionStatusClear { .. } => Level::Operator,
            _ => Level::Viewer,
        }
    }

    /// Shape checks only (lengths, control characters, commands start with `/`, an https push
    /// endpoint). Policy (is this folder allowed? is this command configured?) is the add-on's.
    pub fn validate(&self) -> Result<(), ErrorCode> {
        let ok = match self {
            Self::Hello { client, protocol } => clean(client, MAX_NAME_CHARS) && *protocol >= 1,
            Self::Ping | Self::Snapshot | Self::SessionList | Self::InboxList | Self::TaskList => true,
            Self::FolderList | Self::ProfileList | Self::CommandList | Self::PushUnsubscribe | Self::SettingsSet { .. } => true,
            Self::SessionGet { session_id }
            | Self::SessionWake { session_id }
            | Self::SessionSleep { session_id }
            | Self::SessionRestart { session_id }
            | Self::SessionClose { session_id, .. }
            | Self::SessionStatusClear { session_id } => clean(session_id, MAX_ID_CHARS),
            Self::TaskGet { task_id } => clean(task_id, MAX_ID_CHARS),
            Self::SessionNew { folder, profile, name, .. } => {
                clean(folder, MAX_FOLDER_CHARS) && clean(profile, MAX_NAME_CHARS) && name.as_deref().is_none_or(|n| clean(n, MAX_NAME_CHARS))
            }
            Self::SessionCommand { session_id, command, .. } => {
                clean(session_id, MAX_ID_CHARS) && clean(command, MAX_COMMAND_CHARS) && command.starts_with('/') && !command.contains(char::is_whitespace)
            }
            Self::PushSubscribe { endpoint, p256dh, auth } => {
                clean(endpoint, MAX_ENDPOINT_CHARS) && endpoint.starts_with("https://") && clean(p256dh, 128) && clean(auth, 64)
            }
        };
        if ok { Ok(()) } else { Err(ErrorCode::BadRequest) }
    }
}

/// A request as it travels: `{"id":7,"op":"...","args":{...}}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Request {
    pub id: u64,
    #[serde(flatten)]
    pub op: Op,
}

/// Why a request could not be parsed, with the id when one could be read (to answer it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestError {
    pub id: Option<u64>,
    pub code: ErrorCode,
}

impl Request {
    /// Parses and shape-validates one request. Never panics; size is checked first.
    pub fn parse(bytes: &[u8]) -> Result<Request, RequestError> {
        let fail = |id, code| RequestError { id, code };
        if bytes.len() > MAX_APP_MESSAGE_BYTES {
            return Err(fail(None, ErrorCode::TooLarge));
        }
        let mut v: Value = serde_json::from_slice(bytes).map_err(|_| fail(None, ErrorCode::BadRequest))?;
        // `"args":{}` and `"args":null` mean "no arguments" (a unit op takes no content).
        if let Some(obj) = v.as_object_mut() && obj.get("args").is_some_and(|a| a.is_null() || a.as_object().is_some_and(serde_json::Map::is_empty)) {
            obj.remove("args");
        }
        let id = v.get("id").and_then(Value::as_u64);
        let Some(id_val) = id else { return Err(fail(None, ErrorCode::BadRequest)) };
        let name = v.get("op").and_then(Value::as_str).unwrap_or_default();
        if !Op::NAMES.contains(&name) {
            return Err(fail(Some(id_val), ErrorCode::UnknownOp));
        }
        let req: Request = serde_json::from_value(v).map_err(|_| fail(Some(id_val), ErrorCode::BadRequest))?;
        req.op.validate().map_err(|code| fail(Some(id_val), code))?;
        Ok(req)
    }
}

// ---- responses ---------------------------------------------------------------------------

/// Stable error codes of the app layer (distinct from the relay's [`crate::codes::RefusalCode`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BadRequest,
    UnknownOp,
    TooLarge,
    /// The device's level is too low for this op.
    Forbidden,
    UnknownSession,
    UnknownTask,
    /// The folder is not on the allow list.
    FolderNotAllowed,
    UnknownProfile,
    /// The command is not on the PC's list (or `/remote-control` is not configured).
    CommandNotAllowed,
    /// The session is busy: repeat with `force` after the user confirms.
    Busy,
    /// CSM refused or failed the underlying tool.
    Failed,
    /// CSM is not reachable right now.
    Unavailable,
    RateLimited,
}

impl ErrorCode {
    pub const ALL: &'static [ErrorCode] = &[
        Self::BadRequest,
        Self::UnknownOp,
        Self::TooLarge,
        Self::Forbidden,
        Self::UnknownSession,
        Self::UnknownTask,
        Self::FolderNotAllowed,
        Self::UnknownProfile,
        Self::CommandNotAllowed,
        Self::Busy,
        Self::Failed,
        Self::Unavailable,
        Self::RateLimited,
    ];
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub code: ErrorCode,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub id: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorBody>,
}

impl Response {
    pub fn ok(id: u64, data: Value) -> Self {
        Self { id, ok: true, data: Some(data), error: None }
    }
    pub fn err(id: u64, code: ErrorCode, message: &str) -> Self {
        Self { id, ok: false, data: None, error: Some(ErrorBody { code, message: message.to_owned() }) }
    }
}

// ---- data types --------------------------------------------------------------------------

/// What the phone shows for a session. `needs_*` are the "needs you" states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneStatus {
    Working,
    NeedsApproval,
    NeedsQuestion,
    Idle,
    Done,
    Asleep,
    Exited,
}

impl PhoneStatus {
    pub const fn needs_you(self) -> bool {
        matches!(self, Self::NeedsApproval | Self::NeedsQuestion)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: String,
    pub name: String,
    pub folder: String,
    pub status: PhoneStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    /// The program's own status text (for example what Claude is asking).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Unix seconds of the last status change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    pub ts: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskItem {
    pub id: String,
    pub title: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderInfo {
    pub path: String,
    pub name: String,
    pub pinned: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileInfo {
    pub name: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandInfo {
    pub command: String,
    pub label: String,
}

/// The reply to [`Op::Hello`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloData {
    pub protocol: u8,
    pub level: Level,
    pub host_name: String,
    pub hide_names: bool,
    pub notify: bool,
    /// The host can send pushes (the relay has a VAPID key and this device subscribed).
    pub push_ready: bool,
    /// `/remote-control` is on the PC's command list: the phone enables "Turn on Remote Control".
    pub remote_control: bool,
    pub max_message_bytes: u32,
}

// ---- events ------------------------------------------------------------------------------

/// Host to phone, unprompted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ev", rename_all = "snake_case")]
pub enum Event {
    /// Full state; split into `parts` messages (`part` counts from 1) when it would not fit.
    /// A new `seq` replaces everything the phone knows.
    Snapshot {
        seq: u64,
        part: u32,
        parts: u32,
        sessions: Vec<SessionInfo>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unread: Option<u32>,
    },
    /// Changes since `seq - 1`. A gap in `seq` means: ask for a snapshot. A change too large for one
    /// message is split into several deltas with consecutive seqs, each one a step on its own: the
    /// first says "changes since seq - 1", the next "changes since the one before it", and so on.
    Delta { seq: u64, upserts: Vec<SessionInfo>, removed: Vec<String> },
    /// The inbox changed. It shares the channel's seq counter (a missed message is a gap).
    Inbox { seq: u64, unread: u32 },
    /// The host is about to close the channel.
    Bye { reason: ByeReason },
    /// The result of `session.new` with `remote_control`: the answer said `pending`, and this comes when
    /// the wait ends. `ok` with no `code` is success; a failure carries `code` and `message`. The
    /// session itself stays either way. Not sent if the channel has closed (the phone reads the setting
    /// on its next `session.get`).
    RemoteControl {
        session: String,
        ok: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<ErrorCode>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeReason {
    Revoked,
    LevelChanged,
    Shutdown,
}

/// A message from the host to the phone, told apart by its key (`ok` or `ev`).
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMsg {
    Response(Response),
    Event(Event),
}

impl ServerMsg {
    pub fn parse(bytes: &[u8]) -> Option<ServerMsg> {
        if bytes.len() > MAX_APP_MESSAGE_BYTES {
            return None;
        }
        let v: Value = serde_json::from_slice(bytes).ok()?;
        if v.get("ev").is_some() {
            serde_json::from_value(v).ok().map(ServerMsg::Event)
        } else if v.get("ok").is_some() {
            serde_json::from_value(v).ok().map(ServerMsg::Response)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TooLarge;

/// Serialises a message, refusing anything over the app message cap.
pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, TooLarge> {
    let v = serde_json::to_vec(msg).map_err(|_| TooLarge)?;
    if v.len() > MAX_APP_MESSAGE_BYTES { Err(TooLarge) } else { Ok(v) }
}

/// Splits a snapshot into events that each encode to at most `max_bytes` (a single session larger
/// than that still gets its own event; [`encode`] then refuses it). Always at least one event.
pub fn split_snapshot(seq: u64, sessions: &[SessionInfo], unread: Option<u32>, max_bytes: usize) -> Vec<Event> {
    let size = |s: &SessionInfo| serde_json::to_vec(s).map_or(0, |v| v.len() + 1);
    // Room for the fixed part of the event (tag, seq, part counters, unread), generously.
    let budget = max_bytes.saturating_sub(128);
    let mut chunks: Vec<Vec<SessionInfo>> = vec![Vec::new()];
    let mut used = 0usize;
    for s in sessions {
        let n = size(s);
        if used > 0 && used + n > budget {
            chunks.push(Vec::new());
            used = 0;
        }
        if let Some(last) = chunks.last_mut() {
            last.push(s.clone());
        }
        used += n;
    }
    let parts = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, sessions)| Event::Snapshot { seq, part: u32::try_from(i + 1).unwrap_or(u32::MAX), parts, sessions, unread })
        .collect()
}

/// The decrypted body of a Web Push (the add-on encrypts it with RFC 8291; the service worker
/// shows it). `s`/`sid` are left out when the device chose "hide names".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushPayload {
    pub t: PushKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    /// How many sessions need you.
    pub n: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PushKind {
    NeedsYou,
    Test,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str) -> SessionInfo {
        SessionInfo {
            id: id.into(),
            name: format!("name {id}"),
            folder: "C:/code/x".into(),
            status: PhoneStatus::Idle,
            branch: None,
            message: None,
            group: None,
            profile: None,
            since: None,
        }
    }

    #[test]
    fn request_parses_with_and_without_args() {
        let r = Request::parse(br#"{"id":7,"op":"session.wake","args":{"session_id":"s1"}}"#).unwrap();
        assert_eq!(r, Request { id: 7, op: Op::SessionWake { session_id: "s1".into() } });
        assert_eq!(Request::parse(br#"{"id":1,"op":"ping"}"#).unwrap().op, Op::Ping);
        assert_eq!(Request::parse(br#"{"id":1,"op":"ping","args":{}}"#).unwrap().op, Op::Ping);
        let c = Request::parse(br#"{"id":2,"op":"session.close","args":{"session_id":"a"}}"#).unwrap();
        assert_eq!(c.op, Op::SessionClose { session_id: "a".into(), force: false });
    }

    #[test]
    fn request_errors_carry_the_id_when_known() {
        let e = |t: &str| Request::parse(t.as_bytes()).unwrap_err();
        assert_eq!(e("{"), RequestError { id: None, code: ErrorCode::BadRequest });
        assert_eq!(e(r#"{"op":"ping"}"#), RequestError { id: None, code: ErrorCode::BadRequest });
        assert_eq!(e(r#"{"id":3,"op":"term.write"}"#), RequestError { id: Some(3), code: ErrorCode::UnknownOp });
        assert_eq!(e(r#"{"id":4,"op":"session.wake","args":{}}"#), RequestError { id: Some(4), code: ErrorCode::BadRequest });
        assert_eq!(e(r#"{"id":5,"op":"session.wake","args":{"session_id":""}}"#).code, ErrorCode::BadRequest);
        let big = format!(r#"{{"id":6,"op":"hello","args":{{"client":"{}","protocol":1}}}}"#, "x".repeat(MAX_APP_MESSAGE_BYTES));
        assert_eq!(e(&big).code, ErrorCode::TooLarge);
    }

    #[test]
    fn session_new_has_no_command_or_args_and_unknown_fields_are_ignored() {
        let r = Request::parse(br#"{"id":1,"op":"session.new","args":{"folder":"C:/a","profile":"claude","command":"calc.exe","args":["x"]}}"#).unwrap();
        assert_eq!(r.op, Op::SessionNew { folder: "C:/a".into(), profile: "claude".into(), name: None, remote_control: false });
        let json = serde_json::to_string(&r).unwrap();
        assert!(!json.contains("calc"));
    }

    #[test]
    fn commands_must_be_one_slash_word() {
        let cmd = |c: &str| Op::SessionCommand { session_id: "s".into(), command: c.into(), force: false }.validate();
        assert!(cmd("/remote-control").is_ok());
        assert!(cmd("remote-control").is_err());
        assert!(cmd("/a b").is_err());
        assert!(cmd("/a\nb").is_err());
    }

    #[test]
    fn push_endpoint_must_be_https() {
        let p = |e: &str| Op::PushSubscribe { endpoint: e.into(), p256dh: "k".into(), auth: "a".into() }.validate();
        assert!(p("https://fcm.googleapis.com/x").is_ok());
        assert!(p("http://fcm.googleapis.com/x").is_err());
    }

    #[test]
    fn levels() {
        assert!(Level::Operator.allows(Level::Viewer));
        assert!(!Level::Viewer.allows(Level::Operator));
        let wake = Op::SessionWake { session_id: "a".into() };
        assert_eq!(wake.required_level(), Level::Operator);
        assert_eq!(Op::SessionList.required_level(), Level::Viewer);
        assert_eq!(Op::SettingsSet { hide_names: true, notify: true }.required_level(), Level::Viewer);
        // Every op name in NAMES maps to a variant whose name() is itself.
        assert_eq!(Op::NAMES.len(), 21);
    }

    #[test]
    fn responses_and_events_classify() {
        let ok = encode(&Response::ok(1, serde_json::json!({"a":1}))).unwrap();
        assert!(matches!(ServerMsg::parse(&ok), Some(ServerMsg::Response(r)) if r.ok));
        let ev = encode(&Event::Inbox { seq: 3, unread: 2 }).unwrap();
        assert_eq!(ServerMsg::parse(&ev), Some(ServerMsg::Event(Event::Inbox { seq: 3, unread: 2 })));
        assert_eq!(ServerMsg::parse(b"{}"), None);
        assert_eq!(ServerMsg::parse(b"nope"), None);
    }

    #[test]
    fn snapshot_splits_to_fit_and_keeps_order() {
        let sessions: Vec<_> = (0..200).map(|i| s(&format!("session-{i:04}"))).collect();
        let events = split_snapshot(9, &sessions, Some(1), 4096);
        assert!(events.len() > 1);
        let mut seen = Vec::new();
        for (i, e) in events.iter().enumerate() {
            assert!(serde_json::to_vec(e).unwrap().len() <= 4096);
            let Event::Snapshot { seq, part, parts, sessions, .. } = e else { panic!("not a snapshot") };
            assert_eq!((*seq, *part as usize, *parts as usize), (9, i + 1, events.len()));
            seen.extend(sessions.iter().map(|s| s.id.clone()));
        }
        assert_eq!(seen, sessions.iter().map(|s| s.id.clone()).collect::<Vec<_>>());
        // Empty state is still one event.
        assert_eq!(split_snapshot(1, &[], None, 4096).len(), 1);
    }

    #[test]
    fn encode_refuses_oversize() {
        let big = Response::ok(1, Value::String("x".repeat(MAX_APP_MESSAGE_BYTES)));
        assert_eq!(encode(&big), Err(TooLarge));
    }

    #[test]
    fn needs_you_states() {
        assert!(PhoneStatus::NeedsApproval.needs_you());
        assert!(PhoneStatus::NeedsQuestion.needs_you());
        assert!(!PhoneStatus::Idle.needs_you());
    }
}
