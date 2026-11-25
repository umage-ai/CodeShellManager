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
    /// Which transport to use when the app offers both. `None` is the platform default.
    pub tcp: Option<bool>,
    /// Arguments passed to the app when it is started.
    pub app_args: Vec<OsString>,
    /// Which instance to talk to: a pid, or the path of the state file it keeps (`--instance`,
    /// `CSM_INSTANCE`). `None` picks the newest instance that is not a `--clean` one.
    pub instance: Option<String>,
}

/// Whether `record` is the instance `selector` names (a pid, or a state file path).
fn matches_instance(record: &InstanceRecord, selector: &str) -> bool {
    let selector = selector.trim();
    match selector.parse::<u32>() {
        Ok(pid) => record.pid == pid,
        Err(_) => record.caps().contains(&wire::state_cap(std::path::Path::new(selector))),
    }
}

fn is_clean(record: &InstanceRecord) -> bool {
    record.caps().iter().any(|c| c == wire::CAP_CLEAN)
}

/// The instance `csm` should use among `found`: the named one, else the one with the highest pid
/// that is not a `--clean` instance.
fn pick_instance(mut found: Vec<InstanceRecord>, selector: Option<&str>) -> Option<InstanceRecord> {
    found.sort_by_key(|r| r.pid);
    match selector {
        Some(s) => found.into_iter().rev().find(|r| matches_instance(r, s)),
        None => found.into_iter().rev().find(|r| !is_clean(r)),
    }
}

/// A connection to the running app, made on first use and re-made if it drops between calls.
pub struct RemoteBackend {
    options: RemoteOptions,
    /// The connection and the instance id of the app it is connected to.
    client: Option<(Client, String)>,
}

fn not_running(extra: &str) -> BridgeError {
    BridgeError::Unreachable(format!(
        "CodeShellManager is not running{extra}. Start the app, or run csm with --start (or set {APP_ENV} to the app's path) to start it."
    ))
}

/// The app installed beside this `csm` (the MSI and the zip put them in one folder). Preferred
/// over the PATH lookup: with a stable and a beta install both on PATH, the first one found could
/// be the other channel's app, which never publishes the record this `csm` waits for.
fn sibling_app() -> Option<PathBuf> {
    let dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let bin = dir.join(format!("{}{}", wire::APP_BIN, std::env::consts::EXE_SUFFIX));
    bin.is_file().then_some(bin)
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
        let found: Vec<InstanceRecord> = Discovery::scan(&dir)
            .map_err(|e| unreachable("could not look for CodeShellManager", &e))?
            .into_iter()
            .filter(|r| r.app == APP_ID)
            .collect();
        let only_clean = !found.is_empty() && found.iter().all(is_clean);
        if let Some(r) = pick_instance(found, self.options.instance.as_deref()) {
            return Ok(r);
        }
        if !self.options.start {
            return Err(match &self.options.instance {
                Some(s) => not_running(&format!(" as instance `{s}`")),
                None if only_clean => not_running(" (only a --clean instance is up; pick it with --instance <pid>)"),
                None => not_running(""),
            });
        }
        let bin = match &self.options.app_path {
            Some(p) => p.clone(),
            None => match std::env::var_os(APP_ENV) {
                Some(p) if !p.is_empty() => PathBuf::from(p),
                _ => sibling_app().map_or_else(|| craft_ipc::launch::resolve(wire::APP_BIN), Ok).map_err(|e| {
                    BridgeError::Unreachable(format!(
                        "cannot find the CodeShellManager app to start ({e}). Set {APP_ENV} or pass --app with its full path."
                    ))
                })?,
            },
        };
        craft_ipc::launch::launch_at(APP_ID, &bin, &self.options.app_args, &[], self.options.runtime.as_ref(), &mut |_| {})
            .map_err(|e| BridgeError::Unreachable(format!("could not start {}: {e}", bin.display())))
    }

    fn connect(&self) -> Result<(Client, String), BridgeError> {
        let record = self.find_record()?;
        let use_tcp = self.options.tcp.unwrap_or(crate::ipc_server::USE_TCP_BY_DEFAULT) && record.tcp_port().is_some();
        let connected = if use_tcp { Client::connect_tcp(&record) } else { Client::connect(&record) };
        let mut client = connected.map_err(|e| unreachable("could not connect to CodeShellManager", &e))?;
        let identity = HelloClient {
            app: "csm".to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            pid: std::process::id(),
            instance: craft_ipc::random_uuid().map_err(|e| unreachable("could not create an id", &e))?,
            protocol_min: craft_ipc::wire::PROTOCOL_MIN,
            protocol_max: craft_ipc::wire::PROTOCOL_MAX,
        };
        client.hello(&identity).map_err(|e| unreachable("CodeShellManager did not accept this client", &e))?;
        Ok((client, record.instance))
    }

    /// Whether the instance we are connected to is still published. Asking the dead pipe instead
    /// would block for craft-ipc's whole reply deadline (60 s) on Windows.
    fn still_published(&self, instance: &str) -> bool {
        let Ok(dir) = self.runtime() else { return false };
        Discovery::scan(&dir).is_ok_and(|all| all.iter().any(|r| r.app == APP_ID && r.instance == instance))
    }

    /// The connection, re-made when the app it belonged to is gone (it restarted or quit).
    fn live(&mut self) -> Result<&mut Client, BridgeError> {
        let alive = match &self.client {
            Some((_, instance)) => self.still_published(instance),
            None => false,
        };
        if !alive {
            self.client = None;
            self.client = Some(self.connect()?);
        }
        self.client.as_mut().map(|(c, _)| c).ok_or_else(|| not_running(""))
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
        let (mut client, _) = self.connect()?;
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

/// The app ids (`codeshell`, `codeshell-beta`) with a live instance in the runtime directory, with
/// their pids, sorted. For `csm doctor`: it shows which channel's app is up when this build
/// looks for another one.
pub fn published_apps(runtime: Option<&RuntimeDir>) -> Result<Vec<(String, u32)>, BridgeError> {
    let dir = match runtime {
        Some(d) => d.clone(),
        None => RuntimeDir::resolve().map_err(|e| unreachable("could not open the runtime directory", &e))?,
    };
    let mut all: Vec<(String, u32)> =
        Discovery::scan(&dir).map_err(|e| unreachable("could not look for CodeShellManager", &e))?.into_iter().map(|r| (r.app, r.pid)).collect();
    all.sort();
    Ok(all)
}

/// A second launch of the app: when this user's app already runs on the same state file (and is not
/// a `--clean` one), ask it to show its window (`app.activate`, which restores it from the taskbar
/// or the tray) and answer true, so the new process can exit instead of opening a second window
/// over the same `state.json`. False when none is running or the running one keeps its state
/// elsewhere: start normally.
pub fn activate_running(options: RemoteOptions, state_path: &std::path::Path) -> Result<bool, BridgeError> {
    let mut b = RemoteBackend::new(RemoteOptions { start: false, instance: Some(state_path.display().to_string()), ..options });
    let dir = b.runtime()?;
    // Only a real instance that keeps this very state file: a `--clean` one or one on another
    // file is not the app this launch would share `state.json` with.
    let found: Vec<InstanceRecord> =
        Discovery::scan(&dir).map_err(|e| unreachable("could not look for CodeShellManager", &e))?.into_iter().filter(|r| r.app == APP_ID && !is_clean(r)).collect();
    if pick_instance(found, b.options.instance.as_deref()).is_none() {
        return Ok(false);
    }
    b.with(|c| c.call("app.activate", json!({"from_pid": std::process::id()})))?;
    Ok(true)
}

impl Backend for RemoteBackend {
    /// A fresh backend with the same options and no connection yet: each concurrent call makes
    /// (and drops) a connection of its own, so one parked on consent blocks nothing else.
    fn fork(&self) -> Option<Box<dyn Backend>> {
        Some(Box::new(RemoteBackend::new(self.options.clone())))
    }

    fn tools(&mut self) -> Result<Vec<ToolInfo>, BridgeError> {
        let v = self.with(|c| c.call(wire::METHOD_TOOLS, json!({})))?;
        let list = v.get("tools").cloned().unwrap_or(Value::Null);
        serde_json::from_value(list).map_err(|e| {
            BridgeError::Unreachable(format!("the app sent a tool list this version of csm cannot read ({e}); update csm and the app together"))
        })
    }

    fn call(&mut self, caller: &Caller, tool: &str, params: Value) -> Result<ToolOutput, BridgeError> {
        let started = Instant::now();
        let submitted = self.with(|c| {
            c.call(wire::METHOD_SUBMIT, json!({"caller": serde_json::to_value(caller).unwrap_or(Value::Null), "tool": tool, "params": params}))
        })?;
        let Some(ticket) = submitted.get("ticket").and_then(Value::as_u64) else {
            return Err(BridgeError::Unreachable("the app did not accept the call".to_string()));
        };
        loop {
            if started.elapsed() >= MAX_CALL {
                return Err(BridgeError::Tool(ToolError::TimedOut(format!("`{tool}` did not finish within {} minutes", MAX_CALL.as_secs() / 60))));
            }
            // Not `live()`: a dead connection here means the ticket is gone with it.
            let Some((client, _)) = self.client.as_mut() else {
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
