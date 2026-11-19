//! The `csm` side of the bridge: a [`Backend`] that forwards to the running app.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use craft_ipc::client::{Client, HelloClient};
use craft_ipc::{Discovery, InstanceRecord, LinkError, RuntimeDir};
use csm_engine::{Caller, ToolError, ToolOutput};
use serde_json::{Value, json};

use crate::catalog::{Backend, BridgeError, ToolInfo};
use crate::wire::{self, APP_ID, MAX_WAIT_MS, WaitStatus};

/// The longest one call may take end to end: a consent card left open (2 minutes), then the
/// longest `session_wait` (10 minutes), with margin. Past it the call is reported as timed out
/// here; the engine keeps its own limits either way.
pub const MAX_CALL: Duration = Duration::from_secs(15 * 60);

/// Environment variable naming the app binary to start with `--start`.
pub const APP_ENV: &str = "CSM_APP";

/// How to reach the app, and whether to start it when it is not running.
#[derive(Debug, Clone, Default)]
pub struct RemoteOptions {
    /// Where instance records live. `None` is the per-user default; tests pass a temp dir.
    pub runtime: Option<RuntimeDir>,
    /// Start the app when no instance is running.
    pub start: bool,
    /// The app binary for `start`. `None` reads [`APP_ENV`], then craft-ipc's usual lookup.
    pub app_path: Option<PathBuf>,
    /// Arguments passed to the app when it is started.
    pub app_args: Vec<OsString>,
}

/// A connection to the running app, made on first use and re-made if it drops between calls.
pub struct RemoteBackend {
    options: RemoteOptions,
    client: Option<Client>,
}

fn not_running(extra: &str) -> BridgeError {
    BridgeError::Unreachable(format!(
        "CodeShellManager is not running{extra}. Start the app, or run csm with --start (or set {APP_ENV} to the app's path) to start it."
    ))
}

fn unreachable(context: &str, e: &LinkError) -> BridgeError {
    match e {
        LinkError::Auth => BridgeError::Unreachable(
            "CodeShellManager refused the connection (the access token did not match; is it running as another user?).".to_string(),
        ),
        other => BridgeError::Unreachable(format!("{context}: {other}")),
    }
}

impl RemoteBackend {
    pub fn new(options: RemoteOptions) -> RemoteBackend {
        RemoteBackend { options, client: None }
    }

    fn runtime(&self) -> Result<RuntimeDir, BridgeError> {
        match &self.options.runtime {
            Some(d) => Ok(d.clone()),
            None => RuntimeDir::resolve().map_err(|e| unreachable("could not open the runtime directory", &e)),
        }
    }

    /// The newest live instance, started first when allowed.
    fn find_record(&self) -> Result<InstanceRecord, BridgeError> {
        let dir = self.runtime()?;
        let mut found: Vec<InstanceRecord> =
            Discovery::scan(&dir).map_err(|e| unreachable("could not look for CodeShellManager", &e))?.into_iter().filter(|r| r.app == APP_ID).collect();
        found.sort_by_key(|r| r.pid);
        if let Some(r) = found.pop() {
            return Ok(r);
        }
        if !self.options.start {
            return Err(not_running(""));
        }
        let bin = match &self.options.app_path {
            Some(p) => p.clone(),
            None => match std::env::var_os(APP_ENV) {
                Some(p) if !p.is_empty() => PathBuf::from(p),
                _ => craft_ipc::launch::resolve(APP_ID).map_err(|e| {
                    BridgeError::Unreachable(format!("cannot find the CodeShellManager app to start ({e}). Set {APP_ENV} or pass --app with its full path."))
                })?,
            },
        };
        craft_ipc::launch::launch_at(APP_ID, &bin, &self.options.app_args, &[], self.options.runtime.as_ref(), &mut |_| {})
            .map_err(|e| BridgeError::Unreachable(format!("could not start {}: {e}", bin.display())))
    }

    fn connect(&self) -> Result<Client, BridgeError> {
        let record = self.find_record()?;
        let mut client = Client::connect(&record).map_err(|e| unreachable("could not connect to CodeShellManager", &e))?;
        let identity = HelloClient {
            app: "csm".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            instance: craft_ipc::random_uuid().map_err(|e| unreachable("could not create an id", &e))?,
            protocol_min: craft_ipc::wire::PROTOCOL_MIN,
            protocol_max: craft_ipc::wire::PROTOCOL_MAX,
        };
        client.hello(&identity).map_err(|e| unreachable("CodeShellManager did not accept this client", &e))?;
        Ok(client)
    }

    /// A connection that answers a ping, re-made when the old one is gone (the app restarted).
    fn live(&mut self) -> Result<&mut Client, BridgeError> {
        let alive = match self.client.as_mut() {
            Some(c) => c.call(wire::METHOD_PING, json!({})).is_ok(),
            None => false,
        };
        if !alive {
            self.client = None;
            self.client = Some(self.connect()?);
        }
        self.client.as_mut().ok_or_else(|| not_running(""))
    }

    /// Call `f` on a live connection. A broken connection is dropped so the next call reconnects.
    fn with<T>(&mut self, f: impl FnOnce(&mut Client) -> Result<T, LinkError>) -> Result<T, BridgeError> {
        let client = self.live()?;
        match f(client) {
            Ok(v) => Ok(v),
            Err(e) => {
                if matches!(e, LinkError::Closed | LinkError::Io(_) | LinkError::Timeout) {
                    self.client = None;
                }
                Err(unreachable("CodeShellManager stopped answering", &e))
            }
        }
    }

    /// Watch engine events (`session_changed`, `consent_requested`, …) on a connection of its own,
    /// until `on_event` returns false or the app goes away. Each event is the engine's JSON, with
    /// an `event` field naming it.
    pub fn watch(&mut self, mut on_event: impl FnMut(Value) -> bool) -> Result<(), BridgeError> {
        let mut client = self.connect()?;
        client.subscribe(&[wire::EVENT_NAME]).map_err(|e| unreachable("could not subscribe to events", &e))?;
        loop {
            match client.next_event(Some(Duration::from_secs(30))) {
                Ok(Some(ev)) => {
                    if !on_event(ev.data) {
                        return Ok(());
                    }
                }
                Ok(None) => {}
                Err(LinkError::Closed) => return Err(BridgeError::Unreachable("CodeShellManager closed the connection.".to_string())),
                Err(e) => return Err(unreachable("lost the connection to CodeShellManager", &e)),
            }
        }
    }
}

impl Backend for RemoteBackend {
    fn tools(&mut self) -> Result<Vec<ToolInfo>, BridgeError> {
        let v = self.with(|c| c.call(wire::METHOD_TOOLS, json!({})))?;
        let list = v.get("tools").cloned().unwrap_or(Value::Null);
        serde_json::from_value(list)
            .map_err(|e| BridgeError::Unreachable(format!("the app sent a tool list this version of csm cannot read ({e}); update csm and the app together")))
    }

    fn call(&mut self, caller: &Caller, tool: &str, params: Value) -> Result<ToolOutput, BridgeError> {
        let started = Instant::now();
        let submitted = self.with(|c| c.call(wire::METHOD_SUBMIT, json!({"caller": caller, "tool": tool, "params": params})))?;
        let Some(ticket) = submitted.get("ticket").and_then(Value::as_u64) else {
            return Err(BridgeError::Unreachable("the app did not accept the call".to_string()));
        };
        loop {
            if started.elapsed() >= MAX_CALL {
                return Err(BridgeError::Tool(ToolError::TimedOut(format!("`{tool}` did not finish within {} minutes", MAX_CALL.as_secs() / 60))));
            }
            // Not `live()`: a dead connection here means the ticket is gone with it.
            let Some(client) = self.client.as_mut() else {
                return Err(BridgeError::Unreachable("lost the connection to CodeShellManager while waiting for the result.".to_string()));
            };
            let reply = match client.call(wire::METHOD_WAIT, json!({"ticket": ticket, "timeout_ms": MAX_WAIT_MS})) {
                Ok(v) => v,
                Err(e) => {
                    self.client = None;
                    return Err(unreachable("lost the connection to CodeShellManager while waiting for the result", &e));
                }
            };
            match wire::decode_wait(&reply) {
                Ok(WaitStatus::Pending) => {}
                Ok(WaitStatus::Finished(r)) => return r.map_err(BridgeError::Tool),
                Err(m) => return Err(BridgeError::Unreachable(m)),
            }
        }
    }
}
