//! The engine's tools (docs/tools.md): sessions and the inbox, groups, layout and view, app.
//! Git, run commands, notes and search are separate modules not built yet.

mod app;
mod groups;
mod layout;
mod sessions;

use serde_json::Value;

use crate::engine::Ctx;
use crate::state::State;
use crate::tool::{Caller, ToolError, ToolSpec};

/// Every engine tool, in palette order.
pub(crate) fn specs() -> Vec<ToolSpec> {
    let mut v = sessions::specs();
    v.extend(groups::specs());
    v.extend(layout::specs());
    v.extend(app::specs());
    v
}

/// Rules that hold whatever the person would answer, checked before a consent card is shown
/// (and again after it is answered).
pub(crate) fn precheck(id: &str, caller: &Caller, params: &Value, _state: &State) -> Option<ToolError> {
    if id == "session_send_text"
        && let Caller::External { session_id: Some(own), .. } = caller
        && params.get("session_id").and_then(Value::as_str) == Some(own.as_str())
    {
        return Some(ToolError::Refused(
            "a session cannot send input to itself; that would be an agent typing into its own prompt".into(),
        ));
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
    crate::lifecycle::session_index(state, id)
        .ok_or_else(|| ToolError::NotFound(format!("no session `{id}`; `session_list` shows the ids")))
}

pub(crate) fn find_group(state: &State, id: &str) -> Result<usize, ToolError> {
    state
        .app
        .groups
        .iter()
        .position(|g| g.id == id)
        .ok_or_else(|| ToolError::NotFound(format!("no group `{id}`; `group_list` shows the ids")))
}

/// User-visible names: control characters removed, trimmed, capped.
pub(crate) fn clean_name(s: &str, cap: usize) -> String {
    s.chars().filter(|c| !c.is_control()).collect::<String>().trim().chars().take(cap).collect()
}
