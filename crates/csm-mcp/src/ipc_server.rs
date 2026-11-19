//! The app side of the bridge: [`serve_ipc`] publishes the engine on a local socket so `csm` (and
//! `csm mcp`, which Claude Code starts) can call its tools.
//!
//! Security, in the order a request meets it:
//! 1. craft-ipc authenticates every connection with the 64-hex token from the instance record
//!    (a file only this user can read) and refuses a peer that is another user. Nothing in this
//!    module can switch that off.
//! 2. A request names its caller ([`wire::accept_caller`]): the CLI, or an MCP client by name.
//!    `ui` is refused, so no remote caller is ever [`csm_engine::Caller::Ui`], which is the only
//!    caller that can answer a consent card. The engine enforces the rest: an external call that
//!    needs consent parks until the person answers in the app.
//! 3. The label is a claim by a process that already holds the token (the same user), not proof.
//!    The `csm mcp` bridge always labels its traffic as the MCP client it serves, so a model
//!    behind it cannot relabel itself as the CLI.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use craft_ipc::server::{Activate, Activation, ControlSink, FrameOpen, Frames, Server, ServerConfig};
use craft_ipc::{InstanceRecord, LinkError, RuntimeDir};
use csm_engine::{Engine, Event, Reply, ToolError};
use serde_json::{Value, json};

use crate::catalog::ToolInfo;
use crate::wire::{self, APP_ID, MAX_WAIT_MS};

/// Unclaimed tickets older than this are dropped. Longer than any engine wait (`session_wait`
/// tops out at ten minutes).
const TICKET_TTL: Duration = Duration::from_secs(20 * 60);
const MAX_TICKETS: usize = 256;

/// Anything that stops the bridge from starting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IpcError(pub String);

impl std::fmt::Display for IpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for IpcError {}

impl From<LinkError> for IpcError {
    fn from(e: LinkError) -> Self {
        IpcError(e.to_string())
    }
}

/// Options for [`serve_ipc_with`].
#[derive(Default)]
pub struct IpcOptions {
    /// Where the instance record lives. `None` is the per-user default; tests pass a temp dir.
    pub runtime: Option<RuntimeDir>,
    /// What `link.hello` reports as the app's version. Defaults to this crate's.
    pub app_version: Option<String>,
    /// Called when a peer asks the app to raise its window (`app.activate`). Must be quick.
    pub on_activate: Option<Arc<dyn Fn() + Send + Sync>>,
}

/// A running bridge. Dropping it, or [`Handle::shutdown`], closes the socket and removes the
/// instance record.
pub struct Handle {
    server: Option<Arc<Server>>,
    stop: Arc<AtomicBool>,
    forwarder: Option<JoinHandle<()>>,
}

impl Handle {
    pub fn record(&self) -> Option<&InstanceRecord> {
        self.server.as_ref().map(|s| s.record())
    }

    /// Stop serving and remove the instance record.
    pub fn shutdown(mut self) -> Result<(), IpcError> {
        self.stop_inner()
    }

    fn stop_inner(&mut self) -> Result<(), IpcError> {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(f) = self.forwarder.take() {
            let _ = f.join();
        }
        match self.server.take().map(Arc::try_unwrap) {
            Some(Ok(server)) => server.shutdown().map_err(IpcError::from),
            // Another clone is alive: dropping ours is the best we can do; Drop on the last one stops it.
            Some(Err(_)) | None => Ok(()),
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let _ = self.stop_inner();
    }
}

/// Publish `engine` for `csm` and `csm mcp`, in the per-user runtime directory.
pub fn serve_ipc(engine: Engine) -> Result<Handle, IpcError> {
    serve_ipc_with(engine, IpcOptions::default())
}

pub fn serve_ipc_with(engine: Engine, options: IpcOptions) -> Result<Handle, IpcError> {
    let IpcOptions { runtime, app_version, on_activate } = options;
    let sink = Arc::new(Sink::new(engine.clone()));
    let config = ServerConfig {
        app: APP_ID.to_string(),
        app_version: app_version.unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string()),
        instance: None,
        runtime,
        tcp_port: None,
        sink,
        activate: Arc::new(Raise(on_activate)),
        frames: Arc::new(NoFrames),
    };
    let server = Arc::new(Server::start(config)?);
    let stop = Arc::new(AtomicBool::new(false));
    let events = engine.subscribe();
    let forwarder = {
        let server = Arc::clone(&server);
        let stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("csm-ipc-events".into())
            .spawn(move || forward_events(&server, &events, &stop))
            .map_err(|e| IpcError(format!("could not start the event forwarder: {e}")))?
    };
    Ok(Handle { server: Some(server), stop, forwarder: Some(forwarder) })
}

fn forward_events(server: &Server, events: &Receiver<Event>, stop: &AtomicBool) {
    while !stop.load(Ordering::SeqCst) {
        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(event) => {
                if let Ok(data) = serde_json::to_value(&event) {
                    let _ = server.emit(wire::EVENT_NAME, data);
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

struct Raise(Option<Arc<dyn Fn() + Send + Sync>>);

impl Activate for Raise {
    fn activate(&self, _token: Option<String>, _from_pid: Option<u32>, _open: Option<Value>) -> Activation {
        match &self.0 {
            Some(f) => {
                f();
                Activation::Requested
            }
            None => Activation::No,
        }
    }
}

struct NoFrames;

impl Frames for NoFrames {
    fn open(&self, _params: &serde_json::Map<String, Value>) -> Result<FrameOpen, LinkError> {
        Err(LinkError::NotReady("CodeShellManager serves no frames".into()))
    }
    fn render(&self, _ticket: &str, _header: &Value) -> Result<(Value, Vec<u8>), LinkError> {
        Err(LinkError::NotReady("CodeShellManager serves no frames".into()))
    }
    fn close_ticket(&self, _ticket: &str) {}
}

struct Ticket {
    /// `None` while a `csm.wait` thread holds it.
    rx: Option<Receiver<Reply>>,
    created: Instant,
}

struct Sink {
    engine: Engine,
    tickets: Arc<Mutex<HashMap<u64, Ticket>>>,
    next_ticket: AtomicU64,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Sink {
    fn new(engine: Engine) -> Sink {
        Sink { engine, tickets: Arc::new(Mutex::new(HashMap::new())), next_ticket: AtomicU64::new(1) }
    }

    fn handle(&self, method: &str, params: &Value, tx: &Sender<Value>) {
        match method {
            wire::METHOD_PING => {
                let _ = tx.send(json!({"pong": true}));
            }
            wire::METHOD_TOOLS => {
                let tools: Vec<ToolInfo> = self.engine.registry().all().iter().map(ToolInfo::from_spec).collect();
                let _ = tx.send(json!({"tools": tools}));
            }
            wire::METHOD_SUBMIT => {
                let _ = tx.send(self.submit(params));
            }
            wire::METHOD_WAIT => self.wait(params, tx),
            other => {
                let _ = tx.send(wire::error_reply("unknown_method", format!("this CodeShellManager does not know `{other}`")));
            }
        }
    }

    fn submit(&self, params: &Value) -> Value {
        let Some(obj) = params.as_object() else { return wire::error_reply("bad_params", "params must be an object") };
        let caller = match wire::accept_caller(obj.get("caller")) {
            Ok(c) => c,
            Err(m) => return wire::error_reply("bad_params", m),
        };
        let Some(tool) = obj.get("tool").and_then(Value::as_str) else {
            return wire::error_reply("bad_params", "`tool` must be a tool id");
        };
        let args = obj.get("params").cloned().unwrap_or(Value::Null);
        let key = {
            let mut tickets = lock(&self.tickets);
            let now = Instant::now();
            // A waiting thread has taken `rx` out of its ticket; those are in use, not abandoned.
            tickets.retain(|_, t| t.rx.is_none() || now.duration_since(t.created) < TICKET_TTL);
            if tickets.len() >= MAX_TICKETS {
                return wire::error_reply("busy", "too many calls are waiting; let some finish");
            }
            let key = self.next_ticket.fetch_add(1, Ordering::SeqCst);
            // Reserve the slot first so the count above is honest, fill it after submitting.
            tickets.insert(key, Ticket { rx: None, created: now });
            key
        };
        let rx = self.engine.submit(caller, tool, args);
        if let Some(t) = lock(&self.tickets).get_mut(&key) {
            t.rx = Some(rx);
        }
        json!({"ticket": key})
    }

    fn wait(&self, params: &Value, tx: &Sender<Value>) {
        let key = params.get("ticket").and_then(Value::as_u64);
        let timeout_ms = params.get("timeout_ms").and_then(Value::as_u64).unwrap_or(MAX_WAIT_MS).min(MAX_WAIT_MS);
        let Some(key) = key else {
            let _ = tx.send(wire::error_reply("bad_params", "`ticket` must be a number"));
            return;
        };
        let taken = lock(&self.tickets).get_mut(&key).and_then(|t| t.rx.take());
        let Some(rx) = taken else {
            let _ = tx.send(wire::error_reply("link_unknown", format!("no waiting call {key}, or another request is already waiting on it")));
            return;
        };
        // Waiting on the connection thread would hold up its stop check, so a helper does it and
        // the connection thread polls the channel we hand back.
        let tickets = Arc::clone(&self.tickets);
        let tx2 = tx.clone();
        let spawned = thread::Builder::new().name("csm-ipc-wait".into()).spawn(move || {
            match rx.recv_timeout(Duration::from_millis(timeout_ms)) {
                Ok(reply) => {
                    lock(&tickets).remove(&key);
                    let _ = tx2.send(wire::encode_reply(reply));
                }
                Err(RecvTimeoutError::Timeout) => {
                    if let Some(t) = lock(&tickets).get_mut(&key) {
                        t.rx = Some(rx);
                    }
                    let _ = tx2.send(json!({"status": "pending"}));
                }
                Err(RecvTimeoutError::Disconnected) => {
                    lock(&tickets).remove(&key);
                    let _ = tx2.send(wire::encode_reply(Err(ToolError::Failed("the engine stopped".into()))));
                }
            }
        });
        if spawned.is_err() {
            let _ = tx.send(wire::error_reply("busy", "could not start a wait thread"));
        }
    }
}

impl ControlSink for Sink {
    fn submit(&self, method: String, params: Value) -> Option<Receiver<Value>> {
        let (tx, rx) = mpsc::channel();
        self.handle(&method, &params, &tx);
        Some(rx)
    }
}
