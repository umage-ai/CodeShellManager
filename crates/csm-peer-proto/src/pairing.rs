//! Pairing: the link the host shows and the exchange inside the encrypted pairing channel.
//!
//! 1. The person turns server mode on in the app and starts pairing. The host shows a link,
//!    `csm://pair#<base64url(JSON offer)>`, as text and as a QR code. The offer is a
//!    [`PeerOffer`]: the address (`tcp://10.8.0.2:47300`), the host id, the host's static public
//!    key, a pairing id (128 bit) and a one-time PSK (256 bit). It lives 10 minutes and works once.
//! 2. On the other machine the person pastes the link into "Connect to another CSM". The client
//!    connects to the address, sends the pairing preamble (`0x02 || i`, the offer's pairing id; it
//!    is how the host picks `IKpsk2` before message 1, see [`crate::noise::route`]), runs
//!    `Noise_IKpsk2` ([`crate::noise`]), then sends [`PairMsg::Hello`] with its device name. The
//!    slot is consumed when that first message decrypts, and allows
//!    [`crate::noise::MAX_PAIR_ATTEMPTS`] handshakes.
//! 3. **Both screens show the 4-digit check code** ([`crate::noise::check_code`]) and the
//!    device name. On the host the person chooses Allow (with a level) or Deny. On Allow the host
//!    sends [`PairMsg::Accept`] and adds the client's static key to its device list. Pairing is
//!    never started from the network: the host has to be at "waiting for a device", which only
//!    the app's UI can start.

use serde::{Deserialize, Serialize};

use crate::addr::{AddrError, PeerAddr};
use crate::ids::{KEY_LEN, PAIRING_ID_LEN, PSK_LEN, b64_decode, b64_decode_fixed, b64_encode, host_id, is_valid_id};
use crate::limits::MAX_OFFER_CHARS;
use crate::msg::{Level, PROTOCOL_VERSION, valid_name};

pub const OFFER_VERSION: u8 = 1;
/// The scheme and path of a pairing link.
pub const LINK_PREFIX: &str = "csm://pair#";
/// How long a pairing slot lives, in seconds.
pub const SLOT_SECS: u32 = 600;

/// The offer. Short keys keep the QR small: `a` address, `h` host id, `k` host static public key,
/// `i` pairing id (the preamble's slot selector, not a secret), `p` PSK (binary ones in base64url).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerOffer {
    pub v: u8,
    pub a: String,
    pub h: String,
    pub k: String,
    pub i: String,
    pub p: String,
    /// The relay this host is also reachable through, when it is set up (optional: older links and
    /// direct-only hosts have none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r: Option<RelayRoute>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferError {
    Format,
    Version(u8),
    Field(&'static str),
}

impl std::fmt::Display for OfferError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Format => f.write_str("that is not a pairing link"),
            Self::Version(v) => write!(f, "the pairing link is version {v}, which this app does not read"),
            Self::Field(n) => write!(f, "the pairing link has a bad `{n}` field"),
        }
    }
}

impl std::error::Error for OfferError {}

impl PeerOffer {
    /// An offer for a host listening at `addr`. The host id is derived from the host key.
    pub fn new(addr: &PeerAddr, host_pub: &[u8; KEY_LEN], pairing_id: &[u8; PAIRING_ID_LEN], psk: &[u8; PSK_LEN]) -> Self {
        Self { v: OFFER_VERSION, a: addr.to_url(), h: host_id(host_pub), k: b64_encode(host_pub), i: b64_encode(pairing_id), p: b64_encode(psk), r: None }
    }

    /// The same offer, also reachable through `relay`.
    pub fn with_relay(mut self, relay: RelayRoute) -> Self {
        self.r = Some(relay);
        self
    }

    /// Checks every field's shape, the address (never unspecified), and that the host id is the
    /// one the key has: an offer cannot name a host id its key does not match.
    pub fn validate(&self) -> Result<(), OfferError> {
        if self.v != OFFER_VERSION {
            return Err(OfferError::Version(self.v));
        }
        PeerAddr::parse(&self.a).map_err(|_: AddrError| OfferError::Field("a"))?;
        let key = b64_decode_fixed::<KEY_LEN>(&self.k).ok_or(OfferError::Field("k"))?;
        if !is_valid_id(&self.h) || self.h != host_id(&key) {
            return Err(OfferError::Field("h"));
        }
        if b64_decode_fixed::<PAIRING_ID_LEN>(&self.i).is_none() {
            return Err(OfferError::Field("i"));
        }
        if self.r.as_ref().is_some_and(|r| !r.validate()) {
            return Err(OfferError::Field("r"));
        }
        if b64_decode_fixed::<PSK_LEN>(&self.p).is_none() {
            return Err(OfferError::Field("p"));
        }
        Ok(())
    }

    pub fn addr(&self) -> Option<PeerAddr> {
        PeerAddr::parse(&self.a).ok()
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

    /// `csm://pair#<payload>`.
    pub fn link(&self) -> String {
        format!("{LINK_PREFIX}{}", self.encode())
    }

    /// Accepts the bare payload, `#<payload>` or a link. Never panics, whatever the text.
    pub fn decode(input: &str) -> Result<Self, OfferError> {
        let input = input.trim();
        if input.is_empty() || input.chars().count() > MAX_OFFER_CHARS {
            return Err(OfferError::Format);
        }
        let payload = input.rsplit_once('#').map_or(input, |(head, p)| if head.is_empty() || head == "csm://pair" { p } else { input });
        let json = b64_decode(payload).ok_or(OfferError::Format)?;
        let offer: Self = serde_json::from_slice(&json).map_err(|_| OfferError::Format)?;
        offer.validate()?;
        Ok(offer)
    }
}

/// Messages inside the pairing channel (JSON in Noise transport messages, at most
/// [`crate::limits::MAX_PAIR_MESSAGE`] bytes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum PairMsg {
    /// Client to host, first message after the handshake.
    Hello { v: u32, name: String },
    /// Host to client: the person allowed it. `device_id` is derived from the client's key.
    Accept { device_id: String, level: Level },
    /// Host to client: refused.
    Deny { reason: DenyReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DenyReason {
    /// The person at the host pressed Deny.
    Denied,
    /// Nobody answered in time, or the slot expired or was used.
    Expired,
    /// The client sent something malformed.
    BadRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairError {
    TooLarge,
    Format,
    Version(u32),
    Name,
    DeviceId,
}

impl PairMsg {
    pub fn encode(&self) -> Result<Vec<u8>, PairError> {
        self.validate()?;
        let v = serde_json::to_vec(self).map_err(|_| PairError::Format)?;
        if v.len() > crate::limits::MAX_PAIR_MESSAGE {
            return Err(PairError::TooLarge);
        }
        Ok(v)
    }

    /// Strict: size, shape, protocol version, name and id are all checked.
    pub fn decode(bytes: &[u8]) -> Result<Self, PairError> {
        if bytes.len() > crate::limits::MAX_PAIR_MESSAGE {
            return Err(PairError::TooLarge);
        }
        let m: Self = serde_json::from_slice(bytes).map_err(|_| PairError::Format)?;
        m.validate()?;
        Ok(m)
    }

    fn validate(&self) -> Result<(), PairError> {
        match self {
            Self::Hello { v, name } => {
                if *v != PROTOCOL_VERSION {
                    return Err(PairError::Version(*v));
                }
                if !valid_name(name) {
                    return Err(PairError::Name);
                }
            }
            Self::Accept { device_id, .. } if !is_valid_id(device_id) => return Err(PairError::DeviceId),
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offer() -> PeerOffer {
        let addr = PeerAddr::parse("tcp://10.8.0.2:47300").unwrap();
        PeerOffer::new(&addr, &[2; 32], &[3; 16], &[4; 32])
    }

    #[test]
    fn offer_round_trips_in_all_spellings() {
        let o = offer();
        let enc = o.encode();
        assert_eq!(PeerOffer::decode(&enc).unwrap(), o);
        assert_eq!(PeerOffer::decode(&format!("#{enc}")).unwrap(), o);
        assert_eq!(PeerOffer::decode(&format!("  {}\n", o.link())).unwrap(), o);
        assert!(o.link().starts_with("csm://pair#"));
        assert_eq!(o.addr().unwrap().to_url(), "tcp://10.8.0.2:47300");
        assert_eq!(o.host_pub(), Some([2; 32]));
    }

    #[test]
    fn bad_offers_are_refused_not_panicked_on() {
        assert_eq!(PeerOffer::decode("").unwrap_err(), OfferError::Format);
        assert_eq!(PeerOffer::decode("p=!!!").unwrap_err(), OfferError::Format);
        assert_eq!(PeerOffer::decode("https://evil.example/#abc").unwrap_err(), OfferError::Format);
        assert_eq!(PeerOffer::decode(&"A".repeat(MAX_OFFER_CHARS + 1)).unwrap_err(), OfferError::Format);
        for (addr, why) in [("tcp://0.0.0.0:47300", "a"), ("tcp://[::]:47300", "a"), ("wss://relay.example/", "a"), ("tcp://10.8.0.2:0", "a")] {
            let mut o = offer();
            o.a = addr.into();
            assert_eq!(PeerOffer::decode(&o.encode()).unwrap_err(), OfferError::Field(why), "{addr}");
        }
        let mut o = offer();
        o.h = host_id(&[9; 32]);
        assert_eq!(o.validate().unwrap_err(), OfferError::Field("h"), "a host id that the key does not have");
        let mut o = offer();
        o.v = 9;
        assert_eq!(o.validate().unwrap_err(), OfferError::Version(9));
        let mut o = offer();
        o.p = b64_encode(&[0; 31]);
        assert_eq!(o.validate().unwrap_err(), OfferError::Field("p"));
        let mut o = offer();
        o.i = String::new();
        assert_eq!(o.validate().unwrap_err(), OfferError::Field("i"));
        let mut o = offer();
        o.k = b64_encode(&[1; 33]);
        assert_eq!(o.validate().unwrap_err(), OfferError::Field("k"));
    }

    #[test]
    fn pair_messages_are_strict() {
        let hello = PairMsg::Hello { v: PROTOCOL_VERSION, name: "Laptop".into() };
        assert_eq!(PairMsg::decode(&hello.encode().unwrap()).unwrap(), hello);
        let accept = PairMsg::Accept { device_id: crate::ids::device_id(&[1; 32]), level: Level::Full };
        assert_eq!(PairMsg::decode(&accept.encode().unwrap()).unwrap(), accept);
        let deny = PairMsg::Deny { reason: DenyReason::Denied };
        assert_eq!(PairMsg::decode(&deny.encode().unwrap()).unwrap(), deny);
        assert_eq!(PairMsg::decode(br#"{"t":"hello","v":1,"name":"x"}"#), Err(PairError::Version(1)));
        let bad_name = format!("{{\"t\":\"hello\",\"v\":{PROTOCOL_VERSION},\"name\":\"a\\nb\"}}").into_bytes();
        assert_eq!(PairMsg::decode(&bad_name), Err(PairError::Name));
        assert_eq!(PairMsg::decode(br#"{"t":"accept","device_id":"x","level":"full"}"#), Err(PairError::DeviceId));
        assert_eq!(PairMsg::decode(br#"{"t":"accept","device_id":"aaaaaaaaaaaaaaaaaaaaaaaaaa","level":"root"}"#), Err(PairError::Format));
        assert_eq!(PairMsg::decode(&vec![b' '; crate::limits::MAX_PAIR_MESSAGE + 1]), Err(PairError::TooLarge));
        assert_eq!(PairMsg::decode(b"\xff\xfe"), Err(PairError::Format));
    }
}

/// Where a relay carries this host: the relay's WebSocket address and the relay's id for the host
/// (the relay routes by it; the Noise session still uses the host's own key and id).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelayRoute {
    pub url: String,
    pub host_id: String,
}

/// Whether a relay address may be used: `wss://`, or `ws://` to loopback only (a relay on this PC).
/// Plain WebSocket anywhere else would carry the relay's access token in the clear.
///
/// The check is strict on purpose: the address comes from an untrusted pairing link and from
/// `hosts.json`, and a URL parser that reads `user@host` or a backslash differently from this check
/// would send plain WebSocket to another machine. So userinfo (`@`), backslashes, fragments (`#`),
/// whitespace and control characters are refused outright, and for `ws://` the host must be exactly
/// `127.0.0.1`, `localhost` or `[::1]`, with an optional port.
pub fn relay_url_allowed(url: &str) -> bool {
    if url.len() > 512 || url.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '@' | '\\' | '#')) {
        return false;
    }
    if let Some(rest) = url.strip_prefix("wss://") {
        // TLS checks the certificate against the host, so any host name will do.
        let authority = rest.split(['/', '?']).next().unwrap_or_default();
        return !authority.is_empty() && !authority.starts_with(':');
    }
    let Some(rest) = url.strip_prefix("ws://") else { return false };
    let authority = rest.split(['/', '?']).next().unwrap_or_default();
    loopback_authority(authority)
}

/// `host[:port]` where the host is a loopback literal or `localhost`.
fn loopback_authority(authority: &str) -> bool {
    let (host, tail) = if let Some(after) = authority.strip_prefix('[') {
        let Some((v6, tail)) = after.split_once(']') else { return false };
        if v6 != "::1" {
            return false;
        }
        ("[::1]", tail)
    } else {
        authority.find(':').map_or((authority, ""), |i| (&authority[..i], &authority[i..]))
    };
    if !matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
        return false;
    }
    tail.is_empty() || tail.strip_prefix(':').is_some_and(valid_port)
}

fn valid_port(p: &str) -> bool {
    p.len() <= 5 && p.bytes().all(|b| b.is_ascii_digit()) && p.parse::<u16>().is_ok_and(|n| n != 0)
}

impl RelayRoute {
    /// Checks the address and the relay's host id.
    pub fn validate(&self) -> bool {
        relay_url_allowed(&self.url) && self.url.len() <= 512 && is_valid_id(&self.host_id)
    }
}

#[cfg(test)]
mod relay_url_tests {
    use super::relay_url_allowed;

    #[test]
    fn loopback_and_tls_addresses_are_allowed() {
        for ok in ["ws://127.0.0.1", "ws://127.0.0.1:8080", "ws://127.0.0.1:8080/", "ws://127.0.0.1:8080/v1/peer-host?x=1", "ws://localhost:9", "ws://[::1]:9/v1", "wss://relay.example", "wss://relay.example:443/v1/peer-host"] {
            assert!(relay_url_allowed(ok), "{ok} should be allowed");
        }
    }

    #[test]
    fn userinfo_and_other_host_tricks_are_refused() {
        for bad in [
            "ws://127.0.0.1:80@evil.example/x",
            "ws://127.0.0.1@evil.example/x",
            "ws://user:pass@127.0.0.1:80/",
            "ws://127.0.0.1\\@evil.example/",
            "ws://127.0.0.1\\evil.example/",
            "ws://127.0.0.1#@evil.example/",
            "ws://127.0.0.1:80#x",
            "ws://127.0.0.1.evil.example/",
            "ws://localhost.evil.example/",
            "ws://127.0.0.1%2eevil.example/",
            "ws://127.0.0.1:80:80/",
            "ws://127.0.0.1:/",
            "ws://127.0.0.1:0/",
            "ws://127.0.0.1:65536/",
            "ws://127.0.0.1:port/",
            "ws://[::1/",
            "ws://[::2]:9/",
            "ws://0.0.0.0:9/",
            "ws://10.8.0.2:9/",
            "ws://192.168.1.5/",
            "ws:///127.0.0.1",
            "ws://",
            "ws:/127.0.0.1",
            "ws://127.0.0.1 @evil.example/",
            "ws://evil.example/?@127.0.0.1",
            "ws://evil.example/?127.0.0.1",
            "http://127.0.0.1:9/",
            "https://127.0.0.1:9/",
            "wss://",
            "wss:///relay.example",
            "wss://:443/",
            "wss://user@relay.example/",
            "wss://relay.example\\@evil.example/",
            "wss://relay.example#@evil.example/",
            "",
        ] {
            assert!(!relay_url_allowed(bad), "{bad} must be refused");
        }
    }

    #[test]
    fn oversized_or_control_character_urls_are_refused() {
        let long = format!("wss://relay.example/{}", "a".repeat(600));
        assert!(!relay_url_allowed(&long));
        assert!(!relay_url_allowed("wss://relay.example\n/"));
        assert!(!relay_url_allowed("wss://relay.example\u{7f}/"));
    }
}
