//! The engine thread: one owner of all state, every action a tool call.
//!
//! - [`Engine::call`] blocks (CLI, MCP, tests). [`Engine::submit`] returns a receiver the UI polls
//!   once a frame, so the UI thread never waits on the engine.
//! - Calls run one at a time on the engine thread, in arrival order. A tool must not block: slow
//!   work (spawning, git, waiting for exit) goes to a worker that reports back with
//!   [`Engine::post`] / an event.
//! - An [`Caller::External`] call whose effects [`Effects::needs_consent`] is parked until the
//!   person answers in the app (`consent_answer`, UI only) or it times out. A grant ("always for
//!   this client") skips the question next time.
//! - A panicking tool becomes [`ToolError::Failed`]; the engine thread keeps running.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::events::{Event, Events};
use crate::registry::Registry;
use crate::session::TerminalHandle;
use crate::snapshot::Snapshot;
use crate::state::State;
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

/// How long an external call waits for the person to answer a consent question.
pub const CONSENT_TIMEOUT: Duration = Duration::from_secs(120);

pub type Reply = Result<ToolOutput, ToolError>;

/// What a tool sees while it runs.
pub struct Ctx<'a> {
    pub state: &'a mut State,
    pub caller: &'a Caller,
    pub events: &'a Events,
    pub registry: &'a Registry,
    /// Lets a tool hand work to a background thread that later re-enters the engine
    /// (`engine.post(...)`) without holding the state.
    pub engine: &'a Engine,
    /// The reply channel of the running call, for tools that answer later ([`Ctx::park`]).
    reply: Option<Sender<Reply>>,
}

impl Ctx<'_> {
    /// Takes over the reply of the running call: the tool's own return value is then ignored,
    /// and the reply is sent later with [`ParkedReply::send`] (from a job or an event). `None`
    /// outside a tool call (a posted job has nobody to answer).
    pub fn park(&mut self) -> Option<ParkedReply> {
        self.reply.take().map(ParkedReply)
    }
}

/// The reply of a call that completes later (`session_wait`, a restart, a screen read).
pub struct ParkedReply(Sender<Reply>);

impl ParkedReply {
    /// Sent from the engine thread (a tool, or a job posted back by a worker), the reply is held
    /// until the next snapshot is published, so a caller that reads `engine.view()` right after
    /// the reply sees the change it describes. From any other thread it goes out at once: a worker
    /// that hasn't touched the state has nothing to publish.
    pub fn send(self, reply: Reply) {
        if ON_ENGINE_THREAD.with(Cell::get) {
            DEFERRED.with(|d| d.borrow_mut().push((self.0, reply)));
        } else {
            let _ = self.0.send(reply);
        }
    }
}

thread_local! {
    static ON_ENGINE_THREAD: Cell<bool> = const { Cell::new(false) };
    static DEFERRED: RefCell<Vec<(Sender<Reply>, Reply)>> = const { RefCell::new(Vec::new()) };
}

/// Send the replies held by [`ParkedReply::send`]. Called right after a snapshot is published
/// (`lifecycle::after_step`) and when the engine thread ends.
pub(crate) fn flush_deferred_replies() {
    let held = DEFERRED.with(|d| std::mem::take(&mut *d.borrow_mut()));
    for (tx, reply) in held {
        let _ = tx.send(reply);
    }
}

impl std::fmt::Debug for ParkedReply {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ParkedReply")
    }
}

/// Work posted back to the engine thread by a background worker.
pub type Job = Box<dyn FnOnce(&mut Ctx<'_>) + Send>;

enum Msg {
    Call { caller: Caller, id: String, params: Value, reply: Sender<Reply> },
    Post(Job),
    Shutdown,
}

/// A call parked until the person answers.
struct Pending {
    caller: Caller,
    spec: ToolSpec,
    params: Value,
    reply: Sender<Reply>,
    asked_at: Instant,
}

/// A remembered "always allow": this client may call this tool without asking. Kept in the
/// state (`AppState::mcp_grants`), so it survives a restart; `grant_list` / `grant_revoke`.
pub use csm_core::models::McpGrant as Grant;

/// Cheap to clone; all clones talk to the same engine thread.
#[derive(Clone)]
pub struct Engine {
    tx: Sender<Msg>,
    events: Events,
    registry: Arc<Registry>,
    joined: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
    view: Arc<Mutex<Arc<Snapshot>>>,
    /// Text a session asked to put on the clipboard (OSC 52), waiting for the UI (issue #156).
    /// Kept here, not in an event: events reach IPC clients, the clipboard must not.
    clipboard: Arc<Mutex<Option<String>>>,
}

impl Engine {
    /// Start the engine thread with `state` and any extra tools (each a `specs()` list). Every
    /// engine tool (sessions, groups, layout, app) is always registered; `tools` adds more.
    /// The restore loop starts right away when `state` has sessions to restore.
    pub fn start(state: State, tools: Vec<ToolSpec>) -> Result<Engine, ToolError> {
        let registry = Arc::new(Registry::new(builtin_specs().into_iter().chain(crate::tools::specs()).chain(tools).collect())?);
        let (tx, rx) = mpsc::channel::<Msg>();
        let events = Events::default();
        let engine = Engine {
            tx,
            events: events.clone(),
            registry: registry.clone(),
            joined: Arc::new(Mutex::new(None)),
            view: Arc::new(Mutex::new(Arc::new(Snapshot::default()))),
            clipboard: Arc::new(Mutex::new(None)),
        };
        let worker = engine.clone();
        let (ready_tx, ready_rx) = mpsc::channel::<()>();
        let handle = thread::Builder::new()
            .name("csm-engine".into())
            .spawn(move || run_loop(worker, state, rx, ready_tx))
            .map_err(|e| ToolError::Failed(format!("could not start the engine thread: {e}")))?;
        *engine.joined.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
        // Return only once the first snapshot is published, so `view()` right after `start` is
        // the loaded state, never the empty placeholder (a slow machine used to show an empty app
        // for its first frames). Bounded: start-up does no blocking work on this thread.
        if ready_rx.recv_timeout(Duration::from_secs(10)).is_err() {
            log::warn!("the engine's first snapshot took more than 10 s");
        }
        Ok(engine)
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Subscribe to engine events (UI, IPC clients, the MCP bridge).
    pub fn subscribe(&self) -> Receiver<Event> {
        self.events.subscribe()
    }

    /// The current read model, republished after every state change. One short lock to clone
    /// an `Arc`: safe to call every frame from the UI thread.
    pub fn view(&self) -> Arc<Snapshot> {
        self.view.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    /// The terminal of a live session, for rendering and widget-level input.
    pub fn terminal(&self, session_id: &str) -> Option<TerminalHandle> {
        self.view().terminal(session_id).cloned()
    }

    /// The text a session last asked to put on the clipboard (OSC 52), once: the UI calls this
    /// when it sees [`Event::ClipboardOffered`] and sets the clipboard itself. One short lock,
    /// like [`Engine::view`].
    pub fn take_clipboard(&self) -> Option<String> {
        self.clipboard.lock().unwrap_or_else(PoisonError::into_inner).take()
    }

    pub(crate) fn offer_clipboard(&self, text: String) {
        *self.clipboard.lock().unwrap_or_else(PoisonError::into_inner) = Some(text);
    }

    pub(crate) fn publish(&self, snapshot: Arc<Snapshot>) {
        *self.view.lock().unwrap_or_else(PoisonError::into_inner) = snapshot;
    }

    pub(crate) fn events(&self) -> &Events {
        &self.events
    }

    /// Run a tool and wait for its result. Never call this from the UI thread or from inside a tool.
    pub fn call(&self, caller: Caller, id: &str, params: Value) -> Reply {
        let rx = self.submit(caller, id, params);
        rx.recv().unwrap_or_else(|_| Err(ToolError::Failed("the engine stopped".into())))
    }

    /// Queue a tool call; poll the receiver (`try_recv`) for the result.
    pub fn submit(&self, caller: Caller, id: &str, params: Value) -> Receiver<Reply> {
        let (reply, rx) = mpsc::channel();
        let msg = Msg::Call { caller, id: id.to_string(), params, reply: reply.clone() };
        if self.tx.send(msg).is_err() {
            let _ = reply.send(Err(ToolError::Failed("the engine stopped".into())));
        }
        rx
    }

    /// Run `job` on the engine thread (from a background worker).
    pub fn post(&self, job: Job) {
        let _ = self.tx.send(Msg::Post(job));
    }

    /// Stop the engine thread and wait for it.
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
        let handle = self.joined.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

/// How often the engine wakes with nothing to do (fallback starts, waits, heuristics, saves).
const TICK: Duration = Duration::from_millis(100);

fn run_loop(engine: Engine, mut state: State, rx: Receiver<Msg>, ready: Sender<()>) {
    ON_ENGINE_THREAD.with(|f| f.set(true));
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut next_id: u64 = 1;
    {
        let mut c = ctx(&mut state, &Caller::Ui, &engine, None);
        if catch_unwind(AssertUnwindSafe(|| crate::lifecycle::on_start(&mut c))).is_err() {
            engine.events.emit(Event::Error { message: "Restoring the sessions failed unexpectedly.".into() });
        }
    }
    state.rt.dirty = true;
    crate::lifecycle::after_step(&mut state, &engine);
    let _ = ready.send(());
    loop {
        let msg = match rx.recv_timeout(TICK) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        expire(&mut pending, &engine.events);
        {
            let mut c = ctx(&mut state, &Caller::Ui, &engine, None);
            if catch_unwind(AssertUnwindSafe(|| crate::lifecycle::tick(&mut c))).is_err() {
                engine.events.emit(Event::Error { message: "A scheduled task failed unexpectedly.".into() });
            }
        }
        if let Some(msg) = msg
            && !handle(msg, &engine, &mut state, &mut pending, &mut next_id)
        {
            break;
        }
        crate::lifecycle::after_step(&mut state, &engine);
    }
    for (_, p) in pending.drain() {
        let _ = p.reply.send(Err(ToolError::Failed("the app is closing".into())));
    }
    crate::lifecycle::on_exit(&mut state);
    flush_deferred_replies();
}

/// One message. Returns false to stop the loop.
fn handle(msg: Msg, engine: &Engine, state: &mut State, pending: &mut HashMap<u64, Pending>, next_id: &mut u64) -> bool {
    {
        match msg {
            Msg::Shutdown => return false,
            Msg::Post(job) => {
                let mut ctx = ctx(state, &Caller::Ui, engine, None);
                if catch_unwind(AssertUnwindSafe(|| job(&mut ctx))).is_err() {
                    engine.events.emit(Event::Error { message: "A background task failed unexpectedly.".into() });
                }
            }
            Msg::Call { caller, id, params, reply } => {
                // Consent bookkeeping is handled here, not in a tool, so no tool can skip it.
                match id.as_str() {
                    "consent_answer" => {
                        let r = answer(&caller, params, pending, state, engine);
                        let _ = reply.send(r);
                        return true;
                    }
                    "consent_list" => {
                        let list: Vec<Value> = pending.iter().map(|(k, p)| pending_json(*k, p)).collect();
                        let _ = reply.send(Ok(ToolOutput::new(format!("{} waiting", list.len()), json!(list))));
                        return true;
                    }
                    _ => {}
                }
                let Some(spec) = engine.registry.get(&id).copied() else {
                    let _ = reply.send(Err(ToolError::UnknownTool(id)));
                    return true;
                };
                // Rules that hold whatever the person would answer are checked before asking.
                if let Some(err) = crate::tools::precheck(spec.id, &caller, &params, state) {
                    let _ = reply.send(Err(err));
                    return true;
                }
                if let Caller::External { client, .. } = &caller {
                    let granted = state.app.mcp_grants.iter().any(|g| &g.client == client && g.tool == spec.id);
                    if spec.effects.needs_consent() && !granted {
                        let key = *next_id;
                        *next_id = next_id.saturating_add(1);
                        let p = Pending { caller: caller.clone(), spec, params, reply, asked_at: Instant::now() };
                        engine.events.emit(Event::ConsentRequested { request: pending_json(key, &p) });
                        pending.insert(key, p);
                        return true;
                    }
                }
                run_tool(&spec, state, &caller, params, engine, reply);
            }
        }
    }
    true
}

fn ctx<'a>(state: &'a mut State, caller: &'a Caller, engine: &'a Engine, reply: Option<Sender<Reply>>) -> Ctx<'a> {
    Ctx { state, caller, events: &engine.events, registry: &engine.registry, engine, reply }
}

/// Runs a tool and answers `reply`, unless the tool parked it.
fn run_tool(spec: &ToolSpec, state: &mut State, caller: &Caller, params: Value, engine: &Engine, reply: Sender<Reply>) {
    let mut c = ctx(state, caller, engine, Some(reply));
    let run = spec.run;
    let r = match catch_unwind(AssertUnwindSafe(|| run(&mut c, params))) {
        Ok(r) => r,
        Err(_) => Err(ToolError::Failed(format!("`{}` failed unexpectedly; nothing else was affected", spec.id))),
    };
    let reply = c.reply.take();
    drop(c);
    if let Some(reply) = reply {
        // Publish first, so a caller that reads `view()` after the reply sees the change.
        crate::lifecycle::after_step(state, engine);
        let _ = reply.send(r);
    }
}

fn pending_json(key: u64, p: &Pending) -> Value {
    let (client, session_id) = match &p.caller {
        Caller::External { client, session_id } => (client.clone(), session_id.clone()),
        _ => (String::new(), None),
    };
    json!({
        "request_id": key,
        "client": client,
        "from_session_id": session_id,
        "tool": p.spec.id,
        "title": p.spec.title,
        "summary": p.spec.summary,
        "effects": p.spec.effects,
        "params": p.params,
    })
}

fn expire(pending: &mut HashMap<u64, Pending>, events: &Events) {
    let now = Instant::now();
    let stale: Vec<u64> = pending.iter().filter(|(_, p)| now.duration_since(p.asked_at) >= CONSENT_TIMEOUT).map(|(k, _)| *k).collect();
    for k in stale {
        if let Some(p) = pending.remove(&k) {
            let _ = p.reply.send(Err(ToolError::TimedOut(format!("nobody answered whether `{}` may run", p.spec.id))));
            events.emit(Event::ConsentResolved { request_id: k });
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConsentAnswerParams {
    request_id: u64,
    /// `allow_once`, `allow_always` (this client, this tool) or `deny`.
    answer: ConsentChoice,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
enum ConsentChoice {
    AllowOnce,
    AllowAlways,
    Deny,
}

fn answer(caller: &Caller, params: Value, pending: &mut HashMap<u64, Pending>, state: &mut State, engine: &Engine) -> Reply {
    // Only the person, in the app, can answer. An MCP client must never approve its own request.
    if *caller != Caller::Ui {
        return Err(ToolError::Refused("consent is answered in the app, not over MCP or the CLI".into()));
    }
    let p: ConsentAnswerParams = parse_params(params)?;
    let Some(req) = pending.remove(&p.request_id) else {
        return Err(ToolError::NotFound(format!("no waiting request {}", p.request_id)));
    };
    engine.events.emit(Event::ConsentResolved { request_id: p.request_id });
    match p.answer {
        ConsentChoice::Deny => {
            let _ = req.reply.send(Err(ToolError::Declined(format!("`{}` was not allowed", req.spec.id))));
        }
        choice => {
            if choice == ConsentChoice::AllowAlways
                && let Caller::External { client, .. } = &req.caller
            {
                let g = Grant { client: client.clone(), tool: req.spec.id.to_string() };
                if !state.app.mcp_grants.contains(&g) {
                    state.app.mcp_grants.push(g);
                    state.persist();
                }
            }
            // The state may have changed while the person decided: check the rules again.
            match crate::tools::precheck(req.spec.id, &req.caller, &req.params, state) {
                Some(err) => {
                    let _ = req.reply.send(Err(err));
                }
                None => run_tool(&req.spec, state, &req.caller, req.params, engine, req.reply),
            }
        }
    }
    Ok(ToolOutput::message("Answered."))
}

/// Tools every build has. `consent_answer` and `consent_list` are intercepted by the loop above
/// (their `run` is never reached) but are registered so they are listed and have schemas.
fn builtin_specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "tool_list",
            title: "List tools",
            summary: "Every tool: id, title, summary, toolset and effects. Filter by toolset.",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<ToolListParams>,
            run: tool_list,
        },
        ToolSpec {
            id: "consent_list",
            title: "Waiting approvals",
            summary: "Requests from MCP clients waiting for the person to allow or deny them.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: |_, _| Err(ToolError::Failed("handled by the engine loop".into())),
        },
        ToolSpec {
            id: "consent_answer",
            title: "Answer an approval",
            summary: "Allow once, allow always for this client, or deny a waiting request. App only.",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE,
            input_schema: schema_of::<ConsentAnswerParams>,
            run: |_, _| Err(ToolError::Failed("handled by the engine loop".into())),
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ToolListParams {
    /// Only this toolset (`core`, `groups`, `layout`, `runs`, `git`, `notes`, `search`, `app`, `tasks`).
    toolset: Option<String>,
}

fn tool_list(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ToolListParams = parse_params(params)?;
    let filter = match p.toolset.as_deref() {
        None => None,
        Some(s) => Some(Toolset::from_id(s).ok_or_else(|| ToolError::InvalidParams(format!("no toolset `{s}`")))?),
    };
    let list: Vec<Value> = ctx
        .registry
        .all()
        .iter()
        .filter(|s| filter.is_none_or(|t| s.toolset == t))
        .map(|s| json!({"id": s.id, "title": s.title, "summary": s.summary, "toolset": s.toolset, "effects": s.effects}))
        .collect();
    Ok(ToolOutput::new(format!("{} tools", list.len()), json!(list)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ext() -> Caller {
        Caller::External { client: "test-client".into(), session_id: None }
    }

    fn sensitive() -> ToolSpec {
        ToolSpec {
            id: "test_send",
            title: "Send",
            summary: "test",
            toolset: Toolset::Core,
            effects: Effects::SENDS_INPUT,
            input_schema: empty_schema,
            run: |_, _| Ok(ToolOutput::message("sent")),
        }
    }

    fn panicky() -> ToolSpec {
        #[allow(clippy::panic)]
        fn boom(_: &mut Ctx<'_>, _: Value) -> Reply {
            panic!("boom")
        }
        ToolSpec {
            id: "test_boom",
            title: "Boom",
            summary: "test",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: boom,
        }
    }

    #[test]
    fn ui_calls_run_without_consent() {
        let e = Engine::start(State::default(), vec![sensitive()]).unwrap();
        assert_eq!(e.call(Caller::Ui, "test_send", Value::Null).unwrap().message, "sent");
        e.shutdown();
    }

    #[test]
    fn external_sensitive_call_waits_for_the_person() {
        let e = Engine::start(State::default(), vec![sensitive()]).unwrap();
        let events = e.subscribe();
        let rx = e.submit(ext(), "test_send", Value::Null);
        let Ok(Event::ConsentRequested { request }) = events.recv_timeout(Duration::from_secs(5)) else { panic!("expected a consent request") };
        assert!(rx.try_recv().is_err(), "must not run before the answer");
        // An MCP client cannot answer its own request.
        let id = request["request_id"].clone();
        let refused = e.call(ext(), "consent_answer", json!({"request_id": id, "answer": "allow_once"}));
        assert!(matches!(refused, Err(ToolError::Refused(_))));
        e.call(Caller::Ui, "consent_answer", json!({"request_id": id, "answer": "allow_always"})).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap().message, "sent");
        // Granted: the next call runs straight away.
        assert!(e.call(ext(), "test_send", Value::Null).is_ok());
        e.shutdown();
    }

    #[test]
    fn denied_call_returns_declined() {
        let e = Engine::start(State::default(), vec![sensitive()]).unwrap();
        let events = e.subscribe();
        let rx = e.submit(ext(), "test_send", Value::Null);
        let Ok(Event::ConsentRequested { request }) = events.recv_timeout(Duration::from_secs(5)) else { panic!("expected a consent request") };
        e.call(Caller::Ui, "consent_answer", json!({"request_id": request["request_id"], "answer": "deny"})).unwrap();
        assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err(ToolError::Declined(_))));
        e.shutdown();
    }

    #[test]
    fn a_panicking_tool_does_not_stop_the_engine() {
        let e = Engine::start(State::default(), vec![panicky()]).unwrap();
        assert!(matches!(e.call(Caller::Ui, "test_boom", Value::Null), Err(ToolError::Failed(_))));
        assert!(e.call(Caller::Ui, "tool_list", Value::Null).is_ok());
        e.shutdown();
    }

    #[test]
    fn unknown_tool_is_an_error() {
        let e = Engine::start(State::default(), vec![]).unwrap();
        assert!(matches!(e.call(Caller::Cli, "nope", Value::Null), Err(ToolError::UnknownTool(_))));
        e.shutdown();
    }
}
