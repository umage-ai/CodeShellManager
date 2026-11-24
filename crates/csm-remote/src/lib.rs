//! The client side of a remote window (`docs/design/remote-csm.md`, P1 step 6).
//!
//! A [`RemoteBackend`] is the UI's [`csm_ui::Backend`] and [`csm_ui::ToolPort`] for one paired
//! host. It keeps a mirror of the host's snapshot (a full copy, then deltas), proxies tool calls,
//! answers the view-only calls from the per-client [`csm_ui::view_overlay`], and owns one
//! [`RemoteTerminal`] per pane the UI shows. All network I/O happens on one worker thread
//! (`link`); the UI thread only sends commands and polls results.
//!
//! [`attach`] is the same connection used from a console (`csm attach`, P1 step 8): one session's
//! terminal, tmux-style, with no UI at all.
//!
//! The layers: this crate is L5. It may use `csm-peer` (L4, the transport) and `csm-ui` (L4, the
//! screens' contract); `csm-ui` itself never depends on the transport.

#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod attach;
mod backend;
mod upload;
pub mod connect;
mod link;
pub mod map;
mod shared;
mod terminal;

pub use backend::{ConnectParams, RemoteBackend};
pub use connect::{HostWindow, PairedOffice, paired_offices, window_params};
pub use link::LinkState;
pub use shared::{RemoteHandle, RemotePort, RemoteView, shared};
pub use terminal::RemoteTerminal;
