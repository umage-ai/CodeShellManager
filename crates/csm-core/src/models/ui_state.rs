//! What the Rust app remembers about its own UI: the rail tab, per-group layouts, the inbox panel,
//! the inspector and torn-off groups. New in the Rust app (the WPF app has no such key), so the
//! enums serialise as `snake_case` strings rather than C# numbers.
//!
//! The whole value is read leniently ([`lenient`]): a malformed `Ui` key falls back to the
//! defaults instead of making the entire state file unreadable.

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

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
}

/// A tab of the inspector panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectorTab {
    Notes,
    Runs,
    Git,
    Session,
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
}

impl Default for GroupLayout {
    fn default() -> Self {
        Self { mode: LayoutMode::Side, grid_cols: default_cols(), grid_rows: default_rows(), pinned: Vec::new(), free_tree: None }
    }
}

/// The layout key of the rail's "All" tab, and of the single layout in the headers/flat views.
pub const ALL_LAYOUT_KEY: &str = "__ALL__";

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
        }
    }
}

impl UiState {
    /// True when nothing differs from the defaults (then the key is not written, so a file the
    /// app never customised stays byte-compatible with what the WPF app writes).
    pub fn is_default(&self) -> bool {
        *self == UiState::default()
    }
}

/// Reads a [`UiState`] and falls back to the defaults on `null` or on any shape error.
pub(crate) fn lenient<'de, D: Deserializer<'de>>(d: D) -> Result<UiState, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(serde_json::from_value(v).unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
