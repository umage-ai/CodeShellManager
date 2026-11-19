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

pub use alert_event::{AlertEvent, AlertType};
pub use app_state::{
    AppSettings, AppState, GroupDisplayMode, ProgramStatusProbeMode, SidebarActionIconsMode,
    WindowBounds, WindowPlacementState,
};
pub use group::SessionGroup;
pub use open_with::OpenWithEntry;
pub use recently_closed::RecentlyClosedEntry;
pub use run_command::{RunCommandItem, RunMode};
pub use session::{SessionKind, SessionStatus, ShellSession, get_file_name};
