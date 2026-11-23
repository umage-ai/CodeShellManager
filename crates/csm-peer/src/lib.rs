//! The peer link's transport: another CodeShellManager connects to this one over TCP, inside Noise.
//!
//! - [`PeerServer`] is the server. It implements the engine's [`csm_engine::PeerHost`], so the
//!   `peer_*` tools drive it, and it is **off until the person turns it on** in the app: a new
//!   server opens no socket, reads no network and announces nothing. Nothing in this crate turns
//!   it on by itself: the saved choice is only read by [`PeerServer::resume`], and that choice is
//!   written only by `peer_server_enable`, which the engine refuses for every caller but the UI.
//! - [`client`] is the other end: pairing with an offer link, and connecting as a paired device.
//! - Every connection starts with a cleartext selector frame, then a Noise handshake
//!   (`IKpsk2` to pair, `IK` afterwards; `csm-peer-proto` has the parameters, the prologue and the
//!   test vectors). The server answers a stranger with silence: a closed socket, the same whether
//!   no pairing is open, the id is wrong, or the key is not on the device list.
//! - Files (host key, device list, audit log, saved choice) live in one folder the app gives, which
//!   is limited to the current user ([`fsperm`]). The device list holds public keys only.
//!
//! The messages after the handshake are `csm-peer-proto`'s. This step carries the session
//! handshake, pings, tool calls (as `Caller::Peer`) and consent answers; snapshots and terminal
//! streams are the next steps.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod audit;
pub mod client;
pub mod devices;
pub mod fsperm;
pub mod identity;
mod link;
pub mod netif;
mod serve;
mod server;

pub use link::LinkError;
pub use server::{PeerServer, ServerConfig};

/// The terminal emulator version both ends must share (a minor-version match is required,
/// `csm_peer_proto::msg::check_term_versions`). Every crate of the workspace has one version.
pub const TERM_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Seconds since the Unix epoch (0 if the clock is before it).
pub(crate) fn unix_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Random bytes from the OS. A failure is an error, never a weak value.
pub(crate) fn random<const N: usize>() -> Result<[u8; N], std::io::Error> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| std::io::Error::other(format!("the system has no random numbers: {e}")))?;
    Ok(b)
}
