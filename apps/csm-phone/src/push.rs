//! Web Push body encryption: RFC 8291 over the `aes128gcm` content coding of RFC 8188. The add-on
//! encrypts the `needs-you` payload with the phone's subscription key, so the push service and the
//! relay (which forwards the body) cannot read it.
//!
//! Pure given its inputs: the caller supplies the sender's ephemeral private key and the 16-byte
//! salt, which are random in the product and fixed in the RFC's example (the unit test). Key
//! material is never logged or returned in an error.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use hkdf::Hkdf;
use p256::elliptic_curve::sec1::ToSec1Point;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

/// The record size the add-on writes (RFC 8188 `rs`). One record holds the whole payload.
pub const RECORD_SIZE: u32 = 4096;
/// The last-record delimiter (RFC 8188 section 2).
const LAST_RECORD: u8 = 0x02;
/// AES-GCM tag length.
const TAG: usize = 16;
/// The uncompressed P-256 point length.
pub const POINT_LEN: usize = 65;

/// Encrypts `plain` for the phone whose subscription key is `ua_public` (65 bytes, uncompressed)
/// and auth secret `auth` (16 bytes). The result is the complete body: the header (salt, record
/// size, key id) followed by the single record.
pub fn encrypt(plain: &[u8], ua_public: &[u8], auth: &[u8], as_private: &[u8; 32], salt: &[u8; 16]) -> Result<Vec<u8>, String> {
    if plain.len() + 1 + TAG > RECORD_SIZE as usize {
        return Err("the push payload is too large for one record".into());
    }
    let ua = PublicKey::from_sec1_bytes(ua_public).map_err(|_| "the phone's push key is not a P-256 point".to_owned())?;
    let as_secret = SecretKey::from_slice(as_private).map_err(|_| "the push key could not be made".to_owned())?;
    let as_public = encoded_point(&as_secret)?;
    let shared = p256::ecdh::diffie_hellman(as_secret.to_nonzero_scalar(), ua.as_affine());
    let (cek, nonce) = derive(shared.raw_secret_bytes().as_slice(), auth, ua_public, &as_public, salt)?;
    let mut record = Vec::with_capacity(plain.len() + 1);
    record.extend_from_slice(plain);
    record.push(LAST_RECORD);
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| "the content key is the wrong size".to_owned())?;
    let sealed = cipher.encrypt(Nonce::from_slice(&nonce), record.as_slice()).map_err(|_| "the push body could not be encrypted".to_owned())?;
    let mut body = Vec::with_capacity(21 + POINT_LEN + sealed.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    body.push(POINT_LEN as u8);
    body.extend_from_slice(&as_public);
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// Decrypts a body made by [`encrypt`] with the phone's own keys. The phone does this in the
/// browser; the add-on needs it only to test that a body opens to what was sent.
pub fn decrypt(body: &[u8], ua_private: &[u8; 32], auth: &[u8]) -> Result<Vec<u8>, String> {
    let bad = || "the push body is malformed".to_owned();
    let salt: [u8; 16] = body.get(..16).and_then(|s| s.try_into().ok()).ok_or_else(bad)?;
    let idlen = usize::from(*body.get(20).ok_or_else(bad)?);
    let as_public = body.get(21..21 + idlen).ok_or_else(bad)?;
    let sealed = body.get(21 + idlen..).ok_or_else(bad)?;
    let ua_secret = SecretKey::from_slice(ua_private).map_err(|_| bad())?;
    let ua_public = encoded_point(&ua_secret)?;
    let as_key = PublicKey::from_sec1_bytes(as_public).map_err(|_| bad())?;
    let shared = p256::ecdh::diffie_hellman(ua_secret.to_nonzero_scalar(), as_key.as_affine());
    let (cek, nonce) = derive(shared.raw_secret_bytes().as_slice(), auth, &ua_public, as_public, &salt)?;
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| bad())?;
    let mut record = cipher.decrypt(Nonce::from_slice(&nonce), sealed).map_err(|_| "the push body does not open with these keys".to_owned())?;
    // The padding delimiter is the last non-zero byte, and it must be the last-record marker.
    while record.last() == Some(&0) {
        record.pop();
    }
    if record.pop() != Some(LAST_RECORD) {
        return Err(bad());
    }
    Ok(record)
}

/// The uncompressed public point of a P-256 private key.
fn encoded_point(secret: &SecretKey) -> Result<[u8; POINT_LEN], String> {
    let point = secret.public_key().to_sec1_point(false);
    <[u8; POINT_LEN]>::try_from(point.as_bytes()).map_err(|_| "the public key is the wrong size".to_owned())
}

/// RFC 8291 section 3.3 and RFC 8188 section 2.2: the input keying material from the ECDH secret and
/// the auth secret, then the content key and the nonce for this salt.
fn derive(ecdh: &[u8], auth: &[u8], ua_public: &[u8], as_public: &[u8], salt: &[u8; 16]) -> Result<([u8; 16], [u8; 12]), String> {
    let mut key_info = Vec::with_capacity(14 + 2 * POINT_LEN);
    key_info.extend_from_slice(b"WebPush: info\0");
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let mut ikm = [0u8; 32];
    Hkdf::<Sha256>::new(Some(auth), ecdh).expand(&key_info, &mut ikm).map_err(|_| "the key derivation failed".to_owned())?;
    let prk = Hkdf::<Sha256>::new(Some(salt), &ikm);
    let mut cek = [0u8; 16];
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).map_err(|_| "the key derivation failed".to_owned())?;
    let mut nonce = [0u8; 12];
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).map_err(|_| "the key derivation failed".to_owned())?;
    Ok((cek, nonce))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use csm_remote_proto::ids::b64_decode;

    // RFC 8291 section 5 (the example, with the body's line breaks removed).
    const AUTH: &str = "BTBZMqHH6r4Tts7J_aSIgg";
    const UA_PRIVATE: &str = "q1dXpw3UpT5VOmu_cf_v6ih07Aems3njxI-JWgLcM94";
    const UA_PUBLIC: &str = "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4";
    const AS_PRIVATE: &str = "yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw";
    const BODY: &str = "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN";
    const PLAIN: &[u8] = b"When I grow up, I want to be a watermelon";

    fn bytes(s: &str) -> Vec<u8> {
        b64_decode(s).expect("valid base64url")
    }

    #[test]
    fn the_rfc_8291_example_encrypts_to_the_published_body() {
        let body = bytes(BODY);
        // 21 (header) + 65 (key id) + 41 (plaintext) + 1 (delimiter) + 16 (tag) = 144. The RFC's
        // `Content-Length: 145` does not match its own 41-byte plaintext; the body text is 144 bytes.
        assert_eq!(body.len(), 144);
        let auth = bytes(AUTH);
        let as_private: [u8; 32] = bytes(AS_PRIVATE).try_into().unwrap();
        let salt: [u8; 16] = body[..16].try_into().unwrap();
        let ua_public = bytes(UA_PUBLIC);
        let made = encrypt(PLAIN, &ua_public, &auth, &as_private, &salt).unwrap();
        assert_eq!(made, body, "the same salt and sender key give the RFC's bytes");
    }

    #[test]
    fn the_rfc_8291_example_opens_with_the_phones_keys() {
        let ua_private: [u8; 32] = bytes(UA_PRIVATE).try_into().unwrap();
        assert_eq!(decrypt(&bytes(BODY), &ua_private, &bytes(AUTH)).unwrap(), PLAIN);
    }

    #[test]
    fn a_body_for_the_wrong_auth_or_key_does_not_open() {
        let ua_private: [u8; 32] = bytes(UA_PRIVATE).try_into().unwrap();
        let mut wrong_auth = bytes(AUTH);
        wrong_auth[0] ^= 1;
        assert!(decrypt(&bytes(BODY), &ua_private, &wrong_auth).is_err());
        let mut tampered = bytes(BODY);
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(decrypt(&tampered, &ua_private, &bytes(AUTH)).is_err());
    }

    #[test]
    fn a_key_that_is_not_on_the_curve_and_an_oversize_payload_are_refused() {
        let auth = bytes(AUTH);
        let as_private: [u8; 32] = bytes(AS_PRIVATE).try_into().unwrap();
        let salt = [1u8; 16];
        assert!(encrypt(PLAIN, &[4u8; 65], &auth, &as_private, &salt).is_err());
        assert!(encrypt(&vec![0u8; 5000], &bytes(UA_PUBLIC), &auth, &as_private, &salt).is_err());
        assert!(decrypt(&[0u8; 10], &[1u8; 32], &auth).is_err(), "a short body is refused, not a panic");
    }
}
