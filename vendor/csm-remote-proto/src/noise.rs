//! Noise parameters. This crate does not run Noise (the add-on uses `snow`, the phone a small
//! WebCrypto implementation); it fixes the choices both must agree on, and `vectors/noise.json`
//! proves they do.
//!
//! - **Pairing** (phone not yet known to the host): `Noise_IKpsk2_25519_AESGCM_SHA256`, the phone
//!   is the initiator and already knows the host's static key from the QR; the PSK (position 2) is
//!   the one-time secret from the QR.
//! - **Later connections**: `Noise_IK_25519_AESGCM_SHA256`; the host accepts only a phone static
//!   key on its device list.
//! - Prologue: [`prologue`] binds the handshake to this protocol version and the host id.
//! - Handshake payloads: msg 1 carries the phone's `PairHello` / hello JSON (pairing) or nothing
//!   (IK, free to carry a small JSON hello); msg 2 may be empty. Vectors use short fixed payloads.
//! - Transport: each app message (JSON, at most 48 KiB) is one Noise message; one relay frame
//!   carries exactly one Noise message (at most 65 535 bytes).
//! - **Rekey**: after every [`REKEY_INTERVAL`] messages in a direction, both ends call Noise
//!   `Rekey` on that direction's cipher (the sender after sending message number `k*N`, the receiver
//!   after receiving it). Counting is per direction and starts at 1 at the first transport message.
//!   A channel also should not outlive [`MAX_CHANNEL_SECS`]: the phone reconnects (a new handshake).
//! - **Check code**: [`check_code`] of the final handshake hash, shown on both screens at pairing.

use crate::ids::sha256;

pub const PARAMS_PAIRING: &str = "Noise_IKpsk2_25519_AESGCM_SHA256";
pub const PARAMS_SESSION: &str = "Noise_IK_25519_AESGCM_SHA256";
/// Position of the PSK in the pairing pattern (`psk2`).
pub const PSK_POSITION: u8 = 2;
pub const KEY_LEN: usize = 32;
pub const PSK_LEN: usize = 32;
pub const TAG_LEN: usize = 16;
pub const MAX_NOISE_MESSAGE: usize = 65_535;
pub const REKEY_INTERVAL: u64 = 10_000;
pub const MAX_CHANNEL_SECS: u64 = 3600;

const PROLOGUE_PREFIX: &[u8] = b"csm-remote/1\0";
const CHECK_DOMAIN: &[u8] = b"csm-remote check";

/// `"csm-remote/1" 0x00 host_id` (ASCII).
pub fn prologue(host_id: &str) -> Vec<u8> {
    [PROLOGUE_PREFIX, host_id.as_bytes()].concat()
}

/// Four decimal digits: `u32_be(SHA-256("csm-remote check" || handshake_hash)[0..4]) mod 10000`,
/// zero padded. Catches a QR swapped by someone in the room (the phone and PC show the same code
/// only if they share one handshake).
pub fn check_code(handshake_hash: &[u8]) -> String {
    let mut data = CHECK_DOMAIN.to_vec();
    data.extend_from_slice(handshake_hash);
    let d = sha256(&data);
    let n = u32::from_be_bytes([d[0], d[1], d[2], d[3]]) % 10_000;
    format!("{n:04}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_code_is_four_digits() {
        for i in 0..50u8 {
            let c = check_code(&[i; 32]);
            assert_eq!(c.len(), 4);
            assert!(c.bytes().all(|b| b.is_ascii_digit()));
        }
        assert_ne!(check_code(&[1; 32]), check_code(&[2; 32]));
    }

    #[test]
    fn prologue_binds_host_id() {
        assert_eq!(prologue("abc"), b"csm-remote/1\0abc");
    }
}
