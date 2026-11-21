//! The wire protocol of the CSM phone remote. Pure: no I/O, no clock, no randomness.
//!
//! Three parties speak it (see `README.md` in this crate):
//!
//! - the **add-on** inside CSM (a `host` to the relay),
//! - the **relay**, which routes opaque frames and parses only the [`relay`] layer,
//! - the **phone** app, which holds the other end of every end-to-end channel.
//!
//! Layers, outermost first:
//!
//! 1. [`relay`]: WebSocket text control messages (hello, allow list, pairing slot, push) and the
//!    binary host frames that multiplex phone channels. The relay reads these and nothing else.
//! 2. [`noise`]: the Noise handshake and transport parameters. Everything the relay forwards
//!    after `ready` is Noise ciphertext.
//! 3. [`pairing`]: the QR payload and the pairing exchange inside the encrypted channel.
//! 4. [`msg`]: the phone/add-on messages (requests, responses, events) and the access levels.
//!
//! [`ids`] derives host ids and routing hashes, [`codes`] holds the stable refusal codes, and
//! [`limits`] the caps both ends enforce. `vectors/*.json` are the shared test vectors; a JS
//! implementation is checked against the same files (`tests/vectors.rs` regenerates them with
//! `CSM_UPDATE_VECTORS=1`).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

pub mod codes;
pub mod ids;
pub mod limits;
pub mod msg;
pub mod noise;
pub mod pairing;
pub mod relay;

/// The protocol version carried in hello messages and the binary frame header.
pub const PROTOCOL_VERSION: u8 = 1;
