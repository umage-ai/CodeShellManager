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

use std::collections::HashMap;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::events::{Event, Events};
use crate::registry::Registry;
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

/// A remembered "always allow": this client may call this tool without asking.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Grant {
    pub client: String,
    pub tool: String,
}

/// Cheap to clone; all clones talk to the same engine thread.
#[derive(Clone)]
pub struct Engine {
    tx: Sender<Msg>,
    events: Events,
    registry: Arc<Registry>,
    joined: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl Engine {
    /// Start the engine thread with `state` and the given tool modules (each a `specs()` list).
    pub fn start(state: State, tools: Vec<ToolSpec>) -> Result<Engine, ToolError> {
        let registry = Arc::new(Registry::new(builtin_specs().into_iter().chain(tools).collect())?);
        let (tx, rx) = mpsc::channel::<Msg>();
        let events = Events::default();
        let engine = Engine { tx, events: events.clone(), registry: registry.clone(), joined: Arc::new(Mutex::new(None)) };
        let worker = engine.clone();
        let handle = thread::Builder::new()
            .name("csm-engine".into())
            .spawn(move || run_loop(worker, state, rx))
            .map_err(|e| ToolError::Failed(format!("could not start the engine thread: {e}")))?;
        *engine.joined.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
        Ok(engine)
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Subscribe to engine events (UI, IPC clients, the MCP bridge).
    pub fn subscribe(&self) -> Receiver<Event> {
        self.events.subscribe()
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

fn run_loop(engine: Engine, mut state: State, rx: Receiver<Msg>) {
    let mut pending: HashMap<u64, Pending> = HashMap::new();
    let mut next_id: u64 = 1;
    let mut grants: Vec<Grant> = Vec::new();
    loop {
        let msg = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(m) => Some(m),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        expire(&mut pending, &engine.events);
        let Some(msg) = msg else { continue };
        match msg {
            Msg::Shutdown => break,
            Msg::Post(job) => {
                let mut ctx = ctx(&mut state, &Caller::Ui, &engine);
                if catch_unwind(AssertUnwindSafe(|| job(&mut ctx))).is_err() {
                    engine.events.emit(Event::Error { message: "A background task failed unexpectedly.".into() });
                }
            }
            Msg::Call { caller, id, params, reply } => {
                // Consent bookkeeping is handled here, not in a tool, so no tool can skip it.
                match id.as_str() {
                    "consent_answer" => {
                        let r = answer(&caller, params, &mut pending, &mut grants, &mut state, &engine);
                        let _ = reply.send(r);
                        continue;
                    }
                    "consent_list" => {
                        let list: Vec<Value> = pending.iter().map(|(k, p)| pending_json(*k, p)).collect();
                        let _ = reply.send(Ok(ToolOutput::new(format!("{} waiting", list.len()), json!(list))));
                        continue;
                    }
                    _ => {}
                }
                let Some(spec) = engine.registry.get(&id).copied() else {
                    let _ = reply.send(Err(ToolError::UnknownTool(id)));
                    continue;
                };
                if let Caller::External { client, .. } = &caller {
                    let granted = grants.iter().any(|g| &g.client == client && g.tool == spec.id);
                    if spec.effects.needs_consent() && !granted {
                        let key = next_id;
                        next_id = next_id.saturating_add(1);
                        let p = Pending { caller: caller.clone(), spec, params, reply, asked_at: Instant::now() };
                        engine.events.emit(Event::ConsentRequested { request: pending_json(key, &p) });
                        pending.insert(key, p);
                        continue;
                    }
                }
                let r = run_tool(&spec, &mut state, &caller, params, &engine);
                let _ = reply.send(r);
            }
        }
    }
    for (_, p) in pending.drain() {
        let _ = p.reply.send(Err(ToolError::Failed("the app is closing".into())));
    }
}

fn ctx<'a>(state: &'a mut State, caller: &'a Caller, engine: &'a Engine) -> Ctx<'a> {
    Ctx { state, caller, events: &engine.events, registry: &engine.registry, engine }
}

fn run_tool(spec: &ToolSpec, state: &mut State, caller: &Caller, params: Value, engine: &Engine) -> Reply {
    let mut c = ctx(state, caller, engine);
    let run = spec.run;
    match catch_unwind(AssertUnwindSafe(|| run(&mut c, params))) {
        Ok(r) => r,
        Err(_) => Err(ToolError::Failed(format!("`{}` failed unexpectedly; nothing else was affected", spec.id))),
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
    let stale: Vec<u64> =
        pending.iter().filter(|(_, p)| now.duration_since(p.asked_at) >= CONSENT_TIMEOUT).map(|(k, _)| *k).collect();
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

fn answer(
    caller: &Caller,
    params: Value,
    pending: &mut HashMap<u64, Pending>,
    grants: &mut Vec<Grant>,
    state: &mut State,
    engine: &Engine,
) -> Reply {
    // Only the person, in the app, can answer. An MCP client must never approve its own request.
    if *caller != Caller::Ui {
        return Err(ToolError::Refused("consent is answered in the app, not over MCP or the CLI".into()));
    }
    let p: ConsentAnswerParams = parse_params(params)?;
    let Some(req) = pending.remove(&p.request_id) else {
        return Err(ToolError::NotFound(format!("no waiting request {}", p.request_id)));
    };
    engine.events.emit(Event::ConsentResolved { request_id: p.request_id });
    let result = match p.answer {
        ConsentChoice::Deny => Err(ToolError::Declined(format!("`{}` was not allowed", req.spec.id))),
        choice => {
            if choice == ConsentChoice::AllowAlways
                && let Caller::External { client, .. } = &req.caller
            {
                let g = Grant { client: client.clone(), tool: req.spec.id.to_string() };
                if !grants.contains(&g) {
                    grants.push(g);
                }
            }
            run_tool(&req.spec, state, &req.caller, req.params, engine)
        }
    };
    let _ = req.reply.send(result);
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
    /// Only this toolset (`core`, `groups`, `layout`, `runs`, `git`, `notes`, `search`, `app`).
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
        let Ok(Event::ConsentRequested { request }) = events.recv_timeout(Duration::from_secs(5)) else {
            panic!("expected a consent request")
        };
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
        let Ok(Event::ConsentRequested { request }) = events.recv_timeout(Duration::from_secs(5)) else {
            panic!("expected a consent request")
        };
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
