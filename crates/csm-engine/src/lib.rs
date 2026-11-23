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

pub mod addons;
mod engine;
mod plugins;
pub mod events;
mod plugin_screens;
mod index;
mod launch;
mod lifecycle;
/// `From` conversions from the read model and events to the owned wire types (`csm-peer-proto`).
mod peer_wire;
pub mod output_tap;
/// The seam to the peer server (`csm-peer`): status types, the host trait, the audit events.
pub mod peer_host;
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
pub(crate) mod usage_tail;
pub(crate) mod tools;
pub mod which;
pub mod wsl_probe;

pub use csm_core::channel;
pub use csm_index;
/// Re-exported so the UI can build sizes and input without depending on csm-term directly.
pub use csm_term::{GridSize, input};
pub use engine::{CONSENT_TIMEOUT, Ctx, Engine, Grant, Job, ParkedReply, Reply};
pub use events::{Event, Events};
pub use index::{INDEX_FILE, IndexSetting, location as index_location, wpf_output_db};
pub use plugin_screens::{DeclaredScreen, OPEN_TIMEOUT, ScreenInfo, ScreenPhase, ScreenStore};
pub use peer_host::{AuditEvent, DeviceInfo, EnableRequest, NoPeerHost, PairAnswer, PairingStarted, PeerHost, PeerStatus, PendingPair, Revoke};
pub use plugins::{PluginRuntime, RemoteView, RunState};
pub use registry::{DynamicTool, Registry};
pub use runs::{RunOutputHandle, RunPhase, RunView};
pub use session::{AttachSnapshot, ResizeMark, TerminalHandle};
pub use snapshot::{ClosedView, GroupInfo, GroupView, SessionView, Snapshot};
pub use state::{Autostart, LoadReport, Progress, PtyFactory, State, StateOptions, TerminalProfiles, UrlOpener, real_pty_factory, real_url_opener};
pub use status::{Status, WaitUntil};
pub use tool::{Caller, Effects, ToolError, ToolFn, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};
pub use tools::open::{OpenWithTarget, OpenWithView};
pub use usage_tail::ClaudeRoots;
