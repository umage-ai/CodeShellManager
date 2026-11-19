//! The engine: one owner of all app state, and the registry of tools that act on it.
//!
//! **Everything is a tool** (`docs/tools.md`). The UI, the command palette, the keymap, the CLI
//! and the MCP server all go through [`Engine::call`] / [`Engine::submit`] with a tool id, so
//! every action the person can take is also available to an MCP client, under the same rules.
//!
//! Two things are deliberately not tools, because they are per frame: [`Engine::view`] (the
//! read model, an immutable [`Snapshot`]) and [`Engine::terminal`] (a [`TerminalHandle`] for
//! rendering and widget-level input).
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod engine;
pub mod events;
mod index;
mod launch;
mod lifecycle;
pub mod persist;
pub mod proc;
pub mod registry;
pub mod runs;
mod screen;
mod session;
pub mod snapshot;
pub mod state;
pub mod status;
pub mod tool;
pub(crate) mod tools;
pub mod which;
pub mod wsl_probe;

pub use csm_index;
pub use index::{INDEX_FILE, IndexSetting, location as index_location, wpf_output_db};
pub use engine::{CONSENT_TIMEOUT, Ctx, Engine, Grant, Job, ParkedReply, Reply};
pub use events::{Event, Events};
pub use registry::Registry;
pub use runs::{RunOutputHandle, RunPhase, RunView};
pub use tools::open::{OpenWithTarget, OpenWithView};
pub use session::TerminalHandle;
pub use snapshot::{GroupInfo, GroupView, SessionView, Snapshot};
pub use state::{LoadReport, Progress, PtyFactory, State, StateOptions, real_pty_factory};
pub use status::{Status, WaitUntil};
pub use tool::{Caller, Effects, ToolError, ToolFn, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};
/// Re-exported so the UI can build sizes and input without depending on csm-term directly.
pub use csm_term::{GridSize, input};
