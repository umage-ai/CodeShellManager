//! CodeShellManager's MCP server and local bridge.
//!
//! Everything is a tool (`docs/tools.md`): the registry in `csm-engine` is the only list of tools,
//! and this crate exposes it two ways.
//!
//! - **MCP** ([`McpService`]): JSON-RPC over stdio, tools filtered by enabled toolsets, consent
//!   handled by the engine. It is generic over a [`Backend`], so `csm mcp` forwards to the running
//!   app ([`RemoteBackend`]) and `csm mcp --in-process` runs an engine of its own ([`LocalBackend`]).
//! - **The bridge**: the desktop app calls [`serve_ipc`] to publish its engine on a token-protected
//!   local socket (craft-ipc), and `csm` reaches it through [`RemoteBackend`]. The wire protocol
//!   is in [`wire`].
//!
//! The bridge lives in this crate rather than a separate `csm-ipc` because it is small, has one
//! client and one server, and the app needs exactly one dependency to get both halves.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod catalog;
mod ipc_client;
mod ipc_server;
mod mcp;
pub mod wire;

pub use catalog::{Backend, BridgeError, LocalBackend, ToolInfo};
pub use ipc_client::{APP_ENV, MAX_CALL, RemoteBackend, RemoteOptions};
pub use ipc_server::{USE_TCP_BY_DEFAULT, Handle, IpcError, IpcOptions, serve_ipc, serve_ipc_with};
pub use mcp::{McpOptions, McpService, McpTools};
pub use wire::APP_ID;

/// Re-exported so the app and the CLI name the same instance-record directory type as the bridge.
pub use craft_ipc::RuntimeDir;
