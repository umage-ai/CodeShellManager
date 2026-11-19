//! `pane_float`, `pane_dock`, `pane_tear_off`: real tools, registered by the app alongside the
//! engine's. They edit the Free-mode part of a group's layout, which the engine stores as an
//! opaque object (`GroupLayout::free_tree`) whose shape is the UI's ([`FreeLayout`]), so they
//! live here rather than in `csm-engine`. Like every tool they run on the engine thread, change
//! only the state they are given, and are reachable from the CLI and MCP too.

use csm_core::models::{ALL_LAYOUT_KEY, GroupDisplayMode};
use csm_engine::{Ctx, Effects, Reply, State, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use serde_json::{Value, json};

use crate::demo::tools::PaneParams;
use crate::engine_backend::{layout_from_core, layout_into_core};
use crate::model::{Layout, SessionId};

pub fn tools() -> Vec<ToolSpec> {
    let t = |id, title, summary, run| ToolSpec {
        id,
        title,
        summary,
        toolset: Toolset::Layout,
        effects: Effects::UI,
        input_schema: schema_of::<PaneParams>,
        run,
    };
    vec![
        t(
            "pane_float",
            "Float in this window",
            "Float a pane over the others (switches the group to the Free layout).",
            pane_float as csm_engine::ToolFn,
        ),
        t("pane_dock", "Dock into the split tree", "Dock a floating or torn-off pane back into the layout.", pane_dock),
        t("pane_tear_off", "Move to a new window", "Show a pane in its own OS window.", pane_tear_off),
    ]
}

/// The layout key for a `group` param: a group id, `all`, or (absent) the one shown now.
fn key_of(state: &State, group: Option<&str>) -> Result<String, ToolError> {
    match group {
        None => Ok(match (state.app.settings.group_display_mode, &state.app.ui.rail_tab) {
            (GroupDisplayMode::FilterStrip, Some(g)) => g.clone(),
            _ => ALL_LAYOUT_KEY.to_owned(),
        }),
        Some("all") | Some(ALL_LAYOUT_KEY) => Ok(ALL_LAYOUT_KEY.to_owned()),
        Some(g) if state.app.groups.iter().any(|x| x.id == g) => Ok(g.to_owned()),
        Some(g) => Err(ToolError::NotFound(format!("no group `{g}`"))),
    }
}

fn edit(ctx: &mut Ctx<'_>, params: Value, op: fn(&mut Layout, &str, &[SessionId]) -> &'static str) -> Reply {
    let p: PaneParams = parse_params(params)?;
    if !ctx.state.app.sessions.iter().any(|s| s.id == p.session_id) {
        return Err(ToolError::NotFound(format!("no session `{}`", p.session_id)));
    }
    let key = key_of(ctx.state, p.group.as_deref())?;
    let live: Vec<SessionId> =
        ctx.state.app.sessions.iter().filter(|s| !s.is_dormant && (key == ALL_LAYOUT_KEY || s.group_id == key)).map(|s| s.id.clone()).collect();
    let core = ctx.state.app.ui.layouts.entry(key.clone()).or_default();
    let mut l = layout_from_core(core);
    let message = op(&mut l, &p.session_id, &live);
    layout_into_core(&l, core);
    ctx.state.persist();
    Ok(ToolOutput::new(message, json!({"key": key, "free": l.free().to_value()})))
}

fn pane_float(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    edit(ctx, params, |l, id, live| {
        l.float_pane(id, live);
        "Floating. Drag its header to move it."
    })
}

fn pane_dock(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    edit(ctx, params, |l, id, _| if l.dock_pane(id) { "Docked back into the main window." } else { "Docked to the right of the split tree." })
}

fn pane_tear_off(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    edit(ctx, params, |l, id, _| {
        l.tear_off_pane(id);
        "In its own window now. Closing that window docks it back."
    })
}
