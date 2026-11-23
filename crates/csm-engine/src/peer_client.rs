//! The seam between the engine and the laptop's side of the peer link (`csm-peer`'s client): pairing
//! with another CodeShellManager from its offer link, and the list of the hosts this app knows.
//!
//! Like [`crate::peer_host`], the engine owns the tools and who may call them; the client owns the
//! device key, the host list and the background pairing thread. The engine knows it only through
//! [`PeerClientHost`]. The default, [`NoPeerClient`], does nothing: a build or a mode that wires no
//! client has no remote connections at all.
//!
//! Nothing here listens. The client only dials out, and only from a pairing the person started in
//! the app, or a connection to a host already paired.

use serde::Serialize;

use crate::tool::ToolError;

/// Where a pairing is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PairState {
    /// Nothing started since the app began (or the last pairing was cleared).
    Idle,
    /// Dialling the host and running the secure handshake.
    Connecting,
    /// The handshake is done: the four digits are shown, and the other screen must show the same.
    /// The host's person still has to press Allow.
    CheckCode,
    /// The host allowed this device; the host is stored.
    Paired,
    /// The pairing did not happen; `message` says why.
    Failed,
    /// Cancelled by the person before the host answered.
    Cancelled,
}

/// What `peer_client_pair_status` returns (and `peer_client_pair` returns at once). Nothing secret:
/// no keys, no link.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClientPairStatus {
    pub state: PairState,
    /// The name the host is stored under (or will be).
    pub host_name: Option<String>,
    pub host_id: Option<String>,
    /// The four digits to compare with the host's screen (set from `CheckCode` on).
    pub check_code: Option<String>,
    /// Why it failed, or what is going on.
    pub message: Option<String>,
}

impl Default for ClientPairStatus {
    fn default() -> Self {
        Self { state: PairState::Idle, host_name: None, host_id: None, check_code: None, message: None }
    }
}

/// One host this app is paired with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HostInfo {
    pub host_id: String,
    pub name: String,
    /// `tcp://10.8.0.2:47300`.
    pub address: String,
    /// Unix seconds.
    pub paired_at: u64,
    pub last_connected: Option<u64>,
}

/// The client, as the engine sees it. Methods run on the engine thread: they may read or write a
/// small file and start a thread; they never wait on the network.
pub trait PeerClientHost: Send + Sync {
    /// The state of the latest pairing (or `Idle`).
    fn status(&self) -> ClientPairStatus;
    /// Parses `link` (a `csm://pair#...` offer), then pairs on a background thread and returns at
    /// once with the state `Connecting`. Only `peer_client_pair` calls this, and only for
    /// `Caller::Ui`. `name` is what the host lists this app as (defaults to the address).
    fn pair_start(&self, link: &str, name: Option<&str>) -> Result<ClientPairStatus, ToolError>;
    /// Stops a pairing that waits for the host. Safe for every caller; returns the status now (the
    /// pairing reports `Cancelled` once its thread has let go).
    fn pair_cancel(&self) -> ClientPairStatus;
    /// The hosts this app is paired with.
    fn hosts(&self) -> Result<Vec<HostInfo>, ToolError>;
    /// Removes a host and the host key pinned for it. `Ok(false)` when no such host is stored.
    fn forget(&self, host_id: &str) -> Result<bool, ToolError>;
    /// The app is closing: cancel any pairing in progress.
    fn shutdown(&self) {}
}

/// The client of a build that has none: tests, `--clean`, the demo.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPeerClient;

fn unavailable() -> ToolError {
    ToolError::Failed("connecting to another CodeShellManager isn't available here (this run has no peer client)".into())
}

impl PeerClientHost for NoPeerClient {
    fn status(&self) -> ClientPairStatus {
        ClientPairStatus::default()
    }
    fn pair_start(&self, _: &str, _: Option<&str>) -> Result<ClientPairStatus, ToolError> {
        Err(unavailable())
    }
    fn pair_cancel(&self) -> ClientPairStatus {
        ClientPairStatus::default()
    }
    fn hosts(&self) -> Result<Vec<HostInfo>, ToolError> {
        Ok(Vec::new())
    }
    fn forget(&self, _: &str) -> Result<bool, ToolError> {
        Err(unavailable())
    }
}
