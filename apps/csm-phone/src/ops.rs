//! Phone ops, mapped onto CSM tools (design note, section 4). One function does one thing for each
//! op: the level check, then the add-on's own policy, then at most the one CSM tool the design
//! names. Shape checks (`Op::validate`) already ran when the request was parsed.
//!
//! The phone never names a tool, never passes parameters through, and never sees CSM's JSON: a
//! CSM answer is read field by field and anything unexpected is dropped, not passed on.

use std::collections::{BTreeMap, BTreeSet};

use csm_remote_proto::msg::{
    CommandInfo, ErrorCode, FolderInfo, HelloData, InboxItem, Level, Op, PhoneStatus, ProfileInfo, Request, Response, SessionInfo, TaskItem,
};
use serde_json::{Value, json};

use crate::config::Config;
use crate::csm::{Csm, CsmError};
use crate::policy::{InFlight, LaunchBucket, clean_name, cut, live_phone_sessions};
use crate::store::{Device, PushSub};

/// Fields of a session, task or inbox entry read from CSM, cut to these lengths.
const MAX_TEXT: usize = 256;
const MAX_ID: usize = 64;

/// What one request changed in the device list, so the caller saves it.
pub struct Outcome {
    pub response: Response,
    pub devices_changed: bool,
}

/// Mutable state the ops keep between requests: the launch buckets, the sessions phones created
/// and the in-flight cap. It belongs to the host, not to a channel.
pub struct Engine {
    pub csm: Box<dyn Csm>,
    pub inflight: InFlight,
    pub buckets: BTreeMap<String, LaunchBucket>,
    pub created: BTreeSet<String>,
}

impl Engine {
    pub fn new(csm: Box<dyn Csm>) -> Self {
        Self { csm, inflight: InFlight::new(crate::policy::MAX_IN_FLIGHT), buckets: BTreeMap::new(), created: BTreeSet::new() }
    }

    /// Runs one parsed request for `device` at time `now` (Unix seconds). Never panics: each
    /// refusal is an answer with an error code.
    pub fn handle(&mut self, cfg: &Config, devices: &mut [Device], device_id: &str, req: &Request, now: u64) -> Outcome {
        let id = req.id;
        let Some(idx) = devices.iter().position(|d| d.device_id == device_id) else {
            return Outcome { response: Response::err(id, ErrorCode::Forbidden, "this phone is no longer paired"), devices_changed: false };
        };
        let level = devices.get(idx).map_or(Level::Viewer, |d| d.level);
        if !level.allows(req.op.required_level()) {
            return Outcome { response: Response::err(id, ErrorCode::Forbidden, "this phone's level does not allow that"), devices_changed: false };
        }
        let mut changed = false;
        let response = match self.run(cfg, devices, idx, &req.op, now, &mut changed) {
            Ok(data) => Response::ok(id, data),
            Err((code, msg)) => Response::err(id, code, &msg),
        };
        if let Some(d) = devices.get_mut(idx) {
            d.last_seen = now;
        }
        Outcome { response, devices_changed: changed }
    }

    fn run(&mut self, cfg: &Config, devices: &mut [Device], idx: usize, op: &Op, now: u64, changed: &mut bool) -> Result<Value, (ErrorCode, String)> {
        match op {
            Op::Hello { .. } => {
                let pc = self.pc_commands()?;
                let d = devices.get(idx).cloned().ok_or((ErrorCode::Forbidden, String::new()))?;
                let data = HelloData {
                    protocol: csm_remote_proto::PROTOCOL_VERSION,
                    level: d.level,
                    host_name: if cfg.host_name.is_empty() { os_host_name() } else { cfg.host_name.clone() },
                    hide_names: d.hide_names,
                    notify: d.notify,
                    push_ready: cfg.push && d.push.is_some(),
                    remote_control: pc.iter().any(|c| c == "/remote-control"),
                    max_message_bytes: u32::try_from(csm_remote_proto::limits::MAX_APP_MESSAGE_BYTES).unwrap_or(u32::MAX),
                };
                to_value(&data)
            }
            Op::Ping => Ok(json!({})),
            Op::Snapshot | Op::SessionList => {
                let sessions = self.session_list()?;
                Ok(json!({ "sessions": sessions }))
            }
            Op::SessionGet { session_id } => {
                // CSM answers `{ session, config, program_status, busy, cwd }`; the view is `session`
                // and its program message is `status_message`. Nothing else is read.
                let v = self.csm_call("session_get", json!({ "session_id": session_id }))?;
                let view = v.get("session").filter(|s| s.is_object()).ok_or((ErrorCode::Failed, "CSM sent no session".to_owned()))?;
                let mut s = session_from(view).ok_or((ErrorCode::Failed, "CSM sent no session".to_owned()))?;
                s.message = text(view, "status_message", MAX_TEXT);
                to_value(&s)
            }
            Op::InboxList => {
                let v = self.csm_call("inbox_list", json!({}))?;
                let items: Vec<InboxItem> = list_of(&v).iter().filter_map(inbox_from).collect();
                Ok(json!({ "items": items }))
            }
            Op::TaskList => {
                let v = self.csm_call("task_list", json!({ "include_closed": false }))?;
                let items: Vec<TaskItem> = list_of(&v).iter().filter_map(task_from).collect();
                Ok(json!({ "tasks": items }))
            }
            Op::TaskGet { task_id } => {
                let v = self.csm_call("task_get", json!({ "task_id": task_id }))?;
                let t = task_from(&v).ok_or((ErrorCode::UnknownTask, "no such task".to_owned()))?;
                to_value(&t)
            }
            Op::FolderList => {
                let folders: Vec<FolderInfo> = cfg
                    .folders
                    .iter()
                    .map(|f| FolderInfo { path: cut(&f.path, csm_remote_proto::msg::MAX_FOLDER_CHARS), name: folder_name(&f.path), pinned: f.pinned })
                    .collect();
                Ok(json!({ "folders": folders }))
            }
            Op::ProfileList => {
                let profiles: Vec<ProfileInfo> = cfg.profiles.iter().map(|p| ProfileInfo { name: clean_name(&p.name, MAX_ID), label: clean_name(&p.label, MAX_TEXT) }).collect();
                Ok(json!({ "profiles": profiles }))
            }
            Op::CommandList => {
                let pc = self.pc_commands()?;
                let commands: Vec<CommandInfo> = cfg.allowed_commands(&pc).into_iter().map(|c| CommandInfo { label: c.clone(), command: c }).collect();
                Ok(json!({ "commands": commands }))
            }
            Op::SessionWake { session_id } => {
                self.launch(devices, idx, now)?;
                self.csm_call("session_wake", json!({ "session_id": session_id }))
            }
            Op::SessionSleep { session_id } => self.csm_call("session_sleep", json!({ "session_id": session_id })),
            Op::SessionRestart { session_id } => {
                self.launch(devices, idx, now)?;
                self.csm_call("session_restart", json!({ "session_id": session_id }))
            }
            Op::SessionClose { session_id, force } => self.csm_call("session_close", json!({ "session_id": session_id, "force": force })),
            Op::SessionNew { folder, profile, name, remote_control } => {
                let f = cfg.folder_for(folder).ok_or((ErrorCode::FolderNotAllowed, "that folder is not on the allow list".to_owned()))?;
                let p = cfg.profiles.iter().find(|p| p.name == *profile).ok_or((ErrorCode::UnknownProfile, "no such profile".to_owned()))?;
                self.launch(devices, idx, now)?;
                let listed = self.session_list()?;
                let mut statuses = BTreeMap::new();
                for s in &listed {
                    statuses.insert(s.id.clone(), s.status);
                }
                if live_phone_sessions(&self.created, &statuses) >= usize::try_from(cfg.max_phone_sessions).unwrap_or(usize::MAX) {
                    return Err((ErrorCode::RateLimited, "the phone session limit is reached".to_owned()));
                }
                let mut params = json!({ "folder": f.spelling, "command": p.command, "kind": "local" });
                if let Some(n) = name {
                    params["name"] = json!(clean_name(n, MAX_TEXT));
                }
                let v = self.csm_call("session_new", params)?;
                let id = v.get("id").and_then(Value::as_str).map(|s| cut(s, MAX_ID)).ok_or((ErrorCode::Failed, "CSM did not return a session".to_owned()))?;
                self.created.insert(id.clone());
                // With Remote Control asked for, the host keeps a wait for the session (host.rs, `finish_wait`)
                // and sends `/remote-control` once it is idle; the answer here does not wait for that.
                Ok(json!({ "id": id, "remote_control": if *remote_control { "pending" } else { "off" } }))
            }
            Op::SessionCommand { session_id, command, force } => {
                let pc = self.pc_commands()?;
                if !cfg.allowed_commands(&pc).iter().any(|c| c == command) {
                    return Err((ErrorCode::CommandNotAllowed, "that command is not on the list".to_owned()));
                }
                self.csm_call("session_send_command", json!({ "session_id": session_id, "command": command, "force": force }))
            }
            Op::SessionStatusClear { session_id } => self.csm_call("session_status_clear", json!({ "session_id": session_id })),
            Op::PushSubscribe { endpoint, p256dh, auth } => {
                if !push_host_allowed(&cfg.push_hosts, endpoint) {
                    return Err((ErrorCode::BadRequest, "that push service is not allowed".to_owned()));
                }
                if let Some(d) = devices.get_mut(idx) {
                    d.push = Some(PushSub { endpoint: endpoint.clone(), p256dh: p256dh.clone(), auth: auth.clone() });
                }
                *changed = true;
                Ok(json!({}))
            }
            Op::PushUnsubscribe => {
                if let Some(d) = devices.get_mut(idx) {
                    d.push = None;
                }
                *changed = true;
                Ok(json!({}))
            }
            Op::SettingsSet { hide_names, notify } => {
                if let Some(d) = devices.get_mut(idx) {
                    d.hide_names = *hide_names;
                    d.notify = *notify;
                }
                *changed = true;
                Ok(json!({}))
            }
        }
    }

    /// One CSM call, under the in-flight cap. Beyond the cap the request is refused, not queued.
    fn csm_call(&mut self, tool: &str, params: Value) -> Result<Value, (ErrorCode, String)> {
        let _permit = self.inflight.try_take().ok_or((ErrorCode::RateLimited, "too many requests are in progress".to_owned()))?;
        self.csm.call(tool, params).map_err(|e| match e {
            CsmError::UnknownSession => (ErrorCode::UnknownSession, "no such session".to_owned()),
            CsmError::Busy => (ErrorCode::Busy, "the session is busy".to_owned()),
            CsmError::CommandRefused => (ErrorCode::CommandNotAllowed, "that command is refused".to_owned()),
            CsmError::Failed(m) => (ErrorCode::Failed, m),
            CsmError::Unavailable => (ErrorCode::Unavailable, "CSM is not answering".to_owned()),
        })
    }

    /// The launch bucket of a device: `session.new`, `session.wake`, `session.restart` together.
    fn launch(&mut self, devices: &[Device], idx: usize, now: u64) -> Result<(), (ErrorCode, String)> {
        let id = devices.get(idx).map(|d| d.device_id.clone()).unwrap_or_default();
        let bucket = self.buckets.entry(id).or_default();
        if bucket.admit(now) { Ok(()) } else { Err((ErrorCode::RateLimited, "too many launches; wait a minute".to_owned())) }
    }

    fn session_list(&mut self) -> Result<Vec<SessionInfo>, (ErrorCode, String)> {
        let v = self.csm_call("session_list", json!({}))?;
        Ok(list_of(&v).iter().filter_map(session_from).collect())
    }

    /// One read of CSM's session list for the host-wide tracker (section 7.1). Only the reader
    /// the phone ops use: the same field names and cleaning, no device involved.
    pub fn track(&mut self) -> Result<Vec<SessionInfo>, String> {
        self.session_list().map_err(|(_, message)| message)
    }

    /// The PC's slash commands from `settings_get ClaudeCommands`. Any other shape is an empty list.
    fn pc_commands(&mut self) -> Result<Vec<String>, (ErrorCode, String)> {
        let v = self.csm_call("settings_get", json!({ "key": "ClaudeCommands" }))?;
        let items = match v.get("value").unwrap_or(&v) {
            Value::Array(a) => a.clone(),
            _ => Vec::new(),
        };
        Ok(items
            .iter()
            .filter_map(|i| match i {
                Value::String(s) => Some(s.clone()),
                Value::Object(o) => o.get("command").and_then(Value::as_str).map(str::to_owned),
                _ => None,
            })
            .filter(|s| s.starts_with('/') && s.len() <= 64)
            .collect())
    }
}

/// The PC's name for a phone's hello when the person has not set one: the OS host name (COMPUTERNAME on
/// Windows, HOSTNAME elsewhere), read once and cleaned like any name, or "PC" when neither is usable.
fn os_host_name() -> String {
    static NAME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    NAME.get_or_init(|| {
        let raw = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
        let cleaned = clean_name(&raw, MAX_TEXT);
        if cleaned.is_empty() { "PC".to_owned() } else { cleaned }
    })
    .clone()
}

/// Push endpoints must be https and on the host list, an exact name or a `*.suffix`. The host is read by
/// the relay's rule (`csm_remote_proto::endpoint`), so the add-on refuses what the relay would refuse.
pub fn push_host_allowed(hosts: &[String], endpoint: &str) -> bool {
    let Some(host) = csm_remote_proto::endpoint::push_endpoint_host(endpoint) else { return false };
    hosts.iter().any(|h| {
        let h = h.to_ascii_lowercase();
        match h.strip_prefix("*.") {
            Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
            None => host == h,
        }
    })
}

fn folder_name(path: &str) -> String {
    let last = path.rsplit(['/', '\\']).find(|s| !s.is_empty()).unwrap_or(path);
    clean_name(last, MAX_TEXT)
}

fn list_of(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o.get("sessions").or_else(|| o.get("items")).or_else(|| o.get("tasks")).and_then(Value::as_array).cloned().unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn text(v: &Value, key: &str, max: usize) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(|s| clean_name(s, max)).filter(|s| !s.is_empty())
}

/// CSM's session status words, mapped onto the phone's (decision 8: `error` shows as idle with its
/// message). Unknown words are idle.
pub fn phone_status(word: &str) -> PhoneStatus {
    match word {
        "working" | "starting" => PhoneStatus::Working,
        "needs_you_approval" => PhoneStatus::NeedsApproval,
        "needs_you_question" => PhoneStatus::NeedsQuestion,
        "done_unseen" => PhoneStatus::Done,
        "asleep" => PhoneStatus::Asleep,
        "exited" => PhoneStatus::Exited,
        _ => PhoneStatus::Idle,
    }
}

fn session_from(v: &Value) -> Option<SessionInfo> {
    let id = text(v, "id", MAX_ID)?;
    let status = phone_status(v.get("status").and_then(Value::as_str).unwrap_or(""));
    Some(SessionInfo {
        id,
        name: text(v, "name", MAX_TEXT).unwrap_or_else(|| "Session".to_owned()),
        folder: text(v, "folder", csm_remote_proto::msg::MAX_FOLDER_CHARS).unwrap_or_default(),
        status,
        branch: text(v, "branch", MAX_TEXT),
        message: None,
        // CSM's session view names its group `group_id` (empty when ungrouped; `text` drops that).
        group: text(v, "group_id", MAX_TEXT),
        profile: None,
        since: None,
    })
}

/// An inbox entry is a session view (CSM's `inbox_list` lists the sessions that need the person,
/// then the unseen results). CSM sends no timestamp for it, so `ts` is 0.
fn inbox_from(v: &Value) -> Option<InboxItem> {
    let id = text(v, "id", MAX_ID)?;
    Some(InboxItem { id: id.clone(), title: text(v, "name", MAX_TEXT).unwrap_or_default(), session_id: Some(id), ts: 0 })
}

/// A task (CSM's `task_list` and `task_get`). Its `assignee` is the session it is given to, when
/// one is. The body is never copied (design note, section 4: titles and state only).
fn task_from(v: &Value) -> Option<TaskItem> {
    Some(TaskItem {
        id: text(v, "id", MAX_ID)?,
        title: text(v, "title", MAX_TEXT).unwrap_or_default(),
        state: text(v, "state", MAX_ID).unwrap_or_default(),
        session_id: text(v, "assignee", MAX_ID),
    })
}

fn to_value<T: serde::Serialize>(t: &T) -> Result<Value, (ErrorCode, String)> {
    serde_json::to_value(t).map_err(|_| (ErrorCode::Failed, "could not encode the answer".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_hosts_are_exact_or_wildcard_and_https_only() {
        let hosts = vec!["fcm.googleapis.com".to_owned(), "*.notify.windows.com".to_owned()];
        assert!(push_host_allowed(&hosts, "https://fcm.googleapis.com/fcm/send/abc"));
        assert!(push_host_allowed(&hosts, "https://x.notify.windows.com/w/?token=1"));
        assert!(!push_host_allowed(&hosts, "https://evil.example/fcm.googleapis.com"));
        assert!(!push_host_allowed(&hosts, "https://fcm.googleapis.com.evil.example/x"));
        assert!(!push_host_allowed(&hosts, "http://fcm.googleapis.com/x"));
        assert!(!push_host_allowed(&hosts, "https://notify.windows.com/x"), "the wildcard does not match its own suffix");
        assert!(!push_host_allowed(&hosts, "https://evil.example\\@x.notify.windows.com/"), "a backslash before the @ is not a host");
        assert!(!push_host_allowed(&hosts, "https://fcm.googleapis.com@evil.example/x"), "userinfo is not a host");
        assert!(!push_host_allowed(&hosts, "https://fcm.googleapis.com:8443/x"), "only port 443");
        assert!(push_host_allowed(&hosts, "https://fcm.googleapis.com:443/x"), "port 443 is the default");
    }

    #[test]
    fn the_os_host_name_is_cleaned_and_never_empty() {
        let name = os_host_name();
        assert!(!name.is_empty(), "the fallback is PC");
        assert_eq!(name, clean_name(&name, MAX_TEXT), "already cleaned");
        assert_eq!(os_host_name(), name, "read once");
    }

    #[test]
    fn statuses_map_and_unknown_words_are_idle() {
        assert_eq!(phone_status("needs_you_approval"), PhoneStatus::NeedsApproval);
        assert_eq!(phone_status("error"), PhoneStatus::Idle);
        assert_eq!(phone_status("nonsense"), PhoneStatus::Idle);
    }

    #[test]
    fn hostile_session_names_are_cleaned_and_bad_entries_dropped() {
        let v = json!({"id": "s1", "name": "a\u{202E}b\u{0}", "folder": "/x", "status": "working"});
        let s = session_from(&v).unwrap();
        assert_eq!(s.name, "ab");
        assert!(session_from(&json!({"name": "no id"})).is_none());
    }

    /// The shapes below are CSM's real output (`crates/csm-engine/src/snapshot.rs`, `tools/sessions.rs`,
    /// `tools/tasks.rs`), copied field for field. A reader that drifts from them answers with nothing.
    #[test]
    fn a_csm_session_view_is_read_by_its_real_field_names() {
        let view = json!({
            "id": "s1", "name": "Api", "kind": "local", "folder": "/work/api", "folder_short": "api",
            "command": "claude", "group_id": "g1", "accent": "#ff0000", "status": "needs_you_approval",
            "status_label": "needs you", "status_message": "Approve the edit?", "live": true, "asleep": false,
            "branch": "main", "git_dirty": false
        });
        let s = session_from(&view).unwrap();
        assert_eq!((s.id.as_str(), s.name.as_str(), s.folder.as_str()), ("s1", "Api", "/work/api"));
        assert_eq!(s.status, PhoneStatus::NeedsApproval);
        assert_eq!(s.group.as_deref(), Some("g1"), "the group is group_id");
        assert_eq!(s.branch.as_deref(), Some("main"));
        assert_eq!(s.message, None, "the list carries no program text: session.get does");
    }

    #[test]
    fn an_inbox_entry_is_a_session_view_and_its_name_is_the_title() {
        let view = json!({"id": "s2", "name": "Build", "status": "done_unseen", "folder": "/w"});
        let item = inbox_from(&view).unwrap();
        assert_eq!((item.id.as_str(), item.title.as_str()), ("s2", "Build"));
        assert_eq!(item.session_id.as_deref(), Some("s2"));
        assert_eq!(item.ts, 0, "CSM sends no time for an inbox entry");
    }

    #[test]
    fn a_task_is_read_without_its_body_and_its_assignee_is_the_session() {
        let task = json!({"id": "t1", "project": "p", "title": "Fix it", "body": "secret body", "state": "assigned", "assignee": "s9", "progress": null});
        let t = task_from(&task).unwrap();
        assert_eq!((t.id.as_str(), t.title.as_str(), t.state.as_str()), ("t1", "Fix it", "assigned"));
        assert_eq!(t.session_id.as_deref(), Some("s9"));
        assert!(!serde_json::to_string(&t).unwrap().contains("secret body"), "the body never reaches a phone");
    }
}
