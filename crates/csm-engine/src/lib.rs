//! The engine: one owner of all app state, and the registry of tools that act on it.
//!
//! **Everything is a tool** (`docs/tools.md`). The UI, the command palette, the keymap, the CLI
//! and the MCP server all go through [`Engine::call`] / [`Engine::submit`] with a tool id, so
//! every action the person can take is also available to an MCP client, under the same rules.
#![forbid(unsafe_code)]

mod engine;
pub mod events;
pub mod registry;
pub mod state;
pub mod tool;

pub use engine::{CONSENT_TIMEOUT, Ctx, Engine, Grant, Job, Reply};
pub use events::{Event, Events};
pub use registry::Registry;
pub use state::State;
pub use tool::{Caller, Effects, ToolError, ToolFn, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};
