//! Secrets of process add-ons, kept in the OS keychain (Windows Credential Manager, macOS
//! Keychain, the Secret Service on Linux) and never written to `state.json`.
//!
//! The store is a trait so the engine's tests run against memory and never touch the real
//! keychain. Every call blocks (the keychain may prompt or wait), so callers run it on a worker
//! thread. A value is untrusted text from an add-on: the callers cap its size and key shape
//! ([`valid_key`], [`MAX_VALUE_BYTES`]) before it gets here.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

/// Longest secret value, in bytes. Windows Credential Manager keeps at most 2560 bytes per
/// credential, so this stays under it.
pub const MAX_VALUE_BYTES: usize = 2048;
/// Longest key, in bytes.
pub const MAX_KEY_BYTES: usize = 64;

/// Whether `key` is a legal secret name: lowercase letters, digits and `. _ -`, not empty.
#[must_use]
pub fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= MAX_KEY_BYTES && key.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-_".contains(&b))
}

/// The keychain service name of an add-on's secrets. One service per add-on id, so an add-on can
/// only ever name its own: the id comes from who is asking, never from a parameter.
#[must_use]
pub fn service_for(addon: &str) -> String {
    format!("codeshell-addon:{addon}")
}

/// A place that keeps small secrets by `(service, key)`.
pub trait SecretStore: Send + Sync {
    /// The value, or `None` when there is none.
    ///
    /// # Errors
    /// The store could not be reached (no keychain, locked, denied), in plain words.
    fn get(&self, service: &str, key: &str) -> Result<Option<String>, String>;
    /// Stores `value`, replacing any earlier one.
    ///
    /// # Errors
    /// As [`SecretStore::get`].
    fn set(&self, service: &str, key: &str, value: &str) -> Result<(), String>;
    /// Removes the value; `false` when there was none.
    ///
    /// # Errors
    /// As [`SecretStore::get`].
    fn delete(&self, service: &str, key: &str) -> Result<bool, String>;
}

/// The OS keychain.
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

fn entry(service: &str, key: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(service, key).map_err(describe)
}

fn describe(e: keyring::Error) -> String {
    format!("the OS keychain is not available: {e}")
}

impl SecretStore for KeyringStore {
    fn get(&self, service: &str, key: &str) -> Result<Option<String>, String> {
        match entry(service, key)?.get_password() {
            Ok(v) => Ok(Some(v)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(describe(e)),
        }
    }

    fn set(&self, service: &str, key: &str, value: &str) -> Result<(), String> {
        entry(service, key)?.set_password(value).map_err(describe)
    }

    fn delete(&self, service: &str, key: &str) -> Result<bool, String> {
        match entry(service, key)?.delete_credential() {
            Ok(()) => Ok(true),
            Err(keyring::Error::NoEntry) => Ok(false),
            Err(e) => Err(describe(e)),
        }
    }
}

/// Memory only: for tests, and for runs that must not touch the keychain.
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<HashMap<(String, String), String>>);

impl SecretStore for MemoryStore {
    fn get(&self, service: &str, key: &str) -> Result<Option<String>, String> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).get(&(service.to_owned(), key.to_owned())).cloned())
    }

    fn set(&self, service: &str, key: &str, value: &str) -> Result<(), String> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).insert((service.to_owned(), key.to_owned()), value.to_owned());
        Ok(())
    }

    fn delete(&self, service: &str, key: &str) -> Result<bool, String> {
        Ok(self.0.lock().unwrap_or_else(PoisonError::into_inner).remove(&(service.to_owned(), key.to_owned())).is_some())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_a_small_alphabet() {
        assert!(valid_key("ssh-key.path_1"));
        for bad in ["", "A", "a b", "a/b", "a\0", &"x".repeat(MAX_KEY_BYTES + 1)] {
            assert!(!valid_key(bad), "{bad:?}");
        }
    }

    #[test]
    fn each_addon_has_a_service_of_its_own() {
        assert_ne!(service_for("a"), service_for("b"));
        let m = MemoryStore::default();
        m.set(&service_for("a"), "token", "1").unwrap();
        assert_eq!(m.get(&service_for("a"), "token").unwrap().as_deref(), Some("1"));
        assert_eq!(m.get(&service_for("b"), "token").unwrap(), None);
        assert!(m.delete(&service_for("a"), "token").unwrap());
        assert!(!m.delete(&service_for("a"), "token").unwrap());
    }
}
