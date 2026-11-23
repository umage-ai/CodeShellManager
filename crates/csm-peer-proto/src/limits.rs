//! The caps both ends enforce. Network bytes are untrusted: every decoder checks these before it
//! allocates or parses.

/// One Noise message is at most this many bytes (the Noise specification's limit).
pub const MAX_NOISE_MESSAGE: usize = 65_535;
/// The AES-GCM tag every Noise transport message carries.
pub const TAG_LEN: usize = 16;
/// The largest frame payload: exactly one Noise message.
pub const MAX_FRAME_LEN: usize = MAX_NOISE_MESSAGE;
/// The largest plaintext in one Noise transport message.
pub const MAX_PLAINTEXT: usize = MAX_NOISE_MESSAGE - TAG_LEN;
/// A handshake payload (message 1 and 2) is empty or small.
pub const MAX_HANDSHAKE_PAYLOAD: usize = 1024;
/// A control message (JSON) after its fragments are joined: a snapshot of a few hundred
/// sessions fits, an attack does not.
pub const MAX_CONTROL_BYTES: usize = 4 * 1024 * 1024;
/// Terminal bytes per `term_data` frame; the sender splits larger chunks.
pub const MAX_TERM_CHUNK: usize = 32 * 1024;
/// The serialized grid of one attach, all chunks joined.
pub const MAX_ATTACH_BYTES: usize = 64 * 1024 * 1024;
/// One `term_input` payload (keys, a mouse report, or pasted text).
pub const MAX_INPUT_BYTES: usize = 32 * 1024;
/// The largest terminal dimension either way.
pub const MAX_TERM_DIM: u16 = 2000;
/// A pairing message (JSON) is small.
pub const MAX_PAIR_MESSAGE: usize = 4096;
/// A device or host name, in characters.
pub const MAX_NAME_CHARS: usize = 64;
/// A tool id, a session id or a version string, in bytes.
pub const MAX_ID_BYTES: usize = 128;
/// The longest pairing link or offer text accepted.
pub const MAX_OFFER_CHARS: usize = 1024;
