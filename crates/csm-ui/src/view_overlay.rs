//! The per-client view overlay (`docs/design/remote-csm.md`, section 2.3).
//!
//! The engine keeps the view state per app: the focused session, the rail tab, the layouts, the
//! inbox panel, the inspector, the group view, the window placements, and each group's collapse,
//! tear-off and dock. A remote client keeps its own copy of these, and a call whose declared
//! effects are only [`Effects::CHANGES_UI_STATE`] runs against that copy, with the rules the
//! engine applies. Every other call is forwarded to the server.
//!
//! Nothing is wired into the live app yet. The engine stays the owner, and server-side callers
//! keep using the engine's own view state.
//!
//! - [`is_ui_only`] and [`route`] classify a tool from its effects.
//! - [`apply`] runs a UI-only call against a [`ViewOverlay`], given the groups and sessions it
//!   needs to check the call ([`ViewFacts`]).
//!
//! A UI-only tool whose state is not in the overlay (the window's own OS actions, the add-on
//! screens, a session's status) gets [`Applied::NotInOverlay`]. The caller runs or forwards it.

use std::collections::{BTreeMap, BTreeSet};

use csm_core::models::{ALL_LAYOUT_KEY, GroupLayout, InspectorTab, LayoutMode, WindowPlacementState, WindowSlot};
use csm_engine::{Effects, GroupView, Snapshot, ToolError, parse_params};
use serde::Deserialize;
use serde_json::Value;

use crate::backend::ToolInfo;

/// The one tool that is UI-only in part: a local focus, and a `session_wake` when asleep.
pub const SESSION_FOCUS: &str = "session_focus";

/// The window-placement size limit, in physical pixels (the engine's own).
const MAX_WINDOW_PX: i32 = 1 << 16;

/// How a client routes a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    /// Only `CHANGES_UI_STATE`: applied to the overlay and never sent.
    Local,
    /// `session_focus`: a local focus, plus `session_wake` when the session is asleep.
    Focus,
    /// Anything else: forwarded to the server.
    Forward,
}

/// Whether the effects are UI-only: `CHANGES_UI_STATE`, with `IDEMPOTENT` allowed beside it.
/// `IDEMPOTENT` says the result is stable; it does not change any state, so it does not count.
/// Every other effect bit (launch, input, mutation, network, content...) makes the call forwarded.
pub fn is_ui_only(effects: Effects) -> bool {
    effects.difference(Effects::IDEMPOTENT) == Effects::CHANGES_UI_STATE
}

/// The route of a tool, from its declared effects. `session_focus` (UI plus launch, since waking
/// starts a process) is [`Route::Focus`]; the server only ever gets its wake.
pub fn route(tool: &ToolInfo) -> Route {
    let focus_effects = Effects::UI.union(Effects::LAUNCHES_PROCESS);
    if tool.id == SESSION_FOCUS && tool.effects.difference(Effects::IDEMPOTENT) == focus_effects {
        Route::Focus
    } else if is_ui_only(tool.effects) {
        Route::Local
    } else {
        Route::Forward
    }
}

/// One session, as the overlay checks calls against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFact {
    pub id: String,
    /// The group id; empty when the session is ungrouped.
    pub group: String,
    pub asleep: bool,
}

/// The groups, sessions and inbox a UI-only call is checked against, as the server has them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewFacts {
    /// Group ids.
    pub groups: BTreeSet<String>,
    /// Sessions, in sidebar order.
    pub sessions: Vec<SessionFact>,
    /// Session ids in the inbox: needs-you first, then unseen results.
    pub inbox: Vec<String>,
}

impl ViewFacts {
    /// The facts of a snapshot.
    pub fn from_snapshot(snap: &Snapshot) -> Self {
        Self {
            groups: snap.groups.iter().map(|g| g.id.clone()).collect(),
            sessions: snap
                .sessions
                .iter()
                .map(|s| SessionFact { id: s.id.clone(), group: s.group_id.clone(), asleep: s.asleep })
                .collect(),
            inbox: snap.inbox.clone(),
        }
    }

    fn session(&self, id: &str) -> Option<&SessionFact> {
        self.sessions.iter().find(|s| s.id == id)
    }

    fn has_session(&self, id: &str) -> bool {
        self.session(id).is_some()
    }

    fn has_group(&self, id: &str) -> bool {
        self.groups.contains(id)
    }

    fn require_group(&self, id: &str) -> Result<(), ToolError> {
        if self.has_group(id) { Ok(()) } else { Err(ToolError::NotFound(format!("no group `{id}`"))) }
    }

    fn require_session(&self, id: &str) -> Result<&SessionFact, ToolError> {
        self.session(id).ok_or_else(|| ToolError::NotFound(format!("no session `{id}`")))
    }
}

/// A client's copy of the view state. The fields match the engine's; see the module docs.
#[derive(Debug, Clone, PartialEq)]
pub struct ViewOverlay {
    /// The focused session.
    pub focused: Option<String>,
    /// The rail's group; `None` is "All".
    pub rail_tab: Option<String>,
    /// Layout per group id, plus [`ALL_LAYOUT_KEY`].
    pub layouts: BTreeMap<String, GroupLayout>,
    pub inbox_open: bool,
    pub inbox_height: Option<f32>,
    pub inspector: Option<InspectorTab>,
    pub view: GroupView,
    /// Window slots by key: `main`, `group:<id>` and `pane:<session id>`.
    pub windows: BTreeMap<String, WindowSlot>,
    /// Groups whose section is collapsed (the headers view).
    pub collapsed: BTreeSet<String>,
    /// Groups living in their own window.
    pub torn_off: BTreeSet<String>,
    /// The session last focused in each group: the group's rail tab brings it back (issue #137).
    /// The snapshot does not carry it, so a client seeds it from the server's state.
    pub group_focus: BTreeMap<String, String>,
}

impl Default for ViewOverlay {
    fn default() -> Self {
        Self {
            focused: None,
            rail_tab: None,
            layouts: BTreeMap::new(),
            inbox_open: true,
            inbox_height: None,
            inspector: None,
            view: GroupView::default(),
            windows: BTreeMap::new(),
            collapsed: BTreeSet::new(),
            torn_off: BTreeSet::new(),
            group_focus: BTreeMap::new(),
        }
    }
}

impl ViewOverlay {
    /// The view state of a snapshot. `group_focus` is not in a snapshot and starts empty.
    pub fn from_snapshot(snap: &Snapshot) -> Self {
        Self {
            focused: snap.focused.clone(),
            rail_tab: snap.rail_tab.clone(),
            layouts: snap.layouts.clone(),
            inbox_open: snap.inbox_open,
            inbox_height: snap.inbox_height,
            inspector: snap.inspector,
            view: snap.view,
            windows: snap.windows.clone(),
            collapsed: snap.groups.iter().filter(|g| !g.expanded).map(|g| g.id.clone()).collect(),
            torn_off: snap.groups.iter().filter(|g| g.torn_off).map(|g| g.id.clone()).collect(),
            group_focus: BTreeMap::new(),
        }
    }

    /// The layout key the panes use now: the rail tab's group in the rail view, else `__ALL__`.
    pub fn layout_key(&self) -> String {
        match (self.view, &self.rail_tab) {
            (GroupView::Rail, Some(g)) => g.clone(),
            _ => ALL_LAYOUT_KEY.to_owned(),
        }
    }

    /// The layout key a call's `group` param names: `None` is the layout shown now.
    fn key_of(&self, facts: &ViewFacts, group: Option<&str>) -> Result<String, ToolError> {
        match group {
            None => Ok(self.layout_key()),
            Some("all") | Some(ALL_LAYOUT_KEY) => Ok(ALL_LAYOUT_KEY.to_owned()),
            Some(g) => {
                facts.require_group(g)?;
                Ok(g.to_owned())
            }
        }
    }

    /// The local part of a focus, as the engine's `lifecycle::focus` does it: the focused
    /// session, the group's memory, the layouts' reveal and the rail tab. Waking is the caller's.
    fn focus(&mut self, fact: &SessionFact) {
        self.focused = Some(fact.id.clone());
        let group = fact.group.as_str();
        if !group.is_empty() {
            self.group_focus.insert(group.to_owned(), fact.id.clone());
        }
        for (key, layout) in self.layouts.iter_mut() {
            layout.reveal(&fact.id, key.as_str() == ALL_LAYOUT_KEY || key.as_str() == group);
        }
        if self.view == GroupView::Rail && self.rail_tab.as_deref().is_some_and(|tab| tab != group) {
            self.rail_tab = (!group.is_empty()).then(|| group.to_owned());
        }
    }

    /// The session a rail tab focuses (issue #137): the focused session if it is already in the
    /// group, else the group's remembered session when awake, else its first awake session.
    fn group_tab_focus(&self, facts: &ViewFacts, group: &str) -> Option<String> {
        let in_group = |id: &str| facts.session(id).is_some_and(|s| s.group == group);
        if self.focused.as_deref().is_some_and(in_group) {
            return None;
        }
        let remembered = self
            .group_focus
            .get(group)
            .map(String::as_str)
            .filter(|id| facts.session(id).is_some_and(|s| s.group == group && !s.asleep));
        remembered.map(str::to_owned).or_else(|| facts.sessions.iter().find(|s| s.group == group && !s.asleep).map(|s| s.id.clone()))
    }

    fn view_set(&mut self, facts: &ViewFacts, p: ViewSetParams) -> Result<(), ToolError> {
        if let Some(tab) = p.rail_tab.as_deref()
            && tab != "all"
        {
            facts.require_group(tab)?;
        }
        if let Some(v) = p.view {
            self.view = v;
        }
        if let Some(tab) = p.rail_tab {
            self.rail_tab = (tab != "all").then(|| tab.clone());
            if tab != "all"
                && let Some(id) = self.group_tab_focus(facts, &tab)
                && let Some(fact) = facts.session(&id).cloned()
            {
                self.focus(&fact);
            }
        }
        Ok(())
    }

    fn layout_set(&mut self, facts: &ViewFacts, p: LayoutSetParams) -> Result<(), ToolError> {
        let key = self.key_of(facts, p.group.as_deref())?;
        let mut layout = self.layouts.get(&key).cloned().unwrap_or_default();
        if let Some(m) = p.mode {
            layout.mode = m.into();
        }
        if let Some(preset) = &p.preset {
            let (cols, rows) = parse_preset(preset)
                .ok_or_else(|| ToolError::InvalidParams(format!("no grid preset `{preset}` (1, 2, 2x2, 3x2, 3x3)")))?;
            layout.grid_cols = cols;
            layout.grid_rows = rows;
            layout.mode = LayoutMode::Grid;
        }
        for (name, v) in [("cols", p.cols), ("rows", p.rows)] {
            if let Some(n) = v {
                if !(1..=8).contains(&n) {
                    return Err(ToolError::InvalidParams(format!("`{name}` must be 1-8")));
                }
                if name == "cols" {
                    layout.grid_cols = n;
                } else {
                    layout.grid_rows = n;
                }
                layout.mode = LayoutMode::Grid;
            }
        }
        if let Some(free) = p.free {
            if !free.is_object() {
                return Err(ToolError::InvalidParams("`free` must be a JSON object".into()));
            }
            if serde_json::to_string(&free).map_or(usize::MAX, |t| t.len()) > MAX_FREE_BYTES {
                return Err(ToolError::InvalidParams("`free` is larger than 64 KiB".into()));
            }
            layout.free_tree = Some(free);
        }
        self.layouts.insert(key, layout);
        Ok(())
    }

    fn layout_pin(&mut self, facts: &ViewFacts, p: PinParams) -> Result<(), ToolError> {
        facts.require_session(&p.session_id)?;
        let key = self.key_of(facts, p.group.as_deref())?;
        let layout = self.layouts.entry(key).or_default();
        if !layout.pinned.contains(&p.session_id) {
            layout.pinned.push(p.session_id);
        }
        Ok(())
    }

    fn layout_unpin(&mut self, facts: &ViewFacts, p: PinParams) -> Result<(), ToolError> {
        let key = self.key_of(facts, p.group.as_deref())?;
        if let Some(layout) = self.layouts.get_mut(&key) {
            layout.pinned.retain(|s| *s != p.session_id);
        }
        Ok(())
    }

    fn inbox_panel_set(&mut self, p: InboxPanelParams) -> Result<(), ToolError> {
        if let Some(h) = p.height {
            if !h.is_finite() || !(40.0..=2000.0).contains(&h) {
                return Err(ToolError::InvalidParams("height must be 40-2000".into()));
            }
            self.inbox_height = Some(h);
        }
        if let Some(open) = p.open {
            self.inbox_open = open;
        }
        Ok(())
    }

    fn window_placement_set(&mut self, facts: &ViewFacts, p: WindowPlacementParams) -> Result<(), ToolError> {
        if !(1..=MAX_WINDOW_PX).contains(&p.width) || !(1..=MAX_WINDOW_PX).contains(&p.height) {
            return Err(ToolError::InvalidParams(format!("width and height must be 1-{MAX_WINDOW_PX}")));
        }
        if !(24..=2400).contains(&p.dpi) {
            return Err(ToolError::InvalidParams("dpi must be 24-2400".into()));
        }
        if p.window != "main" && !window_key_is_known(facts, &p.window) {
            return match p.window.split_once(':') {
                Some(("group", _)) | Some(("pane", _)) => Err(ToolError::NotFound(format!("no window `{}`", p.window))),
                _ => Err(ToolError::InvalidParams("`window` is `main`, `group:<id>` or `pane:<session id>`".into())),
            };
        }
        let placement = WindowPlacementState { left: p.left, top: p.top, width: p.width, height: p.height, dpi: p.dpi };
        self.windows.insert(p.window, WindowSlot { placement, maximized: p.maximized });
        // Windows of groups and sessions that are gone are forgotten, as the engine does.
        self.windows.retain(|key, _| key.as_str() == "main" || window_key_is_known(facts, key));
        Ok(())
    }
}

/// Whether a window key names a group or session the facts have. `main` is not checked here.
fn window_key_is_known(facts: &ViewFacts, key: &str) -> bool {
    match key.split_once(':') {
        Some(("group", id)) => facts.has_group(id),
        Some(("pane", id)) => facts.has_session(id),
        _ => false,
    }
}

/// The result of [`apply`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    /// The overlay now holds the call's result. Nothing is sent.
    Local,
    /// A session was focused on the overlay. When `wake` is set the session is asleep on the
    /// server, and the caller sends `session_wake` for it.
    Focused { session_id: String, wake: bool },
    /// Not part of the overlay's state, or not a UI-only call. The caller runs or forwards it.
    NotInOverlay,
}

/// Runs a call against the overlay when it is UI-only (see [`route`]). A forwarded call, and a
/// UI-only call the overlay does not model, return [`Applied::NotInOverlay`] and change nothing.
///
/// An error is what the engine would return for the same params on the same state (the same
/// [`ToolError`] kinds), and changes nothing.
pub fn apply(overlay: &mut ViewOverlay, facts: &ViewFacts, tool: &ToolInfo, params: Value) -> Result<Applied, ToolError> {
    let r = route(tool);
    if r == Route::Forward {
        return Ok(Applied::NotInOverlay);
    }
    match tool.id.as_str() {
        SESSION_FOCUS if r == Route::Focus => {
            let p: IdParams = parse_params(params)?;
            focus_call(overlay, facts, &p.session_id)
        }
        "inbox_next" => {
            let next = facts.inbox.iter().find(|id| overlay.focused.as_ref() != Some(*id)).or_else(|| facts.inbox.first());
            match next {
                Some(id) => focus_call(overlay, facts, id),
                None => Ok(Applied::Local),
            }
        }
        "view_set" => {
            overlay.view_set(facts, parse_params(params)?)?;
            Ok(Applied::Local)
        }
        "layout_set" => {
            overlay.layout_set(facts, parse_params(params)?)?;
            Ok(Applied::Local)
        }
        "layout_pin" => {
            overlay.layout_pin(facts, parse_params(params)?)?;
            Ok(Applied::Local)
        }
        "layout_unpin" => {
            overlay.layout_unpin(facts, parse_params(params)?)?;
            Ok(Applied::Local)
        }
        "inspector_show" => {
            let p: InspectorParams = parse_params(params)?;
            overlay.inspector = p.tab.into();
            Ok(Applied::Local)
        }
        "inbox_panel_set" => {
            overlay.inbox_panel_set(parse_params(params)?)?;
            Ok(Applied::Local)
        }
        "window_placement_set" => {
            overlay.window_placement_set(facts, parse_params(params)?)?;
            Ok(Applied::Local)
        }
        "group_collapse" => {
            let p: CollapseParams = parse_params(params)?;
            facts.require_group(&p.group_id)?;
            if p.collapsed {
                overlay.collapsed.insert(p.group_id);
            } else {
                overlay.collapsed.remove(&p.group_id);
            }
            Ok(Applied::Local)
        }
        "group_tear_off" => {
            let p: GroupParams = parse_params(params)?;
            facts.require_group(&p.group_id)?;
            overlay.torn_off.insert(p.group_id);
            Ok(Applied::Local)
        }
        "group_dock" => {
            let p: GroupParams = parse_params(params)?;
            facts.require_group(&p.group_id)?;
            overlay.torn_off.remove(&p.group_id);
            Ok(Applied::Local)
        }
        _ => Ok(Applied::NotInOverlay),
    }
}

/// A focus on the overlay: the local part, and whether the session is asleep (to be woken).
fn focus_call(overlay: &mut ViewOverlay, facts: &ViewFacts, id: &str) -> Result<Applied, ToolError> {
    let fact = facts.require_session(id)?.clone();
    overlay.focus(&fact);
    Ok(Applied::Focused { session_id: fact.id, wake: fact.asleep })
}

/// The largest `free` layout accepted, serialized (the engine's limit).
const MAX_FREE_BYTES: usize = 64 * 1024;

/// `1`, `2`, `2x2`, `3x2`, `3x3` (and `×`), as the engine reads a grid preset.
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdParams {
    session_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PinParams {
    session_id: String,
    #[serde(default)]
    group: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GroupParams {
    group_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CollapseParams {
    group_id: String,
    collapsed: bool,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ViewSetParams {
    view: Option<GroupView>,
    rail_tab: Option<String>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum ModeParam {
    Focus,
    Side,
    Overview,
    Grid,
    Free,
    Board,
}

impl From<ModeParam> for LayoutMode {
    fn from(m: ModeParam) -> Self {
        match m {
            ModeParam::Focus => LayoutMode::Focus,
            ModeParam::Side => LayoutMode::Side,
            ModeParam::Overview => LayoutMode::Overview,
            ModeParam::Grid => LayoutMode::Grid,
            ModeParam::Free => LayoutMode::Free,
            ModeParam::Board => LayoutMode::Board,
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct LayoutSetParams {
    group: Option<String>,
    mode: Option<ModeParam>,
    preset: Option<String>,
    cols: Option<u8>,
    rows: Option<u8>,
    free: Option<Value>,
}

#[derive(Deserialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum InspectorParam {
    Notes,
    Runs,
    Git,
    Session,
    Usage,
    Docs,
    Hidden,
}

impl From<InspectorParam> for Option<InspectorTab> {
    fn from(t: InspectorParam) -> Self {
        match t {
            InspectorParam::Notes => Some(InspectorTab::Notes),
            InspectorParam::Runs => Some(InspectorTab::Runs),
            InspectorParam::Git => Some(InspectorTab::Git),
            InspectorParam::Session => Some(InspectorTab::Session),
            InspectorParam::Usage => Some(InspectorTab::Usage),
            InspectorParam::Docs => Some(InspectorTab::Docs),
            InspectorParam::Hidden => None,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectorParams {
    tab: InspectorParam,
}

#[derive(Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct InboxPanelParams {
    open: Option<bool>,
    height: Option<f32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowPlacementParams {
    window: String,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    dpi: i32,
    #[serde(default)]
    maximized: bool,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::collections::BTreeSet;
    use std::sync::{Arc, OnceLock};

    use csm_engine::Caller;
    use serde_json::json;

    use super::*;
    use crate::backend::{EnginePort, ToolPort};

    /// The tools of the real registry (the demo engine's, which is the app's plus the demo's
    /// stand-ins). Read once: a demo engine is started only for this list.
    fn registry_tools() -> Arc<[ToolInfo]> {
        static TOOLS: OnceLock<Arc<[ToolInfo]>> = OnceLock::new();
        TOOLS
            .get_or_init(|| {
                let engine = crate::demo_engine().expect("the demo engine starts");
                EnginePort::new(engine).tools()
            })
            .clone()
    }

    fn tool(id: &str) -> ToolInfo {
        registry_tools().iter().find(|t| t.id == id).cloned().unwrap_or_else(|| panic!("`{id}` is in the registry"))
    }

    fn info(id: &str, effects: Effects) -> ToolInfo {
        ToolInfo { id: id.into(), title: String::new(), summary: String::new(), toolset: csm_engine::Toolset::Core, effects, input_schema: json!({}) }
    }

    /// The ui-effect rows of the generated catalogue in `docs/tools.md`: `(id, effects)`.
    fn docs_ui_rows() -> Vec<String> {
        let doc = include_str!("../../../docs/tools.md");
        let begin = doc.find("<!-- xtask:tools:begin").expect("the catalogue begins in docs/tools.md");
        let end = doc.find("<!-- xtask:tools:end").expect("the catalogue ends in docs/tools.md");
        let mut ids = Vec::new();
        for line in doc[begin..end].lines() {
            let cells: Vec<&str> = line.split('|').map(str::trim).collect();
            if cells.len() < 5 || !cells[1].starts_with('`') {
                continue;
            }
            let effects_ui = cells[3].split(',').map(str::trim).any(|e| e == "ui");
            if effects_ui {
                ids.push(cells[1].trim_matches('`').to_owned());
            }
        }
        ids
    }

    #[test]
    fn the_catalogue_lists_ui_tools() {
        let rows = docs_ui_rows();
        assert!(rows.len() >= 15, "the docs list the ui tools: {rows:?}");
        assert!(rows.iter().any(|r| r == SESSION_FOCUS) && rows.iter().any(|r| r == "group_tear_off"));
    }

    #[test]
    fn ui_only_is_the_ui_effect_with_idempotent_at_most() {
        assert!(is_ui_only(Effects::CHANGES_UI_STATE));
        assert!(is_ui_only(Effects::CHANGES_UI_STATE | Effects::IDEMPOTENT), "a stable UI call is still UI-only");
        assert!(!is_ui_only(Effects::IDEMPOTENT), "a read is not a UI call");
        assert!(!is_ui_only(Effects::empty()));
        assert!(!is_ui_only(Effects::CHANGES_UI_STATE | Effects::LAUNCHES_PROCESS));
        assert!(!is_ui_only(Effects::CHANGES_UI_STATE | Effects::MUTATES_STATE));
        assert!(!is_ui_only(Effects::CHANGES_UI_STATE | Effects::SENDS_INPUT));
    }

    #[test]
    fn routes_follow_the_effects_and_session_focus_is_its_own_route() {
        assert_eq!(route(&info("view_set", Effects::UI)), Route::Local);
        assert_eq!(route(&info("window_show", Effects::UI | Effects::IDEMPOTENT)), Route::Local);
        assert_eq!(route(&info("session_new", Effects::MUTATES_STATE | Effects::LAUNCHES_PROCESS)), Route::Forward);
        assert_eq!(route(&info(SESSION_FOCUS, Effects::UI | Effects::LAUNCHES_PROCESS)), Route::Focus);
        // Another tool with the same effects does not get the focus route.
        assert_eq!(route(&info("inbox_next", Effects::UI | Effects::LAUNCHES_PROCESS)), Route::Forward);
        // Focus without the launch is just a local call (a registry that declares it so).
        assert_eq!(route(&info(SESSION_FOCUS, Effects::UI)), Route::Local);
    }

    #[test]
    fn every_ui_effect_tool_in_the_docs_is_ui_only_in_the_real_registry() {
        for id in docs_ui_rows() {
            let t = registry_tools().iter().find(|t| t.id == id).cloned().unwrap_or_else(|| panic!("`{id}` is in docs/tools.md but not in the registry"));
            let expected = if id == SESSION_FOCUS { Route::Focus } else { Route::Local };
            assert_eq!(route(&t), expected, "`{id}` ({:?}) routes as {expected:?}", t.effects);
            assert!(t.effects.contains(Effects::CHANGES_UI_STATE), "`{id}` changes the UI");
        }
    }

    #[test]
    fn every_registry_tool_routes_by_its_effects() {
        let tools = registry_tools();
        let mut local = 0;
        for t in tools.iter() {
            let r = route(t);
            if t.id == SESSION_FOCUS {
                assert_eq!(r, Route::Focus);
            } else if is_ui_only(t.effects) {
                assert_eq!(r, Route::Local, "`{}` ({:?})", t.id, t.effects);
                local += 1;
            } else {
                assert_eq!(r, Route::Forward, "`{}` ({:?})", t.id, t.effects);
            }
        }
        assert!(local >= 15, "the registry has UI-only tools ({local})");
        // Every UI-only tool is documented in docs/tools.md: a catalogue row, or (the `pane_*` tools
        // the app registers) a mention in the text. The demo's own tools (`demo_*`) are not the app's.
        let doc = include_str!("../../../docs/tools.md");
        for t in tools.iter().filter(|t| !t.id.starts_with("demo_")) {
            if is_ui_only(t.effects) {
                assert!(doc.contains(&format!("`{}`", t.id)), "`{}` is UI-only but not in docs/tools.md", t.id);
            }
        }
    }

    /// Facts for a small fixed world: groups `work` and `home`; sessions `a` (work, awake), `b`
    /// (work, asleep), `c` (ungrouped, awake) and `d` (home, awake); the inbox is `b`, `c`.
    fn world() -> ViewFacts {
        let sess = |id: &str, group: &str, asleep: bool| SessionFact { id: id.into(), group: group.into(), asleep };
        ViewFacts {
            groups: ["work", "home"].iter().map(|s| s.to_string()).collect(),
            sessions: vec![sess("a", "work", false), sess("b", "work", true), sess("c", "", false), sess("d", "home", false)],
            inbox: vec!["b".into(), "c".into()],
        }
    }

    fn run(overlay: &mut ViewOverlay, facts: &ViewFacts, id: &str, params: Value) -> Result<Applied, ToolError> {
        apply(overlay, facts, &tool(id), params)
    }

    #[test]
    fn session_focus_is_local_and_wakes_only_an_asleep_session() {
        let facts = world();
        let mut o = ViewOverlay::default();
        let got = run(&mut o, &facts, SESSION_FOCUS, json!({"session_id": "b"})).expect("focus b");
        assert_eq!(got, Applied::Focused { session_id: "b".into(), wake: true });
        assert_eq!(o.focused.as_deref(), Some("b"));
        assert_eq!(o.group_focus.get("work").map(String::as_str), Some("b"), "the group remembers it");

        let got = run(&mut o, &facts, SESSION_FOCUS, json!({"session_id": "a"})).expect("focus a");
        assert_eq!(got, Applied::Focused { session_id: "a".into(), wake: false });
        assert_eq!(o.focused.as_deref(), Some("a"));
    }

    #[test]
    fn session_focus_switches_the_rail_tab_in_the_rail_view_only() {
        let facts = world();
        let mut o = ViewOverlay { rail_tab: Some("home".into()), ..ViewOverlay::default() };
        run(&mut o, &facts, SESSION_FOCUS, json!({"session_id": "a"})).expect("focus a");
        assert_eq!(o.rail_tab.as_deref(), Some("work"));

        let mut flat = ViewOverlay { view: GroupView::Flat, rail_tab: Some("home".into()), ..ViewOverlay::default() };
        run(&mut flat, &facts, SESSION_FOCUS, json!({"session_id": "a"})).expect("focus a");
        assert_eq!(flat.rail_tab.as_deref(), Some("home"), "the tab only follows in the rail view");

        let mut ungrouped = ViewOverlay { rail_tab: Some("work".into()), ..ViewOverlay::default() };
        run(&mut ungrouped, &facts, SESSION_FOCUS, json!({"session_id": "c"})).expect("focus c");
        assert_eq!(ungrouped.rail_tab, None, "an ungrouped session goes to All");
    }

    #[test]
    fn session_focus_refuses_an_unknown_session_and_extra_params() {
        let facts = world();
        let mut o = ViewOverlay::default();
        assert!(matches!(run(&mut o, &facts, SESSION_FOCUS, json!({"session_id": "zz"})), Err(ToolError::NotFound(_))));
        assert!(matches!(run(&mut o, &facts, SESSION_FOCUS, json!({"session_id": "a", "x": 1})), Err(ToolError::InvalidParams(_))));
        assert_eq!(o, ViewOverlay::default(), "a refused call changes nothing");
    }

    #[test]
    fn inbox_next_walks_the_inbox_and_wakes_an_asleep_one() {
        let facts = world();
        let mut o = ViewOverlay::default();
        assert_eq!(run(&mut o, &facts, "inbox_next", json!({})).expect("next"), Applied::Focused { session_id: "b".into(), wake: true });
        assert_eq!(run(&mut o, &facts, "inbox_next", json!({})).expect("next"), Applied::Focused { session_id: "c".into(), wake: false });
        assert_eq!(run(&mut o, &facts, "inbox_next", json!({})).expect("next"), Applied::Focused { session_id: "b".into(), wake: true });

        let empty = ViewFacts { inbox: Vec::new(), ..world() };
        assert_eq!(run(&mut o, &empty, "inbox_next", json!({})).expect("nothing"), Applied::Local);
    }

    #[test]
    fn view_set_rail_tab_focuses_the_remembered_or_first_awake_session() {
        let facts = world();
        // Focused elsewhere: the first awake session of `work` (b is asleep, so a).
        let mut o = ViewOverlay { focused: Some("d".into()), ..ViewOverlay::default() };
        assert_eq!(run(&mut o, &facts, "view_set", json!({"rail_tab": "work"})).expect("tab"), Applied::Local);
        assert_eq!(o.rail_tab.as_deref(), Some("work"));
        assert_eq!(o.focused.as_deref(), Some("a"));

        // The focused session is already in the group: nothing moves.
        let mut o = ViewOverlay { focused: Some("a".into()), ..ViewOverlay::default() };
        run(&mut o, &facts, "view_set", json!({"rail_tab": "work"})).expect("tab");
        assert_eq!(o.focused.as_deref(), Some("a"));

        // The remembered session wins when it is awake.
        let mut o = ViewOverlay { focused: Some("d".into()), ..ViewOverlay::default() };
        o.group_focus.insert("work".into(), "a".into());
        run(&mut o, &facts, "view_set", json!({"rail_tab": "work"})).expect("tab");
        assert_eq!(o.focused.as_deref(), Some("a"));
    }

    #[test]
    fn view_set_refuses_an_unknown_group_and_changes_nothing() {
        let facts = world();
        let mut o = ViewOverlay::default();
        assert!(matches!(run(&mut o, &facts, "view_set", json!({"rail_tab": "nope"})), Err(ToolError::NotFound(_))));
        assert_eq!(o, ViewOverlay::default());
        run(&mut o, &facts, "view_set", json!({"view": "headers", "rail_tab": "all"})).expect("view");
        assert_eq!((o.view, o.rail_tab.clone()), (GroupView::Headers, None));
        assert_eq!(o.layout_key(), ALL_LAYOUT_KEY);
    }

    #[test]
    fn the_layout_key_follows_the_view_and_the_rail_tab() {
        let o = ViewOverlay { rail_tab: Some("work".into()), ..ViewOverlay::default() };
        assert_eq!(o.layout_key(), "work");
        assert_eq!(ViewOverlay { view: GroupView::Headers, ..o.clone() }.layout_key(), ALL_LAYOUT_KEY);
        assert_eq!(ViewOverlay { rail_tab: None, ..o }.layout_key(), ALL_LAYOUT_KEY);
    }

    #[test]
    fn layout_set_applies_modes_presets_and_free_trees() {
        let facts = world();
        let mut o = ViewOverlay::default();
        run(&mut o, &facts, "layout_set", json!({"group": "work", "preset": "2x2"})).expect("preset");
        let l = &o.layouts["work"];
        assert_eq!((l.mode, l.grid_cols, l.grid_rows), (LayoutMode::Grid, 2, 2));

        run(&mut o, &facts, "layout_set", json!({"group": "work", "cols": 4})).expect("cols");
        assert_eq!(o.layouts["work"].grid_cols, 4);

        run(&mut o, &facts, "layout_set", json!({"group": "all", "mode": "overview"})).expect("all");
        assert_eq!(o.layouts[ALL_LAYOUT_KEY].mode, LayoutMode::Overview);

        run(&mut o, &facts, "layout_set", json!({"group": "work", "mode": "free", "free": {"split": 1}})).expect("free");
        assert_eq!(o.layouts["work"].mode, LayoutMode::Free);
        assert_eq!(o.layouts["work"].free_tree, Some(json!({"split": 1})));
    }

    #[test]
    fn layout_set_refuses_bad_params_and_leaves_the_layout_alone() {
        let facts = world();
        let mut o = ViewOverlay::default();
        run(&mut o, &facts, "layout_set", json!({"group": "work", "preset": "2x2"})).expect("preset");
        let before = o.clone();
        for params in [
            json!({"group": "work", "preset": "5"}),
            json!({"group": "work", "cols": 9}),
            json!({"group": "work", "rows": 0}),
            json!({"group": "work", "free": 3}),
            json!({"group": "work", "mode": "sideways"}),
        ] {
            assert!(matches!(run(&mut o, &facts, "layout_set", params.clone()), Err(ToolError::InvalidParams(_))), "{params}");
            assert_eq!(o, before, "{params} changes nothing");
        }
        assert!(matches!(run(&mut o, &facts, "layout_set", json!({"group": "nope"})), Err(ToolError::NotFound(_))));
        let big = json!({"x": "a".repeat(MAX_FREE_BYTES)});
        assert!(matches!(run(&mut o, &facts, "layout_set", json!({"group": "work", "free": big})), Err(ToolError::InvalidParams(_))));
    }

    #[test]
    fn layout_pin_and_unpin_keep_the_session_list_in_order() {
        let facts = world();
        let mut o = ViewOverlay::default();
        run(&mut o, &facts, "layout_pin", json!({"session_id": "b", "group": "work"})).expect("pin b");
        run(&mut o, &facts, "layout_pin", json!({"session_id": "a", "group": "work"})).expect("pin a");
        run(&mut o, &facts, "layout_pin", json!({"session_id": "b", "group": "work"})).expect("pin b again");
        assert_eq!(o.layouts["work"].pinned, vec!["b".to_string(), "a".to_string()]);
        run(&mut o, &facts, "layout_unpin", json!({"session_id": "b", "group": "work"})).expect("unpin");
        assert_eq!(o.layouts["work"].pinned, vec!["a".to_string()]);
        // Unpinning in a layout that was never made is fine, as in the engine.
        run(&mut o, &facts, "layout_unpin", json!({"session_id": "a", "group": "home"})).expect("unpin nothing");
        assert!(!o.layouts.contains_key("home"));
        assert!(matches!(run(&mut o, &facts, "layout_pin", json!({"session_id": "zz"})), Err(ToolError::NotFound(_))));
    }

    #[test]
    fn focus_reveals_a_minimized_session_and_gives_up_the_canvas() {
        let facts = world();
        let mut o = ViewOverlay::default();
        let minimized_and_maximized = || GroupLayout { minimized: vec!["b".into(), "a".into()], maximized: Some("a".into()), ..GroupLayout::default() };
        o.layouts.insert("work".into(), minimized_and_maximized());
        // A layout that does not lay out a `work` session (another group's) keeps its canvas.
        o.layouts.insert("home".into(), minimized_and_maximized());
        run(&mut o, &facts, SESSION_FOCUS, json!({"session_id": "b"})).expect("focus b");
        let l = &o.layouts["work"];
        assert_eq!(l.minimized, vec!["a".to_string()], "the focused session comes back from the strip");
        assert_eq!(l.maximized, None, "a different pane that filled the canvas gives way");
        assert_eq!(o.layouts["home"].maximized.as_deref(), Some("a"), "out of scope: the canvas is kept");
        assert_eq!(o.layouts["home"].minimized, vec!["a".to_string()], "the strip drops the focused session in every layout");

        // The All layout lays out every session, so its canvas gives way too.
        let mut all = ViewOverlay::default();
        all.layouts.insert(ALL_LAYOUT_KEY.into(), minimized_and_maximized());
        run(&mut all, &facts, SESSION_FOCUS, json!({"session_id": "c"})).expect("focus c");
        assert_eq!(all.layouts[ALL_LAYOUT_KEY].maximized, None);
    }

    #[test]
    fn group_collapse_tear_off_and_dock_track_each_group() {
        let facts = world();
        let mut o = ViewOverlay::default();
        run(&mut o, &facts, "group_collapse", json!({"group_id": "work", "collapsed": true})).expect("collapse");
        assert_eq!(o.collapsed, BTreeSet::from(["work".to_string()]));
        run(&mut o, &facts, "group_collapse", json!({"group_id": "work", "collapsed": false})).expect("expand");
        assert!(o.collapsed.is_empty());

        run(&mut o, &facts, "group_tear_off", json!({"group_id": "home"})).expect("tear off");
        assert!(o.torn_off.contains("home"));
        run(&mut o, &facts, "group_dock", json!({"group_id": "home"})).expect("dock");
        assert!(o.torn_off.is_empty());

        assert!(matches!(run(&mut o, &facts, "group_tear_off", json!({"group_id": "nope"})), Err(ToolError::NotFound(_))));
        assert!(matches!(run(&mut o, &facts, "group_collapse", json!({"group_id": "work"})), Err(ToolError::InvalidParams(_))));
    }

    #[test]
    fn inspector_and_inbox_panel_follow_the_params() {
        let facts = world();
        let mut o = ViewOverlay::default();
        run(&mut o, &facts, "inspector_show", json!({"tab": "git"})).expect("git");
        assert_eq!(o.inspector, Some(InspectorTab::Git));
        run(&mut o, &facts, "inspector_show", json!({"tab": "hidden"})).expect("hide");
        assert_eq!(o.inspector, None);
        assert!(matches!(run(&mut o, &facts, "inspector_show", json!({"tab": "nope"})), Err(ToolError::InvalidParams(_))));

        assert!(matches!(run(&mut o, &facts, "inbox_panel_set", json!({"height": 10.0})), Err(ToolError::InvalidParams(_))));
        assert_eq!(o.inbox_height, None, "a bad height changes nothing");
        run(&mut o, &facts, "inbox_panel_set", json!({"height": 300.0, "open": false})).expect("panel");
        assert_eq!((o.inbox_height, o.inbox_open), (Some(300.0), false));
    }

    #[test]
    fn window_placements_are_checked_and_stale_ones_dropped() {
        let facts = world();
        let mut o = ViewOverlay::default();
        o.windows.insert("group:gone".into(), WindowSlot::default());
        let place = |window: &str| json!({"window": window, "left": 10, "top": 20, "width": 800, "height": 600, "dpi": 96, "maximized": true});

        run(&mut o, &facts, "window_placement_set", place("main")).expect("main");
        run(&mut o, &facts, "window_placement_set", place("group:work")).expect("group");
        run(&mut o, &facts, "window_placement_set", place("pane:a")).expect("pane");
        assert!(o.windows.contains_key("main") && o.windows.contains_key("group:work") && o.windows.contains_key("pane:a"));
        assert!(!o.windows.contains_key("group:gone"), "a window of a group that is gone is forgotten");
        assert!(o.windows["main"].maximized);

        assert!(matches!(run(&mut o, &facts, "window_placement_set", place("group:nope")), Err(ToolError::NotFound(_))));
        assert!(matches!(run(&mut o, &facts, "window_placement_set", place("bogus")), Err(ToolError::InvalidParams(_))));
        let mut bad_dpi = place("main");
        bad_dpi["dpi"] = json!(0);
        assert!(matches!(run(&mut o, &facts, "window_placement_set", bad_dpi), Err(ToolError::InvalidParams(_))));
    }

    #[test]
    fn calls_outside_the_overlay_are_not_applied_here() {
        let facts = world();
        let mut o = ViewOverlay::default();
        // A forwarded call.
        assert_eq!(run(&mut o, &facts, "session_new", json!({"folder": "x"})).expect("forward"), Applied::NotInOverlay);
        // A UI-only call whose state is not in the overlay.
        assert_eq!(run(&mut o, &facts, "window_show", json!({})).expect("window"), Applied::NotInOverlay);
        assert_eq!(run(&mut o, &facts, "session_status_clear", json!({"session_id": "a"})).expect("status"), Applied::NotInOverlay);
        assert_eq!(o, ViewOverlay::default(), "neither touches the overlay");
    }

    #[test]
    fn the_overlay_matches_the_engine_for_the_view_tools() {
        let engine = crate::demo_engine().expect("the demo engine starts");
        for name in ["work", "home"] {
            engine.call(Caller::Ui, "group_new", json!({"name": name})).expect("a group");
        }
        let ids: Vec<String> = engine.view().groups.iter().map(|g| g.id.clone()).collect();
        let (g0, g1) = (ids[0].clone(), ids[1].clone());
        let mut o = ViewOverlay::from_snapshot(&engine.view());
        let tools = EnginePort::new(engine.clone()).tools();

        let steps: Vec<(&str, Value)> = vec![
            ("view_set", json!({"rail_tab": g0})),
            ("view_set", json!({"view": "headers"})),
            ("view_set", json!({"view": "rail"})),
            ("layout_set", json!({"group": g0, "preset": "2x2"})),
            ("layout_set", json!({"group": g0, "preset": "5"})),
            ("layout_set", json!({"cols": 9})),
            ("layout_set", json!({"group": "all", "mode": "focus"})),
            ("layout_set", json!({"group": g0, "mode": "free", "free": {"split": 1}})),
            ("layout_set", json!({"group": g0, "free": 3})),
            ("layout_pin", json!({"session_id": "nope"})),
            ("layout_unpin", json!({"session_id": "nope", "group": g0})),
            ("inspector_show", json!({"tab": "git"})),
            ("inspector_show", json!({"tab": "hidden"})),
            ("inbox_panel_set", json!({"height": 10.0})),
            ("inbox_panel_set", json!({"height": 300.0, "open": false})),
            ("window_placement_set", json!({"window": "main", "left": 1, "top": 2, "width": 800, "height": 600, "dpi": 96, "maximized": true})),
            ("window_placement_set", json!({"window": format!("group:{g0}"), "left": 3, "top": 4, "width": 500, "height": 400, "dpi": 120})),
            ("window_placement_set", json!({"window": "group:nope", "left": 0, "top": 0, "width": 1, "height": 1, "dpi": 96})),
            ("group_tear_off", json!({"group_id": g0})),
            ("group_collapse", json!({"group_id": g1, "collapsed": true})),
            ("group_dock", json!({"group_id": g0})),
            ("view_set", json!({"rail_tab": "all"})),
            ("view_set", json!({"rail_tab": "nope"})),
        ];
        for (id, params) in steps {
            let t = tools.iter().find(|t| t.id == id).cloned().unwrap_or_else(|| panic!("`{id}` is registered"));
            let facts = ViewFacts::from_snapshot(&engine.view());
            let got = apply(&mut o, &facts, &t, params.clone());
            let want = engine.call(Caller::Ui, id, params.clone());
            assert_eq!(got.is_ok(), want.is_ok(), "{id} {params}: overlay {got:?}, engine {want:?}");
            let snap = engine.view();
            assert_eq!(o.view, snap.view, "{id} {params}: view");
            assert_eq!(o.rail_tab, snap.rail_tab, "{id} {params}: rail tab");
            assert_eq!(o.layout_key(), snap.layout_key, "{id} {params}: layout key");
            assert_eq!(o.layouts, snap.layouts, "{id} {params}: layouts");
            assert_eq!((o.inbox_open, o.inbox_height), (snap.inbox_open, snap.inbox_height), "{id} {params}: inbox");
            assert_eq!(o.inspector, snap.inspector, "{id} {params}: inspector");
            assert_eq!(o.windows, snap.windows, "{id} {params}: windows");
            let collapsed: BTreeSet<String> = snap.groups.iter().filter(|g| !g.expanded).map(|g| g.id.clone()).collect();
            let torn: BTreeSet<String> = snap.groups.iter().filter(|g| g.torn_off).map(|g| g.id.clone()).collect();
            assert_eq!((&o.collapsed, &o.torn_off), (&collapsed, &torn), "{id} {params}: groups");
        }
    }
}
