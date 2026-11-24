//! Caps. Defaults for the relay (all configurable there) and the hard limits both ends enforce.

/// Largest binary WebSocket message the relay accepts, host frame header included.
pub const MAX_FRAME_BYTES: usize = 64 * 1024;
/// Binary host-frame header: version, kind, channel (u32 BE).
pub const FRAME_HEADER_BYTES: usize = 6;
/// Largest relay-layer control message (JSON text).
pub const MAX_CONTROL_BYTES: usize = 16 * 1024;
/// Largest app message (JSON, before Noise): big snapshots are split to fit.
pub const MAX_APP_MESSAGE_BYTES: usize = 48 * 1024;

pub const MAX_CHANNELS_PER_HOST: usize = 16;
pub const MAX_ALLOW_ENTRIES: usize = 64;
pub const MAX_SOCKETS_PER_IP: usize = 20;
pub const MAX_NEW_CONNECTIONS_PER_IP_PER_MINUTE: usize = 10;
pub const FRAMES_PER_SECOND_PER_CHANNEL: u32 = 50;
pub const BYTES_PER_SECOND_PER_CHANNEL: u32 = 256 * 1024;
/// CSM peers (terminal streams burst and carry whole screens): the caps of a `Peer` channel.
pub const PEER_MAX_CHANNELS_PER_HOST: usize = 16;
pub const PEER_FRAMES_PER_SECOND_PER_CHANNEL: u32 = 2_000;
pub const PEER_BYTES_PER_SECOND_PER_CHANNEL: u32 = 8 * 1024 * 1024;
/// Burst allowance is this many times the per-second rate.
pub const BURST_FACTOR: u32 = 2;
pub const HOST_SILENCE_SECS: u64 = 90;
pub const CLIENT_IDLE_SECS: u64 = 600;
/// A new socket must say hello within this long.
pub const HELLO_TIMEOUT_SECS: u64 = 10;
pub const MAX_PAIRING_ATTEMPTS: u8 = 5;
pub const PAIRING_SLOT_MAX_SECS: u32 = 600;
pub const MAX_HOSTS: usize = 10_000;

pub const PUSHES_PER_HOST_PER_HOUR: usize = 60;
pub const PUSHES_PER_ENDPOINT_PER_HOUR: usize = 20;
/// Web Push payload limit (RFC 8030 section 7.2: a push service must accept 4096 bytes).
pub const MAX_PUSH_BODY_BYTES: usize = 4096;
pub const MAX_PUSH_ENDPOINT_BYTES: usize = 1024;
pub const MAX_PUSH_TTL_SECS: u32 = 7 * 24 * 3600;

pub const MAX_HOST_ACCESS_TOKEN_BYTES: usize = 128;
