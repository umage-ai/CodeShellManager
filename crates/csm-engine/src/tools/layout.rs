//! `layout` tools: group view and rail tab, a layout per group, pins, the inspector and the
//! inbox panel. All UI state (persisted so the app reopens as it was).

use csm_core::models::{ALL_LAYOUT_KEY, GroupDisplayMode, GroupLayout, InspectorTab, LayoutMode};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{current, find, find_group};
use crate::engine::{Ctx, Reply};
use crate::snapshot::{self, GroupView};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    let t = |id, title, summary, effects, input_schema, run| ToolSpec { id, title, summary, toolset: Toolset::Layout, effects, input_schema, run };
    vec![
        t("view_get", "Current view", "The group view (rail, headers, flat), the rail tab, the focused session, the inspector and the inbox panel.", Effects::READ_ONLY, empty_schema, view_get),
        t("view_set", "Change view…", "Switch the group view (rail, headers, flat) and/or the rail tab (a group id, or \"all\").", Effects::UI, schema_of::<ViewSetParams>, view_set),
        t("layout_get", "Current layout", "A group's layout (default: the one shown now): mode, grid size, pinned sessions.", Effects::READ_ONLY, schema_of::<LayoutGetParams>, layout_get),
        t(
            "layout_set",
            "Change layout…",
            "Set a group's layout: mode focus, side (pinned side by side), overview, grid (preset 1, 2, 2x2, 3x2, 3x3, or cols x rows) or free.",
            Effects::UI,
            schema_of::<LayoutSetParams>,
            layout_set,
        ),
        t("layout_pin", "Pin side by side", "Pin a session into the side-by-side layout.", Effects::UI, schema_of::<PinParams>, layout_pin),
        t("layout_unpin", "Unpin", "Remove a session from the side-by-side layout.", Effects::UI, schema_of::<PinParams>, layout_unpin),
        t("inspector_show", "Show inspector…", "Show the inspector on a tab (notes, runs, git, session) or hide it.", Effects::UI, schema_of::<InspectorParams>, inspector_show),
        t("inbox_panel_set", "Inbox panel", "Open or close the Needs-you panel and/or set its height.", Effects::UI, schema_of::<InboxPanelParams>, inbox_panel_set),
    ]
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ViewSetParams {
    view: Option<GroupView>,
    /// A group id, or `all`.
    rail_tab: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct LayoutGetParams {
    /// Group id, or `all`. Default: the layout shown now.
    group: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct LayoutSetParams {
    /// Group id, or `all`. Default: the layout shown now.
    group: Option<String>,
    mode: Option<ModeParam>,
    /// Grid preset: `1`, `2`, `2x2`, `3x2`, `3x3`. Implies mode grid.
    preset: Option<String>,
    /// Custom grid columns (1-8). Implies mode grid.
    cols: Option<u8>,
    /// Custom grid rows (1-8). Implies mode grid.
    rows: Option<u8>,
}

#[derive(Deserialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ModeParam {
    Focus,
    Side,
    Overview,
    Grid,
    Free,
}

impl From<ModeParam> for LayoutMode {
    fn from(m: ModeParam) -> Self {
        match m {
            ModeParam::Focus => LayoutMode::Focus,
            ModeParam::Side => LayoutMode::Side,
            ModeParam::Overview => LayoutMode::Overview,
            ModeParam::Grid => LayoutMode::Grid,
            ModeParam::Free => LayoutMode::Free,
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PinParams {
    session_id: String,
    /// Group id or `all`. Default: the layout shown now.
    #[serde(default)]
    group: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum InspectorParam {
    Notes,
    Runs,
    Git,
    Session,
    Hidden,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct InspectorParams {
    tab: InspectorParam,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct InboxPanelParams {
    open: Option<bool>,
    /// Logical pixels (40-2000).
    height: Option<f32>,
}

fn view_json(ctx: &mut Ctx<'_>) -> Value {
    let snap = current(ctx);
    json!({
        "view": snap.view,
        "rail_tab": snap.rail_tab,
        "layout_key": snap.layout_key,
        "focused": snap.focused,
        "inspector": snap.inspector,
        "inbox_open": snap.inbox_open,
        "inbox_height": snap.inbox_height,
    })
}

fn view_get(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let v = view_json(ctx);
    Ok(ToolOutput::new("The current view.", v))
}

fn view_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ViewSetParams = parse_params(params)?;
    if let Some(tab) = &p.rail_tab
        && tab != "all"
    {
        find_group(ctx.state, tab)?;
    }
    if let Some(v) = p.view {
        ctx.state.app.settings.group_display_mode = match v {
            GroupView::Rail => GroupDisplayMode::FilterStrip,
            GroupView::Headers => GroupDisplayMode::InlineHeaders,
            GroupView::Flat => GroupDisplayMode::None,
        };
    }
    if let Some(tab) = p.rail_tab {
        ctx.state.app.ui.rail_tab = (tab != "all").then_some(tab);
    }
    ctx.state.persist();
    let v = view_json(ctx);
    Ok(ToolOutput::new("View changed.", v))
}

/// Resolves a `group` param to a layout key.
fn key_of(ctx: &Ctx<'_>, group: Option<&str>) -> Result<String, ToolError> {
    match group {
        None => Ok(snapshot::layout_key(ctx.state)),
        Some("all") | Some(ALL_LAYOUT_KEY) => Ok(ALL_LAYOUT_KEY.to_owned()),
        Some(g) => {
            find_group(ctx.state, g)?;
            Ok(g.to_owned())
        }
    }
}

fn layout_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: LayoutGetParams = parse_params(params)?;
    let key = key_of(ctx, p.group.as_deref())?;
    let l = ctx.state.app.ui.layouts.get(&key).cloned().unwrap_or_default();
    Ok(ToolOutput::new(format!("{:?} layout", l.mode).to_lowercase(), json!({"key": key, "layout": l})))
}

fn parse_preset(p: &str) -> Option<(u8, u8)> {
    Some(match p.trim().to_ascii_lowercase().replace('×', "x").as_str() {
        "1" | "1x1" => (1, 1),
        "2" | "2x1" => (2, 1),
        "2x2" | "4" => (2, 2),
        "3x2" | "6" => (3, 2),
        "3x3" | "9" => (3, 3),
        _ => return None,
    })
}

fn layout_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: LayoutSetParams = parse_params(params)?;
    let key = key_of(ctx, p.group.as_deref())?;
    let mut l: GroupLayout = ctx.state.app.ui.layouts.get(&key).cloned().unwrap_or_default();
    if let Some(m) = p.mode {
        l.mode = m.into();
    }
    if let Some(preset) = &p.preset {
        let (c, r) = parse_preset(preset).ok_or_else(|| ToolError::InvalidParams(format!("no grid preset `{preset}` (1, 2, 2x2, 3x2, 3x3)")))?;
        l.grid_cols = c;
        l.grid_rows = r;
        l.mode = LayoutMode::Grid;
    }
    for (name, v) in [("cols", p.cols), ("rows", p.rows)] {
        if let Some(n) = v {
            if !(1..=8).contains(&n) {
                return Err(ToolError::InvalidParams(format!("`{name}` must be 1-8")));
            }
            if name == "cols" {
                l.grid_cols = n;
            } else {
                l.grid_rows = n;
            }
            l.mode = LayoutMode::Grid;
        }
    }
    ctx.state.app.ui.layouts.insert(key.clone(), l.clone());
    ctx.state.persist();
    Ok(ToolOutput::new("Layout changed.", json!({"key": key, "layout": l})))
}

fn layout_pin(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: PinParams = parse_params(params)?;
    find(ctx.state, &p.session_id)?;
    let key = key_of(ctx, p.group.as_deref())?;
    let l = ctx.state.app.ui.layouts.entry(key.clone()).or_default();
    if !l.pinned.contains(&p.session_id) {
        l.pinned.push(p.session_id.clone());
    }
    let pinned = l.pinned.clone();
    ctx.state.persist();
    Ok(ToolOutput::new("Pinned.", json!({"key": key, "pinned": pinned})))
}

fn layout_unpin(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: PinParams = parse_params(params)?;
    let key = key_of(ctx, p.group.as_deref())?;
    let pinned = match ctx.state.app.ui.layouts.get_mut(&key) {
        Some(l) => {
            l.pinned.retain(|s| *s != p.session_id);
            l.pinned.clone()
        }
        None => Vec::new(),
    };
    ctx.state.persist();
    Ok(ToolOutput::new("Unpinned.", json!({"key": key, "pinned": pinned})))
}

fn inspector_show(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: InspectorParams = parse_params(params)?;
    ctx.state.app.ui.inspector = match p.tab {
        InspectorParam::Notes => Some(InspectorTab::Notes),
        InspectorParam::Runs => Some(InspectorTab::Runs),
        InspectorParam::Git => Some(InspectorTab::Git),
        InspectorParam::Session => Some(InspectorTab::Session),
        InspectorParam::Hidden => None,
    };
    ctx.state.persist();
    Ok(ToolOutput::message(if ctx.state.app.ui.inspector.is_some() { "Inspector shown." } else { "Inspector hidden." }))
}

fn inbox_panel_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: InboxPanelParams = parse_params(params)?;
    if let Some(h) = p.height {
        if !h.is_finite() || !(40.0..=2000.0).contains(&h) {
            return Err(ToolError::InvalidParams("height must be 40-2000".into()));
        }
        ctx.state.app.ui.inbox_height = Some(h);
    }
    if let Some(o) = p.open {
        ctx.state.app.ui.inbox_open = o;
    }
    ctx.state.persist();
    Ok(ToolOutput::new("Inbox panel updated.", json!({"open": ctx.state.app.ui.inbox_open, "height": ctx.state.app.ui.inbox_height})))
}
