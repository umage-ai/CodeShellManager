//! New session forms: a process add-on may contribute a short form to the New session dialog
//! (`[[new_session]]`, docs/addons.md, "New session forms"). The form has a picker filled from one
//! of the add-on's tools (`data.choices`) and a submit button that runs another of its tools with
//! the chosen id. Nothing here is specific to one add-on: the dialog asks these tools, and the
//! add-on's own tools do the work.
//!
//! Both calls run the add-on's tools on a worker thread and answer the parked call when they are
//! done, so the engine never waits on a process. The state of each form (loading, the choices or
//! the reason it failed, the submit in progress or its outcome) is runtime only and is cleared when
//! the add-on stops.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use csm_core::addons::manifest::{MAX_NEW_SESSION_NAME, clean_text};
use csm_core::addons::{AddonKind, NewSessionSpec};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::addons::{AddonState, Catalogue, effects_words};
use crate::engine::{Ctx, Engine, Reply};
use crate::plugins::{self, RunState};
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

/// How long a form's tool may take to answer (a little over the add-on call timeout).
const WAIT: Duration = Duration::from_secs(35);
/// How long a submit may stay pending (the add-on answers `data.pending` with a `request_id`, and
/// the same tool is asked again with that id) before CodeShellManager gives up on it. A launch takes
/// tens of seconds, so this is generous; the add-on's own job may still finish after it.
pub const SUBMIT_CAP: Duration = Duration::from_secs(15 * 60);
/// The pause between two asks for a pending submit (an add-on that answers at once must not be spun).
const POLL_GAP: Duration = Duration::from_secs(1);
/// Longest `request_id` a pending answer may carry; the characters are letters, digits, `-`, `_`, `.`, `:`.
const MAX_REQUEST_ID: usize = 200;
/// Most choices one form keeps.
pub const MAX_CHOICES: usize = 200;
/// Longest choice id and label, in characters.
const MAX_ID: usize = 200;
const MAX_LABEL: usize = 120;
const MAX_DETAIL: usize = 200;

/// One entry of a form's picker.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Choice {
    pub id: String,
    pub label: String,
    pub detail: String,
}

/// Where a form's picker stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum LoadState {
    #[default]
    Idle,
    Loading,
    Ready(Vec<Choice>),
    Failed(String),
}

/// Where a form's submit stands. `Running` carries the add-on's last progress line ("" at first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum SubmitState {
    #[default]
    Idle,
    Running(String),
    Done(String),
    Failed(String),
}

/// The runtime state of one form, keyed by `addon/source` in `State`. Each answer carries the token
/// of the request it answers: an answer whose token is no longer the form's (the add-on stopped, or
/// the form was asked again) is dropped instead of written over the newer state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FormRun {
    pub load: LoadState,
    pub submit: SubmitState,
    pub load_token: u64,
    pub submit_token: u64,
}

/// A token unique for the life of the process (see [`FormRun`]).
fn next_token() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// What the dialog shows for one form (`snapshot.new_sessions`).
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NewSessionView {
    pub addon: String,
    pub addon_name: String,
    /// The form's id within the add-on (`new_session_load` and `new_session_submit` take it).
    pub source: String,
    pub title: String,
    pub submit_label: String,
    pub choice_title: String,
    /// The name field's heading when the form has one (`name_param`); the dialog draws the field
    /// only then.
    pub name_title: Option<String>,
    /// `idle`, `loading`, `ready` or `failed`.
    pub load: &'static str,
    pub choices: Vec<Choice>,
    pub load_error: Option<String>,
    /// `idle`, `running`, `done` or `failed`.
    pub submit: &'static str,
    /// What the submit said: its message when done, the reason when it failed.
    pub submit_text: Option<String>,
}

pub(crate) fn key(addon: &str, source: &str) -> String {
    format!("{addon}/{source}")
}

/// Every form an enabled process add-on whose process is running offers, in add-on id order:
/// the add-on's name and the form.
pub(crate) fn offered(cat: &Catalogue) -> Vec<(String, String, &NewSessionSpec)> {
    let mut out = Vec::new();
    for e in &cat.entries {
        let Some(p) = e.parsed.as_deref() else { continue };
        let running = cat.runtime.get(&p.meta.id).is_some_and(|r| r.state == RunState::Running);
        if e.state != AddonState::Loaded || p.meta.kind != AddonKind::Process || !running {
            continue;
        }
        for s in &p.new_sessions {
            out.push((p.meta.id.clone(), p.meta.name.clone(), s));
        }
    }
    out.sort_by(|a, b| (a.0.as_str(), a.2.id.as_str()).cmp(&(b.0.as_str(), b.2.id.as_str())));
    out
}

/// The views the dialog draws, from what is offered and the runtime state of each form.
pub(crate) fn views(cat: &Catalogue, runs: &BTreeMap<String, FormRun>) -> Vec<NewSessionView> {
    offered(cat)
        .into_iter()
        .map(|(addon, addon_name, s)| {
            let run = runs.get(&key(&addon, &s.id)).cloned().unwrap_or_default();
            let (load, choices, load_error) = match run.load {
                LoadState::Idle => ("idle", Vec::new(), None),
                LoadState::Loading => ("loading", Vec::new(), None),
                LoadState::Ready(c) => ("ready", c, None),
                LoadState::Failed(e) => ("failed", Vec::new(), Some(e)),
            };
            let (submit, submit_text) = match run.submit {
                SubmitState::Idle => ("idle", None),
                SubmitState::Running(progress) => ("running", Some(progress).filter(|p| !p.is_empty())),
                SubmitState::Done(m) => ("done", Some(m)),
                SubmitState::Failed(e) => ("failed", Some(e)),
            };
            NewSessionView {
                addon,
                addon_name,
                source: s.id.clone(),
                title: s.title.clone(),
                submit_label: s.submit.clone(),
                choice_title: s.choice_title.clone(),
                name_title: s.name_param.as_ref().map(|_| s.name_title.clone()),
                load,
                choices,
                load_error,
                submit,
                submit_text,
            }
        })
        .collect()
}

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "new_session_sources",
            title: "List New session forms",
            summary: "The New session forms enabled process add-ons offer (their title, the choices they have loaded and the state of the last submit), for the dialog and MCP clients.",
            toolset: Toolset::Addons,
            effects: Effects::IDEMPOTENT,
            input_schema: crate::tool::empty_schema,
            run: sources,
        },
        ToolSpec {
            id: "new_session_load",
            title: "Load a New session form",
            summary: "Ask the add-on for the choices of one of its New session forms (its own choices tool). Starts the add-on's process if needed and may reach the network, so MCP clients are asked first.",
            toolset: Toolset::Addons,
            effects: Effects::MUTATES_STATE | Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK,
            input_schema: schema_of::<FormParams>,
            run: load,
        },
        ToolSpec {
            id: "new_session_submit",
            title: "Submit a New session form",
            summary: "Run one of an add-on's New session forms with a choice from its loaded list (for example create an agent in the chosen project), and the name typed in its name field when it has one. The add-on's tool does the work. Its remote opens only for the person in the app; any other caller is told which remote to open. Starts processes and may reach the network, so MCP clients are asked first, and the consent card names the add-on tool the form runs.",
            toolset: Toolset::Addons,
            effects: Effects::MUTATES_STATE | Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK | Effects::WRITES_FILESYSTEM,
            input_schema: schema_of::<SubmitParams>,
            run: submit,
        },
    ]
}

fn sources(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let views = views(&ctx.state.rt.addons, &ctx.state.rt.new_sessions);
    Ok(ToolOutput::new(format!("{} form(s) offered.", views.len()), json!({"sources": views})))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct FormParams {
    /// The add-on that offers the form (from `new_session_sources`).
    addon: String,
    /// The form's id within the add-on.
    source: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SubmitParams {
    /// The add-on that offers the form.
    addon: String,
    /// The form's id within the add-on.
    source: String,
    /// One of the choices the form has loaded (its `id`).
    choice: String,
    /// The typed name, for a form with a name field. Empty means no name. At most
    /// [`MAX_NEW_SESSION_NAME`] characters, with no control or bidi-control characters: anything
    /// else is refused, not cut.
    #[serde(default)]
    name: Option<String>,
}

/// The offered form `addon/source`, with the add-on's name, or why there is none.
fn find(ctx: &Ctx<'_>, addon: &str, source: &str) -> Result<NewSessionSpec, ToolError> {
    offered(&ctx.state.rt.addons)
        .into_iter()
        .find(|(a, _, s)| a == addon && s.id == source)
        .map(|(_, _, s)| s.clone())
        .ok_or_else(|| ToolError::NotFound(format!("no New session form `{source}` from add-on `{addon}`; `new_session_sources` shows what is offered")))
}

/// The typed name a submit carries: `None` when it is absent or blank, the trimmed name otherwise.
/// A name longer than [`MAX_NEW_SESSION_NAME`] characters, or with a control or bidi-control
/// character, is refused (the add-on never sees a name it would have to clean up).
pub(crate) fn typed_name(raw: Option<&str>) -> Result<Option<String>, String> {
    let Some(name) = raw.map(str::trim).filter(|s| !s.is_empty()) else { return Ok(None) };
    if name.chars().count() > MAX_NEW_SESSION_NAME || clean_text(name, usize::MAX) != name {
        return Err(format!("the name is longer than {MAX_NEW_SESSION_NAME} characters or has control characters"));
    }
    Ok(Some(name.to_owned()))
}

/// For a form tool (`new_session_load`, `new_session_submit`) run by a caller other than the
/// person, the add-on tool the form runs and that tool's declared effects. The consent card shows
/// them: a form's own effects are only the form's, so the grant is never reused for the add-on tool
/// (see `engine.rs`). `None` for every other tool.
pub(crate) fn consent_target(cat: &Catalogue, tool: &str, params: &Value, effects_of: impl Fn(&str) -> Option<Effects>) -> Option<Value> {
    if tool != "new_session_load" && tool != "new_session_submit" {
        return None;
    }
    let addon = params.get("addon").and_then(Value::as_str).unwrap_or_default();
    let source = params.get("source").and_then(Value::as_str).unwrap_or_default();
    let form = offered(cat).into_iter().find(|(a, _, s)| a == addon && s.id == source).map(|(_, _, s)| s.clone());
    let (target, effects) = match form {
        Some(s) => {
            let target = if tool == "new_session_load" { s.choices_tool } else { s.tool };
            let effects = effects_of(&target);
            (target, effects)
        }
        None => (String::new(), None),
    };
    // The effects as the consent dialog words them ("starts programs, writes files"); "not running"
    // when the add-on's tool is not in the registry (the call will fail for that reason).
    Some(json!({
        "addon": addon,
        "tool": target,
        "effects": effects.map_or_else(|| "not running".to_owned(), effects_words),
    }))
}

fn set_run(ctx: &mut Ctx<'_>, k: &str, f: impl FnOnce(&mut FormRun)) {
    f(ctx.state.rt.new_sessions.entry(k.to_owned()).or_default());
    ctx.state.touch();
}

/// Engine thread: applies an answer to the form only if its entry is still there and `current`
/// says the answer belongs to the form's present request. A worker that finished after its add-on
/// stopped (the entry is gone) or after the form was asked again writes nothing.
fn apply_answer(ctx: &mut Ctx<'_>, k: &str, current: impl FnOnce(&FormRun) -> bool, f: impl FnOnce(&mut FormRun)) {
    let Some(run) = ctx.state.rt.new_sessions.get_mut(k) else { return };
    if !current(run) {
        return;
    }
    f(run);
    ctx.state.touch();
}

fn load(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: FormParams = parse_params(params)?;
    let spec = find(ctx, &p.addon, &p.source)?;
    let k = key(&p.addon, &p.source);
    if ctx.state.rt.new_sessions.get(&k).is_some_and(|r| r.load == LoadState::Loading) {
        return Ok(ToolOutput::new("Already loading.", Value::Null));
    }
    if ctx.registry.get(&spec.choices_tool).is_none() {
        let why = format!("`{}` is not running; the add-on `{}` may have stopped", spec.choices_tool, p.addon);
        set_run(ctx, &k, |r| r.load = LoadState::Failed(why.clone()));
        return Err(ToolError::Refused(why));
    }
    let Some(parked) = ctx.park() else { return Err(ToolError::Failed("cannot wait for the add-on here".into())) };
    // A submit still running keeps its state; any other finished one starts afresh with the form.
    let token = next_token();
    set_run(ctx, &k, |r| {
        r.load = LoadState::Loading;
        r.load_token = token;
        if !matches!(r.submit, SubmitState::Running(_)) {
            r.submit = SubmitState::Idle;
        }
    });
    let job = Ask { addon: p.addon.clone(), tool: spec.choices_tool.clone(), args: json!({}), who: plugins::caller_json(ctx.state, ctx.caller) };
    let rx = ask(ctx.engine, &job);
    let engine = ctx.engine.clone();
    let tool = spec.choices_tool.clone();
    let spawned = thread::Builder::new().name("csm-new-session".into()).spawn(move || {
        let answer = rx.recv_timeout(WAIT).unwrap_or_else(|_| Err(ToolError::TimedOut(format!("`{tool}` did not answer"))));
        engine.post(Box::new(move |ctx: &mut Ctx<'_>| {
            let reply = answer.and_then(|out| {
                let choices = choices_of(&out.data).map_err(ToolError::Failed)?;
                let n = choices.len();
                Ok((n, choices))
            });
            let (store, out) = match reply {
                Ok((n, choices)) => (LoadState::Ready(choices), Ok(ToolOutput::new(format!("{n} choice(s)."), Value::Null))),
                Err(e) => (LoadState::Failed(e.to_string()), Err(e)),
            };
            apply_answer(ctx, &k, |r| r.load_token == token, |r| r.load = store);
            parked.send(out);
        }));
    });
    if let Err(e) = spawned {
        set_run(ctx, &key(&p.addon, &p.source), |r| r.load = LoadState::Failed("could not start a worker thread".into()));
        return Err(ToolError::Failed(format!("could not start a worker thread: {e}")));
    }
    Ok(ToolOutput::new("Loading.", Value::Null))
}

/// `data.choices` of a choices answer: an array of `{id, label?, detail?}`. Bad entries are
/// dropped; duplicates and anything past [`MAX_CHOICES`] are ignored. A missing list is an error.
pub(crate) fn choices_of(data: &Value) -> Result<Vec<Choice>, String> {
    let items = data.get("choices").and_then(Value::as_array).ok_or("the add-on's answer has no `choices` list")?;
    let mut out: Vec<Choice> = Vec::new();
    for item in items {
        let Some(obj) = item.as_object() else { continue };
        let Some(id) = obj.get("id").and_then(Value::as_str).map(|s| clean_text(s, MAX_ID)).filter(|s| !s.is_empty()) else { continue };
        if out.iter().any(|c| c.id == id) {
            continue;
        }
        if out.len() >= MAX_CHOICES {
            break;
        }
        let label = obj.get("label").and_then(Value::as_str).map(|s| clean_text(s, MAX_LABEL)).filter(|s| !s.is_empty()).unwrap_or_else(|| id.clone());
        let detail = obj.get("detail").and_then(Value::as_str).map(|s| clean_text(s, MAX_DETAIL)).unwrap_or_default();
        out.push(Choice { id, label, detail });
    }
    Ok(out)
}

/// One ask of an add-on's tool: the tool, the arguments and who asked (the add-on's `caller`).
struct Ask {
    addon: String,
    tool: String,
    args: Value,
    who: Value,
}

/// Sends `job` to the add-on; the answer comes on the returned receiver.
fn ask(engine: &Engine, job: &Ask) -> mpsc::Receiver<Reply> {
    let (tx, rx) = mpsc::channel::<Reply>();
    engine.host.dispatch(job.addon.clone(), job.tool.clone(), job.args.clone(), job.who.clone(), tx);
    rx
}

/// Whether a submit's answer is still pending.
enum Pending {
    Final,
    Waiting(String),
    Malformed,
}

/// A submit's answer is pending when its `data` says `pending: true`; then `data.request_id` names
/// the job, and the same tool is asked again with it. `pending` without a usable id is an error.
fn pending_of(data: &Value) -> Pending {
    if data.get("pending").and_then(Value::as_bool) != Some(true) {
        return Pending::Final;
    }
    match data.get("request_id").and_then(Value::as_str).map(str::trim) {
        Some(id) if valid_request_id(id) => Pending::Waiting(id.to_owned()),
        _ => Pending::Malformed,
    }
}

/// Letters, digits and `-_.:`, at most [`MAX_REQUEST_ID`] characters.
fn valid_request_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= MAX_REQUEST_ID && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// The remote a submit's final answer asks CodeShellManager to open: `data.open_remote`, one of the
/// add-on's remote ids (`remote_list`). Anything else is no request.
fn open_remote_of(data: &Value) -> Option<String> {
    data.get("open_remote").and_then(Value::as_str).map(str::trim).filter(|id| valid_request_id(id)).map(str::to_owned)
}

/// Whether the form still belongs to submit `token` (the add-on did not stop, and nothing newer was
/// submitted), asked on the engine thread. A worker stops polling once it is not.
fn still_current(engine: &Engine, k: &str, token: u64) -> bool {
    let (tx, rx) = mpsc::channel::<bool>();
    let k = k.to_owned();
    engine.post(Box::new(move |ctx: &mut Ctx<'_>| {
        let current = ctx.state.rt.new_sessions.get(&k).is_some_and(|r| r.submit_token == token);
        let _ = tx.send(current);
    }));
    rx.recv_timeout(WAIT).unwrap_or(false)
}

/// The submit's final answer, on the worker thread. A pending answer shows its progress on the form
/// (while `token` is still the submit's), pauses, and asks the same tool again with the
/// `request_id`, until the answer is final or [`SUBMIT_CAP`] has passed.
fn drive(engine: &Engine, mut job: Ask, mut rx: mpsc::Receiver<Reply>, deadline: Instant, k: &str, token: u64) -> Reply {
    loop {
        let out = rx.recv_timeout(WAIT).unwrap_or_else(|_| Err(ToolError::TimedOut(format!("`{}` did not answer", job.tool))))?;
        let request_id = match pending_of(&out.data) {
            Pending::Final => return Ok(out),
            Pending::Malformed => return Err(ToolError::Failed(format!("`{}` said it is still running but gave no request_id", job.tool))),
            Pending::Waiting(id) => id,
        };
        if Instant::now() >= deadline {
            let minutes = SUBMIT_CAP.as_secs() / 60;
            return Err(ToolError::TimedOut(format!("`{}` is still running after {minutes} minutes; check the add-on's own list before trying again", job.tool)));
        }
        if !still_current(engine, k, token) {
            return Err(ToolError::Refused("the add-on stopped while this was starting; nothing more is asked".into()));
        }
        let progress = clean_text(&out.message, 500);
        let k = k.to_owned();
        engine.post(Box::new(move |ctx: &mut Ctx<'_>| apply_answer(ctx, &k, |r| r.submit_token == token, |r| r.submit = SubmitState::Running(progress))));
        thread::sleep(POLL_GAP);
        if let Some(args) = job.args.as_object_mut() {
            args.insert("request_id".into(), Value::String(request_id));
        }
        rx = ask(engine, &job);
    }
}

fn submit(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SubmitParams = parse_params(params)?;
    let spec = find(ctx, &p.addon, &p.source)?;
    let name = typed_name(p.name.as_deref()).map_err(ToolError::InvalidParams)?;
    if name.is_some() && spec.name_param.is_none() {
        return Err(ToolError::InvalidParams(format!("the form `{}` has no name field; leave `name` out", p.source)));
    }
    let k = key(&p.addon, &p.source);
    let run = ctx.state.rt.new_sessions.get(&k).cloned().unwrap_or_default();
    let LoadState::Ready(choices) = &run.load else {
        return Err(ToolError::Refused("load the form's choices first (`new_session_load`)".into()));
    };
    if !choices.iter().any(|c| c.id == p.choice) {
        return Err(ToolError::InvalidParams(format!("`{}` is not one of the loaded choices", clean_text(&p.choice, 64))));
    }
    if matches!(run.submit, SubmitState::Running(_)) {
        return Err(ToolError::Refused("this form is already starting something; wait for it".into()));
    }
    if ctx.registry.get(&spec.tool).is_none() {
        return Err(ToolError::Refused(format!("`{}` is not running; the add-on `{}` may have stopped", spec.tool, p.addon)));
    }
    let Some(parked) = ctx.park() else { return Err(ToolError::Failed("cannot wait for the add-on here".into())) };
    let mut args = spec.params.as_object().cloned().unwrap_or_else(Map::new);
    args.insert(spec.choice_param.clone(), Value::String(p.choice.clone()));
    if let (Some(name), Some(param)) = (name, &spec.name_param) {
        args.insert(param.clone(), Value::String(name));
    }
    // Only the person's own submit opens the remote it names. Any other caller gets the answer with
    // the remote named, so it can ask for the session itself (remote_open, under its own consent).
    let opens = matches!(ctx.caller, Caller::Ui);
    let token = next_token();
    set_run(ctx, &k, |r| {
        r.submit = SubmitState::Running(String::new());
        r.submit_token = token;
    });
    let job = Ask { addon: p.addon.clone(), tool: spec.tool.clone(), args: Value::Object(args), who: plugins::caller_json(ctx.state, ctx.caller) };
    let rx = ask(ctx.engine, &job);
    let engine = ctx.engine.clone();
    let deadline = Instant::now() + SUBMIT_CAP;
    let addon = p.addon.clone();
    let spawned = thread::Builder::new().name("csm-new-session".into()).spawn(move || {
        let answer = drive(&engine, job, rx, deadline, &k, token);
        // A launch that names the remote to open (`open_remote`) opens it now, as the person's own
        // action: the form was submitted by them, and remote_open only opens this add-on's remotes.
        // Another caller's submit is not the person's, so nothing opens; its answer names the remote.
        let remote = answer.as_ref().ok().and_then(|out| open_remote_of(&out.data));
        let open_err = match (&remote, opens) {
            (Some(remote), true) => {
                let params = json!({"addon": addon, "remote_id": remote});
                engine.call(Caller::Ui, "remote_open", params).err().map(|e| e.to_string())
            }
            _ => None,
        };
        let answer = match (answer, &remote) {
            (Ok(mut out), Some(remote)) if !opens => {
                out.message = format!("{} Not opened: call remote_open with addon `{addon}` and remote_id `{remote}` to open it.", out.message.trim());
                Ok(out)
            }
            (answer, _) => answer,
        };
        engine.post(Box::new(move |ctx: &mut Ctx<'_>| {
            let (state, reply) = match (answer, open_err) {
                (Ok(out), None) => (SubmitState::Done(clean_text(&out.message, 500)), Ok(out)),
                // The launch happened; its session did not open. The form stays open and says so.
                (Ok(out), Some(why)) => (SubmitState::Failed(format!("{} Its session did not open: {why}", clean_text(&out.message, 500))), Ok(out)),
                (Err(e), _) => (SubmitState::Failed(e.to_string()), Err(e)),
            };
            apply_answer(ctx, &k, |r| r.submit_token == token, |r| r.submit = state);
            parked.send(reply);
        }));
    });
    if let Err(e) = spawned {
        set_run(ctx, &key(&p.addon, &p.source), |r| r.submit = SubmitState::Failed("could not start a worker thread".into()));
        return Err(ToolError::Failed(format!("could not start a worker thread: {e}")));
    }
    Ok(ToolOutput::new("Starting.", Value::Null))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn choices_keep_the_good_entries_and_drop_the_rest() {
        let data = json!({"choices": [
            {"id": "uog", "label": "UOG\u{202E}", "detail": "3 agents"},
            {"id": "uog", "label": "again"},
            {"id": "", "label": "empty id"},
            {"label": "no id"},
            "not an object",
            {"id": "x".repeat(300), "label": "long"},
            {"id": "plain"},
            {"id": "a\u{0007}b", "label": "", "detail": 7},
        ]});
        let got = choices_of(&data).unwrap();
        let ids: Vec<&str> = got.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(ids.len(), 4, "{ids:?}");
        assert_eq!(got[0], Choice { id: "uog".into(), label: "UOG".into(), detail: "3 agents".into() });
        assert_eq!(got[1].id.chars().count(), MAX_ID, "a long id is cut, not refused");
        assert_eq!(got[1].label, "long");
        assert_eq!((got[2].id.as_str(), got[2].label.as_str()), ("plain", "plain"), "a missing label is the id");
        assert_eq!(got[3].id, "ab", "control characters are dropped from ids");
    }

    #[test]
    fn a_missing_or_wrong_choices_list_is_an_error_not_a_panic() {
        assert!(choices_of(&json!({})).is_err());
        assert!(choices_of(&json!({"choices": "nope"})).is_err());
        assert!(choices_of(&json!({"choices": null})).is_err());
        assert_eq!(choices_of(&json!({"choices": []})).unwrap(), Vec::new());
    }

    #[test]
    fn choices_are_capped() {
        let many: Vec<Value> = (0..(MAX_CHOICES + 50)).map(|i| json!({"id": format!("c{i}")})).collect();
        assert_eq!(choices_of(&json!({"choices": many})).unwrap().len(), MAX_CHOICES);
    }

    #[test]
    fn a_pending_answer_needs_a_plain_request_id() {
        let waiting = |data: Value| match pending_of(&data) {
            Pending::Waiting(id) => Some(id),
            _ => None,
        };
        assert_eq!(waiting(json!({"pending": true, "request_id": "abc-123_x.y:z"})).as_deref(), Some("abc-123_x.y:z"));
        assert_eq!(waiting(json!({"pending": true, "request_id": "  abcdefgh  "})).as_deref(), Some("abcdefgh"), "surrounding spaces are not part of the id");
        assert!(matches!(pending_of(&json!({"pending": true})), Pending::Malformed), "pending with no id is an error, not a success");
        assert!(matches!(pending_of(&json!({"pending": true, "request_id": ""})), Pending::Malformed));
        assert!(matches!(pending_of(&json!({"pending": true, "request_id": "a b"})), Pending::Malformed), "spaces are refused");
        assert!(matches!(pending_of(&json!({"pending": true, "request_id": "a\u{7}b"})), Pending::Malformed), "control characters are refused");
        assert!(matches!(pending_of(&json!({"pending": true, "request_id": "x".repeat(MAX_REQUEST_ID + 1)})), Pending::Malformed));
        assert!(matches!(pending_of(&json!({"pending": true, "request_id": 7})), Pending::Malformed));
        assert!(matches!(pending_of(&json!({"pending": false, "request_id": "abcdefgh"})), Pending::Final));
        assert!(matches!(pending_of(&json!({"pending": "yes", "request_id": "abcdefgh"})), Pending::Final), "only a boolean true is pending");
        assert!(matches!(pending_of(&Value::Null), Pending::Final));
    }

    #[test]
    fn form_answers_are_only_applied_for_the_request_they_belong_to() {
        let mut run = FormRun { load_token: 7, submit_token: 9, ..FormRun::default() };
        let current = |r: &FormRun| r.submit_token == 9;
        assert!(current(&run));
        run.submit_token = 10;
        assert!(!current(&run), "a newer submit makes the older answer stale");
        assert_ne!(next_token(), next_token(), "tokens are unique for the life of the process");
    }

    #[test]
    fn a_typed_name_is_trimmed_blank_means_none_and_hostile_or_long_is_refused() {
        assert_eq!(typed_name(None), Ok(None));
        assert_eq!(typed_name(Some("   ")), Ok(None), "blank is no name");
        assert_eq!(typed_name(Some(" Gamma ")), Ok(Some("Gamma".into())));
        assert!(typed_name(Some("a\u{202E}b")).is_err(), "a bidi control is refused, not dropped");
        assert!(typed_name(Some("a\tb")).is_err(), "a control character is refused");
        assert!(typed_name(Some(&"x".repeat(MAX_NEW_SESSION_NAME))).is_ok());
        assert!(typed_name(Some(&"x".repeat(MAX_NEW_SESSION_NAME + 1))).is_err());
        assert!(typed_name(Some(&"é".repeat(MAX_NEW_SESSION_NAME))).is_ok(), "the cap counts characters, not bytes");
    }
}
