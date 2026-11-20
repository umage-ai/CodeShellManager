//! `tasks` tools: the Board's tasks (`tasks.json`), one toolset of their own.
//!
//! The model, the state machine and the permission rules are pure, in `csm_core::tasks`; this
//! module applies them to the engine's state. Who may do what (decided 2026-11-20):
//!
//! - The person (the app, the CLI) may do everything. `task_accept` and `task_reopen` are theirs
//!   alone.
//! - An agent (an MCP client in one of our sessions, `Caller::External { session_id }`) keeps its
//!   task bookkeeping (`task_new`, `task_claim`, `task_update`, `task_done`, `task_handoff`)
//!   without a consent card, but only in its own project, only on tasks assigned to itself (or
//!   unassigned ones, to claim), and with at most 50 open tasks created per session.
//! - `task_assign` pastes the task into a session (`session_send_text`'s path and refusals) and
//!   `task_delete` loses data: their effects make an MCP caller ask.
//!
//! A task's text is untrusted (an agent wrote it, and it is later pasted into a prompt): control
//! characters and bidi overrides are removed and the sizes are capped before it is stored.

use csm_core::tasks::{self, Actor, Op, Task, TaskState};
use serde::Deserialize;
use serde_json::{Value, json};

use super::find;
use crate::engine::{Ctx, Reply};
use crate::session::now_ms;
use crate::snapshot::project_of;
use crate::state::State;
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

/// Longest project key a caller may name.
const MAX_PROJECT: usize = 1024;

pub(super) fn specs() -> Vec<ToolSpec> {
    let t = |id, title, summary, effects, input_schema, run| ToolSpec { id, title, summary, toolset: Toolset::Tasks, effects, input_schema, run };
    let state = Effects::MUTATES_STATE;
    vec![
        t(
            "task_list",
            "List tasks",
            "The Board's tasks: id, project, title, state (backlog, assigned, review, done, cancelled), assignee, progress. An agent sees its own project's; filter by project, state or assignee. Done and cancelled ones only with include_closed.",
            Effects::READ_ONLY,
            schema_of::<ListParams>,
            task_list,
        ),
        t("task_get", "Task details", "One task in full, with its body.", Effects::READ_ONLY, schema_of::<IdParams>, task_get),
        t(
            "task_new",
            "New task…",
            "Add a task to a project's backlog: a title (at most 200 characters) and an optional body (at most 16 KiB). An agent's task goes to its own project; the person names a project or a session_id whose project it is.",
            state,
            schema_of::<NewParams>,
            task_new,
        ),
        t(
            "task_update",
            "Edit task…",
            "Change a task's title, body, progress note or order, or cancel it (the person only). An agent may update only the tasks assigned to itself.",
            state,
            schema_of::<UpdateParams>,
            task_update,
        ),
        t(
            "task_claim",
            "Claim task",
            "Take an unassigned backlog task: an agent claims it for itself; the person names the session_id. Nothing is sent to the session.",
            state,
            schema_of::<ClaimParams>,
            task_claim,
        ),
        t(
            "task_done",
            "Mark task done",
            "The assignee says the task is done: it moves to Review, where the person accepts or reopens it. An optional note says what was done.",
            state,
            schema_of::<NoteParams>,
            task_done,
        ),
        t(
            "task_handoff",
            "Hand task back",
            "Give an assigned task back to the backlog, unassigned, with an optional note (why, and how far it got).",
            state,
            schema_of::<NoteParams>,
            task_handoff,
        ),
        t(
            "task_assign",
            "Assign task…",
            "Assign a task to a session in its project and (send, default true) paste \"Task <id> from <creator>: <title>\" and the body into it with Enter, as session_send_text does: refused when the session is asleep, starting, exited or working mid-turn (unless force), and for the caller's own session. force also takes a task from another assignee.",
            state.union(Effects::SENDS_INPUT),
            schema_of::<AssignParams>,
            task_assign,
        ),
        t(
            "task_accept",
            "Accept task",
            "Accept a task in Review: it is Done. The person only (the app or the CLI), never over MCP.",
            state,
            schema_of::<IdParams>,
            task_accept,
        ),
        t(
            "task_reopen",
            "Reopen task",
            "Send a task in Review (or Done, or Cancelled) back: to its assignee when it has one, else to the backlog, with an optional note. The person only, never over MCP.",
            state,
            schema_of::<NoteParams>,
            task_reopen,
        ),
        t("task_delete", "Delete task…", "Delete a task for good.", state.union(Effects::MAY_DISCARD_DATA), schema_of::<IdParams>, task_delete),
    ]
}

// ── Who is asking ───────────────────────────────────────────────────────────────────────

/// The caller as the permission rules see it. An agent whose session is gone is refused.
pub(crate) fn actor(state: &State, caller: &Caller) -> Result<Actor, ToolError> {
    match caller {
        Caller::Ui | Caller::Cli => Ok(Actor::Person),
        Caller::External { session_id: None, .. } => Ok(Actor::Outsider),
        Caller::External { session_id: Some(sid), .. } => {
            let s = state
                .app
                .sessions
                .iter()
                .find(|s| &s.id == sid)
                .ok_or_else(|| ToolError::Refused("the session this client runs in is gone; tasks need a live session".into()))?;
            Ok(Actor::Agent { session_id: sid.clone(), project: project_of(state, s) })
        }
    }
}

fn source(caller: &Caller) -> &'static str {
    match caller {
        Caller::Ui => "ui",
        Caller::Cli => "cli",
        Caller::External { .. } => "mcp",
    }
}

fn created_by(caller: &Caller) -> String {
    match caller {
        Caller::Ui | Caller::Cli => tasks::CREATED_BY_PERSON.to_owned(),
        Caller::External { session_id: Some(s), .. } => format!("session:{s}"),
        Caller::External { client, .. } => format!("mcp:{}", super::clean_name(client, 60)),
    }
}

fn session_name(state: &State, id: &str) -> Option<String> {
    state.app.sessions.iter().find(|s| s.id == id).map(|s| if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() })
}

/// Who made a task, for people: "the user", a session's name, or an MCP client's.
fn creator_name(state: &State, t: &Task) -> String {
    if t.created_by == tasks::CREATED_BY_PERSON {
        return "the user".into();
    }
    if let Some(sid) = t.creator_session() {
        return session_name(state, sid).unwrap_or_else(|| "a closed session".into());
    }
    t.created_by.strip_prefix("mcp:").map_or_else(|| t.created_by.clone(), str::to_owned)
}

fn task_json(state: &State, t: &Task) -> Value {
    let mut v = serde_json::to_value(t).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        let groups = |g: &str| state.app.groups.iter().find(|x| x.id == g).map(|x| x.name.clone());
        o.insert("project_name".into(), json!(tasks::project_label(&t.project, groups)));
        o.insert("assignee_name".into(), json!(t.assignee.as_deref().and_then(|a| session_name(state, a))));
        o.insert("created_by_name".into(), json!(creator_name(state, t)));
    }
    v
}

fn get<'a>(state: &'a State, id: &str) -> Result<&'a Task, ToolError> {
    state.rt.tasks.get(id).ok_or_else(|| ToolError::NotFound(format!("no task `{id}`; `task_list` shows the ids")))
}

fn permit(state: &State, caller: &Caller, op: Op, task: Option<&Task>, project: Option<&str>) -> Result<Actor, ToolError> {
    let a = actor(state, caller)?;
    let open = match &a {
        Actor::Agent { session_id, .. } => state.rt.tasks.open_created_by(session_id),
        _ => 0,
    };
    tasks::permit(&a, op, task, project, open).map_err(|e| ToolError::Refused(e.0))?;
    Ok(a)
}

/// Checked before a consent card is shown (and again after it is answered), so a call the rules
/// refuse never asks the person: `task_assign` and `task_delete` (which ask), and the
/// person-only `task_accept` / `task_reopen`.
pub(super) fn precheck(id: &str, caller: &Caller, params: &Value, state: &State) -> Option<ToolError> {
    let task_id = params.get("task_id").and_then(Value::as_str).unwrap_or_default();
    let task = state.rt.tasks.get(task_id);
    match id {
        "task_assign" => {
            let target = params.get("session_id").and_then(Value::as_str).unwrap_or_default();
            let send = params.get("send").and_then(Value::as_bool).unwrap_or(true);
            if send
                && let Caller::External { session_id: Some(own), .. } = caller
                && own == target
            {
                return Some(ToolError::Refused("a session cannot send input to itself; claim the task with task_claim instead".into()));
            }
            let project = state.app.sessions.iter().find(|s| s.id == target).map(|s| project_of(state, s));
            permit(state, caller, Op::Assign, task, project.as_deref()).err()
        }
        "task_delete" => permit(state, caller, Op::Delete, task, None).err(),
        "task_accept" => permit(state, caller, Op::Accept, task, None).err(),
        "task_reopen" => permit(state, caller, Op::Reopen, task, None).err(),
        _ => None,
    }
}

fn reject(e: tasks::TaskError) -> ToolError {
    ToolError::Refused(e.0)
}

fn note(n: Option<&str>) -> Result<Option<String>, ToolError> {
    match n {
        None => Ok(None),
        Some(n) => tasks::clean_note(n).map(|n| Some(n).filter(|n| !n.is_empty())).map_err(ToolError::InvalidParams),
    }
}

/// Applies `f` to a task and saves; answers with the task.
fn change(ctx: &mut Ctx<'_>, id: &str, message: &str, f: impl FnOnce(&mut Task, &mut csm_core::tasks::TaskFile) -> Result<(), ToolError>) -> Reply {
    let mut file = std::mem::take(&mut ctx.state.rt.tasks);
    let r = match file.get(id).cloned() {
        None => Err(ToolError::NotFound(format!("no task `{id}`; `task_list` shows the ids"))),
        Some(mut t) => f(&mut t, &mut file).map(|()| {
            if let Some(slot) = file.get_mut(id) {
                *slot = t;
            }
        }),
    };
    ctx.state.rt.tasks = file;
    r?;
    ctx.state.persist_tasks();
    let t = get(ctx.state, id)?;
    Ok(ToolOutput::new(format!("{message} {}: {}.", t.id, t.title), task_json(ctx.state, t)))
}

// ── Params ──────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ListParams {
    /// A project key (`repo:<root>` or `group:<id>`, as tasks and sessions show it). An agent
    /// always gets its own project's.
    project: Option<String>,
    /// Only tasks in this state.
    state: Option<TaskState>,
    /// Only tasks assigned to this session.
    assignee: Option<String>,
    /// Include done and cancelled tasks.
    include_closed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdParams {
    task_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NewParams {
    /// One line, at most 200 characters.
    title: String,
    /// Details, at most 16 KiB.
    #[serde(default)]
    body: String,
    /// The project key. Default: the calling agent's project, or `session_id`'s.
    project: Option<String>,
    /// A session whose project the task belongs to (for the person; an agent's is its own).
    session_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct UpdateParams {
    task_id: String,
    title: Option<String>,
    body: Option<String>,
    /// A short progress note (at most 500 characters; empty clears it).
    progress: Option<String>,
    /// Its position in its column (ascending).
    order: Option<i64>,
    /// Cancel the task (the person only).
    #[serde(default)]
    cancel: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ClaimParams {
    task_id: String,
    /// The session that takes it. An agent claims for itself (omit it, or give its own id).
    session_id: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NoteParams {
    task_id: String,
    /// What was done, or why (at most 500 characters).
    note: Option<String>,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AssignParams {
    task_id: String,
    /// The session that gets it (in the task's project).
    session_id: String,
    /// Paste the task into the session and press Enter (default true).
    #[serde(default = "yes")]
    send: bool,
    /// Send even while the session works mid-turn, and take the task from another assignee.
    #[serde(default)]
    force: bool,
}

// ── Tools ───────────────────────────────────────────────────────────────────────────────

fn task_list(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ListParams = parse_params(params)?;
    let a = permit(ctx.state, ctx.caller, Op::Read, None, p.project.as_deref())?;
    let project = match &a {
        Actor::Agent { project, .. } => Some(project.clone()),
        _ => p.project,
    };
    let list: Vec<Value> = ctx
        .state
        .rt
        .tasks
        .tasks
        .iter()
        .filter(|t| project.as_ref().is_none_or(|pr| &t.project == pr))
        .filter(|t| p.state.is_none_or(|s| t.state == s))
        .filter(|t| p.include_closed || p.state.is_some() || t.state.is_open())
        .filter(|t| p.assignee.as_ref().is_none_or(|a| t.assignee.as_ref() == Some(a)))
        .map(|t| task_json(ctx.state, t))
        .collect();
    Ok(ToolOutput::new(format!("{} tasks", list.len()), json!(list)))
}

fn task_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    let t = get(ctx.state, &p.task_id)?;
    permit(ctx.state, ctx.caller, Op::Read, Some(t), None)?;
    Ok(ToolOutput::new(format!("{}: {}", t.id, t.title), task_json(ctx.state, t)))
}

fn task_new(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NewParams = parse_params(params)?;
    let title = tasks::clean_title(&p.title).map_err(ToolError::InvalidParams)?;
    let body = tasks::clean_body(&p.body).map_err(ToolError::InvalidParams)?;
    let named = match (&p.project, &p.session_id) {
        (Some(pr), _) => {
            let pr: String = pr.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_owned();
            if pr.len() > MAX_PROJECT || !(pr.starts_with("repo:") || pr.starts_with("group:")) {
                return Err(ToolError::InvalidParams("a project is `repo:<root>` or `group:<id>`, as sessions and tasks show it".into()));
            }
            Some(pr)
        }
        (None, Some(sid)) => {
            let i = find(ctx.state, sid)?;
            ctx.state.app.sessions.get(i).map(|s| project_of(ctx.state, s))
        }
        (None, None) => None,
    };
    let a = permit(ctx.state, ctx.caller, Op::New, None, named.as_deref())?;
    let project = match (a, named) {
        (Actor::Agent { project, .. }, _) => project,
        (_, Some(pr)) => pr,
        (_, None) => return Err(ToolError::InvalidParams("say which project: project, or a session_id whose project it is".into())),
    };
    let id = ctx.state.rt.tasks.add(&project, title, body, created_by(ctx.caller), source(ctx.caller), now_ms()).map_err(reject)?;
    ctx.state.persist_tasks();
    let t = get(ctx.state, &id)?;
    Ok(ToolOutput::new(format!("Added task {}: {}.", t.id, t.title), task_json(ctx.state, t)))
}

fn task_update(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: UpdateParams = parse_params(params)?;
    let op = if p.cancel { Op::Cancel } else { Op::Update };
    permit(ctx.state, ctx.caller, op, Some(get(ctx.state, &p.task_id)?), None)?;
    let title = p.title.as_deref().map(tasks::clean_title).transpose().map_err(ToolError::InvalidParams)?;
    let body = p.body.as_deref().map(tasks::clean_body).transpose().map_err(ToolError::InvalidParams)?;
    let progress = p.progress.as_deref().map(tasks::clean_note).transpose().map_err(ToolError::InvalidParams)?;
    let now = now_ms();
    change(ctx, &p.task_id, if p.cancel { "Cancelled" } else { "Updated" }, |t, _| {
        if let Some(v) = title {
            t.title = v;
        }
        if let Some(v) = body {
            t.body = v;
        }
        if let Some(v) = progress {
            t.progress = Some(v).filter(|v| !v.is_empty());
        }
        if let Some(o) = p.order {
            t.order = o;
        }
        t.updated_ms = now;
        if p.cancel {
            tasks::cancel(t, now).map_err(reject)?;
        }
        Ok(())
    })
}

fn task_claim(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ClaimParams = parse_params(params)?;
    let a = permit(ctx.state, ctx.caller, Op::Claim, Some(get(ctx.state, &p.task_id)?), None)?;
    let who = match (&a, p.session_id) {
        (Actor::Agent { session_id, .. }, None) => session_id.clone(),
        (Actor::Agent { session_id, .. }, Some(s)) if &s == session_id => s,
        (Actor::Agent { .. }, Some(_)) => {
            return Err(ToolError::Refused("an agent claims tasks for itself; use task_assign for another session".into()));
        }
        (_, Some(s)) => s,
        (_, None) => return Err(ToolError::InvalidParams("say which session takes it: session_id".into())),
    };
    let i = find(ctx.state, &who)?;
    let theirs = ctx.state.app.sessions.get(i).map(|s| project_of(ctx.state, s));
    if theirs.as_deref() != Some(get(ctx.state, &p.task_id)?.project.as_str()) {
        return Err(ToolError::Refused("that session works on another project".into()));
    }
    let now = now_ms();
    change(ctx, &p.task_id, "Claimed", |t, _| tasks::claim(t, &who, now).map_err(reject))
}

fn task_done(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NoteParams = parse_params(params)?;
    permit(ctx.state, ctx.caller, Op::Done, Some(get(ctx.state, &p.task_id)?), None)?;
    let n = note(p.note.as_deref())?;
    let now = now_ms();
    change(ctx, &p.task_id, "Ready for review:", |t, _| tasks::done(t, n, now).map_err(reject))
}

fn task_handoff(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NoteParams = parse_params(params)?;
    permit(ctx.state, ctx.caller, Op::Handoff, Some(get(ctx.state, &p.task_id)?), None)?;
    let n = note(p.note.as_deref())?;
    let now = now_ms();
    change(ctx, &p.task_id, "Back in the backlog:", |t, file| {
        let order = file.next_order(&t.project, TaskState::Backlog);
        tasks::handoff(t, n, order, now).map_err(reject)
    })
}

fn task_assign(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: AssignParams = parse_params(params.clone())?;
    if let Some(e) = precheck("task_assign", ctx.caller, &params, ctx.state) {
        return Err(e);
    }
    let i = find(ctx.state, &p.session_id)?;
    let target_project = ctx.state.app.sessions.get(i).map(|s| project_of(ctx.state, s));
    let mut t = get(ctx.state, &p.task_id)?.clone();
    if target_project.as_deref() != Some(t.project.as_str()) {
        return Err(ToolError::Refused("that session works on another project; assign the task to a session in its project".into()));
    }
    tasks::assign(&mut t, &p.session_id, p.force, now_ms()).map_err(reject)?;
    // Send first: a refused paste (asleep, starting, mid-turn) leaves the task as it was.
    let name = if p.send {
        let prompt = tasks::assignment_prompt(&t, &creator_name(ctx.state, &t));
        super::sessions::paste_into(ctx, &p.session_id, &prompt, true, p.force)?
    } else {
        session_name(ctx.state, &p.session_id).unwrap_or_default()
    };
    if let Some(slot) = ctx.state.rt.tasks.get_mut(&p.task_id) {
        *slot = t;
    }
    ctx.state.persist_tasks();
    let t = get(ctx.state, &p.task_id)?;
    let msg = if p.send { format!("Assigned {} to {name} and sent it.", t.id) } else { format!("Assigned {} to {name}.", t.id) };
    Ok(ToolOutput::new(msg, task_json(ctx.state, t)))
}

fn task_accept(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    permit(ctx.state, ctx.caller, Op::Accept, Some(get(ctx.state, &p.task_id)?), None)?;
    let now = now_ms();
    change(ctx, &p.task_id, "Accepted", |t, _| tasks::accept(t, now).map_err(reject))
}

fn task_reopen(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NoteParams = parse_params(params)?;
    permit(ctx.state, ctx.caller, Op::Reopen, Some(get(ctx.state, &p.task_id)?), None)?;
    let n = note(p.note.as_deref())?;
    let now = now_ms();
    change(ctx, &p.task_id, "Reopened", |t, _| tasks::reopen(t, n, now).map_err(reject))
}

fn task_delete(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    let t = get(ctx.state, &p.task_id)?;
    permit(ctx.state, ctx.caller, Op::Delete, Some(t), None)?;
    let title = t.title.clone();
    ctx.state.rt.tasks.tasks.retain(|t| t.id != p.task_id);
    ctx.state.persist_tasks();
    Ok(ToolOutput::new(format!("Deleted task {}: {title}.", p.task_id), json!({"task_id": p.task_id})))
}
