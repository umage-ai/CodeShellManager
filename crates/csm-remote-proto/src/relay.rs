//! The relay layer: the only thing the relay parses.
//!
//! Two kinds of WebSocket message:
//!
//! - **Text** = one JSON control message, a tagged object (`{"t":"hello",...}`). Host to relay:
//!   [`HostToRelay`]. Relay to host: [`RelayToHost`]. Phone to relay (the first message only):
//!   [`ClientToRelay`]. Relay to phone: [`RelayToClient`].
//! - **Binary** on the host socket = a [`HostFrame`]: `version(1) | kind(1) | channel(u32 BE) |
//!   payload`. Many phone channels share the one host socket. On a phone socket, binary messages
//!   are the raw Noise messages, with no header (one socket is one channel).
//!
//! The relay never looks inside `Data` payloads.

use serde::{Deserialize, Serialize};

use crate::PROTOCOL_VERSION;
use crate::codes::RefusalCode;
use crate::ids::{PAIRING_ID_LEN, TOKEN_LEN};
use crate::limits::FRAME_HEADER_BYTES;

// ---- text control messages --------------------------------------------------------------

/// Host to relay.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum HostToRelay {
    /// First message on `/v1/host`. `host_secret` is base64url of the 32-byte secret (sent inside
    /// TLS; the relay checks it hashes to `host_id`). `access` is the operator's enrolment token,
    /// when the relay needs one.
    Hello {
        v: u8,
        host_id: String,
        host_secret: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        access: Option<String>,
    },
    /// Replace the allow list: base64url of `token_hash(routing_token)` per paired device.
    /// Channels of devices no longer listed are closed with `revoked`.
    Allow { tokens: Vec<String> },
    /// Open (replace) the one pairing slot. `expires_in` is seconds, at most 600.
    PairingOpen { pairing_id: String, expires_in: u32 },
    /// Close the pairing slot (after the device was confirmed, denied or the offer cancelled).
    PairingClose,
    /// Forward one pre-encrypted Web Push (RFC 8291 `aes128gcm` body, base64url).
    Push(PushRequest),
}

/// A push the add-on has already encrypted; the relay adds the VAPID header and POSTs it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushRequest {
    /// Echoed in [`RelayToHost::PushResult`].
    pub id: u32,
    pub endpoint: String,
    /// Seconds the push service may hold the message.
    pub ttl: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub urgency: Option<Urgency>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    /// base64url of the encrypted body (at most 4096 bytes decoded).
    pub body: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Urgency {
    VeryLow,
    Low,
    Normal,
    High,
}

impl Urgency {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VeryLow => "very-low",
            Self::Low => "low",
            Self::Normal => "normal",
            Self::High => "high",
        }
    }
}

/// Relay to host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum RelayToHost {
    /// The hello was accepted. The host now sends its allow list (and pairing slot).
    Welcome { v: u8, limits: LimitsInfo },
    /// Allow list installed (`count` entries).
    AllowOk { count: u32 },
    /// Pairing slot installed.
    PairingOk { expires_in: u32 },
    PushResult {
        id: u32,
        ok: bool,
        /// The push service's HTTP status, when it answered.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<RefusalCode>,
    },
    /// A control message was refused (the connection stays unless the code says otherwise).
    Error { code: RefusalCode },
}

/// The caps the relay applies, so the add-on can size its messages.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsInfo {
    pub max_frame_bytes: u32,
    pub max_channels: u32,
    pub max_allow_entries: u32,
    pub max_pairing_secs: u32,
    pub push: bool,
}

/// Phone to relay: the first message of a `/v1/client` socket. Exactly one of `token` (base64url
/// of the 32-byte routing token) or `pairing_id` (base64url of the 16-byte slot id) is set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientToRelay {
    Hello {
        v: u8,
        host_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pairing_id: Option<String>,
    },
}

/// Relay to phone. After `Ready`, every message in both directions is a raw Noise message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum RelayToClient {
    Ready { v: u8 },
    Error { code: RefusalCode },
}

impl RelayToClient {
    pub const fn ready() -> Self {
        Self::Ready { v: PROTOCOL_VERSION }
    }
}

pub fn to_text<T: Serialize>(msg: &T) -> String {
    // Our message types contain only strings, numbers and options: serialising cannot fail.
    serde_json::to_string(msg).unwrap_or_default()
}

// ---- binary host frames ------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameKind {
    /// A Noise message for/from a channel.
    Data = 1,
    /// Relay to host: a phone opened a channel. Payload: [`OpenInfo`].
    Open = 2,
    /// Either way: the channel is over. Payload: `u16 BE`, a [`RefusalCode`] number or 0.
    Close = 3,
}

impl FrameKind {
    fn from_u8(b: u8) -> Option<Self> {
        match b {
            1 => Some(Self::Data),
            2 => Some(Self::Open),
            3 => Some(Self::Close),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostFrame<'a> {
    pub kind: FrameKind,
    pub channel: u32,
    pub payload: &'a [u8],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    TooShort,
    Version(u8),
    /// A kind this version does not know: ignore the frame (forward compatibility).
    UnknownKind(u8),
}

pub fn encode_frame(kind: FrameKind, channel: u32, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(FRAME_HEADER_BYTES + payload.len());
    v.push(PROTOCOL_VERSION);
    v.push(kind as u8);
    v.extend_from_slice(&channel.to_be_bytes());
    v.extend_from_slice(payload);
    v
}

pub fn decode_frame(bytes: &[u8]) -> Result<HostFrame<'_>, FrameError> {
    let (Some(&ver), Some(&kind), Some(ch), Some(payload)) = (bytes.first(), bytes.get(1), bytes.get(2..6), bytes.get(FRAME_HEADER_BYTES..)) else {
        return Err(FrameError::TooShort);
    };
    if ver != PROTOCOL_VERSION {
        return Err(FrameError::Version(ver));
    }
    let kind = FrameKind::from_u8(kind).ok_or(FrameError::UnknownKind(kind))?;
    let channel = u32::from_be_bytes(ch.try_into().map_err(|_| FrameError::TooShort)?);
    Ok(HostFrame { kind, channel, payload })
}

/// Payload of an `Open` frame: which credential the phone presented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenInfo {
    /// A paired device: the allow-list entry (`token_hash`) that matched.
    Device([u8; 32]),
    /// A phone pairing: the slot id it named.
    Pairing([u8; PAIRING_ID_LEN]),
}

impl OpenInfo {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Device(h) => [&[0u8][..], h].concat(),
            Self::Pairing(p) => [&[1u8][..], p].concat(),
        }
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        match bytes.split_first()? {
            (0, rest) => Some(Self::Device(rest.try_into().ok()?)),
            (1, rest) => Some(Self::Pairing(rest.try_into().ok()?)),
            _ => None,
        }
    }
}

pub fn close_payload(code: Option<RefusalCode>) -> [u8; 2] {
    code.map_or(0, RefusalCode::as_u16).to_be_bytes()
}

/// `None` = closed normally. An unknown number is also `None`.
pub fn parse_close_payload(payload: &[u8]) -> Option<RefusalCode> {
    let b: [u8; 2] = payload.get(..2)?.try_into().ok()?;
    RefusalCode::from_u16(u16::from_be_bytes(b))
}

/// Length of a routing token, re-exported for the phone side.
pub const ROUTING_TOKEN_LEN: usize = TOKEN_LEN;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let f = encode_frame(FrameKind::Data, 0x01020304, b"abc");
        assert_eq!(f, [1, 1, 1, 2, 3, 4, b'a', b'b', b'c']);
        let d = decode_frame(&f).unwrap();
        assert_eq!((d.kind, d.channel, d.payload), (FrameKind::Data, 0x01020304, &b"abc"[..]));
    }

    #[test]
    fn decode_never_panics_on_short_or_odd_input() {
        for len in 0..FRAME_HEADER_BYTES {
            assert_eq!(decode_frame(&vec![1u8; len]), Err(FrameError::TooShort));
        }
        assert_eq!(decode_frame(&[9, 1, 0, 0, 0, 0]), Err(FrameError::Version(9)));
        assert_eq!(decode_frame(&[1, 77, 0, 0, 0, 0]), Err(FrameError::UnknownKind(77)));
        assert!(decode_frame(&[1, 1, 0, 0, 0, 0]).unwrap().payload.is_empty());
    }

    #[test]
    fn open_info_round_trip() {
        for o in [OpenInfo::Device([5; 32]), OpenInfo::Pairing([6; 16])] {
            assert_eq!(OpenInfo::decode(&o.encode()), Some(o));
        }
        assert_eq!(OpenInfo::decode(&[]), None);
        assert_eq!(OpenInfo::decode(&[0, 1, 2]), None);
        assert_eq!(OpenInfo::decode(&[2; 33]), None);
    }

    #[test]
    fn close_payload_round_trip() {
        assert_eq!(parse_close_payload(&close_payload(None)), None);
        assert_eq!(parse_close_payload(&close_payload(Some(RefusalCode::Revoked))), Some(RefusalCode::Revoked));
        assert_eq!(parse_close_payload(&[]), None);
    }

    #[test]
    fn control_messages_parse_and_ignore_unknown_fields() {
        let m: HostToRelay = serde_json::from_str(r#"{"t":"hello","v":1,"host_id":"x","host_secret":"y","extra":1}"#).unwrap();
        assert!(matches!(m, HostToRelay::Hello { access: None, .. }));
        assert!(serde_json::from_str::<HostToRelay>(r#"{"t":"nope"}"#).is_err());
        assert_eq!(to_text(&RelayToClient::Error { code: RefusalCode::HostOffline }), r#"{"t":"error","code":"host_offline"}"#);
        assert_eq!(to_text(&RelayToClient::ready()), r#"{"t":"ready","v":1}"#);
    }
}
