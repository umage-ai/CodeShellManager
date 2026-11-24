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

/// Upload chunk: one binary frame, one Noise message (the same as a terminal chunk).
pub const UPLOAD_CHUNK: usize = MAX_TERM_CHUNK;
/// Chunks the laptop may have sent but not yet acknowledged, per upload.
pub const UPLOAD_CREDIT: usize = 8;
/// The office acknowledges after this many written chunks (and at `UploadEnd`).
pub const UPLOAD_ACK_EVERY: usize = 4;
/// Uploads active at once across all devices.
pub const UPLOAD_ACTIVE_TOTAL: usize = 8;
/// Bytes in one file upload.
pub const MAX_UPLOAD_FILE: u64 = 64 * 1024 * 1024;
/// Bytes in one image upload.
pub const MAX_UPLOAD_IMAGE: u64 = 32 * 1024 * 1024;
/// Files in one drop.
pub const MAX_UPLOAD_FILES_PER_DROP: usize = 20;
/// Bytes in one drop, all files together.
pub const MAX_UPLOAD_DROP: u64 = 256 * 1024 * 1024;
/// A session id, as it is used as a folder name: `[A-Za-z0-9_-]`, 1 to 64 bytes.
pub const MAX_UPLOAD_SESSION_ID: usize = 64;
/// A sanitised upload name, in bytes (the extension is kept).
pub const MAX_UPLOAD_NAME_BYTES: usize = 120;
/// The laptop's name for an upload is a label of at most this many bytes on the wire.
pub const MAX_UPLOAD_LABEL_BYTES: usize = 512;
