//! A static X25519 key pair: the host's, or a client device's. The private half is a secret and
//! lives in a file only the current user can read ([`crate::fsperm`]); the public half is what the
//! other side pins, and what the ids derive from.

use std::path::Path;

use csm_peer_proto::ids::{KEY_LEN, b64_decode_fixed, b64_encode, device_id, host_id};
use serde::{Deserialize, Serialize};
use snow::params::DHChoice;
use snow::resolvers::{CryptoResolver, DefaultResolver};

/// A key pair. `Debug` does not print the private key.
#[derive(Clone, PartialEq, Eq)]
pub struct Identity {
    private: [u8; KEY_LEN],
    public: [u8; KEY_LEN],
}

impl std::fmt::Debug for Identity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Identity").field("public", &b64_encode(&self.public)).finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    v: u8,
    private: String,
    public: String,
}

/// The public key of a private key, by the same X25519 the Noise handshake uses.
fn derive_public(private: &[u8; KEY_LEN]) -> Option<[u8; KEY_LEN]> {
    let mut dh = DefaultResolver.resolve_dh(&DHChoice::Curve25519)?;
    dh.set(private);
    dh.pubkey().try_into().ok()
}

impl Identity {
    /// A fresh random key pair.
    pub fn generate() -> std::io::Result<Self> {
        loop {
            let private = crate::random::<KEY_LEN>()?;
            if let Some(public) = derive_public(&private) {
                return Ok(Self { private, public });
            }
        }
    }

    pub fn public(&self) -> &[u8; KEY_LEN] {
        &self.public
    }

    pub(crate) fn private(&self) -> &[u8; KEY_LEN] {
        &self.private
    }

    /// As a host: the id the offer carries and the prologue binds.
    pub fn host_id(&self) -> String {
        host_id(&self.public)
    }

    /// As a client device: the id the host lists it under.
    pub fn device_id(&self) -> String {
        device_id(&self.public)
    }

    /// Loads the key at `path`, or makes and saves one when there is no file. A file that exists
    /// but is damaged is an error, never replaced: a new key would silently unpair everything.
    pub fn load_or_create(path: &Path) -> std::io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is damaged; move it aside to start over (every device must then pair again)", path.display()))
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let id = Self::generate()?;
                crate::fsperm::write_private(path, id.to_json().as_bytes())?;
                Ok(id)
            }
            Err(e) => Err(e),
        }
    }

    fn to_json(&self) -> String {
        serde_json::to_string_pretty(&File { v: 1, private: b64_encode(&self.private), public: b64_encode(&self.public) }).unwrap_or_default()
    }

    fn parse(text: &str) -> Option<Self> {
        let f: File = serde_json::from_str(text).ok()?;
        if f.v != 1 {
            return None;
        }
        let private = b64_decode_fixed::<KEY_LEN>(&f.private)?;
        let public = b64_decode_fixed::<KEY_LEN>(&f.public)?;
        (derive_public(&private)? == public).then_some(Self { private, public })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_keys_differ_and_ids_are_valid() {
        let (a, b) = (Identity::generate().unwrap(), Identity::generate().unwrap());
        assert_ne!(a.public(), b.public());
        assert!(csm_peer_proto::ids::is_valid_id(&a.host_id()));
        assert_ne!(a.host_id(), a.device_id());
    }

    #[test]
    fn saved_key_loads_back_and_a_damaged_one_is_refused_not_replaced() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("host.key");
        let first = Identity::load_or_create(&p).unwrap();
        assert_eq!(Identity::load_or_create(&p).unwrap(), first);
        assert!(!std::fs::read_to_string(&p).unwrap().is_empty());
        std::fs::write(&p, "{ not json").unwrap();
        assert!(Identity::load_or_create(&p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{ not json", "left alone");
        // A public key that does not belong to the private one is damage too.
        let other = Identity::generate().unwrap();
        let tampered = File { v: 1, private: b64_encode(first.private()), public: b64_encode(other.public()) };
        std::fs::write(&p, serde_json::to_string(&tampered).unwrap()).unwrap();
        assert!(Identity::load_or_create(&p).is_err());
    }

    #[test]
    fn debug_does_not_show_the_private_key() {
        let a = Identity::generate().unwrap();
        assert!(!format!("{a:?}").contains(&b64_encode(a.private())));
    }
}
