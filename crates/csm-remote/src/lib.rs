//! The client side of a remote window (`docs/design/remote-csm.md`, P1 step 6).
//!
//! A [`RemoteBackend`] is the UI's [`csm_ui::Backend`] and [`csm_ui::ToolPort`] for one paired
//! host. It keeps a mirror of the host's snapshot (a full copy, then deltas), proxies tool calls,
//! answers the view-only calls from the per-client [`csm_ui::view_overlay`], and owns one
//! [`RemoteTerminal`] per pane the UI shows. All network I/O happens on one worker thread
//! (`link`); the UI thread only sends commands and polls results.
//!
//! The layers: this crate is L5. It may use `csm-peer` (L4, the transport) and `csm-ui` (L4, the
//! screens' contract); `csm-ui` itself never depends on the transport.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod backend;
pub mod connect;
mod link;
pub mod map;
mod shared;
mod terminal;

pub use backend::{ConnectParams, RemoteBackend};
pub use connect::{HostWindow, window_params};
pub use shared::{RemoteHandle, RemotePort, RemoteView, shared};
pub use link::LinkState;
pub use terminal::RemoteTerminal;
