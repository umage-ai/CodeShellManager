//! Ids and hashes. Domain separated and distinct from the phone remote's: a host id or device id
//! of one profile never equals one of the other for the same key. Binary values travel as
//! unpadded base64url, ids as lowercase RFC 4648 base32 (no padding).

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

pub const KEY_LEN: usize = 32;
pub const PSK_LEN: usize = 32;
pub const PAIRING_ID_LEN: usize = 16;
pub const ID_LEN: usize = 26;
const HOST_DOMAIN: &[u8] = b"csm-peer host";
const DEVICE_DOMAIN: &[u8] = b"csm-peer device";

pub fn b64_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes unpadded base64url into exactly `N` bytes, in the one canonical spelling.
pub fn b64_decode_fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != (N * 4).div_ceil(3) {
        return None;
    }
    let v = URL_SAFE_NO_PAD.decode(s).ok()?;
    v.try_into().ok()
}

pub fn b64_decode(s: &str) -> Option<Vec<u8>> {
    URL_SAFE_NO_PAD.decode(s).ok()
}

fn base32_lower(bytes: &[u8]) -> String {
    const A: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(A[((buf >> bits) & 31) as usize]));
        }
        buf &= (1 << bits) - 1;
    }
    if bits > 0 {
        out.push(char::from(A[((buf << (5 - bits)) & 31) as usize]));
    }
    out
}

fn derive(domain: &[u8], key: &[u8; KEY_LEN]) -> String {
    let d = Sha256::new().chain_update(domain).chain_update(key).finalize();
    let mut s = base32_lower(&d);
    s.truncate(ID_LEN);
    s
}

/// `base32(SHA-256("csm-peer host" || host_static_public))[..26]`: bound to the host's key, so a
/// pairing offer cannot name a host id that its key does not have.
pub fn host_id(host_static_public: &[u8; KEY_LEN]) -> String {
    derive(HOST_DOMAIN, host_static_public)
}

/// `base32(SHA-256("csm-peer device" || device_static_public))[..26]`.
pub fn device_id(device_static_public: &[u8; KEY_LEN]) -> String {
    derive(DEVICE_DOMAIN, device_static_public)
}

/// Whether `s` has the shape of a host or device id.
pub fn is_valid_id(s: &str) -> bool {
    s.len() == ID_LEN && s.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base32_known_values() {
        assert_eq!(base32_lower(b""), "");
        assert_eq!(base32_lower(b"f"), "my");
        assert_eq!(base32_lower(b"foobar"), "mzxw6ytboi");
    }

    #[test]
    fn ids_are_shaped_bound_to_the_key_and_separated() {
        let k = [7u8; 32];
        assert!(is_valid_id(&host_id(&k)));
        assert_ne!(host_id(&k), host_id(&[8; 32]));
        assert_ne!(host_id(&k), device_id(&k), "host and device ids are domain separated");
        assert!(!is_valid_id("short"));
        assert!(!is_valid_id(&"A".repeat(ID_LEN)));
    }

    #[test]
    fn b64_is_canonical_only() {
        let s = b64_encode(&[1u8; 32]);
        assert_eq!(b64_decode_fixed::<32>(&s), Some([1u8; 32]));
        assert_eq!(b64_decode_fixed::<32>(&format!("{s}=")), None);
        assert_eq!(b64_decode_fixed::<16>(&s), None);
        assert_eq!(b64_decode_fixed::<32>("not base64!"), None);
    }

    #[test]
    fn hex_round_trip() {
        assert_eq!(unhex(&hex(&[0, 255, 16])), Some(vec![0, 255, 16]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(unhex("é"), None);
    }
}
