//! The sidebar's multi-select actions, its one-shot sort, and Duplicate.
//!
//! A selection of rows acts through the `*_many` tools (C# `ResolveActionTargets`: Restart (N)…,
//! Sleep (N), Wake (N), Close (N)…, Move to group). Each checks every id first and does nothing
//! when one is unknown. `session_sort` rewrites the order once (C# `ApplySort`); the person keeps
//! dragging afterwards. `session_duplicate` is the WPF "Duplicate session" (Ctrl+Alt+T).

use csm_core::models::{RunCommandItem, SessionStatus};
use csm_core::session_sort::{self, SortField, SortKey};
use csm_core::time::Timestamp;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{current, find, find_group};
use crate::engine::{Ctx, Reply};
use crate::lifecycle::{self, new_id};
use crate::session::now_ms;
use crate::snapshot;
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

/// More ids than any sidebar holds; a bound on what one call may ask for.
const MAX_IDS: usize = 1000;

pub(super) fn specs() -> Vec<ToolSpec> {
    let g = |id, title, summary, effects, input_schema, run| ToolSpec { id, title, summary, toolset: Toolset::Groups, effects, input_schema, run };
    vec![
        ToolSpec {
            id: "session_duplicate",
            title: "Duplicate session",
            summary: "Open a copy of a session: same folder, command, arguments, kind, connection, group, terminal profile and run commands, named like \"api (2)\" and placed right after it. A Claude copy starts a new conversation.",
            toolset: Toolset::Core,
            effects: Effects::MUTATES_STATE.union(Effects::LAUNCHES_PROCESS),
            input_schema: schema_of::<DuplicateParams>,
            run: session_duplicate,
        },
        g(
            "session_sort",
            "Sort sessions…",
            "Reorder the sidebar once by name, folder, last_active, branch, dirty or repo (descending defaults to the field's natural direction: newest and dirty first, text A to Z). group limits it to one group's sessions (\"\" = ungrouped), which keep their slots; default all.",
            Effects::MUTATES_STATE,
            schema_of::<SortParams>,
            session_sort,
        ),
        g(
            "session_sleep_many",
            "Sleep sessions",
            "Put several sessions to sleep (sleeping ones are skipped).",
            Effects::MAY_DISCARD_DATA.union(Effects::MUTATES_STATE),
            schema_of::<IdsParams>,
            session_sleep_many,
        ),
        g(
            "session_wake_many",
            "Wake sessions",
            "Wake several sleeping sessions (awake ones are skipped). Claude sessions resume their conversations, spaced by the launch stagger.",
            Effects::MUTATES_STATE.union(Effects::LAUNCHES_PROCESS),
            schema_of::<IdsParams>,
            session_wake_many,
        ),
        g(
            "session_restart_many",
            "Restart sessions…",
            "Restart several running sessions one after another (sleeping ones are skipped). Needs force: true for more than one (it takes a while). Replies when all have started again.",
            Effects::MAY_DISCARD_DATA.union(Effects::LAUNCHES_PROCESS),
            schema_of::<IdsForceParams>,
            session_restart_many,
        ),
        g(
            "session_close_many",
            "Close sessions…",
            "Close several sessions (they go to Recently closed). Refuses when any is busy unless force is true.",
            Effects::MAY_DISCARD_DATA.union(Effects::MUTATES_STATE),
            schema_of::<IdsForceParams>,
            session_close_many,
        ),
        g(
            "session_move_many",
            "Move sessions to group…",
            "Put several sessions in a group (group_id null = Ungrouped), keeping their order, at the end of the group or right before or after another session of that group.",
            Effects::MUTATES_STATE,
            schema_of::<MoveManyParams>,
            session_move_many,
        ),
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdsParams {
    /// The sessions, as `session_list` gives their ids.
    session_ids: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdsForceParams {
    session_ids: Vec<String>,
    /// Confirms the bulk action (the app asks the person first and then passes it).
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MoveManyParams {
    session_ids: Vec<String>,
    /// The group; null or omitted = Ungrouped.
    #[serde(default)]
    group_id: Option<String>,
    /// Place them right before this session (in the target group, not one of the moved).
    #[serde(default)]
    before: Option<String>,
    /// Place them right after this session (in the target group, not one of the moved).
    #[serde(default)]
    after: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum SortBy {
    Name,
    Folder,
    LastActive,
    Branch,
    Dirty,
    Repo,
}

impl From<SortBy> for SortField {
    fn from(b: SortBy) -> SortField {
        match b {
            SortBy::Name => SortField::Name,
            SortBy::Folder => SortField::Folder,
            SortBy::LastActive => SortField::LastActive,
            SortBy::Branch => SortField::Branch,
            SortBy::Dirty => SortField::Dirty,
            SortBy::Repo => SortField::Repo,
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SortParams {
    by: SortBy,
    /// Default: the field's natural direction (last_active and dirty descend, the rest ascend).
    #[serde(default)]
    descending: Option<bool>,
    /// A group id, "" for the ungrouped sessions, or "all" (default).
    #[serde(default)]
    group: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DuplicateParams {
    session_id: String,
    /// Focus the copy (default: true from the app, false for MCP clients).
    #[serde(default)]
    focus: Option<bool>,
}

/// The ids, each known, without repeats, in sidebar order.
fn resolve(ctx: &Ctx<'_>, ids: &[String]) -> Result<Vec<String>, ToolError> {
    if ids.is_empty() {
        return Err(ToolError::InvalidParams("`session_ids` is empty".into()));
    }
    if ids.len() > MAX_IDS {
        return Err(ToolError::InvalidParams(format!("at most {MAX_IDS} sessions at once")));
    }
    for id in ids {
        find(ctx.state, id)?;
    }
    Ok(ctx.state.app.sessions.iter().filter(|s| ids.contains(&s.id)).map(|s| s.id.clone()).collect())
}

fn session_sleep_many(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdsParams = parse_params(params)?;
    let ids = resolve(ctx, &p.session_ids)?;
    let live: Vec<String> = ids.into_iter().filter(|id| ctx.state.app.sessions.iter().any(|s| s.id == *id && !s.is_dormant)).collect();
    let mut slept = Vec::new();
    let mut failed = Vec::new();
    for id in &live {
        match lifecycle::sleep_session(ctx, id) {
            Ok(_) => slept.push(id.clone()),
            Err(e) => failed.push(json!({"session_id": id, "error": e.to_string()})),
        }
    }
    let message = match (slept.len(), failed.len()) {
        (0, 0) => "Nothing to do: they are all asleep already.".to_owned(),
        (n, 0) => format!("{n} sessions are asleep."),
        (n, f) => format!("{n} sessions are asleep; {f} could not sleep now."),
    };
    Ok(ToolOutput::new(message, json!({"slept": slept, "failed": failed})))
}

fn session_wake_many(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdsParams = parse_params(params)?;
    let ids = resolve(ctx, &p.session_ids)?;
    if ctx.state.rt.shutting_down {
        return Err(ToolError::Refused("the app is closing".into()));
    }
    let asleep: Vec<String> = ids.into_iter().filter(|id| ctx.state.app.sessions.iter().any(|s| s.id == *id && s.is_dormant)).collect();
    let mut woken = Vec::new();
    let mut failed = Vec::new();
    for id in &asleep {
        match lifecycle::wake_session(ctx, id, None) {
            Ok(_) => woken.push(id.clone()),
            Err(e) => failed.push(json!({"session_id": id, "error": e.to_string()})),
        }
    }
    let message = match (woken.len(), failed.len()) {
        (0, 0) => "Nothing to do: they are all awake.".to_owned(),
        (n, 0) => format!("Waking {n} sessions."),
        (n, f) => format!("Waking {n} sessions; {f} could not be woken."),
    };
    Ok(ToolOutput::new(message, json!({"woken": woken, "failed": failed})))
}

fn session_restart_many(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdsForceParams = parse_params(params)?;
    let ids = resolve(ctx, &p.session_ids)?;
    let live: Vec<String> = ids.into_iter().filter(|id| ctx.state.app.sessions.iter().any(|s| s.id == *id && !s.is_dormant)).collect();
    if live.is_empty() {
        return Ok(ToolOutput::message("Nothing to restart: none of them is running."));
    }
    if let Some(err) = lifecycle::restart_refusal(ctx.state) {
        return Err(err);
    }
    if live.len() > 1 && !p.force {
        let claude = ctx.state.app.sessions.iter().filter(|s| live.contains(&s.id) && crate::launch::is_claude_command(&s.command)).count();
        let total = i64::try_from(live.len()).unwrap_or(i64::MAX);
        let text =
            csm_core::bulk_restart::build_confirm_text(&format!("Restart {} sessions?", live.len()), total, i64::try_from(claude).unwrap_or(0));
        return Err(ToolError::Refused(format!("{text}\nPass force: true to go ahead.")));
    }
    let reply = ctx.park();
    lifecycle::restart_many(ctx, live, reply);
    Ok(ToolOutput::message("Restarting."))
}

fn session_close_many(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdsForceParams = parse_params(params)?;
    let ids = resolve(ctx, &p.session_ids)?;
    if !p.force {
        let busy: Vec<String> = ids.iter().filter_map(|id| lifecycle::busy_reason(ctx.state, id).map(|why| format!("{id} ({why})"))).collect();
        if !busy.is_empty() {
            return Err(ToolError::Refused(format!("{} sessions are busy: {}. Pass force: true to close them anyway.", busy.len(), busy.join(", "))));
        }
    }
    let mut closed = Vec::new();
    for id in &ids {
        if lifecycle::close_session(ctx, id, true).is_ok() {
            closed.push(id.clone());
        }
    }
    Ok(ToolOutput::new(format!("Closed {} sessions. Ctrl+Shift+T reopens them one by one.", closed.len()), json!({"closed": closed})))
}

/// Anchor-based like `session_move_to_group`: the first moved session goes before / after the
/// anchor (or after the group's last other session), each next one right after the previous,
/// so the moved block keeps its order and lands as one.
fn session_move_many(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: MoveManyParams = parse_params(params)?;
    let ids = resolve(ctx, &p.session_ids)?;
    let group = p.group_id.clone().unwrap_or_default();
    if !group.is_empty() {
        find_group(ctx.state, &group)?;
    }
    let anchor = match (&p.before, &p.after) {
        (Some(_), Some(_)) => return Err(ToolError::InvalidParams("give `before` or `after`, not both".into())),
        (Some(b), None) => Some((b.clone(), false)),
        (None, Some(a)) => Some((a.clone(), true)),
        (None, None) => None,
    };
    if let Some((a, _)) = &anchor {
        if ids.contains(a) {
            return Err(ToolError::InvalidParams(format!("`{a}` is one of the sessions being moved; anchor on another one")));
        }
        let ai = find(ctx.state, a)?;
        if ctx.state.app.sessions.get(ai).is_some_and(|s| s.group_id != group) {
            return Err(ToolError::InvalidParams(format!("`{a}` is not in the target group")));
        }
    }
    let first_anchor =
        anchor.or_else(|| ctx.state.app.sessions.iter().rev().find(|s| s.group_id == group && !ids.contains(&s.id)).map(|s| (s.id.clone(), true)));
    for s in ctx.state.app.sessions.iter_mut().filter(|s| ids.contains(&s.id)) {
        s.group_id = group.clone();
    }
    let sessions = &mut ctx.state.app.sessions;
    let mut prev: Option<String> = None;
    for id in &ids {
        match (&prev, &first_anchor) {
            (Some(p), _) => {
                csm_core::session_order::move_relative(sessions, |s| s.id.as_str(), id, p, true);
            }
            (None, Some((a, after))) => {
                csm_core::session_order::move_relative(sessions, |s| s.id.as_str(), id, a, *after);
            }
            // An empty group and no anchor: the first stays where it is; the rest follow it.
            (None, None) => {}
        }
        prev = Some(id.clone());
    }
    ctx.state.persist();
    let gname = ctx.state.app.groups.iter().find(|g| g.id == group).map_or("Ungrouped".to_owned(), |g| g.name.clone());
    Ok(ToolOutput::new(format!("Moved {} sessions to {gname}.", ids.len()), json!({"session_ids": ids, "group_id": group})))
}

fn sort_key(v: &snapshot::SessionView) -> SortKey {
    SortKey {
        id: v.id.clone(),
        name: v.name.clone(),
        folder: v.folder.clone(),
        last_active_ms: v.last_activity_ms,
        branch: v.branch.clone(),
        // Only a live session's git state is known (WPF: only live view models carry it).
        dirty: v.live.then_some(v.git_dirty),
        repo_root: v.repo_root.clone(),
    }
}

fn session_sort(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SortParams = parse_params(params)?;
    let field: SortField = p.by.into();
    let descending = p.descending.unwrap_or_else(|| field.default_descending());
    // None = every session; Some(g) = the sessions whose group_id is g ("" = ungrouped).
    let scope: Option<String> = match p.group.as_deref() {
        None | Some("all") => None,
        Some("") => Some(String::new()),
        Some(g) => {
            find_group(ctx.state, g)?;
            Some(g.to_owned())
        }
    };
    let snap = current(ctx);
    let in_scope = |gid: &str| scope.as_deref().is_none_or(|g| g == gid);
    let keys: Vec<SortKey> = snap.sessions.iter().filter(|v| in_scope(&v.group_id)).map(sort_key).collect();
    let order = session_sort::sorted_ids(&keys, field, descending);
    let moved = session_sort::reorder_in_place(&mut ctx.state.app.sessions, |s| s.id.as_str(), |s| in_scope(&s.group_id), &order);
    if moved {
        ctx.state.persist();
    }
    let dir = match (field, descending) {
        (SortField::LastActive, true) => "newest first",
        (SortField::LastActive, false) => "oldest first",
        (SortField::Dirty, true) => "dirty first",
        (SortField::Dirty, false) => "clean first",
        (_, false) => "A to Z",
        (_, true) => "Z to A",
    };
    let message = if moved {
        format!("Sorted {} sessions by {} ({dir}).", order.len(), field.label().to_lowercase())
    } else {
        format!("Already sorted by {} ({dir}).", field.label().to_lowercase())
    };
    Ok(ToolOutput::new(message, json!({"by": field.id(), "descending": descending, "order": order, "changed": moved})))
}

fn session_duplicate(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: DuplicateParams = parse_params(params)?;
    if ctx.state.rt.shutting_down {
        return Err(ToolError::Refused("the app is closing".into()));
    }
    let i = find(ctx.state, &p.session_id)?;
    let Some(parent) = ctx.state.app.sessions.get(i).cloned() else { return Err(ToolError::NotFound("gone".into())) };
    let base = if parent.name.trim().is_empty() { parent.default_display_name() } else { parent.name.clone() };
    let taken: Vec<String> =
        ctx.state.app.sessions.iter().map(|s| if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() }).collect();
    let now = Timestamp::from_epoch_ms(now_ms());
    let mut s = parent.clone();
    s.id = new_id();
    s.name = csm_core::session_names::duplicate_name(&base, &taken);
    // A program-pushed colour belongs to the original's process (WPF passes no override).
    s.color_override = None;
    s.status = SessionStatus::Running;
    s.created_at = now.clone();
    s.last_activity_at = now;
    // A copy starts its own conversation.
    s.claude_session_id = None;
    s.is_dormant = false;
    s.run_commands = parent.run_commands.iter().map(|r| RunCommandItem { id: new_id(), ..r.clone() }).collect();
    let id = s.id.clone();
    let name = s.name.clone();
    let seed = s.run_commands.is_empty();
    ctx.state.app.sessions.insert((i + 1).min(ctx.state.app.sessions.len()), s);
    if let Err(msg) = lifecycle::launch(ctx, &id, false) {
        if let Some(j) = lifecycle::session_index(ctx.state, &id) {
            ctx.state.app.sessions.remove(j);
        }
        return Err(ToolError::Failed(format!("The copy could not be started: {msg}")));
    }
    if seed {
        crate::runs::seed_async(ctx, &id);
    }
    if p.focus.unwrap_or(!ctx.caller.is_external()) {
        lifecycle::focus(ctx, &id)?;
    }
    ctx.state.persist();
    Ok(ToolOutput::new(format!("Duplicated {} as {name}.", parent.name), json!({"session_id": id, "name": name, "after": p.session_id})))
}
