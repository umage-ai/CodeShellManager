//! The stdio shell: newline-delimited JSON-RPC 2.0 with CSM (docs/addons.md, "Process add-ons").
//!
//! Threads:
//! - a reader thread takes every line from stdin: requests and notifications go to the main
//!   thread's queue, answers to our own `csm/call`s go to the caller waiting for them;
//! - the main thread answers `initialize`, `tools/call` and the screen protocol, sends the screens'
//!   renders and the `csm/status` line, and never calls CSM itself;
//! - the host thread owns the [`Host`] (pairing, devices, the relay connection). It makes the CSM
//!   calls its ops need (through [`StdioCsm`]) and receives the person's commands over a channel.
//!
//! The person-only tools (pairing, decisions, devices) refuse every caller whose `kind` is not `ui`
//! or `cli`, so an agent or another add-on cannot pair a phone or revoke one. The screens are the
//! person's own: their events come from the person's clicks, and each one is checked in
//! [`crate::screens`] before it becomes a command.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use csm_remote_proto::msg::Level;
use serde_json::{Value, json};

use crate::config::{self, Config};
use crate::csm::{Csm, CsmError, classify};
use crate::host::{Host, Out};
use crate::policy::cut;
use crate::relay_client::{self, Command, now_secs};
use crate::screens::{self, Live, SCREEN_IDS, Ui, View};
use crate::store;

/// The add-on protocol version this program speaks (`PROTOCOL_API` in CSM).
pub const API: u64 = 1;
pub const ADD_ON_NAME: &str = "Phone remote";
/// A CSM call is answered within this time or it counts as unavailable (CSM itself gives 30 s).
const CALL_TIMEOUT: Duration = Duration::from_secs(35);
/// How long a tool waits for the host thread to answer a command.
const ASK_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the main loop waits for the host's view before it carries on without it.
const VIEW_TIMEOUT: Duration = Duration::from_millis(300);
/// The main loop wakes this often to publish renders and the status line.
const LOOP_TICK: Duration = Duration::from_millis(200);
/// How often the main loop asks the host for a fresh view of the config, devices and offer.
const VIEW_EVERY: Duration = Duration::from_millis(500);
/// Open screens per add-on (`docs/addons.md`, "Limits").
const MAX_INSTANCES: usize = 8;
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
        spec(TOOL_PAIR_START, "Pair a phone", "Open a pairing. The pairing screen shows the code for the phone. Person only.", json!({"type": "object"}), vec!["accesses_network"]),
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

/// The screens this add-on declares in `initialize` (`docs/addons.md`, "Declaring and opening").
pub fn screen_specs() -> Vec<Value> {
    vec![
        json!({"id": screens::PAIR, "title": "Pair a phone", "icon": "phone", "scope": "global", "summary": "Pair a phone with this PC"}),
        json!({"id": screens::DEVICES, "title": "Phones", "icon": "devices", "scope": "global", "summary": "Paired phones and their levels"}),
        json!({"id": screens::SETTINGS, "title": "Phone remote settings", "icon": "list", "scope": "global", "summary": "Relay, folders, profiles, commands and push"}),
    ]
}

/// Everything written to stdout goes through here, one line at a time.
struct Io {
    out: Mutex<io::Stdout>,
    waiting: Mutex<BTreeMap<u64, Sender<Value>>>,
    next_id: AtomicU64,
}

impl Io {
    fn new() -> Self {
        Self { out: Mutex::new(io::stdout()), waiting: Mutex::new(BTreeMap::new()), next_id: AtomicU64::new(1_000_000) }
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
    /// What the host last reported, for the screens and the indicator.
    shared: Arc<Mutex<Live>>,
}

impl Session {
    fn idle(problem: Option<String>) -> Self {
        Self { problem, enabled: false, relay_set: false, commands: None, shared: Arc::new(Mutex::new(Live::default())) }
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
    set_live(&session, |live| live.enabled = session.enabled);
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

/// Locks the shared state. A poisoned lock still holds the state, so it is used as it is.
fn lock(shared: &Mutex<Live>) -> MutexGuard<'_, Live> {
    shared.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn set_live(session: &Session, f: impl FnOnce(&mut Live)) {
    f(&mut lock(&session.shared));
}

/// What the host reports while it runs: problems go to the log, everything else to the shared
/// state the screens and the indicator read.
fn on_host_out(io: &Io, shared: &Mutex<Live>, out: &Out) {
    if let Out::Problem(p) = out {
        io.log(p);
    }
    lock(shared).absorb(out);
}

/// Runs `f` on a copy of what the host last reported, with the time and the store problem.
fn with_view<T>(session: &Session, f: impl FnOnce(&View) -> T) -> T {
    let live = lock(&session.shared).clone();
    let view = View { live: &live, now: now_secs(), problem: session.problem.as_deref() };
    f(&view)
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
        let pending = lock(&session.shared).pending.clone();
        let pending = pending.map(|(check, who)| json!({"check": check, "name": who}));
        let devices = if session.commands.is_some() { devices_count(session)? } else { 0 };
        return Ok(("Phone remote status.".into(), json!({"enabled": session.enabled, "relay_set": session.relay_set, "devices": devices, "pending": pending, "problem": session.problem})));
    }
    if !is_person(params) {
        return Err((-32000, "only the person can do this (the app or `csm`), not an agent or another add-on".into()));
    }
    match name {
        TOOL_PAIR_START => {
            // The link (it carries the key for the phone) goes only to the pairing screen, never into
            // a tool result: the answer says where the person finds it.
            ask(session, |reply| Command::StartPairing { app_version: env!("CARGO_PKG_VERSION").to_owned(), reply })?.map_err(|why| (-32000, why))?;
            Ok(("Pairing open. The pairing screen shows the code for the phone.".into(), json!({})))
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

// ---- the screens -----------------------------------------------------------------------------

/// One open screen: which screen, its revision, the tree last sent and the screen's own state.
struct Instance {
    screen: String,
    rev: u64,
    tree: Value,
    ui: Ui,
}

/// `screen/open` (a request): the first tree, as revision 1.
fn open_screen(session: &Session, open: &mut BTreeMap<String, Instance>, params: &Value) -> Result<Value, (i64, String)> {
    let screen = params.get("screen").and_then(Value::as_str).unwrap_or_default();
    let instance = params.get("instance").and_then(Value::as_str).unwrap_or_default();
    if !SCREEN_IDS.contains(&screen) {
        return Err((-32602, "no such screen in this add-on".into()));
    }
    if instance.is_empty() || instance.len() > 64 || instance.chars().any(char::is_control) {
        return Err((-32602, "the instance name is missing or bad".into()));
    }
    if !open.contains_key(instance) && open.len() >= MAX_INSTANCES {
        return Err((-32000, "too many of this add-on's screens are open".into()));
    }
    let ui = Ui::default();
    let root = with_view(session, |view| screens::tree(screen, view, &ui)).ok_or((-32602, "no such screen in this add-on".to_owned()))?;
    open.insert(instance.to_owned(), Instance { screen: screen.to_owned(), rev: 1, tree: root.clone(), ui });
    Ok(json!({"rev": 1, "root": root}))
}

/// `screen/event` (a notification): the screen's own state changes, and the host gets the commands.
fn screen_event(session: &Session, open: &mut BTreeMap<String, Instance>, params: &Value) {
    let instance = params.get("instance").and_then(Value::as_str).unwrap_or_default();
    let key = params.get("key").and_then(Value::as_str).unwrap_or_default();
    let kind = params.get("kind").and_then(Value::as_str).unwrap_or_default();
    let value = params.get("value").cloned().unwrap_or(Value::Null);
    let Some(inst) = open.get_mut(instance) else { return };
    // The answer to the last press is shown until the next one; a new press clears it.
    set_live(session, |live| live.notice = None);
    let commands = with_view(session, |view| screens::event(&inst.screen, view, &mut inst.ui, key, kind, &value));
    for command in commands {
        if let Err((_, why)) = send(session, command) {
            set_live(session, |live| live.notice = Some(cut(&why, 200)));
        }
    }
}

/// Publishes a render for every open screen whose tree changed since it was last sent.
fn render_open(io: &Io, session: &Session, open: &mut BTreeMap<String, Instance>) {
    if open.is_empty() {
        return;
    }
    let live = lock(&session.shared).clone();
    let view = View { live: &live, now: now_secs(), problem: session.problem.as_deref() };
    for (instance, inst) in open.iter_mut() {
        let Some(tree) = screens::tree(&inst.screen, &view, &inst.ui) else { continue };
        if tree != inst.tree {
            inst.rev = inst.rev.saturating_add(1);
            inst.tree = tree.clone();
            io.send(&json!({"jsonrpc": "2.0", "method": "screen/render", "params": {"instance": instance, "rev": inst.rev, "root": tree}}));
        }
    }
}

/// Asks the host for a fresh copy of its config, devices and offer expiry. Waits briefly; a host
/// that is busy keeps the last copy.
fn refresh_view(session: &Session) {
    let Some(commands) = session.commands.as_ref() else { return };
    let (tx, rx) = mpsc::channel();
    if commands.send(Command::View(tx)).is_err() {
        return;
    }
    if let Ok(view) = rx.recv_timeout(VIEW_TIMEOUT) {
        set_live(session, |live| live.host = Some(view));
    }
}

/// Sends `csm/status` when the line changed (CSM coalesces the rest).
fn publish_status(io: &Io, session: &Session, last: &mut Option<(String, &'static str)>) {
    let (text, tone) = with_view(session, screens::status_line);
    if last.as_ref().is_some_and(|(t, k)| *t == text && *k == tone) {
        return;
    }
    io.send(&json!({"jsonrpc": "2.0", "method": "csm/status", "params": {"text": text, "tone": tone}}));
    *last = Some((text, tone));
}

// ---- the main loop ---------------------------------------------------------------------------

/// One message from CSM: a request answered here, or a notification.
fn handle(io: &Arc<Io>, session: &mut Session, open: &mut BTreeMap<String, Instance>, msg: Value, started: &mut bool) {
    let method = msg.get("method").and_then(Value::as_str).unwrap_or_default().to_owned();
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let id = msg.get("id").cloned();
    match (method.as_str(), id.as_ref()) {
        ("initialize", Some(id)) => {
            let data_dir = params.get("data_dir").and_then(Value::as_str).map(PathBuf::from);
            *session = match data_dir {
                Some(dir) => start(io, dir),
                None => Session::idle(Some("no data folder: CSM is running with --clean or in an isolated run, so nothing is kept".into())),
            };
            *started = true;
            let tools: Vec<Value> = tool_specs()
                .into_iter()
                .map(|t| json!({"id": t.id, "title": t.title, "summary": t.summary, "input_schema": t.schema, "effects": t.effects}))
                .collect();
            // Screens only when CSM says it draws them (`capabilities.screens`, docs/addons.md).
            let screens_ok = params.pointer("/capabilities/screens").and_then(Value::as_u64).is_some_and(|v| v >= 1);
            let screens = if screens_ok { screen_specs() } else { Vec::new() };
            io.reply(id, json!({"api": API, "name": ADD_ON_NAME, "tools": tools, "subscriptions": [], "screens": screens}));
        }
        ("tools/call", Some(id)) => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default().to_owned();
            match tool(session, &name, &params) {
                Ok((message, data)) => io.reply(id, json!({"message": message, "data": data})),
                Err((code, message)) => io.reply_err(id, code, &message),
            }
        }
        ("screen/open", Some(id)) => match open_screen(session, open, &params) {
            Ok(answer) => io.reply(id, answer),
            Err((code, message)) => io.reply_err(id, code, &message),
        },
        ("screen/event", None) => screen_event(session, open, &params),
        // A refresh asks for a full render: an empty tree never matches, so the next publish sends one.
        ("screen/refresh", None) => {
            if let Some(inst) = params.get("instance").and_then(Value::as_str).and_then(|i| open.get_mut(i)) {
                inst.tree = Value::Null;
            }
        }
        ("screen/closed", None) => {
            if let Some(instance) = params.get("instance").and_then(Value::as_str) {
                open.remove(instance);
            }
        }
        (_, Some(id)) => io.reply_err(id, -32601, "method not found"),
        // Other notifications are not used by this add-on.
        (_, None) => {}
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
    let mut open: BTreeMap<String, Instance> = BTreeMap::new();
    let mut started = false;
    let mut last_status: Option<(String, &'static str)> = None;
    let mut last_view: Option<Instant> = None;
    loop {
        match inbox_rx.recv_timeout(LOOP_TICK) {
            Ok(msg) => handle(&io, &mut session, &mut open, msg, &mut started),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        if started {
            if last_view.is_none_or(|t| t.elapsed() >= VIEW_EVERY) {
                refresh_view(&session);
                last_view = Some(Instant::now());
            }
            publish_status(&io, &session, &mut last_status);
            render_open(&io, &session, &mut open);
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

    #[test]
    fn a_screen_opens_only_when_the_add_on_declares_it() {
        let session = Session::idle(Some("no data folder".into()));
        let mut open = BTreeMap::new();
        let ok = open_screen(&session, &mut open, &json!({"screen": screens::PAIR, "instance": "i1"})).unwrap();
        assert_eq!(ok["rev"], 1);
        assert!(ok["root"].is_object(), "a tree even without a host: it says why");
        let refused = open_screen(&session, &mut open, &json!({"screen": "phone-remote.nope", "instance": "i2"})).expect_err("not declared");
        assert_eq!(refused.0, -32602);
        let bad = open_screen(&session, &mut open, &json!({"screen": screens::DEVICES, "instance": ""})).expect_err("no instance");
        assert_eq!(bad.0, -32602);
    }

    #[test]
    fn at_most_eight_screens_are_open() {
        let session = Session::idle(None);
        let mut open = BTreeMap::new();
        for i in 0..MAX_INSTANCES {
            open_screen(&session, &mut open, &json!({"screen": screens::SETTINGS, "instance": format!("i{i}")})).unwrap();
        }
        assert!(open_screen(&session, &mut open, &json!({"screen": screens::SETTINGS, "instance": "one-more"})).is_err());
    }

    #[test]
    fn the_declared_screens_are_the_three_of_the_design_note() {
        let specs = screen_specs();
        assert_eq!(specs.len(), 3);
        for s in specs {
            let id = s["id"].as_str().unwrap();
            assert!(SCREEN_IDS.contains(&id));
            assert!(s["title"].as_str().is_some_and(|t| !t.is_empty() && t.chars().count() <= 64));
        }
    }

    #[test]
    fn the_pairing_tool_never_returns_the_offer() {
        // The tool's answer is built from the message and data only; the data has no link in it.
        let source = include_str!("shell.rs");
        let pair_arm = source.split("TOOL_PAIR_START => {").nth(1).unwrap_or_default();
        let arm = pair_arm.split("TOOL_PAIR_STOP").next().unwrap_or_default();
        assert!(!arm.contains("\"url\""), "the link never leaves the screen");
    }
}
