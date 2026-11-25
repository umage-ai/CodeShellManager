//! Pairing: the QR payload and the exchange inside the encrypted pairing channel.
//!
//! 1. The PC shows `https://<app origin>/v/<version>/#p=<base64url(JSON offer)>`. The fragment never
//!    leaves the phone. The offer is a [`PairingOffer`]: relay URL, host id, host static public
//!    key, pairing slot id (128 bit) and the one-time PSK (256 bit); it lives 10 minutes, one use.
//! 2. The phone connects to the relay with the `pairing_id`, runs `Noise_IKpsk2` (see [`crate::noise`]),
//!    then sends [`PairMsg::Confirm`] with its [`crate::noise::check_code`] as its first transport
//!    message. Only a phone that read the host's message 2 (so it knows the PSK) can send it: the
//!    host shows nothing for the channel until this message decrypts and matches its own code.
//! 3. The phone then sends [`PairMsg::Hello`] with its device name. The PC shows the device name
//!    and the check code; on Allow it sends
//!    [`PairMsg::Accept`] with a fresh routing token and the granted level, on Deny
//!    [`PairMsg::Deny`]. The phone stores `{relay, host_id, host_pub, routing_token, name}`.

use serde::{Deserialize, Serialize};

use crate::ids::{PAIRING_ID_LEN, TOKEN_LEN, b64_decode, b64_decode_fixed, b64_encode, is_valid_host_id};
use crate::limits::PAIRING_SLOT_MAX_SECS;
use crate::msg::Level;
use crate::noise::{KEY_LEN, PSK_LEN};

pub const OFFER_VERSION: u8 = 1;
pub const MAX_DEVICE_NAME_CHARS: usize = 64;
const FRAGMENT_KEY: &str = "p=";

/// The QR payload. Short keys keep the QR small: `r` relay URL (`wss://...`), `h` host id, `k` host
/// static public key, `i` pairing id, `p` PSK (binary ones in base64url).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairingOffer {
    pub v: u8,
    pub r: String,
    pub h: String,
    pub k: String,
    pub i: String,
    pub p: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfferError {
    Format,
    Version(u8),
    Field(&'static str),
}

/// `ws://127.0.0.1` with an optional port and an optional path, and nothing else. A userinfo part
/// (`ws://127.0.0.1@host`) or a longer host name (`ws://127.0.0.1.example`) is refused (SEC-69).
pub fn is_loopback_ws_relay(relay: &str) -> bool {
    let Some(rest) = relay.strip_prefix("ws://127.0.0.1") else { return false };
    let tail = match rest.strip_prefix(':') {
        Some(after_colon) => {
            let digits = after_colon.bytes().take_while(u8::is_ascii_digit).count();
            if digits == 0 || digits > 5 {
                return false;
            }
            match after_colon.get(digits..) {
                Some(t) => t,
                None => return false,
            }
        }
        None => rest,
    };
    tail.is_empty() || tail.starts_with('/')
}

impl PairingOffer {
    pub fn new(relay: &str, host_id: &str, host_pub: &[u8; KEY_LEN], pairing_id: &[u8; PAIRING_ID_LEN], psk: &[u8; PSK_LEN]) -> Self {
        Self {
            v: OFFER_VERSION,
            r: relay.to_owned(),
            h: host_id.to_owned(),
            k: b64_encode(host_pub),
            i: b64_encode(pairing_id),
            p: b64_encode(psk),
        }
    }

    /// Checks every field's shape; the relay URL must be `wss://` (or `ws://` to loopback, for tests).
    pub fn validate(&self) -> Result<(), OfferError> {
        if self.v != OFFER_VERSION {
            return Err(OfferError::Version(self.v));
        }
        if !(self.r.starts_with("wss://") || is_loopback_ws_relay(&self.r)) || self.r.len() > 256 || self.r.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return Err(OfferError::Field("r"));
        }
        if !is_valid_host_id(&self.h) {
            return Err(OfferError::Field("h"));
        }
        if b64_decode_fixed::<KEY_LEN>(&self.k).is_none() {
            return Err(OfferError::Field("k"));
        }
        if b64_decode_fixed::<PAIRING_ID_LEN>(&self.i).is_none() {
            return Err(OfferError::Field("i"));
        }
        if b64_decode_fixed::<PSK_LEN>(&self.p).is_none() {
            return Err(OfferError::Field("p"));
        }
        Ok(())
    }

    pub fn host_pub(&self) -> Option<[u8; KEY_LEN]> {
        b64_decode_fixed(&self.k)
    }
    pub fn pairing_id(&self) -> Option<[u8; PAIRING_ID_LEN]> {
        b64_decode_fixed(&self.i)
    }
    pub fn psk(&self) -> Option<[u8; PSK_LEN]> {
        b64_decode_fixed(&self.p)
    }

    /// base64url of the compact JSON (fields in the order above).
    pub fn encode(&self) -> String {
        b64_encode(serde_json::to_string(self).unwrap_or_default().as_bytes())
    }

    /// Accepts the bare payload, `p=<payload>`, `#p=<payload>` or a full URL containing `#p=...`.
    pub fn decode(input: &str) -> Result<Self, OfferError> {
        let input = input.trim();
        let frag = input.rsplit_once('#').map_or(input, |(_, f)| f);
        let payload = frag.strip_prefix(FRAGMENT_KEY).unwrap_or(frag);
        let json = b64_decode(payload).ok_or(OfferError::Format)?;
        let offer: Self = serde_json::from_slice(&json).map_err(|_| OfferError::Format)?;
        offer.validate()?;
        Ok(offer)
    }

    /// `<app_origin>/v/<app_version>/#p=<payload>`. `app_origin` has no trailing slash.
    pub fn url(&self, app_origin: &str, app_version: &str) -> String {
        format!("{}/v/{}/#{}{}", app_origin.trim_end_matches('/'), app_version, FRAGMENT_KEY, self.encode())
    }
}

/// How long a pairing slot may live (seconds), never more than the relay's cap.
pub const SLOT_SECS: u32 = PAIRING_SLOT_MAX_SECS;

/// Messages inside the pairing channel (JSON in Noise transport messages).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum PairMsg {
    /// Phone to host, the first transport message: key confirmation. A phone that does not know
    /// the PSK never reads the host's message 2, so it can never send this; `check` must equal the
    /// host's own check code (the same handshake).
    Confirm { check: String },
    /// Phone to host, after `Confirm`: the device name.
    Hello { v: u8, name: String },
    /// Host to phone: the PC owner allowed it. `routing_token` is base64url of 32 bytes.
    Accept { routing_token: String, level: Level, device_id: String },
    /// Host to phone: refused.
    Deny { reason: DenyReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The person at the PC pressed Deny.
    Denied,
    /// Nobody answered in time, or the slot expired.
    Expired,
    /// The phone sent something malformed.
    BadRequest,
}

impl PairMsg {
    /// A name is 1..=64 characters with no control characters.
    pub fn valid_name(name: &str) -> bool {
        let n = name.trim().chars().count();
        (1..=MAX_DEVICE_NAME_CHARS).contains(&n) && !name.chars().any(char::is_control)
    }

    pub fn routing_token(&self) -> Option<[u8; TOKEN_LEN]> {
        match self {
            Self::Accept { routing_token, .. } => b64_decode_fixed(routing_token),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::host_id;

    fn offer() -> PairingOffer {
        PairingOffer::new("wss://relay.example.test/", &host_id(&[1; 32]), &[2; 32], &[3; 16], &[4; 32])
    }

    #[test]
    fn offer_round_trips_in_all_spellings() {
        let o = offer();
        let enc = o.encode();
        assert_eq!(PairingOffer::decode(&enc).unwrap(), o);
        assert_eq!(PairingOffer::decode(&format!("p={enc}")).unwrap(), o);
        assert_eq!(PairingOffer::decode(&format!("#p={enc}")).unwrap(), o);
        let url = o.url("https://phone.example.test/", "0.1.0");
        assert!(url.starts_with("https://phone.example.test/v/0.1.0/#p="));
        assert_eq!(PairingOffer::decode(&url).unwrap(), o);
    }

    #[test]
    fn a_look_alike_loopback_relay_is_not_accepted_in_an_offer() {
        for relay in ["ws://127.0.0.1.evil.example", "ws://127.0.0.1@evil.example", "ws://localhost.evil.example", "ws://localhost:9000"] {
            let o = PairingOffer::new(relay, &host_id(&[1; 32]), &[2; 32], &[3; 16], &[4; 32]);
            assert_eq!(o.validate(), Err(OfferError::Field("r")), "{relay}");
        }
        let ok = PairingOffer::new("ws://127.0.0.1:9000", &host_id(&[1; 32]), &[2; 32], &[3; 16], &[4; 32]);
        assert!(ok.validate().is_ok());
    }

    #[test]
    fn bad_offers_are_refused_not_panicked_on() {
        assert_eq!(PairingOffer::decode("").unwrap_err(), OfferError::Format);
        assert_eq!(PairingOffer::decode("p=!!!").unwrap_err(), OfferError::Format);
        let mut o = offer();
        o.r = "http://evil.example".into();
        assert_eq!(PairingOffer::decode(&o.encode()).unwrap_err(), OfferError::Field("r"));
        let mut o = offer();
        o.h = "short".into();
        assert_eq!(o.validate().unwrap_err(), OfferError::Field("h"));
        let mut o = offer();
        o.v = 9;
        assert_eq!(o.validate().unwrap_err(), OfferError::Version(9));
        let mut o = offer();
        o.p = b64_encode(&[0; 31]);
        assert_eq!(o.validate().unwrap_err(), OfferError::Field("p"));
    }

    #[test]
    fn names() {
        assert!(PairMsg::valid_name("Pixel 9"));
        assert!(!PairMsg::valid_name(""));
        assert!(!PairMsg::valid_name("   "));
        assert!(!PairMsg::valid_name("a\nb"));
        assert!(!PairMsg::valid_name(&"x".repeat(65)));
    }
}
