//! The seam between the engine and the peer link's server (`csm-peer`, one layer above).
//!
//! The engine owns the tools (`peer_server_enable`, `peer_pair_start`, ...) and the rules about
//! who may call them; the host owns the socket, the keys and the device list. The engine knows the
//! host only through [`PeerHost`], so nothing in the engine can listen on a network. The default
//! host, [`NoPeerHost`], does nothing: a build or a mode (tests, `--clean`, the demo) that wires no
//! host has no remote access at all.
//!
//! None of this state lives in `state.json`, in `settings_set` keys or in a `.csm` backup. The host
//! keeps its own files (see `csm-peer`), so a migration or an import can never turn it on.

use std::net::SocketAddr;

use serde::Serialize;

use crate::tool::{Effects, ToolError};

/// What the person is shown, and what `peer_status` returns. Nothing secret: no keys, no pairing
/// link (`peer_pair_start` returns that, to the app only).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct PeerStatus {
    /// This build has a host at all. `false`: every other field is empty and every tool but
    /// `peer_status` fails.
    pub available: bool,
    /// The person turned it on and has not turned it off. After a restart this is what the app
    /// resumes; it says nothing about whether a socket is open (see `listening`).
    pub enabled: bool,
    /// `ip:port` the listener is bound to right now. `None` while off, or while waiting.
    pub listening: Option<String>,
    /// Enabled but not listening yet (the chosen address is missing, for instance a tunnel that
    /// is down): why, and that the host keeps retrying. Never another address.
    pub waiting: Option<String>,
    /// A pairing offer is open: seconds left. The offer itself is not here.
    pub pairing_open_secs: Option<u64>,
    /// A device asked to pair and waits for Allow or Deny.
    pub pending_pair: Option<PendingPair>,
    /// Paired devices, and how many are connected now.
    pub devices: usize,
    pub connected: usize,
    /// The key and device files are limited to the current user (mode 0600 / an ACL).
    pub private_files_restricted: bool,
}

/// A device that completed the pairing handshake and waits for the person's answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PendingPair {
    pub device_name: String,
    /// Four digits both screens show; the person compares them before allowing.
    pub check_code: String,
    pub expires_in_secs: u64,
}

/// One paired device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceInfo {
    pub id: String,
    pub name: String,
    pub level: String,
    /// Unix seconds.
    pub paired_at: u64,
    pub last_seen: Option<u64>,
    pub connected: bool,
}

/// What `peer_server_enable` asks for. The address was checked by the tool already; a host
/// checks it again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnableRequest {
    pub addr: SocketAddr,
    /// The person chose "all interfaces" under Advanced.
    pub allow_all_interfaces: bool,
}

/// The pairing offer: the link the other CSM is given.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PairingStarted {
    /// `csm://pair#...`. Secret for its lifetime (10 minutes, one use).
    pub link: String,
    pub address: String,
    pub expires_in_secs: u64,
}

/// The person's answer to a pending pairing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairAnswer {
    pub allow: bool,
    /// The check code the person read on both screens. Required to allow; a wrong one is refused.
    pub check_code: Option<String>,
    /// `full`; the other levels are not available yet.
    pub level: String,
}

/// Which devices to revoke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Revoke {
    One(String),
    All,
}

/// Something the engine saw that belongs in the remote-access log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditEvent {
    /// A call to a tool that is refused for this caller (an agent trying to turn remote access on).
    Refused { tool: String, caller: String },
    /// A non-read call from a paired device.
    PeerCall { device_id: String, device_name: String, tool: String, session: Option<String>, effects: Effects },
    /// A consent card answered from a device (or the app).
    ConsentAnswered { by: String, request_id: u64, allow: bool },
}

/// The peer server, as the engine sees it. Methods run on the engine thread: they may bind a
/// socket or write a small file, never wait on the network.
pub trait PeerHost: Send + Sync {
    fn status(&self) -> PeerStatus;
    /// Start listening on the chosen address and remember the choice. Only `peer_server_enable`
    /// calls this, and only for `Caller::Ui`.
    fn enable(&self, req: EnableRequest, by: &str) -> Result<PeerStatus, ToolError>;
    /// Stop listening, close every connection with a `bye`, close any open pairing, forget the
    /// choice. Safe for every caller.
    fn disable(&self, by: &str) -> PeerStatus;
    /// Open a pairing offer (needs the listener). Only for `Caller::Ui`.
    fn pair_start(&self, by: &str) -> Result<PairingStarted, ToolError>;
    /// Answer the pending pairing. Only for `Caller::Ui`.
    fn pair_answer(&self, answer: PairAnswer, by: &str) -> Result<PeerStatus, ToolError>;
    fn devices(&self) -> Vec<DeviceInfo>;
    /// Delete device keys and close their connections at once; how many were revoked.
    fn revoke(&self, target: Revoke, by: &str) -> Result<usize, ToolError>;
    fn audit(&self, event: AuditEvent);
    /// The app is closing: close everything. Not a person's choice, so the saved choice stays.
    fn shutdown(&self);
}

/// The host of a build that has none: tests, `--clean`, the demo.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoPeerHost;

fn unavailable() -> ToolError {
    ToolError::Failed("remote access isn't available here (this run has no peer server)".into())
}

impl PeerHost for NoPeerHost {
    fn status(&self) -> PeerStatus {
        PeerStatus::default()
    }
    fn enable(&self, _: EnableRequest, _: &str) -> Result<PeerStatus, ToolError> {
        Err(unavailable())
    }
    fn disable(&self, _: &str) -> PeerStatus {
        PeerStatus::default()
    }
    fn pair_start(&self, _: &str) -> Result<PairingStarted, ToolError> {
        Err(unavailable())
    }
    fn pair_answer(&self, _: PairAnswer, _: &str) -> Result<PeerStatus, ToolError> {
        Err(unavailable())
    }
    fn devices(&self) -> Vec<DeviceInfo> {
        Vec::new()
    }
    fn revoke(&self, _: Revoke, _: &str) -> Result<usize, ToolError> {
        Ok(0)
    }
    fn audit(&self, _: AuditEvent) {}
    fn shutdown(&self) {}
}
