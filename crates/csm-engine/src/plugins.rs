//! Process add-ons (docs/addons.md, "Process add-ons"): the host that starts each enabled process
//! add-on as a child process, talks JSON-RPC to it over stdin and stdout, registers its tools, and
//! restarts it when it crashes.
//!
//! Threads (none of them is the engine thread or the UI thread):
//! - **manager** (one): owns the running supervisors. It receives the desired list from the engine
//!   (`Cmd::Sync`), restarts or stops one plugin (`Cmd::Restart`), forwards subscribed engine
//!   events to the plugins, and on shutdown stops them all within a budget.
//! - **supervisor** (one per plugin): starts the child, does the `initialize` handshake, runs the
//!   child's message loop, and restarts it with backoff. It reports status and tools back by
//!   posting a job to the engine thread, never by waiting for it.
//! - **writer**, **stdout reader**, **stderr pump** (per child): pipe I/O only, with bounded queues.
//! - **call** (per call): a tool call from the app waits here for the plugin's answer; a `csm/call`
//!   from a plugin runs here, so a call waiting for consent never stalls the plugin's stream.
//!
//! The engine thread never blocks on any of this: it posts `Cmd`s, reads the tool table it was
//! given, and hands tool calls to a call thread.
//!
//! Generations: every supervisor gets a number from the manager, higher than any before it. A
//! report or tool table from an older generation is ignored, so a stopped process can never
//! overwrite the one that replaced it.

use std::collections::{HashMap, HashSet};
use std::io::BufReader;
use std::path::PathBuf;
use std::process::Child;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use csm_core::addons::manifest::clean_text;
use csm_core::addons::protocol::{self as proto, DeclaredTool, Incoming, ProcessEffect, RpcError};
use csm_services::plugins::{self as io, LineRead, StderrTail};
use serde::Serialize;
use serde_json::{Value, json};

use crate::addons::{self, AddonState};
use crate::engine::{Ctx, Job, Reply};
use crate::events::Event;
use crate::plugin_screens::{self, ScreenStore};
use crate::registry::{DynamicTool, is_add_on_tool_id};
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset};

/// How long a started process has to answer `initialize`.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a tool call waits for the plugin's answer.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(30);
/// How often the loops wake with nothing to do.
const POLL: Duration = Duration::from_millis(50);
/// A process that ran this long before it failed starts the failure count again.
const STABLE_AFTER: Duration = Duration::from_secs(60);
/// Consecutive failures before a plugin is left alone until it is restarted by hand.
const MAX_RESTARTS: u32 = 5;
/// The longest wait between two restarts.
const MAX_BACKOFF: Duration = Duration::from_secs(30);
/// Messages queued for one plugin's stdin.
const OUT_QUEUE: usize = 64;
/// Lines queued from one plugin's stdout before its reader waits.
const IN_QUEUE: usize = 64;
/// Tool calls in flight for one plugin.
const MAX_CALLS_IN_FLIGHT: usize = 16;
/// `csm/call` requests from one plugin being answered at once.
const MAX_CSM_CALLS: usize = 8;
/// Tool calls from the app waiting on add-on processes at once, across all add-ons.
const MAX_CALL_THREADS: usize = 64;
/// Distinct id, title and summary strings kept for the life of the app (see [`intern`]).
const MAX_INTERNED: usize = 4096;
/// The stderr bytes kept for the status (the last ones).
const STDERR_TAIL: usize = 2048;
/// How long shutdown waits for the plugins to go.
pub const SHUTDOWN_BUDGET: Duration = Duration::from_secs(3);
/// Longest text a plugin may send as a tool's message.
const MAX_MESSAGE: usize = 500;

/// A job for the engine thread (`Engine::post`).
pub(crate) type PostFn = Arc<dyn Fn(Job) + Send + Sync>;
/// A tool call on behalf of a plugin: the engine's own path, consent included. Blocks until the
/// engine answers, so it runs on a call thread only.
pub(crate) type CallFn = Arc<dyn Fn(Caller, &str, Value) -> Reply + Send + Sync>;

/// Where a process add-on stands, as the status and Settings show it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Started; the `initialize` handshake is not done yet.
    Starting,
    /// Answered the handshake; its tools are registered.
    Running,
    /// Exited or misbehaved; it starts again after a short wait (see `restarts`).
    Crashed,
    /// Failed too many times in a row. `restart` starts it again.
    GaveUp,
    /// Stopped: disabled, the app is closing, or it was replaced.
    Stopped,
}

/// One process add-on's runtime state, as the engine keeps it for the status and Settings.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PluginRuntime {
    pub state: RunState,
    pub pid: Option<u32>,
    /// Restarts since the app started (a manual restart counts).
    pub restarts: u32,
    /// The last thing that went wrong, in plain words.
    pub last_error: Option<String>,
    /// The plugin's own name, from the handshake.
    pub name: Option<String>,
    /// Its tools, `addon.name`, while it is running.
    pub tools: Vec<String>,
    /// Tools and subscriptions the handshake dropped, one line each.
    pub problems: Vec<String>,
    pub subscriptions: Vec<String>,
    /// The end of what it wrote to stderr (capped).
    pub stderr_tail: String,
    /// The supervisor that reported this; a newer one wins.
    #[serde(skip)]
    pub(crate) generation: u64,
}

impl PluginRuntime {
    /// What a plugin shows before its first report.
    pub fn starting() -> PluginRuntime {
        PluginRuntime::new(0)
    }

    fn new(generation: u64) -> PluginRuntime {
        PluginRuntime {
            state: RunState::Starting,
            pid: None,
            restarts: 0,
            last_error: None,
            name: None,
            tools: Vec::new(),
            problems: Vec::new(),
            subscriptions: Vec::new(),
            stderr_tail: String::new(),
            generation,
        }
    }
}

/// A process add-on the engine wants running: the enabled, approved contents of its manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginSpec {
    pub addon: String,
    /// The add-on folder: the child's working directory.
    pub dir: PathBuf,
    pub argv: Vec<String>,
    pub effects: Vec<ProcessEffect>,
    /// The add-on's own data folder (`initialize` `data_dir`): set only when it declared
    /// `stores_data` and the run has a state folder; created when the process starts.
    pub data_dir: Option<PathBuf>,
    /// The digest of the approved `addon.toml`; a change means a new process.
    pub digest: String,
}

/// Commands to the manager thread.
enum Cmd {
    /// The complete desired list; anything not in it stops.
    Sync(Vec<PluginSpec>),
    Restart(String),
    Shutdown,
}

/// Commands to one supervisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ctl {
    Stop,
    Restart,
}

/// The write side of one initialized plugin, as the call paths see it.
pub(crate) struct Link {
    out: SyncSender<String>,
    pending: Mutex<HashMap<u64, Sender<Result<Value, RpcError>>>>,
    next_id: AtomicU64,
    inflight: Arc<AtomicUsize>,
    subs: Vec<&'static str>,
}

/// Why a request to a plugin got no answer.
pub(crate) enum ReqError {
    /// Not sent: the plugin's queue is full.
    Refused(String),
    TimedOut,
    Failed(String),
}

impl Link {
    /// A notification to the plugin. Dropped (false) when its queue is full or the line is over the cap.
    pub(crate) fn notify(&self, method: &str, params: Value) -> bool {
        let line = proto::notification_line(method, params);
        fits_line(&line) && self.out.try_send(line).is_ok()
    }

    /// A request, waiting up to `timeout` for the answer. Blocks: call it on a thread of its own.
    pub(crate) fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, ReqError> {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let line = proto::request_line(id, method, params);
        if !fits_line(&line) {
            return Err(ReqError::Failed(format!("the `{method}` request is over 1 MiB")));
        }
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(id, tx);
        if self.out.try_send(line).is_err() {
            self.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            return Err(ReqError::Refused("the add-on is not taking requests right now".into()));
        }
        match rx.recv_timeout(timeout) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(ReqError::Failed(clean_text(&e.message, MAX_MESSAGE))),
            Err(RecvTimeoutError::Timeout) => {
                self.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
                Err(ReqError::TimedOut)
            }
            Err(RecvTimeoutError::Disconnected) => Err(ReqError::Failed("the add-on stopped before it answered".into())),
        }
    }
}

type Links = Arc<Mutex<HashMap<String, Arc<Link>>>>;

/// The running add-ons. One per engine; shared by the engine and its call threads.
pub(crate) struct Host {
    cmds: Sender<Cmd>,
    links: Links,
    /// Call threads waiting for a plugin's answer (see [`MAX_CALL_THREADS`]).
    calls: Arc<AtomicUsize>,
    manager: Mutex<Option<JoinHandle<()>>>,
    /// The screens of the running add-ons (`plugin_screens`).
    screens: ScreenStore,
}

impl Host {
    /// Starts the manager thread. `events` is the engine's event stream (forwarded to the
    /// plugins that subscribed to a kind). Failing to start the thread only disables add-on
    /// processes; the rest of the app is unaffected.
    pub(crate) fn start(post: PostFn, call: CallFn, events: Receiver<Event>, screens: ScreenStore) -> Host {
        let (cmds, rx) = mpsc::channel::<Cmd>();
        let links: Links = Arc::new(Mutex::new(HashMap::new()));
        let manager_links = links.clone();
        let manager_screens = screens.clone();
        let manager = thread::Builder::new()
            .name("csm-plugins".into())
            .spawn(move || manager(rx, events, manager_links, post, call, manager_screens))
            .map_err(|e| log::error!("could not start the add-on process manager: {e}"))
            .ok();
        Host { cmds, links, calls: Arc::new(AtomicUsize::new(0)), manager: Mutex::new(manager), screens }
    }

    /// The screens of the running add-ons.
    pub(crate) fn screens(&self) -> &ScreenStore {
        &self.screens
    }

    /// The write side of `addon`'s running process, once its handshake is done.
    pub(crate) fn link(&self, addon: &str) -> Option<Arc<Link>> {
        self.links.lock().unwrap_or_else(PoisonError::into_inner).get(addon).cloned()
    }

    /// The desired list: starts what is new or changed, stops what is gone. Never blocks.
    pub(crate) fn sync(&self, specs: Vec<PluginSpec>) {
        let _ = self.cmds.send(Cmd::Sync(specs));
    }

    /// Starts `addon` again now (after a crash, a give-up, or by hand). Never blocks.
    pub(crate) fn restart(&self, addon: &str) {
        let _ = self.cmds.send(Cmd::Restart(addon.to_owned()));
    }

    /// Stops every plugin and waits up to [`SHUTDOWN_BUDGET`] for them to go. Idempotent.
    pub(crate) fn shutdown(&self) {
        let _ = self.cmds.send(Cmd::Shutdown);
        let handle = self.manager.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(h) = handle {
            let _ = h.join();
        }
    }

    /// Tells `addon` about one of its own sessions (`session_asleep`, `session_awake`) if it
    /// subscribed to that kind. Dropped when its queue is full. Never blocks.
    pub(crate) fn notify_session(&self, addon: &str, kind: &str, session_id: &str) {
        let link = self.links.lock().unwrap_or_else(PoisonError::into_inner).get(addon).cloned();
        let Some(link) = link.filter(|l| l.subs.contains(&kind)) else { return };
        let line = proto::notification_line("event", json!({"event": kind, "session_id": session_id}));
        if fits_line(&line) && link.out.try_send(line).is_err() {
            log::debug!("an add-on's queue is full; the `{kind}` event is not sent to it");
        }
    }

    /// Runs a tool of a running add-on on a call thread and answers `reply` with the result.
    /// At most [`MAX_CALL_THREADS`] such calls wait at once; more are refused. If the thread
    /// cannot start, `reply` is dropped and the caller gets the engine's "stopped".
    pub(crate) fn dispatch(&self, addon: String, tool: String, params: Value, caller: Value, reply: Sender<Reply>) {
        let Some(slot) = InflightSlot::take(&self.calls, MAX_CALL_THREADS) else {
            let _ = reply.send(Err(ToolError::Refused("too many add-on calls are waiting; try again shortly".into())));
            return;
        };
        let links = self.links.clone();
        let spawned = thread::Builder::new().name("csm-plugin-call".into()).spawn(move || {
            let _slot = slot;
            let r = call_tool(&links, &addon, &tool, params, caller);
            let _ = reply.send(r);
        });
        if let Err(e) = spawned {
            log::error!("could not start a call thread for an add-on tool: {e}");
        }
    }
}

/// Manager thread: applies commands, forwards events, and stops everything on shutdown.
fn manager(cmds: Receiver<Cmd>, events: Receiver<Event>, links: Links, post: PostFn, call: CallFn, screens: ScreenStore) {
    let mut running: HashMap<String, Running> = HashMap::new();
    let mut generation: u64 = 0;
    loop {
        match cmds.recv_timeout(POLL) {
            Ok(Cmd::Sync(list)) => sync_running(&mut running, list, &mut generation, &links, &post, &call, &screens),
            Ok(Cmd::Restart(id)) => {
                if let Some(r) = running.get(&id) {
                    let _ = r.ctl.send(Ctl::Restart);
                }
            }
            Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        while let Ok(ev) = events.try_recv() {
            forward(&ev, &links);
        }
    }
    for r in running.values() {
        let _ = r.ctl.send(Ctl::Stop);
    }
    let deadline = Instant::now() + SHUTDOWN_BUDGET;
    while Instant::now() < deadline && running.values().any(|r| r.handle.as_ref().is_some_and(|h| !h.is_finished())) {
        thread::sleep(POLL);
    }
}

struct Running {
    digest: String,
    generation: u64,
    ctl: Sender<Ctl>,
    handle: Option<JoinHandle<()>>,
}

fn sync_running(running: &mut HashMap<String, Running>, list: Vec<PluginSpec>, generation: &mut u64, links: &Links, post: &PostFn, call: &CallFn, screens: &ScreenStore) {
    let wanted: HashMap<String, PluginSpec> = list.into_iter().map(|s| (s.addon.clone(), s)).collect();
    let stale: Vec<String> =
        running.iter().filter(|(id, r)| wanted.get(*id).is_none_or(|s| s.digest != r.digest)).map(|(id, _)| id.clone()).collect();
    for id in stale {
        if let Some(r) = running.remove(&id) {
            let _ = r.ctl.send(Ctl::Stop);
            // The old process's tools go now; the new process's come after its handshake.
            let (old_generation, addon) = (r.generation, id.clone());
            post(Box::new(move |ctx: &mut Ctx<'_>| {
                if ctx.engine.plugin_tool_gen(&addon).is_none_or(|g| g <= old_generation) {
                    ctx.engine.set_plugin_tools(&addon, old_generation, Vec::new());
                }
            }));
        }
    }
    for (id, spec) in wanted {
        if running.contains_key(&id) {
            continue;
        }
        *generation = generation.saturating_add(1);
        let env = Env {
            addon: id.clone(),
            generation: *generation,
            links: links.clone(),
            post: post.clone(),
            call: call.clone(),
            screens: screens.clone(),
            tail: Arc::new(StderrTail::new(STDERR_TAIL)),
        };
        let (ctl_tx, ctl_rx) = mpsc::channel::<Ctl>();
        let digest = spec.digest.clone();
        let spawned = thread::Builder::new().name(format!("csm-plugin-{id}")).spawn(move || supervise(spec, ctl_rx, env));
        match spawned {
            Ok(h) => {
                running.insert(id, Running { digest, generation: *generation, ctl: ctl_tx, handle: Some(h) });
            }
            Err(e) => log::error!("could not start the supervisor of add-on `{id}`: {e}"),
        }
    }
}

/// Sends an engine event to the plugins that subscribed to its kind. Dropped when a plugin's
/// queue is full (a plugin that does not read its input does not hold up the others).
fn forward(ev: &Event, links: &Links) {
    let subs: Vec<Arc<Link>> = {
        let m = links.lock().unwrap_or_else(PoisonError::into_inner);
        if m.is_empty() {
            return;
        }
        m.values().cloned().collect()
    };
    let Ok(v) = serde_json::to_value(ev) else { return };
    let Some(kind) = v.get("event").and_then(Value::as_str) else { return };
    let targets: Vec<&Arc<Link>> = subs.iter().filter(|l| l.subs.contains(&kind)).collect();
    if targets.is_empty() {
        return;
    }
    let line = proto::notification_line("event", v.clone());
    if !fits_line(&line) {
        return;
    }
    for l in targets {
        if l.out.try_send(line.clone()).is_err() {
            log::debug!("an add-on's queue is full; the `{kind}` event is not sent to it");
        }
    }
}

/// What a supervisor and its helpers share.
#[derive(Clone)]
struct Env {
    addon: String,
    generation: u64,
    links: Links,
    post: PostFn,
    call: CallFn,
    screens: ScreenStore,
    tail: Arc<StderrTail>,
}

impl Env {
    fn report(&self, rt: &mut PluginRuntime) {
        rt.stderr_tail = self.tail.text();
        let (addon, generation, snapshot) = (self.addon.clone(), self.generation, rt.clone());
        (self.post)(Box::new(move |ctx: &mut Ctx<'_>| store_runtime(ctx, &addon, generation, snapshot)));
    }

    /// Posts the plugin's complete list of remotes (replacing the last one).
    fn set_remotes(&self, list: Vec<csm_core::addons::Remote>) {
        let (addon, generation) = (self.addon.clone(), self.generation);
        (self.post)(Box::new(move |ctx: &mut Ctx<'_>| store_remotes(ctx, &addon, generation, list)));
    }

    /// The process stopped: what it offered and set at runtime goes with it.
    fn clear_contributions(&self) {
        // Its screens show "not running" at once; the rest goes with the engine thread's next step.
        self.screens.mark_stopped(&self.addon, self.generation);
        let (addon, generation) = (self.addon.clone(), self.generation);
        (self.post)(Box::new(move |ctx: &mut Ctx<'_>| clear_contributions(ctx, &addon, generation)));
    }

    fn apply_tools(&self, tools: Vec<DynamicTool>) {
        let (addon, generation) = (self.addon.clone(), self.generation);
        (self.post)(Box::new(move |ctx: &mut Ctx<'_>| {
            if is_running_process(&ctx.state.rt.addons, &addon) {
                ctx.engine.set_plugin_tools(&addon, generation, tools);
            }
        }));
    }
}

/// Engine thread: keeps a runtime report if its add-on is still an enabled process add-on and
/// the report is not older than the one stored.
fn store_runtime(ctx: &mut Ctx<'_>, addon: &str, generation: u64, rt: PluginRuntime) {
    if !is_running_process(&ctx.state.rt.addons, addon) {
        return;
    }
    let cat = &mut ctx.state.rt.addons;
    if cat.runtime.get(addon).is_some_and(|old| old.generation > generation) {
        return;
    }
    cat.runtime.insert(addon.to_owned(), rt);
    ctx.state.rt.dirty = true;
}

/// Whether `addon` is an enabled process add-on right now (the engine thread's view).
fn is_running_process(cat: &addons::Catalogue, addon: &str) -> bool {
    addons::entry(cat, addon).is_some_and(|e| e.state == AddonState::Loaded && e.parsed.as_deref().is_some_and(|p| p.process.is_some()))
}

/// A process add-on's status override and annotation on one session it opened.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PluginMark {
    pub addon: String,
    /// The process that set it; a stopped process's marks go with it.
    pub generation: u64,
    pub status: Option<crate::status::Status>,
    pub annotation: Option<String>,
}

/// One entry under "Remote…", as the snapshot (and so quick-open and the palette) shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RemoteView {
    /// The add-on that offers it.
    pub addon: String,
    pub addon_name: String,
    /// Unique within the add-on; `remote_open` takes `addon` and this.
    pub id: String,
    pub label: String,
    pub detail: String,
    /// `session` (opens as given) or `tool` (the add-on's own tool says what to open).
    pub opens: &'static str,
}

/// The entries of every running process add-on, in add-on id order.
pub(crate) fn remote_views(cat: &addons::Catalogue) -> Vec<RemoteView> {
    let mut out = Vec::new();
    for (addon, list) in &cat.remotes {
        let addon_name = cat.runtime.get(addon).and_then(|r| r.name.clone()).unwrap_or_else(|| addon.clone());
        for r in list {
            out.push(RemoteView {
                addon: addon.clone(),
                addon_name: addon_name.clone(),
                id: r.id.clone(),
                label: r.label.clone(),
                detail: r.detail.clone(),
                opens: match r.open {
                    csm_core::addons::RemoteOpen::Session(_) => "session",
                    csm_core::addons::RemoteOpen::Tool { .. } => "tool",
                },
            });
        }
    }
    out
}

/// The add-on a call comes from (`add-on <id>`, the caller a plugin's `csm/call` runs as), if it
/// is one. An MCP client cannot claim such a name (refused at the bridge).
pub(crate) fn addon_of(caller: &Caller) -> Option<&str> {
    match caller {
        Caller::External { client, .. } => client.strip_prefix("add-on ").filter(|id| csm_core::addons::manifest::valid_id(id)),
        _ => None,
    }
}

/// The folder an add-on may keep its own data in: `<state folder>/addon-data/<id>/`. `None`
/// under `--clean` and when there is no state folder (an isolated run): nothing is written then.
pub(crate) fn data_dir_for(save_path: Option<&std::path::Path>, clean: bool, addon: &str) -> Option<PathBuf> {
    if clean || !csm_core::addons::manifest::valid_id(addon) {
        return None;
    }
    save_path.and_then(std::path::Path::parent).map(|p| p.join("addon-data").join(addon))
}

/// Engine thread: whether `caller` is an enabled process add-on whose approved manifest lists
/// `tool` in `[process] calls`. The entry is `Loaded` only while its `addon.toml` digest is the
/// one the person approved, so a changed file loses the list until it is approved again.
pub(crate) fn standing_call(cat: &addons::Catalogue, caller: &Caller, tool: &str) -> bool {
    let Some(addon) = addon_of(caller) else { return false };
    addons::entry(cat, addon)
        .filter(|e| e.state == AddonState::Loaded)
        .and_then(|e| e.parsed.as_deref())
        .and_then(|p| p.process.as_ref())
        .is_some_and(|p| p.calls.iter().any(|c| c == tool))
}

/// The `caller` object CSM puts in a `tools/call` it sends a plugin. Filled here, from the
/// engine's own record of who asked; nothing the plugin or the arguments say can change it.
/// `kind` is `ui` (the person in the app), `cli`, `external` (an MCP client: `client`, and when
/// it runs in one of our sessions `session_id`, `session_name` and `project`) or `addon`
/// (another add-on's `csm/call`: `addon` is its id).
pub(crate) fn caller_json(state: &crate::state::State, caller: &Caller) -> Value {
    match caller {
        Caller::Ui => json!({"kind": "ui"}),
        Caller::Cli => json!({"kind": "cli"}),
        Caller::External { .. } if addon_of(caller).is_some() => {
            json!({"kind": "addon", "addon": addon_of(caller).unwrap_or_default()})
        }
        Caller::External { client, session_id } => {
            let mut o = serde_json::Map::new();
            o.insert("kind".into(), json!("external"));
            o.insert("client".into(), json!(clean_text(client, 80)));
            if let Some(sid) = session_id {
                o.insert("session_id".into(), json!(sid));
                if let Some(s) = state.app.sessions.iter().find(|s| &s.id == sid) {
                    o.insert("session_name".into(), json!(clean_text(&s.name, 80)));
                    o.insert("project".into(), json!(crate::snapshot::project_of(state, s)));
                }
            }
            Value::Object(o)
        }
    }
}

/// Engine thread: what `addon` (an enabled process add-on) declared in `[process] effects`.
pub(crate) fn declared_effects(cat: &addons::Catalogue, addon: &str) -> Vec<ProcessEffect> {
    addons::entry(cat, addon)
        .filter(|e| e.state == AddonState::Loaded)
        .and_then(|e| e.parsed.as_deref())
        .and_then(|p| p.process.as_ref())
        .map(|p| p.effects.iter().filter_map(|n| ProcessEffect::from_name(n)).collect())
        .unwrap_or_default()
}

/// Engine thread: drops what `addon` contributed at runtime (its remotes, and its marks on
/// sessions) if the process that did is not newer than `generation`.
pub(crate) fn clear_contributions(ctx: &mut Ctx<'_>, addon: &str, generation: u64) {
    let newer = ctx.engine.plugin_tool_gen(addon).is_some_and(|g| g > generation);
    if newer {
        return;
    }
    let had_remotes = ctx.state.rt.addons.remotes.remove(addon).is_some();
    let before = ctx.state.rt.plugin_marks.len();
    ctx.state.rt.plugin_marks.retain(|_, m| !(m.addon == addon && m.generation <= generation));
    if had_remotes || ctx.state.rt.plugin_marks.len() != before {
        ctx.state.touch();
    }
}

/// Engine thread: keeps a plugin's list of remotes if its add-on is still an enabled process
/// add-on and the list is not from an older process than the tools registered.
fn store_remotes(ctx: &mut Ctx<'_>, addon: &str, generation: u64, list: Vec<csm_core::addons::Remote>) {
    if !is_running_process(&ctx.state.rt.addons, addon) || ctx.engine.plugin_tool_gen(addon).is_some_and(|g| g > generation) {
        return;
    }
    if list.is_empty() {
        ctx.state.rt.addons.remotes.remove(addon);
    } else {
        ctx.state.rt.addons.remotes.insert(addon.to_owned(), list);
    }
    ctx.state.touch();
}

/// How one run of a child ended.
enum Exit {
    Stopped,
    Restart,
    Failed(String),
}

/// Engine thread: makes the running processes match the catalogue. Starts and stops them
/// through the host, and forgets the runtime and tools of add-ons that are no longer enabled.
pub(crate) fn sync(ctx: &mut Ctx<'_>) {
    let (save_path, clean) = (ctx.state.options.save_path.clone(), ctx.state.options.clean);
    let specs: Vec<PluginSpec> = ctx
        .state
        .rt
        .addons
        .entries
        .iter()
        .filter(|e| e.state == AddonState::Loaded)
        .filter_map(|e| {
            let p = e.parsed.as_deref()?;
            let proc = p.process.as_ref()?;
            let digest = e.digest.clone()?;
            let effects = proc.effects.iter().filter_map(|n| ProcessEffect::ALL.into_iter().find(|x| x.name() == n.as_str())).collect();
            let data_dir = if proc.effects.iter().any(|x| x == "stores_data") { data_dir_for(save_path.as_deref(), clean, &p.meta.id) } else { None };
            Some(PluginSpec { addon: p.meta.id.clone(), dir: e.path.clone(), argv: proc.argv.clone(), effects, data_dir, digest })
        })
        .collect();
    let wanted: Vec<String> = specs.iter().map(|s| s.addon.clone()).collect();
    ctx.state.rt.addons.runtime.retain(|id, _| wanted.contains(id));
    for owner in ctx.engine.plugin_tool_owners() {
        if !wanted.contains(&owner) {
            ctx.engine.clear_plugin_tools(&owner);
        }
    }
    // Disabled or removed add-ons: whatever they offered or set on sessions goes now.
    let gone: Vec<String> = ctx.state.rt.addons.remotes.keys().cloned().chain(ctx.state.rt.plugin_marks.values().map(|m| m.addon.clone())).filter(|a| !wanted.contains(a)).collect();
    for a in gone {
        clear_contributions(ctx, &a, u64::MAX);
    }
    // A disabled add-on's screens close; an enabled one keeps them across a restart.
    for owner in ctx.engine.host.screens().owners() {
        if !wanted.contains(&owner) {
            ctx.engine.host.screens().remove_addon(&owner);
        }
    }
    ctx.engine.host.sync(specs);
}

/// Engine thread: starts `addon` again.
pub(crate) fn restart(ctx: &mut Ctx<'_>, addon: &str) {
    ctx.engine.host.restart(addon);
}

/// Registry entries for a plugin's declared tools. A tool whose text cannot be kept (see
/// [`intern`]) is left out and logged.
pub(crate) fn dynamic_tools(tools: &[DeclaredTool]) -> Vec<DynamicTool> {
    tools
        .iter()
        .filter_map(|t| {
            let effects = t.effects.iter().fold(Effects::empty(), |acc, e| {
                acc | match e {
                    ProcessEffect::LaunchesProcess => Effects::LAUNCHES_PROCESS,
                    ProcessEffect::SendsInput => Effects::SENDS_INPUT,
                    ProcessEffect::ReadsSessionContent => Effects::READS_SESSION_CONTENT,
                    ProcessEffect::WritesFilesystem => Effects::WRITES_FILESYSTEM,
                    ProcessEffect::AccessesNetwork => Effects::ACCESSES_NETWORK,
                    ProcessEffect::StoresSecrets => Effects::STORES_SECRETS,
                    // Writes only inside its own folder: the class of `task_new`, no consent card.
                    ProcessEffect::StoresData => Effects::MUTATES_STATE,
                }
            });
            let (Some(id), Some(title), Some(summary)) = (intern(&t.id), intern(&t.title), intern(&t.summary)) else {
                log::error!("add-on tool `{}` was not registered: too many distinct tool texts this run", t.id);
                return None;
            };
            Some(DynamicTool {
                spec: ToolSpec {
                    id,
                    title,
                    summary,
                    toolset: Toolset::Addons,
                    effects,
                    input_schema: placeholder_schema,
                    run: placeholder_run,
                },
                schema: t.input_schema.clone(),
            })
        })
        .collect()
}

/// A registry entry needs a `fn`; the real schema is in the registry and the real run is in
/// the host. Neither is reached through these.
fn placeholder_schema() -> Value {
    crate::tool::empty_schema()
}

fn placeholder_run(_: &mut Ctx<'_>, _: Value) -> Reply {
    Err(ToolError::Failed("add-on tools run in their add-on".into()))
}

/// A `'static` copy of a string, shared by equal strings, so a restart of the same plugin does
/// not leak again. At most [`MAX_INTERNED`] distinct strings are kept for the life of the app;
/// beyond that `None` (the caller drops the tool), so a plugin that changes its text on every
/// start cannot grow memory without limit.
fn intern(s: &str) -> Option<&'static str> {
    static TABLE: Mutex<Option<HashSet<&'static str>>> = Mutex::new(None);
    let mut guard = TABLE.lock().unwrap_or_else(PoisonError::into_inner);
    intern_in(guard.get_or_insert_with(HashSet::new), MAX_INTERNED, s)
}

/// [`intern`] on a given table with a given cap, so the cap can be tested without filling the
/// process-wide table that other code (and tests running in parallel) share.
fn intern_in(table: &mut HashSet<&'static str>, cap: usize, s: &str) -> Option<&'static str> {
    if let Some(found) = table.get(s) {
        return Some(found);
    }
    if table.len() >= cap {
        return None;
    }
    let leaked: &'static str = Box::leak(s.to_owned().into_boxed_str());
    table.insert(leaked);
    Some(leaked)
}

/// Whether one wire line fits the protocol's cap. A line over it would make the plugin stop
/// reading, so CSM refuses to send it instead.
fn fits_line(line: &str) -> bool {
    line.len() <= proto::MAX_LINE_BYTES
}

/// One supervisor: runs the child, restarts it with backoff, and reports each change.
fn supervise(spec: PluginSpec, ctl: Receiver<Ctl>, env: Env) {
    let mut rt = PluginRuntime::new(env.generation);
    let mut failures: u32 = 0;
    loop {
        rt.state = RunState::Starting;
        rt.pid = None;
        env.report(&mut rt);
        let started = Instant::now();
        let exit = run_once(&spec, &ctl, &env, &mut rt);
        rt.pid = None;
        rt.tools.clear();
        env.apply_tools(Vec::new());
        env.clear_contributions();
        match exit {
            Exit::Stopped => {
                rt.state = RunState::Stopped;
                env.report(&mut rt);
                return;
            }
            Exit::Restart => {
                failures = 0;
                rt.restarts = rt.restarts.saturating_add(1);
            }
            Exit::Failed(msg) => {
                rt.last_error = Some(msg);
                rt.restarts = rt.restarts.saturating_add(1);
                if started.elapsed() >= STABLE_AFTER {
                    failures = 0;
                }
                failures = failures.saturating_add(1);
                if failures > MAX_RESTARTS {
                    rt.state = RunState::GaveUp;
                    env.report(&mut rt);
                    match ctl.recv() {
                        Ok(Ctl::Restart) => {
                            failures = 0;
                            continue;
                        }
                        _ => {
                            rt.state = RunState::Stopped;
                            env.report(&mut rt);
                            return;
                        }
                    }
                }
                rt.state = RunState::Crashed;
                env.report(&mut rt);
                match ctl.recv_timeout(backoff(failures)) {
                    Ok(Ctl::Stop) | Err(RecvTimeoutError::Disconnected) => {
                        rt.state = RunState::Stopped;
                        env.report(&mut rt);
                        return;
                    }
                    Ok(Ctl::Restart) => failures = 0,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }
}

/// The wait before restart number `failures` (1, 2, 4, 8, 16, then the cap).
fn backoff(failures: u32) -> Duration {
    let secs = 1u64.checked_shl(failures.saturating_sub(1)).unwrap_or(u64::MAX);
    Duration::from_secs(secs).min(MAX_BACKOFF)
}

/// What the stdout reader hands the supervisor.
enum Inbound {
    Line(String),
    /// A line over the cap: the process is stopped.
    TooLong,
}

/// Starts the child once and drives it until it stops, fails, or is told to.
fn run_once(spec: &PluginSpec, ctl: &Receiver<Ctl>, env: &Env, rt: &mut PluginRuntime) -> Exit {
    let program = spec.argv.first().map_or("the add-on", String::as_str).to_owned();
    let mut child = match crate::proc::spawn_piped(&spec.argv, &spec.dir) {
        Ok(c) => c,
        Err(e) => return Exit::Failed(format!("could not start `{program}`: {e}")),
    };
    rt.pid = Some(child.id());
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        stop_child(&mut child);
        return Exit::Failed("could not open the add-on's pipes".into());
    };
    let (out_tx, out_rx) = mpsc::sync_channel::<String>(OUT_QUEUE);
    thread::spawn(move || {
        let mut w = stdin;
        for line in out_rx {
            if io::write_line(&mut w, &line).is_err() {
                break;
            }
        }
    });
    let (in_tx, in_rx) = mpsc::sync_channel::<Inbound>(IN_QUEUE);
    thread::spawn(move || {
        let mut r = BufReader::new(stdout);
        loop {
            match io::read_line_capped(&mut r, proto::MAX_LINE_BYTES) {
                Ok(LineRead::Line(s)) => {
                    if in_tx.send(Inbound::Line(s)).is_err() {
                        break;
                    }
                }
                Ok(LineRead::TooLong) => {
                    let _ = in_tx.send(Inbound::TooLong);
                    break;
                }
                Ok(LineRead::Eof) | Err(_) => break,
            }
        }
    });
    let tail = env.tail.clone();
    thread::spawn(move || io::pump_stderr(stderr, &tail));

    let mut link: Option<Arc<Link>> = None;
    let exit = drive(spec, ctl, env, rt, &mut child, &in_rx, &out_tx, &mut link);
    // Whatever happened, the child is gone and anyone still waiting gets an answer.
    stop_child(&mut child);
    if let Some(l) = link {
        {
            let mut m = env.links.lock().unwrap_or_else(PoisonError::into_inner);
            if m.get(&env.addon).is_some_and(|x| Arc::ptr_eq(x, &l)) {
                m.remove(&env.addon);
            }
        }
        let waiting: Vec<Sender<Result<Value, RpcError>>> =
            l.pending.lock().unwrap_or_else(PoisonError::into_inner).drain().map(|(_, tx)| tx).collect();
        for tx in waiting {
            let _ = tx.send(Err(RpcError { code: proto::ERR_TOOL, message: "the add-on stopped before it answered".into(), data: Value::Null }));
        }
    }
    exit
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The message loop of one run. Returns when the child stops, misbehaves, times out, or is told
/// to stop or restart.
#[allow(clippy::too_many_arguments)]
fn drive(
    spec: &PluginSpec,
    ctl: &Receiver<Ctl>,
    env: &Env,
    rt: &mut PluginRuntime,
    child: &mut Child,
    in_rx: &Receiver<Inbound>,
    out_tx: &SyncSender<String>,
    link: &mut Option<Arc<Link>>,
) -> Exit {
    let init_id: u64 = 1;
    let deadline = Instant::now() + HANDSHAKE_TIMEOUT;
    // The data folder is made when the process starts (on demand, never for an add-on that did
    // not declare `stores_data`). If it cannot be made the plugin is told there is none.
    let data_dir = spec.data_dir.as_ref().and_then(|d| match std::fs::create_dir_all(d) {
        Ok(()) => Some(d.display().to_string()),
        Err(e) => {
            log::warn!("add-on {}: could not create its data folder {}: {e}", env.addon, d.display());
            None
        }
    });
    let init = json!({
        "api": proto::PROTOCOL_API,
        "csm_version": env!("CARGO_PKG_VERSION"),
        "capabilities": proto::capabilities(),
        "data_dir": data_dir,
    });
    if out_tx.send(proto::request_line(init_id, "initialize", init)).is_err() {
        return Exit::Failed("could not send initialize".into());
    }
    loop {
        match ctl.try_recv() {
            Ok(Ctl::Stop) | Err(TryRecvError::Disconnected) => return Exit::Stopped,
            Ok(Ctl::Restart) => return Exit::Restart,
            Err(TryRecvError::Empty) => {}
        }
        if link.is_none() && Instant::now() >= deadline {
            return Exit::Failed(format!("did not answer initialize within {} seconds", HANDSHAKE_TIMEOUT.as_secs()));
        }
        if let Some(l) = link.as_ref() {
            env.screens.flush();
            plugin_screens::flush_held(&env.screens, l, &env.addon);
        }
        match in_rx.recv_timeout(POLL) {
            Ok(Inbound::TooLong) => return Exit::Failed(format!("sent a message over {} bytes; stopped", proto::MAX_LINE_BYTES)),
            Ok(Inbound::Line(line)) => {
                let msg = match proto::parse_line(&line) {
                    Ok(m) => m,
                    Err(e) => return Exit::Failed(format!("sent a bad message ({e}); stopped")),
                };
                match msg {
                    Incoming::Response { id, outcome } if id == init_id && link.is_none() => {
                        let result = match outcome {
                            Ok(v) => v,
                            Err(e) => return Exit::Failed(format!("initialize failed: {}", clean_text(&e.message, 200))),
                        };
                        let hs = match proto::parse_handshake(&result, &env.addon, &spec.effects) {
                            Ok(h) => h,
                            Err(e) => return Exit::Failed(format!("initialize refused: {e}")),
                        };
                        let new_link = Arc::new(Link {
                            out: out_tx.clone(),
                            pending: Mutex::new(HashMap::new()),
                            next_id: AtomicU64::new(init_id + 1),
                            inflight: Arc::new(AtomicUsize::new(0)),
                            subs: hs.subscriptions.clone(),
                        });
                        env.links.lock().unwrap_or_else(PoisonError::into_inner).insert(env.addon.clone(), new_link.clone());
                        *link = Some(new_link);
                        rt.state = RunState::Running;
                        rt.name = Some(hs.name);
                        rt.tools = hs.tools.iter().map(|t| t.id.clone()).collect();
                        rt.problems = hs.problems;
                        rt.subscriptions = hs.subscriptions.iter().map(|s| (*s).to_owned()).collect();
                        env.apply_tools(dynamic_tools(&hs.tools));
                        screens_ready(env, link.as_ref(), rt.name.clone(), hs.screens);
                        env.report(rt);
                    }
                    Incoming::Response { id, outcome } => {
                        let waiter = link.as_ref().and_then(|l| l.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id));
                        if let Some(tx) = waiter {
                            let _ = tx.send(outcome);
                        }
                    }
                    Incoming::Request { id, method, params } => {
                        if link.is_none() {
                            let _ = out_tx.try_send(proto::response_err(&id, proto::ERR_METHOD_NOT_FOUND, "send initialize first", Value::Null));
                        } else if method == "csm/call" {
                            csm_call(env, out_tx, id, &params);
                        } else {
                            let text = format!("no method `{}`", clean_text(&method, 64));
                            let _ = out_tx.try_send(proto::response_err(&id, proto::ERR_METHOD_NOT_FOUND, &text, Value::Null));
                        }
                    }
                    Incoming::Notification { method, params } => {
                        if method == "csm/remotes_changed" {
                            match proto::parse_remotes(&params, &env.addon) {
                                Ok((list, problems)) => {
                                    for p in problems {
                                        log::warn!("add-on {}: {p}", env.addon);
                                    }
                                    env.set_remotes(list);
                                }
                                Err(e) => log::warn!("add-on {}: csm/remotes_changed ignored: {e}", env.addon),
                            }
                        } else if method == "screen/render" || method == "screen/patch" {
                            screen_update(env, link.as_ref(), &method, &params);
                        } else if method == "csm/log" {
                            let text = params.get("message").and_then(Value::as_str).map(|s| clean_text(s, 300)).unwrap_or_default();
                            log::info!("add-on {}: {text}", env.addon);
                        }
                    }
                }
            }
            Err(RecvTimeoutError::Timeout) => {
                if let Ok(Some(status)) = child.try_wait() {
                    return Exit::Failed(format!("exited ({status})"));
                }
            }
            Err(RecvTimeoutError::Disconnected) => {
                // Its stdout closed. It may still be running, so it is killed rather than waited
                // for: a wait could hang and ignore a stop or a disable.
                let _ = child.kill();
                return Exit::Failed(match child.wait().ok() {
                    Some(s) => format!("closed its output and exited ({s})"),
                    None => "closed its output".into(),
                });
            }
        }
    }
}

/// The handshake declared `screens`: offer them, and open again the instances an earlier process
/// of this add-on left.
fn screens_ready(env: &Env, link: Option<&Arc<Link>>, name: Option<String>, screens: Vec<csm_core::addons::screen::ScreenDecl>) {
    let name = name.unwrap_or_else(|| env.addon.clone());
    let reopen = env.screens.set_declared(&env.addon, env.generation, &name, screens);
    let Some(link) = link else { return };
    for (instance, screen, context) in reopen {
        plugin_screens::request_open(env.screens.clone(), link.clone(), env.addon.clone(), env.generation, instance, screen, context, None);
    }
}

/// A `screen/render` or `screen/patch` from the plugin: validated and stored. A refusal is told to
/// the plugin (`screen/error`); a patch on the wrong base asks for a full render (`screen/refresh`).
fn screen_update(env: &Env, link: Option<&Arc<Link>>, method: &str, params: &Value) {
    use plugin_screens::Applied;
    let applied = env.screens.update(&env.addon, env.generation, params, method == "screen/patch");
    let Some(link) = link else { return };
    let instance = params.get("instance").and_then(Value::as_str).unwrap_or_default();
    match applied {
        Applied::Yes | Applied::Ignored => {}
        Applied::Refresh => {
            link.notify("screen/refresh", json!({"instance": instance}));
        }
        Applied::Refused(msg) => {
            log::warn!("add-on {}: {method} refused: {msg}", env.addon);
            link.notify("screen/error", json!({"instance": instance, "message": clean_text(&msg, MAX_MESSAGE)}));
        }
    }
}

/// A `csm/call` request: runs the tool through the engine as the add-on's own caller, on a call
/// thread, and answers when it is done. At most [`MAX_CSM_CALLS`] run at once per add-on.
fn csm_call(env: &Env, out: &SyncSender<String>, id: Value, params: &Value) {
    let Some(tool) = params.get("tool").and_then(Value::as_str).map(str::to_owned) else {
        let _ = out.try_send(proto::response_err(&id, proto::ERR_INVALID_PARAMS, "csm/call needs a `tool`", Value::Null));
        return;
    };
    let args = params.get("params").cloned().unwrap_or(Value::Null);
    let Some(slot) = CsmSlot::take(&env.addon) else {
        let _ = out.try_send(proto::response_err(&id, proto::ERR_BUSY, "too many calls in flight from this add-on; wait for them", Value::Null));
        return;
    };
    let call = env.call.clone();
    let out = out.clone();
    let caller = Caller::External { client: format!("add-on {}", env.addon), session_id: None };
    let spawned = thread::Builder::new().name("csm-plugin-csm-call".into()).spawn(move || {
        let _slot = slot;
        let line = match call(caller, &tool, args) {
            Ok(ToolOutput { message, data }) => {
                let line = proto::response_ok(&id, json!({"message": clean_text(&message, MAX_MESSAGE), "data": data}));
                if fits_line(&line) {
                    line
                } else {
                    proto::response_err(&id, proto::ERR_TOOL, "the result is over 1 MiB, which an add-on cannot read; ask for less", Value::Null)
                }
            }
            Err(e) => proto::response_err(&id, proto::ERR_TOOL, &e.to_string(), serde_json::to_value(&e).unwrap_or(Value::Null)),
        };
        let _ = out.send(line);
    });
    if let Err(e) = spawned {
        log::error!("could not start a csm/call thread: {e}");
    }
}

/// The `csm/call`s in flight, per add-on, across all threads. Held while a call runs.
static CSM_IN_FLIGHT: Mutex<Option<HashMap<String, usize>>> = Mutex::new(None);

struct CsmSlot {
    addon: String,
}

impl CsmSlot {
    /// A slot for one more call, or `None` when the add-on already has [`MAX_CSM_CALLS`].
    fn take(addon: &str) -> Option<CsmSlot> {
        let mut m = CSM_IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        let map = m.get_or_insert_with(HashMap::new);
        let n = map.entry(addon.to_owned()).or_insert(0);
        if *n >= MAX_CSM_CALLS {
            return None;
        }
        *n += 1;
        Some(CsmSlot { addon: addon.to_owned() })
    }
}

impl Drop for CsmSlot {
    fn drop(&mut self) {
        let mut m = CSM_IN_FLIGHT.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = m.as_mut().and_then(|map| map.get_mut(&self.addon)) {
            *n = n.saturating_sub(1);
        }
    }
}

/// A tool call from the app to a running plugin: writes `tools/call` and waits for the answer.
fn call_tool(links: &Links, addon: &str, tool: &str, params: Value, caller: Value) -> Reply {
    let link = links
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(addon)
        .cloned()
        .ok_or_else(|| ToolError::Refused(format!("the add-on `{addon}` is not running; enable it, or restart it in Settings > Add-ons")))?;
    if link.inflight.fetch_add(1, Ordering::AcqRel) >= MAX_CALLS_IN_FLIGHT {
        link.inflight.fetch_sub(1, Ordering::AcqRel);
        return Err(ToolError::Refused(format!("the add-on `{addon}` has {MAX_CALLS_IN_FLIGHT} calls in flight; wait for them")));
    }
    let _slot = InflightSlot(link.inflight.clone());
    let id = link.next_id.fetch_add(1, Ordering::AcqRel);
    let line = proto::request_line(id, "tools/call", json!({"name": tool, "arguments": params, "caller": caller}));
    if !fits_line(&line) {
        return Err(ToolError::InvalidParams(format!("the arguments for `{tool}` are over 1 MiB, which an add-on cannot read; send less")));
    }
    let (tx, rx) = mpsc::channel();
    link.pending.lock().unwrap_or_else(PoisonError::into_inner).insert(id, tx);
    if link.out.try_send(line).is_err() {
        link.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
        return Err(ToolError::Failed(format!("the add-on `{addon}` is not taking requests right now; try again")));
    }
    match rx.recv_timeout(TOOL_TIMEOUT) {
        Ok(Ok(result)) => reply_from(tool, result),
        Ok(Err(e)) => Err(ToolError::Failed(format!("`{tool}`: {}", clean_text(&e.message, MAX_MESSAGE)))),
        Err(RecvTimeoutError::Timeout) => {
            link.pending.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            Err(ToolError::TimedOut(format!("`{tool}` did not answer within {} seconds", TOOL_TIMEOUT.as_secs())))
        }
        Err(RecvTimeoutError::Disconnected) => Err(ToolError::Failed(format!("`{tool}`: the add-on stopped before it answered"))),
    }
}

/// The result of a tool call: `{"message": ..., "data": ...}`, both optional.
fn reply_from(tool: &str, result: Value) -> Reply {
    let Some(obj) = result.as_object() else {
        return Err(ToolError::Failed(format!("`{tool}` answered with something other than an object")));
    };
    let message = obj
        .get("message")
        .and_then(Value::as_str)
        .map(|s| clean_text(s, MAX_MESSAGE))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "Done.".to_owned());
    Ok(ToolOutput::new(message, obj.get("data").cloned().unwrap_or(Value::Null)))
}

/// One count held while a call waits: released when the holder is dropped.
struct InflightSlot(Arc<AtomicUsize>);

impl InflightSlot {
    /// A count for one more waiting call, or `None` when `limit` are already waiting.
    fn take(counter: &Arc<AtomicUsize>, limit: usize) -> Option<InflightSlot> {
        if counter.fetch_add(1, Ordering::AcqRel) >= limit {
            counter.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        Some(InflightSlot(counter.clone()))
    }
}

impl Drop for InflightSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The add-on id of a dynamic tool (`addon.name`), if the id has the add-on form.
pub(crate) fn owner_of(id: &str) -> Option<&str> {
    is_add_on_tool_id(id).then(|| id.split_once('.').map_or("", |(a, _)| a))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn the_data_folder_is_beside_the_state_file_and_absent_when_clean_or_isolated() {
        let p = std::path::Path::new("/x/y/state.json");
        assert_eq!(data_dir_for(Some(p), false, "review"), Some(PathBuf::from("/x/y").join("addon-data").join("review")));
        assert_eq!(data_dir_for(Some(p), true, "review"), None, "--clean writes nothing");
        assert_eq!(data_dir_for(None, false, "review"), None, "an isolated run has no state folder");
        assert_eq!(data_dir_for(Some(p), false, "../evil"), None, "only a valid add-on id names a folder");
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), Duration::from_secs(1));
        assert_eq!(backoff(2), Duration::from_secs(2));
        assert_eq!(backoff(3), Duration::from_secs(4));
        assert_eq!(backoff(40), MAX_BACKOFF);
    }

    #[test]
    fn interning_shares_equal_strings() {
        let a = intern("demo.echo").unwrap();
        let b = intern("demo.echo").unwrap();
        assert!(std::ptr::eq(a, b));
    }

    #[test]
    fn interning_stops_at_the_cap_but_still_finds_known_strings() {
        // A table of its own: filling the process-wide one would starve tests running in
        // parallel (it did on CI).
        let mut table = HashSet::new();
        let a = intern_in(&mut table, 2, "cap-test.a").unwrap();
        assert!(intern_in(&mut table, 2, "cap-test.b").is_some());
        assert!(intern_in(&mut table, 2, "cap-test.fresh-after-the-cap").is_none(), "a new string past the cap is refused");
        assert!(std::ptr::eq(intern_in(&mut table, 2, "cap-test.a").unwrap(), a), "a known string is still shared");
    }

    #[test]
    fn call_threads_are_bounded() {
        let counter = Arc::new(AtomicUsize::new(0));
        let held: Vec<InflightSlot> = (0..MAX_CALL_THREADS).filter_map(|_| InflightSlot::take(&counter, MAX_CALL_THREADS)).collect();
        assert_eq!(held.len(), MAX_CALL_THREADS);
        assert!(InflightSlot::take(&counter, MAX_CALL_THREADS).is_none());
        drop(held);
        assert_eq!(counter.load(Ordering::Acquire), 0, "every refused take gave its count back");
    }

    #[test]
    fn declared_effects_map_to_tool_effects() {
        let tools = vec![DeclaredTool {
            id: "demo.w".into(),
            title: "W".into(),
            summary: String::new(),
            input_schema: json!({"type": "object"}),
            effects: vec![ProcessEffect::WritesFilesystem, ProcessEffect::AccessesNetwork],
        }];
        let d = dynamic_tools(&tools);
        assert_eq!(d[0].spec.toolset, Toolset::Addons);
        assert!(d[0].spec.effects.contains(Effects::WRITES_FILESYSTEM | Effects::ACCESSES_NETWORK));
        assert!(d[0].spec.effects.needs_consent());
        let none = vec![DeclaredTool { effects: Vec::new(), ..tools[0].clone() }];
        assert!(!dynamic_tools(&none)[0].spec.effects.needs_consent());
    }

    #[test]
    fn csm_call_slots_are_capped_and_released() {
        let name = "slot-test-addon";
        let held: Vec<CsmSlot> = (0..MAX_CSM_CALLS).filter_map(|_| CsmSlot::take(name)).collect();
        assert_eq!(held.len(), MAX_CSM_CALLS);
        assert!(CsmSlot::take(name).is_none());
        drop(held);
        assert!(CsmSlot::take(name).is_some());
    }

    #[test]
    fn owner_is_the_prefix() {
        assert_eq!(owner_of("demo.echo"), Some("demo"));
        assert_eq!(owner_of("session_list"), None);
    }
}
