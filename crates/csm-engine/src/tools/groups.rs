//! `groups` tools: the group list, moving sessions between groups, and the group menu's bulk
//! actions (restart all, sleep all, close all, tear off / dock).

use csm_core::models::SessionGroup;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{clean_name, current, find, find_group};
use crate::engine::{Ctx, Reply};
use crate::lifecycle::{self, new_id};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    let t = |id, title, summary, effects, input_schema, run| ToolSpec { id, title, summary, toolset: Toolset::Groups, effects, input_schema, run };
    vec![
        t(
            "group_list",
            "List groups",
            "Every group in order: id, name, session count, how many need you.",
            Effects::READ_ONLY,
            empty_schema,
            group_list,
        ),
        t("group_new", "New group…", "Create a group.", Effects::MUTATES_STATE, schema_of::<NameParams>, group_new),
        t("group_rename", "Rename group…", "Rename a group.", Effects::MUTATES_STATE, schema_of::<RenameParams>, group_rename),
        t(
            "group_delete",
            "Delete group",
            "Delete a group; its sessions move to Ungrouped (nothing is closed).",
            Effects::MUTATES_STATE,
            schema_of::<GroupParams>,
            group_delete,
        ),
        t(
            "group_move",
            "Move group",
            "Move a group to a position in the group order (0 = first).",
            Effects::MUTATES_STATE,
            schema_of::<MoveGroupParams>,
            group_move,
        ),
        t(
            "group_collapse",
            "Collapse group",
            "Collapse or expand a group's section in the headers view.",
            Effects::UI,
            schema_of::<CollapseParams>,
            group_collapse,
        ),
        t(
            "session_move_to_group",
            "Move to group…",
            "Put a session in a group (group_id null = Ungrouped), optionally right before or after another session of that group.",
            Effects::MUTATES_STATE,
            schema_of::<MoveParams>,
            session_move_to_group,
        ),
        t(
            "group_restart_all",
            "Restart all in group…",
            "Restart every live session of a group, one after another. Needs force: true when it is more than one session (it takes a while).",
            Effects::MAY_DISCARD_DATA.union(Effects::LAUNCHES_PROCESS),
            schema_of::<ForceGroupParams>,
            group_restart_all,
        ),
        t(
            "group_sleep_all",
            "Sleep all in group",
            "Put every live session of a group to sleep.",
            Effects::MAY_DISCARD_DATA.union(Effects::MUTATES_STATE),
            schema_of::<GroupParams>,
            group_sleep_all,
        ),
        t(
            "group_close_all",
            "Close all in group…",
            "Close every session of a group (live and asleep). Refuses when any is busy unless force is true.",
            Effects::MAY_DISCARD_DATA.union(Effects::MUTATES_STATE),
            schema_of::<ForceGroupParams>,
            group_close_all,
        ),
        t(
            "group_tear_off",
            "Move group to its own window",
            "Show a group in its own OS window.",
            Effects::UI,
            schema_of::<GroupParams>,
            group_tear_off,
        ),
        t("group_dock", "Dock group", "Bring a torn-off group back into the main window.", Effects::UI, schema_of::<GroupParams>, group_dock),
    ]
}

const MAX_GROUP_NAME: usize = 80;

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NameParams {
    name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GroupParams {
    group_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenameParams {
    group_id: String,
    name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForceGroupParams {
    group_id: String,
    /// Confirms a bulk action (the app asks the person first and then passes it).
    #[serde(default)]
    force: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MoveGroupParams {
    group_id: String,
    index: usize,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CollapseParams {
    group_id: String,
    collapsed: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct MoveParams {
    session_id: String,
    /// The group; null or omitted = Ungrouped.
    #[serde(default)]
    group_id: Option<String>,
    /// Place it right before this session (which must be in the target group).
    #[serde(default)]
    before: Option<String>,
    /// Place it right after this session (which must be in the target group).
    #[serde(default)]
    after: Option<String>,
}

fn group_list(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let snap = current(ctx);
    Ok(ToolOutput::new(format!("{} groups", snap.groups.len()), json!(snap.groups)))
}

fn group_new(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NameParams = parse_params(params)?;
    let name = clean_name(&p.name, MAX_GROUP_NAME);
    if name.is_empty() {
        return Err(ToolError::InvalidParams("the group name is empty".into()));
    }
    let id = new_id();
    let order = i32::try_from(ctx.state.app.groups.len()).unwrap_or(i32::MAX);
    ctx.state.app.groups.push(SessionGroup { id: id.clone(), name: name.clone(), sort_order: order, is_expanded: true });
    ctx.state.persist();
    Ok(ToolOutput::new(format!("Created group {name}."), json!({"group_id": id, "name": name})))
}

fn group_rename(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: RenameParams = parse_params(params)?;
    let gi = find_group(ctx.state, &p.group_id)?;
    let name = clean_name(&p.name, MAX_GROUP_NAME);
    if name.is_empty() {
        return Err(ToolError::InvalidParams("the group name is empty".into()));
    }
    if let Some(g) = ctx.state.app.groups.get_mut(gi) {
        g.name = name.clone();
    }
    ctx.state.persist();
    Ok(ToolOutput::new(format!("Renamed the group to {name}."), json!({"group_id": p.group_id})))
}

fn group_delete(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: GroupParams = parse_params(params)?;
    let gi = find_group(ctx.state, &p.group_id)?;
    let g = ctx.state.app.groups.remove(gi);
    let mut moved = 0;
    for s in &mut ctx.state.app.sessions {
        if s.group_id == g.id {
            s.group_id.clear();
            moved += 1;
        }
    }
    for (i, g) in ctx.state.app.groups.iter_mut().enumerate() {
        g.sort_order = i32::try_from(i).unwrap_or(i32::MAX);
    }
    let ui = &mut ctx.state.app.ui;
    ui.layouts.remove(&g.id);
    ui.torn_off_groups.retain(|t| *t != g.id);
    ui.group_focus.remove(&g.id);
    if ui.rail_tab.as_deref() == Some(g.id.as_str()) {
        ui.rail_tab = None;
    }
    ctx.state.persist();
    Ok(ToolOutput::new(format!("Deleted group {}; {moved} sessions are now ungrouped.", g.name), json!({"moved": moved})))
}

fn group_move(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: MoveGroupParams = parse_params(params)?;
    let gi = find_group(ctx.state, &p.group_id)?;
    let g = ctx.state.app.groups.remove(gi);
    let at = p.index.min(ctx.state.app.groups.len());
    ctx.state.app.groups.insert(at, g);
    for (i, g) in ctx.state.app.groups.iter_mut().enumerate() {
        g.sort_order = i32::try_from(i).unwrap_or(i32::MAX);
    }
    ctx.state.persist();
    Ok(ToolOutput::new("Moved the group.", json!({"group_id": p.group_id, "index": at})))
}

fn group_collapse(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: CollapseParams = parse_params(params)?;
    let gi = find_group(ctx.state, &p.group_id)?;
    if let Some(g) = ctx.state.app.groups.get_mut(gi) {
        g.is_expanded = !p.collapsed;
    }
    ctx.state.persist();
    Ok(ToolOutput::message(if p.collapsed { "Collapsed." } else { "Expanded." }))
}

/// Anchor-based: the move resolves to "before/after session X" in the one ordered list, never an
/// index carried from another list (the WPF drag-and-drop bug's five causes).
fn session_move_to_group(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: MoveParams = parse_params(params)?;
    find(ctx.state, &p.session_id)?;
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
        let ai = find(ctx.state, a)?;
        if ctx.state.app.sessions.get(ai).is_some_and(|s| s.group_id != group) {
            return Err(ToolError::InvalidParams(format!("`{a}` is not in the target group")));
        }
    }
    let i = find(ctx.state, &p.session_id)?;
    if let Some(s) = ctx.state.app.sessions.get_mut(i) {
        s.group_id = group.clone();
    }
    let sessions = &mut ctx.state.app.sessions;
    match anchor {
        Some((a, after)) => {
            csm_core::session_order::move_relative(sessions, |s| s.id.as_str(), &p.session_id, &a, after);
        }
        None => {
            // No anchor: the end of the group, so it is found where the group's rows are.
            let last = sessions.iter().rev().find(|s| s.group_id == group && s.id != p.session_id).map(|s| s.id.clone());
            if let Some(last) = last {
                csm_core::session_order::move_relative(sessions, |s| s.id.as_str(), &p.session_id, &last, true);
            }
        }
    }
    ctx.state.persist();
    let gname = ctx.state.app.groups.iter().find(|g| g.id == group).map_or("Ungrouped".to_owned(), |g| g.name.clone());
    Ok(ToolOutput::new(format!("Moved to {gname}."), json!({"session_id": p.session_id, "group_id": group})))
}

fn live_in_group(ctx: &Ctx<'_>, group: &str) -> Vec<String> {
    ctx.state.app.sessions.iter().filter(|s| s.group_id == group && !s.is_dormant).map(|s| s.id.clone()).collect()
}

fn group_restart_all(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ForceGroupParams = parse_params(params)?;
    let gi = find_group(ctx.state, &p.group_id)?;
    let ids = live_in_group(ctx, &p.group_id);
    if ids.is_empty() {
        return Ok(ToolOutput::message("Nothing to restart: no session in the group is running."));
    }
    if let Some(err) = lifecycle::restart_refusal(ctx.state) {
        return Err(err);
    }
    if ids.len() > 1 && !p.force {
        let claude = ctx.state.app.sessions.iter().filter(|s| ids.contains(&s.id) && crate::launch::is_claude_command(&s.command)).count();
        let total = i64::try_from(ids.len()).unwrap_or(i64::MAX);
        let name = ctx.state.app.groups.get(gi).map(|g| g.name.clone()).unwrap_or_default();
        let text = csm_core::bulk_restart::build_confirm_text(&format!("Restart all in {name}?"), total, i64::try_from(claude).unwrap_or(0));
        return Err(ToolError::Refused(format!("{text}\nPass force: true to go ahead.")));
    }
    let reply = ctx.park();
    lifecycle::restart_many(ctx, ids, reply);
    Ok(ToolOutput::message("Restarting."))
}

fn group_sleep_all(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: GroupParams = parse_params(params)?;
    find_group(ctx.state, &p.group_id)?;
    let ids = live_in_group(ctx, &p.group_id);
    let mut n = 0;
    for id in &ids {
        if lifecycle::sleep_session(ctx, id).is_ok() {
            n += 1;
        }
    }
    Ok(ToolOutput::new(format!("{n} sessions are asleep."), json!({"slept": n})))
}

fn group_close_all(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ForceGroupParams = parse_params(params)?;
    find_group(ctx.state, &p.group_id)?;
    let ids: Vec<String> = ctx.state.app.sessions.iter().filter(|s| s.group_id == p.group_id).map(|s| s.id.clone()).collect();
    if !p.force {
        let busy: Vec<String> = ids.iter().filter_map(|id| lifecycle::busy_reason(ctx.state, id).map(|why| format!("{id} ({why})"))).collect();
        if !busy.is_empty() {
            return Err(ToolError::Refused(format!("{} sessions are busy: {}. Pass force: true to close them anyway.", busy.len(), busy.join(", "))));
        }
    }
    let mut n = 0;
    for id in &ids {
        if lifecycle::close_session(ctx, id, true).is_ok() {
            n += 1;
        }
    }
    Ok(ToolOutput::new(format!("Closed {n} sessions."), json!({"closed": n})))
}

fn group_tear_off(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: GroupParams = parse_params(params)?;
    find_group(ctx.state, &p.group_id)?;
    if !ctx.state.app.ui.torn_off_groups.contains(&p.group_id) {
        ctx.state.app.ui.torn_off_groups.push(p.group_id.clone());
    }
    ctx.state.persist();
    Ok(ToolOutput::new("The group has its own window.", json!({"group_id": p.group_id})))
}

fn group_dock(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: GroupParams = parse_params(params)?;
    find_group(ctx.state, &p.group_id)?;
    ctx.state.app.ui.torn_off_groups.retain(|g| *g != p.group_id);
    ctx.state.persist();
    Ok(ToolOutput::new("The group is back in the main window.", json!({"group_id": p.group_id})))
}
