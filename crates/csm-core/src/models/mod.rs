//! The persisted data models. Field names serialise PascalCase and enums as numbers, exactly
//! as System.Text.Json writes the C# classes, so an existing `state.json` loads unchanged.

mod alert_event;
mod app_state;
mod group;
mod open_with;
mod recently_closed;
mod run_command;
mod serde_util;
mod session;
mod slash_command;
mod ui_state;

pub use alert_event::{AlertEvent, AlertType};
pub use app_state::{
    AppSettings, AppState, CloseAction, DEFAULT_INDEX_MAX_MB, DEFAULT_USAGE_RETENTION_DAYS, GroupDisplayMode, INDEX_MAX_MB_RANGE, McpGrant, ProgramStatusProbeMode,
    SidebarActionIconsMode, UI_ZOOM_DEFAULT, UI_ZOOM_RANGE, USAGE_RETENTION_DAYS_RANGE, WindowBounds, WindowPlacementState,
};
pub use group::SessionGroup;
pub use open_with::OpenWithEntry;
pub use recently_closed::RecentlyClosedEntry;
pub use run_command::{RunCommandItem, RunMode};
pub use session::{SessionKind, SessionStatus, ShellSession, get_file_name, is_argument_safe_name};
pub use slash_command::SlashCommand;
pub use ui_state::{
    ALL_LAYOUT_KEY, GRID_PRESETS, GroupLayout, InspectorTab, LayoutMode, MAX_CUSTOM_GRIDS, MAX_GRID_SIDE, UiState, WindowSlot, group_window_key,
    pane_window_key, parse_grid_preset,
};
