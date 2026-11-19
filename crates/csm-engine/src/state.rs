//! Everything the engine owns. Only the engine thread touches it, so it needs no locks of its
//! own; anything shared with other threads (a terminal the UI renders) is behind its own handle.
//!
//! Filled in by the engine work (sessions, groups, layouts, runtime handles). Keep it plain
//! data plus handles; behaviour lives in tools.

use csm_core::models::AppState;

#[derive(Default)]
pub struct State {
    /// The persisted part (sessions, groups, settings), read from and written to disk by the
    /// engine's persistence tools.
    pub app: AppState,
}
