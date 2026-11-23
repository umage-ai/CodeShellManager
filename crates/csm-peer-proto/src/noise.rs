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
//!
//! ## Telling a pairing connection from a session
//!
//! The responder must choose its pattern before it reads message 1 (the protocol name is hashed
//! into `h` and `ck`), and there is no relay to choose for it. So every connection starts with one
//! cleartext **preamble frame** ([`Preamble`]), then message 1 in the next frame:
//!
//! - `0x01`: a session (`IK`). Always routed to the session path.
//! - `0x02 || pairing_id` (16 bytes, the offer's `i`): pairing (`IKpsk2`).
//!
//! [`route`] decides. It reaches the pairing path only when a slot is open and the id matches
//! (compared in constant time, against a dummy id when no slot is open). Anything else (no slot,
//! wrong id, wrong length, unknown tag) is [`Route::Refuse`]: close without writing a byte, so a
//! scanner cannot tell "no slot" from "wrong id". The preamble reveals only what the client chose.
//!
//! A slot is consumed only when the first transport message (the pairing `Hello`) decrypts: in
//! `IKpsk2` the PSK is mixed in with message 2, so message 1 alone proves nothing about it. Each
//! slot allows at most [`MAX_PAIR_ATTEMPTS`] handshakes (a wrong PSK counts); then it closes. A
//! session client cannot reach the pairing path (tag `0x01`), and a pairing client cannot reach
//! the session path (tag `0x02`; and an `IK` message 1 does not decrypt under `IKpsk2`, or the
//! reverse, since the protocol name differs).
//!
//! ## What the check code does and does not prove
//!
//! It catches a link swapped by someone who does not take part in the handshake (a different
//! handshake gives different codes). It does **not** stop an active swapper who sits in the middle
//! and chooses ephemeral keys: with four digits, about 10^4 tries (milliseconds) make both screens
//! agree. Pairing therefore happens in person or over a channel the person trusts (the link is the
//! secret); the code is a mix-up guard, not a defence against that attacker. See the design,
//! section 4.

use crate::ids::{PAIRING_ID_LEN, sha256};

/// Handshakes one pairing slot allows (a wrong PSK counts) before it closes.
pub const MAX_PAIR_ATTEMPTS: u8 = 3;
pub const PREAMBLE_SESSION: u8 = 0x01;
pub const PREAMBLE_PAIRING: u8 = 0x02;

/// The cleartext frame before Noise message 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preamble {
    Session,
    Pairing([u8; PAIRING_ID_LEN]),
}

impl Preamble {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Session => vec![PREAMBLE_SESSION],
            Self::Pairing(id) => [&[PREAMBLE_PAIRING][..], &id[..]].concat(),
        }
    }

    /// Strict: exactly the tag, plus 16 bytes for pairing. Never panics.
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes {
            [PREAMBLE_SESSION] => Some(Self::Session),
            [PREAMBLE_PAIRING, id @ ..] => <[u8; PAIRING_ID_LEN]>::try_from(id).ok().map(Self::Pairing),
            _ => None,
        }
    }
}

/// Where a connection goes after its preamble.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Session,
    Pairing,
    /// Close without writing anything.
    Refuse,
}

fn ct_eq(a: &[u8; PAIRING_ID_LEN], b: &[u8; PAIRING_ID_LEN]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Routes a preamble frame. `open_slot` is the pairing id of the open slot, if any.
pub fn route(preamble: &[u8], open_slot: Option<&[u8; PAIRING_ID_LEN]>) -> Route {
    match Preamble::decode(preamble) {
        Some(Preamble::Session) => Route::Session,
        Some(Preamble::Pairing(id)) => {
            let dummy = [0u8; PAIRING_ID_LEN];
            let same = ct_eq(&id, open_slot.unwrap_or(&dummy));
            if same && open_slot.is_some() { Route::Pairing } else { Route::Refuse }
        }
        None => Route::Refuse,
    }
}

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
/// a link swapped by someone outside the handshake shows different codes. It is not proof against
/// an active swapper who grinds ephemerals (see the module docs).
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
    fn preamble_round_trips_and_is_strict() {
        let id = [7u8; PAIRING_ID_LEN];
        for p in [Preamble::Session, Preamble::Pairing(id)] {
            assert_eq!(Preamble::decode(&p.encode()), Some(p));
        }
        assert_eq!(Preamble::Pairing(id).encode().len(), 17);
        for bad in [&[][..], &[0], &[3], &[1, 0], &[2], &[2, 1, 2, 3], &[2; 18][..]] {
            assert_eq!(Preamble::decode(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn routing_cannot_cross_paths_and_hides_slot_state() {
        let id = [7u8; PAIRING_ID_LEN];
        let other = [8u8; PAIRING_ID_LEN];
        let pair = Preamble::Pairing(id).encode();
        assert_eq!(route(&Preamble::Session.encode(), Some(&id)), Route::Session);
        assert_eq!(route(&Preamble::Session.encode(), None), Route::Session);
        assert_eq!(route(&pair, Some(&id)), Route::Pairing);
        assert_eq!(route(&pair, Some(&other)), Route::Refuse);
        assert_eq!(route(&pair, None), Route::Refuse, "no open slot");
        assert_eq!(route(&Preamble::Pairing([0; PAIRING_ID_LEN]).encode(), None), Route::Refuse, "the dummy id is not a key");
        assert_eq!(route(b"", Some(&id)), Route::Refuse);
        assert_eq!(route(&[9, 9], Some(&id)), Route::Refuse);
    }

    #[test]
    fn rekey_fires_on_every_interval() {
        let mut r = Rekey::new();
        let fired = (1..=REKEY_INTERVAL * 2 + 1).filter(|_| r.tick()).count();
        assert_eq!(fired, 2);
        assert_eq!(r.count(), REKEY_INTERVAL * 2 + 1);
    }
}
