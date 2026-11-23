//! The stdio shell: newline-delimited JSON-RPC 2.0 with CSM (docs/addons.md, "Process add-ons").
//!
//! Threads:
//! - a reader thread takes every line from stdin: requests and notifications go to the main
//!   thread's queue, answers to our own `csm/call`s go to the caller waiting for them;
//! - the main thread answers `initialize` and `tools/call`, and never calls CSM itself;
//! - the host thread owns the [`Host`] (pairing, devices, the relay connection). It makes the CSM
//!   calls its ops need (through [`StdioCsm`]) and receives the person's commands over a channel.
//!
//! The person-only tools (pairing, decisions, devices) refuse every caller whose `kind` is not `ui`
//! or `cli`, so an agent or another add-on cannot pair a phone or revoke one.

use std::collections::HashMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use csm_remote_proto::msg::Level;
use serde_json::{Value, json};

use crate::config::{self, Config};
use crate::csm::{Csm, CsmError, classify};
use crate::host::{Host, Out};
use crate::policy::cut;
use crate::relay_client::{self, Command};
use crate::store;

/// The add-on protocol version this program speaks (`PROTOCOL_API` in CSM).
pub const API: u64 = 1;
pub const ADD_ON_NAME: &str = "Phone remote";
/// A CSM call is answered within this time or it counts as unavailable (CSM itself gives 30 s).
const CALL_TIMEOUT: Duration = Duration::from_secs(35);
/// How long a tool waits for the host thread to answer a command.
const ASK_TIMEOUT: Duration = Duration::from_secs(5);
/// CSM never sends a message over 1 MiB; a longer line is dropped.
const MAX_LINE_BYTES: usize = 1024 * 1024;

pub const TOOL_STATUS: &str = "phone-remote.status";
pub const TOOL_PAIR_START: &str = "phone-remote.pair_start";
pub const TOOL_PAIR_STOP: &str = "phone-remote.pair_stop";
pub const TOOL_PAIR_DECIDE: &str = "phone-remote.pair_decide";
pub const TOOL_DEVICE_LIST: &str = "phone-remote.device_list";
pub const TOOL_DEVICE_REVOKE: &str = "phone-remote.device_revoke";
pub const TOOL_DEVICE_LEVEL: &str = "phone-remote.device_level";

/// One tool as declared in `initialize`. `effects` must each be one of the `[process] effects` in
/// `samples/addons/phone-remote/addon.toml` (a test checks it).
pub struct ToolSpec {
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub schema: Value,
    pub effects: Vec<&'static str>,
}

fn level_schema() -> Value {
    json!({"type": "string", "enum": ["viewer", "operator"]})
}

/// The tools this add-on declares.
pub fn tool_specs() -> Vec<ToolSpec> {
    let spec = |id, title, summary, schema, effects| ToolSpec { id, title, summary, schema, effects };
    vec![
        spec(TOOL_STATUS, "Status", "Whether the phone remote runs, its relay, the paired phones and any phone waiting for an answer.", json!({"type": "object"}), vec![]),
        spec(TOOL_PAIR_START, "Pair a phone", "Open a pairing: returns the link for the phone (the pairing screen shows it as a QR code). Person only.", json!({"type": "object"}), vec!["accesses_network"]),
        spec(TOOL_PAIR_STOP, "Stop pairing", "Close the pairing; a waiting phone is denied. Person only.", json!({"type": "object"}), vec!["accesses_network"]),
        spec(
            TOOL_PAIR_DECIDE,
            "Allow or deny",
            "Allow the waiting phone at a level, or deny it. Person only.",
            json!({"type": "object", "properties": {"allow": {"type": "boolean"}, "level": level_schema()}, "required": ["allow"]}),
            vec!["accesses_network"],
        ),
        spec(TOOL_DEVICE_LIST, "Paired phones", "The paired phones: name, level and times. Person only.", json!({"type": "object"}), vec![]),
        spec(
            TOOL_DEVICE_REVOKE,
            "Revoke a phone",
            "Remove a paired phone; its open channel is closed at once. Person only.",
            json!({"type": "object", "properties": {"device_id": {"type": "string"}}, "required": ["device_id"]}),
            vec!["stores_data"],
        ),
        spec(
            TOOL_DEVICE_LEVEL,
            "Change a phone's level",
            "Set a paired phone to viewer or operator. Person only.",
            json!({"type": "object", "properties": {"device_id": {"type": "string"}, "level": level_schema()}, "required": ["device_id", "level"]}),
            vec!["stores_data"],
        ),
    ]
}

/// What the host thread shares with the main thread: the decision a phone is waiting for.
#[derive(Default)]
struct Shared {
    pending: Option<(String, String)>,
}

/// Everything written to stdout goes through here, one line at a time.
struct Io {
    out: Mutex<io::Stdout>,
    waiting: Mutex<HashMap<u64, Sender<Value>>>,
    next_id: AtomicU64,
}

impl Io {
    fn new() -> Self {
        Self { out: Mutex::new(io::stdout()), waiting: Mutex::new(HashMap::new()), next_id: AtomicU64::new(1_000_000) }
    }

    /// One message out. A failed write means CSM is gone; the caller just carries on.
    fn send(&self, v: &Value) -> bool {
        let Ok(mut out) = self.out.lock() else { return false };
        writeln!(out, "{v}").and_then(|()| out.flush()).is_ok()
    }

    fn reply(&self, id: &Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn reply_err(&self, id: &Value, code: i64, message: &str) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": cut(message, 400)}}));
    }

    fn log(&self, message: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": "csm/log", "params": {"message": cut(message, 500)}}));
    }
}

/// The add-on's view of CSM for the host: `csm/call` over stdout, the answer off the reader thread.
struct StdioCsm {
    io: Arc<Io>,
}

impl Csm for StdioCsm {
    fn call(&self, tool: &str, params: Value) -> Result<Value, CsmError> {
        let id = self.io.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        if let Ok(mut waiting) = self.io.waiting.lock() {
            waiting.insert(id, tx);
        }
        let sent = self.io.send(&json!({"jsonrpc": "2.0", "id": id, "method": "csm/call", "params": {"tool": tool, "params": params}}));
        let answer = if sent { rx.recv_timeout(CALL_TIMEOUT).ok() } else { None };
        if let Ok(mut waiting) = self.io.waiting.lock() {
            waiting.remove(&id);
        }
        let Some(msg) = answer else { return Err(CsmError::Unavailable) };
        if let Some(err) = msg.get("error") {
            let text = err.pointer("/data/message").or_else(|| err.get("message")).and_then(Value::as_str).unwrap_or_default();
            return Err(classify(text));
        }
        Ok(msg.get("result").and_then(|r| r.get("data")).cloned().unwrap_or(Value::Null))
    }
}

/// The reader thread: routes every line from CSM. Ends when stdin closes.
fn read_loop(io: &Io, inbox: &Sender<Value>) {
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.len() > MAX_LINE_BYTES {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if msg.get("method").is_some() {
            if inbox.send(msg).is_err() {
                break;
            }
        } else if let Some(id) = msg.get("id").and_then(Value::as_u64)
            && let Ok(mut waiting) = io.waiting.lock()
            && let Some(tx) = waiting.remove(&id)
        {
            let _ = tx.send(msg);
        }
    }
}

/// The running add-on: what the main thread needs to answer tools.
struct Session {
    /// Why the add-on cannot serve, in plain words (no data folder, a damaged store, ...).
    problem: Option<String>,
    enabled: bool,
    relay_set: bool,
    commands: Option<Sender<Command>>,
    shared: Arc<Mutex<Shared>>,
}

impl Session {
    fn idle(problem: Option<String>) -> Self {
        Self { problem, enabled: false, relay_set: false, commands: None, shared: Arc::new(Mutex::new(Shared::default())) }
    }
}

/// Opens the data folder, reads the settings and starts the host thread. Every failure is kept in
/// the session as a problem: nothing here panics, and nothing connects.
fn start(io: &Arc<Io>, dir: PathBuf) -> Session {
    let mut session = Session::idle(None);
    let cfg: Config = match fs::read_to_string(dir.join(store::CONFIG_FILE)) {
        Ok(text) => config::parse(&text),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Config::default(),
        Err(e) => {
            session.problem = Some(format!("config.json could not be read: {e}"));
            return session;
        }
    };
    for p in &cfg.problems {
        io.log(p);
    }
    let opened = match store::open(&dir) {
        Ok(o) => o,
        Err(e) => {
            session.problem = Some(e.to_string());
            return session;
        }
    };
    let secret = match store::host_secret(&dir, !opened.devices.is_empty()) {
        Ok(s) => s,
        Err(e) => {
            session.problem = Some(e.to_string());
            return session;
        }
    };
    session.relay_set = !cfg.relay.is_empty();
    session.enabled = cfg.connect && cfg.relay_allowed();
    let url = cfg.relay.clone();
    let enabled = session.enabled;
    let mut host = match Host::new(dir, opened, cfg, &secret, Box::new(StdioCsm { io: Arc::clone(io) })) {
        Ok(h) => h,
        Err(e) => {
            session.problem = Some(e);
            return session;
        }
    };
    let (tx, rx) = mpsc::channel::<Command>();
    session.commands = Some(tx);
    let host_io = Arc::clone(io);
    let shared = Arc::clone(&session.shared);
    let spawned = std::thread::Builder::new().name("phone-host".into()).spawn(move || {
        let stop = AtomicBool::new(false);
        let mut on_out = |o: &Out| on_host_out(&host_io, &shared, o);
        if enabled {
            relay_client::run(&mut host, &url, &secret, &stop, &rx, &mut on_out);
        } else {
            relay_client::serve_offline(&mut host, &stop, &rx, &mut on_out);
        }
    });
    if spawned.is_err() {
        session.commands = None;
        session.problem = Some("the host thread could not start".into());
    }
    session
}

/// What the host reports while it runs: problems go to the log, a waiting phone to the shared
/// state the status tool reads.
fn on_host_out(io: &Io, shared: &Mutex<Shared>, out: &Out) {
    match out {
        Out::Problem(p) => io.log(p),
        Out::Pending { check, name } => {
            if let Ok(mut s) = shared.lock() {
                s.pending = Some((check.clone(), name.clone()));
            }
        }
        Out::PairingEnded => {
            if let Ok(mut s) = shared.lock() {
                s.pending = None;
            }
        }
        Out::Frame(_) | Out::Control(_) | Out::PairingUrl(_) | Out::DevicesChanged => {}
    }
}

/// The tool result, or the refusal the caller sees.
type ToolResult = Result<(String, Value), (i64, String)>;

fn parse_level(v: Option<&Value>) -> Option<Level> {
    match v.and_then(Value::as_str)? {
        "viewer" => Some(Level::Viewer),
        "operator" => Some(Level::Operator),
        _ => None,
    }
}

/// Whether the caller is the person (the app's UI or `csm <tool>`). Everything else is refused.
fn is_person(params: &Value) -> bool {
    matches!(params.pointer("/caller/kind").and_then(Value::as_str), Some("ui" | "cli"))
}

fn not_running(session: &Session) -> (i64, String) {
    (-32000, session.problem.clone().unwrap_or_else(|| "the phone remote is not running".into()))
}

/// A command to the host thread that needs no answer.
fn send(session: &Session, command: Command) -> Result<(), (i64, String)> {
    let Some(commands) = session.commands.as_ref() else { return Err(not_running(session)) };
    commands.send(command).map_err(|_| (-32000, "the phone remote stopped".to_owned()))
}

/// A command to the host thread and its answer, waited for briefly.
fn ask<T>(session: &Session, make: impl FnOnce(Sender<T>) -> Command) -> Result<T, (i64, String)> {
    let Some(commands) = session.commands.as_ref() else { return Err(not_running(session)) };
    let (tx, rx) = mpsc::channel();
    commands.send(make(tx)).map_err(|_| (-32000, "the phone remote stopped".to_owned()))?;
    rx.recv_timeout(ASK_TIMEOUT).map_err(|_| (-32000, "the phone remote is busy; try again in a moment".to_owned()))
}

fn devices_count(session: &Session) -> Result<usize, (i64, String)> {
    let views = ask(session, Command::Devices)?;
    Ok(views.len())
}

fn tool(session: &Session, name: &str, params: &Value) -> ToolResult {
    let args = params.get("arguments").cloned().unwrap_or(Value::Null);
    if name == TOOL_STATUS {
        let pending = session.shared.lock().ok().and_then(|s| s.pending.clone());
        let pending = pending.map(|(check, who)| json!({"check": check, "name": who}));
        let devices = if session.commands.is_some() { devices_count(session)? } else { 0 };
        return Ok(("Phone remote status.".into(), json!({"enabled": session.enabled, "relay_set": session.relay_set, "devices": devices, "pending": pending, "problem": session.problem})));
    }
    if !is_person(params) {
        return Err((-32000, "only the person can do this (the app or `csm`), not an agent or another add-on".into()));
    }
    match name {
        TOOL_PAIR_START => {
            let version = env!("CARGO_PKG_VERSION").to_owned();
            let url = ask(session, |reply| Command::StartPairing { app_version: version, reply })?.map_err(|why| (-32000, why))?;
            Ok(("Pairing open. Scan the code with the phone.".into(), json!({"url": url})))
        }
        TOOL_PAIR_STOP => {
            send(session, Command::StopPairing)?;
            Ok(("Pairing stopped.".into(), json!({})))
        }
        TOOL_PAIR_DECIDE => {
            let allow = args.get("allow").and_then(Value::as_bool).ok_or((-32602, "allow must be true or false".to_owned()))?;
            let level = if allow {
                Some(parse_level(args.get("level")).ok_or((-32602, "level must be viewer or operator to allow a phone".to_owned()))?)
            } else {
                None
            };
            send(session, Command::Decide(level))?;
            Ok(("Answer sent.".into(), json!({})))
        }
        TOOL_DEVICE_LIST => {
            let views = ask(session, Command::Devices)?;
            Ok((format!("{} paired phone(s).", views.len()), json!({"devices": views})))
        }
        TOOL_DEVICE_REVOKE => {
            let id = args.get("device_id").and_then(Value::as_str).ok_or((-32602, "device_id is required".to_owned()))?;
            send(session, Command::Revoke(id.to_owned()))?;
            Ok(("Phone removed.".into(), json!({})))
        }
        TOOL_DEVICE_LEVEL => {
            let id = args.get("device_id").and_then(Value::as_str).ok_or((-32602, "device_id is required".to_owned()))?;
            let level = parse_level(args.get("level")).ok_or((-32602, "level must be viewer or operator".to_owned()))?;
            send(session, Command::SetLevel(id.to_owned(), level))?;
            Ok(("Level changed.".into(), json!({})))
        }
        _ => Err((-32601, "no such tool".into())),
    }
}

/// The program's main loop: runs until CSM closes stdin.
pub fn run() {
    let io = Arc::new(Io::new());
    let (inbox_tx, inbox_rx) = mpsc::channel::<Value>();
    let reader_io = Arc::clone(&io);
    let reader = std::thread::Builder::new().name("phone-stdin".into()).spawn(move || read_loop(&reader_io, &inbox_tx));
    if reader.is_err() {
        return;
    }
    let mut session = Session::idle(Some("the add-on has not been initialised".into()));
    while let Ok(msg) = inbox_rx.recv() {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default().to_owned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match (method.as_str(), msg.get("id")) {
            ("initialize", Some(id)) => {
                let data_dir = params.get("data_dir").and_then(Value::as_str).map(PathBuf::from);
                session = match data_dir {
                    Some(dir) => start(&io, dir),
                    None => Session::idle(Some("no data folder: CSM is running with --clean or in an isolated run, so nothing is kept".into())),
                };
                let tools: Vec<Value> = tool_specs()
                    .into_iter()
                    .map(|t| json!({"id": t.id, "title": t.title, "summary": t.summary, "input_schema": t.schema, "effects": t.effects}))
                    .collect();
                io.reply(id, json!({"api": API, "name": ADD_ON_NAME, "tools": tools, "subscriptions": []}));
            }
            ("tools/call", Some(id)) => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
                match tool(&session, &name, &params) {
                    Ok((message, data)) => io.reply(id, json!({"message": message, "data": data})),
                    Err((code, message)) => io.reply_err(id, code, &message),
                }
            }
            (_, Some(id)) => io.reply_err(id, -32601, "method not found"),
            // Notifications (events) are not used by this add-on.
            (_, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn person_check_is_only_ui_and_cli() {
        assert!(is_person(&json!({"caller": {"kind": "ui"}})));
        assert!(is_person(&json!({"caller": {"kind": "cli"}})));
        assert!(!is_person(&json!({"caller": {"kind": "external", "client": "claude-code"}})));
        assert!(!is_person(&json!({"caller": {"kind": "addon"}})));
        assert!(!is_person(&json!({"arguments": {"caller": {"kind": "ui"}}})), "an argument named caller does not count");
        assert!(!is_person(&json!({})));
    }

    #[test]
    fn levels_parse_only_the_two_names() {
        assert_eq!(parse_level(Some(&json!("viewer"))), Some(Level::Viewer));
        assert_eq!(parse_level(Some(&json!("operator"))), Some(Level::Operator));
        assert_eq!(parse_level(Some(&json!("admin"))), None);
        assert_eq!(parse_level(Some(&json!(3))), None);
        assert_eq!(parse_level(None), None);
    }

    #[test]
    fn every_declared_tool_id_is_namespaced_and_well_formed() {
        for t in tool_specs() {
            let (prefix, name) = t.id.split_once('.').expect("a dot");
            assert_eq!(prefix, "phone-remote");
            assert!(!name.is_empty() && name.len() <= 48 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'), "{}", t.id);
            assert_eq!(t.schema["type"], "object");
        }
    }

    #[test]
    fn the_person_only_tools_refuse_agents_before_any_command() {
        let session = Session::idle(None);
        for name in [TOOL_PAIR_START, TOOL_PAIR_STOP, TOOL_PAIR_DECIDE, TOOL_DEVICE_LIST, TOOL_DEVICE_REVOKE, TOOL_DEVICE_LEVEL] {
            let params = json!({"caller": {"kind": "external"}, "arguments": {"allow": true, "device_id": "d", "level": "viewer"}});
            let err = tool(&session, name, &params).expect_err("refused");
            assert_eq!(err.0, -32000, "{name}");
            assert!(err.1.contains("only the person"), "{name}: {}", err.1);
        }
    }

    #[test]
    fn status_works_without_a_host_and_says_why() {
        let session = Session::idle(Some("no data folder".into()));
        let (_, data) = tool(&session, TOOL_STATUS, &json!({"caller": {"kind": "external"}})).unwrap();
        assert_eq!(data["enabled"], false);
        assert_eq!(data["problem"], "no data folder");
    }

    #[test]
    fn a_decision_without_a_host_is_an_error_not_a_panic() {
        let session = Session::idle(None);
        let params = json!({"caller": {"kind": "ui"}, "arguments": {"allow": true}});
        let err = tool(&session, TOOL_PAIR_DECIDE, &params).expect_err("no level for an allow");
        assert_eq!(err.0, -32602);
        let params = json!({"caller": {"kind": "ui"}, "arguments": {"allow": false}});
        let err = tool(&session, TOOL_PAIR_DECIDE, &params).expect_err("no host");
        assert!(err.1.contains("not running") || err.1.contains("no data folder") || err.1.contains("initialised"), "{}", err.1);
    }
}
