//! The engine's tools (docs/tools.md): sessions and the inbox, groups, layout and view, app.
//! Git is in [`git`]; notes and search are in `search`; run commands are a separate module not built yet.

mod addons;
mod app;
mod autostart;
mod backup;
mod bulk;
mod docs;
pub(crate) mod git;
mod groups;
mod layout;
pub(crate) mod look;
pub mod open;
mod platform;
mod runs;
mod screens;
mod search;
mod remotes;
mod sessions;
mod slash;
mod snapshots;
mod tasks;
mod usage;

use serde_json::Value;

use crate::engine::Ctx;
use crate::state::State;
use crate::tool::{Caller, ToolError, ToolSpec};

/// Every engine tool, in palette order.
pub(crate) fn specs() -> Vec<ToolSpec> {
    let mut v = sessions::specs();
    v.extend(groups::specs());
    v.extend(bulk::specs());
    v.extend(layout::specs());
    v.extend(app::specs());
    v.extend(autostart::specs());
    v.extend(git::specs());
    v.extend(runs::specs());
    v.extend(open::specs());
    v.extend(platform::specs());
    v.extend(look::specs());
    v.extend(search::specs());
    v.extend(docs::specs());
    v.extend(slash::specs());
    v.extend(backup::specs());
    v.extend(usage::specs());
    v.extend(snapshots::specs());
    v.extend(tasks::specs());
    v.extend(addons::specs());
    v.extend(remotes::specs());
    v.extend(screens::specs());
    v
}

/// Rules that hold whatever the person would answer, checked before a consent card is shown
/// (and again after it is answered).
pub(crate) fn precheck(id: &str, caller: &Caller, params: &Value, state: &State) -> Option<ToolError> {
    if matches!(id, "session_send_text" | "session_send_command")
        && let Caller::External { session_id: Some(own), .. } = caller
        && params.get("session_id").and_then(Value::as_str) == Some(own.as_str())
    {
        return Some(ToolError::Refused("a session cannot send input to itself; that would be an agent typing into its own prompt".into()));
    }
    if let Some(e) = backup::precheck(id, caller).or_else(|| app::precheck(id, caller)).or_else(|| open::precheck(id, caller)) {
        return Some(e);
    }
    if id.starts_with("task_")
        && let Some(e) = tasks::precheck(id, caller, params, state)
    {
        return Some(e);
    }
    None
}

/// The current snapshot, rebuilt first if this step changed anything.
pub(crate) fn current(ctx: &mut Ctx<'_>) -> std::sync::Arc<crate::snapshot::Snapshot> {
    crate::lifecycle::after_step(ctx.state, ctx.engine);
    ctx.state.rt.prev.clone()
}

/// Index of a session, or a clear error.
pub(crate) fn find(state: &State, id: &str) -> Result<usize, ToolError> {
    crate::lifecycle::session_index(state, id).ok_or_else(|| ToolError::NotFound(format!("no session `{id}`; `session_list` shows the ids")))
}

pub(crate) fn find_group(state: &State, id: &str) -> Result<usize, ToolError> {
    state.app.groups.iter().position(|g| g.id == id).ok_or_else(|| ToolError::NotFound(format!("no group `{id}`; `group_list` shows the ids")))
}

/// User-visible names: control characters removed, trimmed, capped.
pub(crate) fn clean_name(s: &str, cap: usize) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(cap).collect()
}
