//! Noise parameters of the "peer" profile. This crate does not run Noise (the transport crate
//! uses `snow`); it fixes the choices both ends must agree on, and `vectors/noise.json` proves a
//! second implementation can.
//!
//! The profile is the phone remote's with a different prologue, so a phone handshake and a peer
//! handshake can never be mistaken for each other (a mismatch fails the handshake):
//!
//! - **Pairing** (device not yet on the host's list): `Noise_IKpsk2_25519_AESGCM_SHA256`. The
//!   client is the initiator and knows the host's static key from the pairing link; the PSK
//!   (position 2) is the one-time secret from the link.
//! - **Later connections**: `Noise_IK_25519_AESGCM_SHA256`. The host accepts only an initiator
//!   static key on its device list.
//! - **Prologue**: [`prologue`] is `"csm-peer/1" 0x00 host_id`. The `1` is the wire generation;
//!   a client of another generation fails the handshake, whatever it sends.
//! - **Handshake payloads**: empty, or at most [`crate::limits::MAX_HANDSHAKE_PAYLOAD`] bytes.
//! - **Transport**: one [`crate::codec::Frame`] plaintext (or fragment) per Noise message, one
//!   Noise message per [`crate::frame`] frame.
//! - **Rekey**: after every [`REKEY_INTERVAL`] messages in a direction, both ends call Noise
//!   `Rekey` on that direction's cipher: the sender right after sending message number `k*N`, the
//!   receiver right after receiving it. Counting is per direction and starts at 1. [`Rekey`]
//!   does the counting.
//! - **Check code**: [`check_code`] of the final handshake hash, shown on both screens at pairing.

use crate::ids::sha256;

pub const PARAMS_PAIRING: &str = "Noise_IKpsk2_25519_AESGCM_SHA256";
pub const PARAMS_SESSION: &str = "Noise_IK_25519_AESGCM_SHA256";
/// Position of the PSK in the pairing pattern (`psk2`).
pub const PSK_POSITION: u8 = 2;
pub const REKEY_INTERVAL: u64 = 10_000;

const PROLOGUE_PREFIX: &[u8] = b"csm-peer/1\0";
const CHECK_DOMAIN: &[u8] = b"csm-peer check";

/// `"csm-peer/1" 0x00 host_id` (ASCII).
pub fn prologue(host_id: &str) -> Vec<u8> {
    [PROLOGUE_PREFIX, host_id.as_bytes()].concat()
}

/// Four decimal digits: `u32_be(SHA-256("csm-peer check" || handshake_hash)[0..4]) mod 10000`,
/// zero padded. Both screens show it at pairing: they agree only if they share one handshake, so
/// a link swapped by someone else shows different codes.
pub fn check_code(handshake_hash: &[u8]) -> String {
    let mut data = CHECK_DOMAIN.to_vec();
    data.extend_from_slice(handshake_hash);
    let d = sha256(&data);
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 10_000;
    format!("{n:04}")
}

/// Counts the messages of one direction and says when to rekey that direction's cipher.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Rekey {
    count: u64,
}

impl Rekey {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call after each message sent (or received) in this direction. `true` means: rekey now.
    pub fn tick(&mut self) -> bool {
        self.count = self.count.wrapping_add(1);
        self.count.is_multiple_of(REKEY_INTERVAL)
    }

    pub fn count(&self) -> u64 {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_code_is_four_digits_and_differs_per_hash() {
        for i in 0..50u8 {
            let c = check_code(&[i; 32]);
            assert_eq!(c.len(), 4);
            assert!(c.bytes().all(|b| b.is_ascii_digit()));
        }
        assert_ne!(check_code(&[1; 32]), check_code(&[2; 32]));
    }

    #[test]
    fn peer_and_phone_profiles_are_distinct() {
        assert_eq!(prologue("abc"), b"csm-peer/1\0abc");
        assert_ne!(prologue("abc"), b"csm-remote/1\0abc".to_vec());
    }

    #[test]
    fn rekey_fires_on_every_interval() {
        let mut r = Rekey::new();
        let fired = (1..=REKEY_INTERVAL * 2 + 1).filter(|_| r.tick()).count();
        assert_eq!(fired, 2);
        assert_eq!(r.count(), REKEY_INTERVAL * 2 + 1);
    }
}
