//! What the Rust app remembers about its own UI: the rail tab, per-group layouts, the inbox panel,
//! the inspector and torn-off groups. New in the Rust app (the WPF app has no such key), so the
//! enums serialise as `snake_case` strings rather than C# numbers.
//!
//! The whole value is read leniently ([`lenient`]): a malformed `Ui` key falls back to the
//! defaults instead of making the entire state file unreadable.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

use super::WindowPlacementState;

/// How a group's panes are arranged (docs/plan.md section 4, "Layouts").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    /// One pane; the sidebar is the switcher.
    Focus,
    /// The pinned sessions, auto-tiled. The default.
    #[default]
    Side,
    /// Every live session as an auto-fitting grid of mini terminals.
    Overview,
    /// A fixed preset (`grid_cols` x `grid_rows`).
    Grid,
    /// Free-hand split tree (ratios are the UI's to persist).
    Free,
    /// The Board: live sessions as cards in status columns, beside the project's tasks.
    Board,
}

/// A tab of the inspector panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectorTab {
    Notes,
    Runs,
    Git,
    Session,
    /// Claude token and cost analytics (`usage_session`).
    Usage,
    /// The project docs (README.md, CLAUDE.md, AGENTS.md...) of the session's folder.
    Docs,
}

fn default_cols() -> u8 {
    2
}
fn default_rows() -> u8 {
    2
}
fn default_true() -> bool {
    true
}

/// One group's layout. Keyed in [`UiState::layouts`] by group id, or [`ALL_LAYOUT_KEY`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct GroupLayout {
    pub mode: LayoutMode,
    #[serde(default = "default_cols")]
    pub grid_cols: u8,
    #[serde(default = "default_rows")]
    pub grid_rows: u8,
    /// Sessions pinned for side-by-side, in order.
    pub pinned: Vec<String>,
    /// Free layout: an opaque tree the UI owns (split ratios, floating panes).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub free_tree: Option<serde_json::Value>,
    /// Panes minimized out of this layout (their sessions keep running), in the order they left.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub minimized: Vec<String>,
    /// The pane filling the whole canvas for now; the layout underneath is kept as it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximized: Option<String>,
}

impl Default for GroupLayout {
    fn default() -> Self {
        Self {
            mode: LayoutMode::Side,
            grid_cols: default_cols(),
            grid_rows: default_rows(),
            pinned: Vec::new(),
            free_tree: None,
            minimized: Vec::new(),
            maximized: None,
        }
    }
}

impl GroupLayout {
    /// A session was focused: it comes back from the minimized strip, and a different pane that
    /// filled the canvas gives way (when `in_scope`, i.e. this layout lays the session out), so
    /// the focused one can be seen. Returns whether anything changed.
    pub fn reveal(&mut self, id: &str, in_scope: bool) -> bool {
        let before = self.minimized.len();
        self.minimized.retain(|m| m != id);
        let mut changed = self.minimized.len() != before;
        if in_scope && self.maximized.as_deref().is_some_and(|m| m != id) {
            self.maximized = None;
            changed = true;
        }
        changed
    }

    /// Drops every reference to a session that no longer exists.
    pub fn forget(&mut self, id: &str) {
        self.pinned.retain(|p| p != id);
        self.minimized.retain(|m| m != id);
        if self.maximized.as_deref() == Some(id) {
            self.maximized = None;
        }
    }
}

/// The grid presets every Grid menu offers, in menu order: one row (1, 2, 4×1, 6×1), then two
/// and three rows (2×2, 3×2, 6×2, 3×3). `(cols, rows)`.
pub const GRID_PRESETS: [(u8, u8); 8] = [(1, 1), (2, 1), (4, 1), (6, 1), (2, 2), (3, 2), (6, 2), (3, 3)];

/// The largest grid side `layout_set` accepts.
pub const MAX_GRID_SIDE: u8 = 8;

/// How many custom grid sizes are remembered ([`UiState::custom_grids`]).
pub const MAX_CUSTOM_GRIDS: usize = 6;

/// A grid preset's name: `1`, `2`, `4x1`, `6x1`, `2x2`, `3x2`, `6x2`, `3x3` (`×` for `x`, any
/// case), plus the cell counts `4`, `6` and `9` for 2×2, 3×2 and 3×3.
pub fn parse_grid_preset(p: &str) -> Option<(u8, u8)> {
    let p = p.trim().to_ascii_lowercase().replace('×', "x");
    match p.as_str() {
        "1" => return Some((1, 1)),
        "2" => return Some((2, 1)),
        "4" => return Some((2, 2)),
        "6" => return Some((3, 2)),
        "9" => return Some((3, 3)),
        _ => {}
    }
    let (c, r) = p.split_once('x')?;
    let g = (c.trim().parse().ok()?, r.trim().parse().ok()?);
    GRID_PRESETS.contains(&g).then_some(g)
}

/// The layout key of the rail's "All" tab, and of the single layout in the headers/flat views.
pub const ALL_LAYOUT_KEY: &str = "__ALL__";

/// A torn-off window's saved bounds: its normal (restored) rectangle in physical pixels with the
/// DPI of the monitor it was on (see `window_placement`), and whether it was maximized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "PascalCase", default)]
pub struct WindowSlot {
    pub placement: WindowPlacementState,
    pub maximized: bool,
}

/// The [`UiState::windows`] key of a torn-off group's window.
pub fn group_window_key(group_id: &str) -> String {
    format!("group:{group_id}")
}

/// The [`UiState::windows`] key of a torn-off pane's window.
pub fn pane_window_key(session_id: &str) -> String {
    format!("pane:{session_id}")
}

/// See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct UiState {
    /// The rail's selected group; `None` is the "All" tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rail_tab: Option<String>,
    pub layouts: BTreeMap<String, GroupLayout>,
    #[serde(default = "default_true")]
    pub inbox_open: bool,
    /// Height of the inbox panel in logical pixels; `None` is the UI's default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox_height: Option<f32>,
    /// The inspector's tab, `None` when it is hidden.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inspector: Option<InspectorTab>,
    /// Groups living in their own OS window.
    pub torn_off_groups: Vec<String>,
    /// The session that had focus, restored on start.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focused_session: Option<String>,
    /// Bounds of torn-off windows, by [`group_window_key`] / [`pane_window_key`]. Kept after a
    /// dock, so tearing off again reopens where it was. Read entry by entry: a bad one is
    /// dropped, never the rest of the UI state.
    #[serde(deserialize_with = "lenient_windows", skip_serializing_if = "BTreeMap::is_empty")]
    pub windows: BTreeMap<String, WindowSlot>,
    /// The session last focused in each group, by group id (issue #137): selecting the group's
    /// rail tab focuses it again. Read entry by entry, like `windows`.
    #[serde(deserialize_with = "lenient_group_focus", skip_serializing_if = "BTreeMap::is_empty")]
    pub group_focus: BTreeMap<String, String>,
    /// Custom grid sizes the person set (`layout_set` with `cols`/`rows`), newest first, at most
    /// [`MAX_CUSTOM_GRIDS`], none of them a [`GRID_PRESETS`] entry. `(cols, rows)`. Read
    /// leniently: a bad entry is dropped.
    #[serde(deserialize_with = "lenient_grids", skip_serializing_if = "Vec::is_empty")]
    pub custom_grids: Vec<(u8, u8)>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            rail_tab: None,
            layouts: BTreeMap::new(),
            inbox_open: true,
            inbox_height: None,
            inspector: None,
            torn_off_groups: Vec::new(),
            focused_session: None,
            windows: BTreeMap::new(),
            group_focus: BTreeMap::new(),
            custom_grids: Vec::new(),
        }
    }
}

impl UiState {
    /// True when nothing differs from the defaults (then the key is not written, so a file the
    /// app never customised stays byte-compatible with what the WPF app writes).
    pub fn is_default(&self) -> bool {
        *self == UiState::default()
    }

    /// Remembers a custom grid size, newest first: moved to the front when already there, and
    /// the oldest dropped past [`MAX_CUSTOM_GRIDS`]. A preset or an out-of-range size is not
    /// remembered. Returns whether the list changed.
    pub fn remember_custom_grid(&mut self, cols: u8, rows: u8) -> bool {
        let g = (cols, rows);
        if !custom_grid_ok(g) || self.custom_grids.first() == Some(&g) {
            return false;
        }
        self.custom_grids.retain(|x| *x != g);
        self.custom_grids.insert(0, g);
        self.custom_grids.truncate(MAX_CUSTOM_GRIDS);
        true
    }

    /// Forgets a remembered custom grid size. Returns whether it was there.
    pub fn forget_custom_grid(&mut self, cols: u8, rows: u8) -> bool {
        let before = self.custom_grids.len();
        self.custom_grids.retain(|x| *x != (cols, rows));
        self.custom_grids.len() != before
    }
}

/// A size worth remembering: each side 1-[`MAX_GRID_SIDE`], and not a preset.
fn custom_grid_ok(g: (u8, u8)) -> bool {
    (1..=MAX_GRID_SIDE).contains(&g.0) && (1..=MAX_GRID_SIDE).contains(&g.1) && !GRID_PRESETS.contains(&g)
}

fn lenient_grids<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<(u8, u8)>, D::Error> {
    let serde_json::Value::Array(a) = serde_json::Value::deserialize(d)? else { return Ok(Vec::new()) };
    let mut out: Vec<(u8, u8)> = Vec::new();
    for v in a {
        if let Ok(g) = serde_json::from_value::<(u8, u8)>(v)
            && custom_grid_ok(g)
            && !out.contains(&g)
            && out.len() < MAX_CUSTOM_GRIDS
        {
            out.push(g);
        }
    }
    Ok(out)
}

/// Reads a [`UiState`] and falls back to the defaults on `null` or on any shape error.
pub(crate) fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<UiState, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

fn lenient_group_focus<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, String>, D::Error> {
    let serde_json::Value::Object(m) = serde_json::Value::deserialize(d)? else { return Ok(BTreeMap::new()) };
    Ok(m.into_iter()
        .filter_map(|(k, v)| match v {
            serde_json::Value::String(s) => Some((k, s)),
            _ => None,
        })
        .collect())
}

fn lenient_windows<'de, D: Deserializer<'de>>(d: D) -> Result<BTreeMap<String, WindowSlot>, D::Error> {
    let serde_json::Value::Object(m) = serde_json::Value::deserialize(d)? else { return Ok(BTreeMap::new()) };
    Ok(m.into_iter().filter_map(|(k, v)| serde_json::from_value(v).ok().map(|w| (k, w))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_focus_round_trips_and_a_bad_entry_costs_only_itself() {
        let mut ui = UiState::default();
        assert!(!serde_json::to_string(&ui).unwrap().contains("GroupFocus"), "not written when empty");
        ui.group_focus.insert("g1".into(), "s2".into());
        let json = serde_json::to_string(&ui).unwrap();
        assert!(json.contains(r#""GroupFocus":{"g1":"s2"}"#), "{json}");
        assert_eq!(serde_json::from_str::<UiState>(&json).unwrap(), ui);
        let mixed = r#"{"Inspector":"git","GroupFocus":{"a":"x","b":7,"c":null}}"#;
        let back: UiState = serde_json::from_str(mixed).unwrap();
        assert_eq!(back.inspector, Some(InspectorTab::Git));
        assert_eq!(back.group_focus.len(), 1);
        let back: UiState = serde_json::from_str(r#"{"GroupFocus":"nonsense","Inspector":"git"}"#).unwrap();
        assert!(back.group_focus.is_empty() && back.inspector == Some(InspectorTab::Git));
    }

    #[test]
    fn window_slots_round_trip_and_a_bad_one_costs_only_itself() {
        let mut ui = UiState::default();
        let slot = WindowSlot { placement: WindowPlacementState { left: -10, top: 20, width: 900, height: 700, dpi: 144 }, maximized: true };
        ui.windows.insert(group_window_key("g1"), slot);
        let json = serde_json::to_string(&ui).unwrap();
        assert!(json.contains(r#""group:g1":{"Placement":{"Left":-10"#), "{json}");
        let back: UiState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ui);

        let mixed = r#"{"Inspector":"git","Windows":{"pane:a":{"Placement":{"Left":1,"Top":2,"Width":3,"Height":4,"Dpi":96}},"pane:b":"nonsense"}}"#;
        let back: UiState = serde_json::from_str(mixed).unwrap();
        assert_eq!(back.inspector, Some(InspectorTab::Git), "the rest of the UI state survives");
        assert_eq!(back.windows.len(), 1);
        assert_eq!(back.windows[&pane_window_key("a")].placement.width, 3);
        assert!(!back.windows[&pane_window_key("a")].maximized);

        let not_a_map: UiState = serde_json::from_str(r#"{"Windows":[1,2]}"#).unwrap();
        assert!(not_a_map.windows.is_empty());
        assert!(!serde_json::to_string(&UiState::default()).unwrap().contains("Windows"));
    }

    #[test]
    fn grid_presets_parse_by_name_and_cell_count() {
        let named =
            [("1", (1, 1)), ("2", (2, 1)), ("4x1", (4, 1)), ("6×1", (6, 1)), ("2X2", (2, 2)), ("4", (2, 2)), (" 3x2 ", (3, 2)), ("6", (3, 2))];
        for (name, g) in named {
            assert_eq!(parse_grid_preset(name), Some(g), "{name}");
        }
        assert_eq!(parse_grid_preset("6x2"), Some((6, 2)));
        assert_eq!(parse_grid_preset("9"), Some((3, 3)));
        for bad in ["", "x", "5x5", "7", "2x", "x2", "-1x2", "300x1", "1x1x1"] {
            assert_eq!(parse_grid_preset(bad), None, "{bad}");
        }
        for g in GRID_PRESETS {
            assert_eq!(parse_grid_preset(&format!("{}x{}", g.0, g.1)), Some(g), "every preset parses as CxR");
        }
    }

    #[test]
    fn custom_grids_are_remembered_newest_first_deduplicated_and_capped() {
        let mut ui = UiState::default();
        assert!(!ui.remember_custom_grid(3, 2), "a preset is not remembered");
        assert!(!ui.remember_custom_grid(9, 1) && !ui.remember_custom_grid(0, 2), "out of range");
        assert!(ui.remember_custom_grid(5, 1));
        assert!(ui.remember_custom_grid(4, 2));
        assert!(!ui.remember_custom_grid(4, 2), "already the newest");
        assert!(ui.remember_custom_grid(5, 1), "moves to the front");
        assert_eq!(ui.custom_grids, vec![(5, 1), (4, 2)]);
        for g in [(1, 2), (1, 3), (2, 3), (4, 4), (5, 5)] {
            ui.remember_custom_grid(g.0, g.1);
        }
        assert_eq!(ui.custom_grids.len(), MAX_CUSTOM_GRIDS);
        assert_eq!(ui.custom_grids.first(), Some(&(5, 5)));
        assert!(!ui.custom_grids.contains(&(4, 2)), "the oldest went");
        assert!(ui.forget_custom_grid(5, 1));
        assert!(!ui.forget_custom_grid(5, 1), "already gone");
        assert_eq!(ui.custom_grids.len(), MAX_CUSTOM_GRIDS - 1);

        let json = serde_json::to_string(&ui).unwrap();
        assert!(json.contains(r#""CustomGrids":[[5,5],"#), "{json}");
        assert_eq!(serde_json::from_str::<UiState>(&json).unwrap(), ui);
        assert!(!serde_json::to_string(&UiState::default()).unwrap().contains("CustomGrids"));
        // Hostile files: bad entries, presets, duplicates and extras are dropped; the rest stays.
        let back: UiState = serde_json::from_str(
            r#"{"Inspector":"git","CustomGrids":[[5,1],"x",[3,2],[5,1],[0,1],[1,300],[7,7],[1,2],[1,3],[2,3],[4,4],[5,5],[8,8]]}"#,
        )
        .unwrap();
        assert_eq!(back.inspector, Some(InspectorTab::Git));
        assert_eq!(back.custom_grids, vec![(5, 1), (7, 7), (1, 2), (1, 3), (2, 3), (4, 4)]);
        let back: UiState = serde_json::from_str(r#"{"CustomGrids":{"a":1},"Inspector":"git"}"#).unwrap();
        assert!(back.custom_grids.is_empty() && back.inspector == Some(InspectorTab::Git));
    }

    #[test]
    fn round_trips_and_defaults() {
        let mut ui = UiState::default();
        ui.layouts.insert("g".into(), GroupLayout { mode: LayoutMode::Grid, grid_cols: 3, grid_rows: 2, ..Default::default() });
        ui.inspector = Some(InspectorTab::Git);
        let json = serde_json::to_string(&ui).unwrap();
        assert!(json.contains(r#""Mode":"grid""#), "{json}");
        let back: UiState = serde_json::from_str(&json).unwrap();
        assert_eq!(back, ui);
        let empty: UiState = serde_json::from_str("{}").unwrap();
        assert!(empty.inbox_open);
    }

    #[test]
    fn minimized_and_maximized_persist_and_focus_reveals() {
        let mut l = GroupLayout { minimized: vec!["a".into(), "b".into()], maximized: Some("c".into()), ..Default::default() };
        let json = serde_json::to_string(&l).unwrap();
        assert!(json.contains(r#""Minimized":["a","b"]"#) && json.contains(r#""Maximized":"c""#), "{json}");
        assert_eq!(serde_json::from_str::<GroupLayout>(&json).unwrap(), l);
        // Absent in older files, and not written while empty.
        let old: GroupLayout = serde_json::from_str(r#"{"Mode":"free"}"#).unwrap();
        assert!(old.minimized.is_empty() && old.maximized.is_none());
        assert!(!serde_json::to_string(&old).unwrap().contains("Minimized"));
        // Focusing c keeps it maximized; focusing a brings a back and un-maximizes c.
        assert!(!l.reveal("c", true));
        assert!(l.reveal("a", true));
        assert_eq!((l.minimized.clone(), l.maximized.clone()), (vec!["b".to_string()], None));
        // Another scope's layout keeps its maximized pane.
        let mut other = GroupLayout { maximized: Some("x".into()), ..Default::default() };
        assert!(!other.reveal("a", false));
        assert_eq!(other.maximized.as_deref(), Some("x"));
        l.maximized = Some("b".into());
        l.forget("b");
        assert!(l.minimized.is_empty() && l.maximized.is_none());
    }
}
