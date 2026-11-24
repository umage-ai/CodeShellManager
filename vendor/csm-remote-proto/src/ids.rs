//! Ids and hashes. All derivations are domain separated; encodings are lowercase RFC 4648 base32
//! (no padding) for host ids and unpadded base64url for everything else binary.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

pub const HOST_ID_LEN: usize = 26;
pub const HOST_SECRET_LEN: usize = 32;
pub const TOKEN_LEN: usize = 32;
pub const PAIRING_ID_LEN: usize = 16;
const HOST_DOMAIN: &[u8] = b"csm-relay host";
const TOKEN_DOMAIN: &[u8] = b"csm-relay token";

pub fn b64_encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes unpadded base64url into exactly `N` bytes.
pub fn b64_decode_fixed<const N: usize>(s: &str) -> Option<[u8; N]> {
    // Only the canonical length: one value, one spelling (padding makes the length differ).
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

/// `base32(SHA-256("csm-relay host" || host_secret))[..26]`.
pub fn host_id(secret: &[u8; HOST_SECRET_LEN]) -> String {
    let d = Sha256::new().chain_update(HOST_DOMAIN).chain_update(secret).finalize();
    let mut s = base32_lower(&d);
    s.truncate(HOST_ID_LEN);
    s
}

pub fn is_valid_host_id(s: &str) -> bool {
    s.len() == HOST_ID_LEN && s.bytes().all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b))
}

/// `SHA-256("csm-relay token" || routing_token)`: what the host gives the relay for each device.
pub fn token_hash(token: &[u8; TOKEN_LEN]) -> [u8; 32] {
    Sha256::new().chain_update(TOKEN_DOMAIN).chain_update(token).finalize().into()
}

/// Constant-time equality (false on a length mismatch).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
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
        // RFC 4648 test vectors, lowercased and unpadded.
        assert_eq!(base32_lower(b""), "");
        assert_eq!(base32_lower(b"f"), "my");
        assert_eq!(base32_lower(b"fo"), "mzxq");
        assert_eq!(base32_lower(b"foobar"), "mzxw6ytboi");
    }

    #[test]
    fn host_id_shape() {
        let id = host_id(&[7; 32]);
        assert!(is_valid_host_id(&id));
        assert_ne!(id, host_id(&[8; 32]));
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
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex("é"), None);
    }
}
