//! Explorer's "Open in CodeShellManager" right-click entry: the per-user off switch.
//!
//! The entry itself is written by the MSI, per machine (`installer/CodeShellManager2.wxs`):
//! `HKLM\Software\Classes\{Directory\shell, Directory\Background\shell, Drive\shell}\<verb>` with a
//! `command` of `"<exe>" --open-folder "%V"`. Nothing in the app writes or removes those keys, so
//! the uninstaller removes exactly what the installer made.
//!
//! "Show it in Explorer's menu" is the person's own choice, so it lives in their own hive: when
//! off, the same three key paths under `HKCU\Software\Classes` get the string value
//! [`DISABLE_VALUE`] (`LegacyDisable`), which makes Explorer skip the verb; HKCU is merged over HKLM
//! in the classes view. When on, that value is removed, and the keys with it when nothing else is
//! in them (a key the person or another program put there is never deleted).
//!
//! The verb's key name is per channel ([`verb_name`]) so a stable and a beta install each have
//! their own entry and their own switch.
//!
//! The registry is behind [`Store`], so tests never touch the real one. These functions do I/O:
//! call them from the engine or a worker, never from frame code.

use std::io;
use std::sync::Mutex;

use csm_core::channel::Channel;
use serde::Serialize;

/// The value that hides a verb from Explorer's menu.
pub const DISABLE_VALUE: &str = "LegacyDisable";

/// The three places Explorer looks for verbs on folders, relative to `Software\Classes`: a folder
/// itself, the empty space inside an open folder (`%V` is that folder), and a drive.
pub const CLASS_KEYS: [&str; 3] = [r"Directory\shell", r"Directory\Background\shell", r"Drive\shell"];

/// The verb's key name for a channel.
pub const fn verb_name(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "CodeShellManager",
        Channel::Beta => "CodeShellManagerBeta",
    }
}

/// The text Explorer shows for a channel.
pub const fn verb_text(channel: Channel) -> &'static str {
    match channel {
        Channel::Stable => "Open in CodeShellManager",
        Channel::Beta => "Open in CodeShellManager (Beta)",
    }
}

/// The three verb keys of a channel, relative to `Software\Classes`.
pub fn verb_keys(channel: Channel) -> Vec<String> {
    CLASS_KEYS.iter().map(|k| format!(r"{k}\{}", verb_name(channel))).collect()
}

/// What the registry says.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Status {
    /// This system has an Explorer menu this app can manage (Windows, with a registry).
    pub supported: bool,
    /// The installer's entry is there (an MSI install; a portable zip or a development build has none).
    pub installed: bool,
    /// The person has not hidden it: none of the three user-level keys says `LegacyDisable`.
    pub shown: bool,
    /// The verb's text, for messages.
    pub text: String,
    /// Where the switch lives, for messages.
    pub location: String,
}

/// The classes registry, as far as the switch needs it. Keys are relative to `Software\Classes`.
pub trait Store: Send + Sync {
    /// A string value in the current user's classes; `None` when the key or value is not there.
    fn user_get(&self, key: &str, name: &str) -> io::Result<Option<String>>;
    /// Writes a `REG_SZ` value in the current user's classes, creating the key.
    fn user_set(&self, key: &str, name: &str, value: &str) -> io::Result<()>;
    /// Deletes a value; no error when it is not there.
    fn user_delete_value(&self, key: &str, name: &str) -> io::Result<()>;
    /// Deletes the current user's key when it has no values and no subkeys. True when it was
    /// deleted; false when it was not there or still holds something.
    fn user_prune(&self, key: &str) -> io::Result<bool>;
    /// Whether the machine-wide key exists (the installer's).
    fn machine_has(&self, key: &str) -> io::Result<bool>;
    /// Where the user-level keys are, for messages.
    fn location(&self) -> String;
}

/// What the registry says for `channel`.
pub fn status(store: &dyn Store, channel: Channel) -> io::Result<Status> {
    let keys = verb_keys(channel);
    let installed = match keys.first() {
        Some(k) => store.machine_has(k)?,
        None => false,
    };
    let mut shown = true;
    for k in &keys {
        if store.user_get(k, DISABLE_VALUE)?.is_some() {
            shown = false;
        }
    }
    Ok(Status { supported: true, installed, shown, text: verb_text(channel).to_string(), location: store.location() })
}

/// Shows (`true`) or hides (`false`) the entry for the current user, and returns what the registry
/// says afterwards. Hiding writes the value in all three keys; showing removes it, and removes the
/// keys it leaves empty.
pub fn set(store: &dyn Store, channel: Channel, shown: bool) -> io::Result<Status> {
    for key in verb_keys(channel) {
        if shown {
            store.user_delete_value(&key, DISABLE_VALUE)?;
            prune_up(store, &key)?;
        } else {
            store.user_set(&key, DISABLE_VALUE, "")?;
        }
    }
    status(store, channel)
}

/// Deletes `key` and then each parent that this left empty (up to the class root, `Directory` or
/// `Drive`, itself), and stops at the first key that still holds something.
fn prune_up(store: &dyn Store, key: &str) -> io::Result<()> {
    let mut path = key.to_string();
    loop {
        if !store.user_prune(&path)? {
            return Ok(());
        }
        match path.rsplit_once('\\') {
            Some((parent, _)) => path = parent.to_string(),
            None => return Ok(()),
        }
    }
}

/// This system's registry, over the real hives. `None` where there is none (not Windows).
pub fn system() -> Option<Box<dyn Store>> {
    #[cfg(windows)]
    {
        Some(Box::new(Registry::system()))
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// ── In memory ──────────────────────────────────────────────────────────────────────────────────

/// A registry in memory: tests, and the default of an engine that must not touch the real one.
#[derive(Debug, Default)]
pub struct MemoryStore {
    inner: Mutex<Memory>,
}

#[derive(Debug, Default)]
struct Memory {
    /// User keys (lower-cased paths), each with its values.
    user: std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    machine: std::collections::BTreeSet<String>,
}

impl MemoryStore {
    fn lock(&self) -> std::sync::MutexGuard<'_, Memory> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What the installer wrote: the machine-wide keys of `channel`.
    pub fn install(&self, channel: Channel) {
        let mut m = self.lock();
        for k in verb_keys(channel) {
            m.machine.insert(k.to_lowercase());
        }
    }

    /// A value a program other than this one put in the user's classes (or the value this one wrote).
    pub fn put_user(&self, key: &str, name: &str, value: &str) {
        let mut m = self.lock();
        let mut path = String::new();
        for part in key.to_lowercase().split('\\') {
            if !path.is_empty() {
                path.push('\\');
            }
            path.push_str(part);
            m.user.entry(path.clone()).or_default();
        }
        m.user.entry(key.to_lowercase()).or_default().insert(name.to_lowercase(), value.to_string());
    }

    /// The user's key paths now, sorted (lower-case).
    pub fn user_keys(&self) -> Vec<String> {
        self.lock().user.keys().cloned().collect()
    }

    pub fn user_value(&self, key: &str, name: &str) -> Option<String> {
        self.lock().user.get(&key.to_lowercase()).and_then(|v| v.get(&name.to_lowercase())).cloned()
    }
}

impl Store for MemoryStore {
    fn user_get(&self, key: &str, name: &str) -> io::Result<Option<String>> {
        Ok(self.user_value(key, name))
    }

    fn user_set(&self, key: &str, name: &str, value: &str) -> io::Result<()> {
        self.put_user(key, name, value);
        Ok(())
    }

    fn user_delete_value(&self, key: &str, name: &str) -> io::Result<()> {
        if let Some(v) = self.lock().user.get_mut(&key.to_lowercase()) {
            v.remove(&name.to_lowercase());
        }
        Ok(())
    }

    fn user_prune(&self, key: &str) -> io::Result<bool> {
        let key = key.to_lowercase();
        let mut m = self.lock();
        let child = format!(r"{key}\");
        let empty = m.user.get(&key).is_some_and(std::collections::BTreeMap::is_empty) && !m.user.keys().any(|k| k.starts_with(&child));
        if empty {
            m.user.remove(&key);
        }
        Ok(empty)
    }

    fn machine_has(&self, key: &str) -> io::Result<bool> {
        Ok(self.lock().machine.contains(&key.to_lowercase()))
    }

    fn location(&self) -> String {
        r"HKEY_CURRENT_USER\Software\Classes".to_string()
    }
}

// ── Windows ────────────────────────────────────────────────────────────────────────────────────

/// The real registry: `HKCU\<user_base>` for the person's keys, `HKLM\<machine_base>` for the
/// installer's. [`Registry::system`] uses `Software\Classes` for both; tests give a scratch key.
#[cfg(windows)]
pub struct Registry {
    user_base: String,
    machine_base: String,
    /// Read the installer's side from HKCU instead of HKLM (tests only: nothing under HKLM is touched).
    machine_in_user_hive: bool,
}

#[cfg(windows)]
impl Registry {
    pub fn system() -> Self {
        Registry { user_base: r"Software\Classes".into(), machine_base: r"Software\Classes".into(), machine_in_user_hive: false }
    }

    /// Both bases under the current user, for a test (`HKCU\Software\umage-csm-test-<random>`):
    /// the machine side is HKCU too, so a test never reads or writes HKLM.
    pub fn scratch(user_base: &str, machine_base: &str) -> Self {
        Registry { user_base: user_base.into(), machine_base: machine_base.into(), machine_in_user_hive: true }
    }

    fn user_path(&self, key: &str) -> String {
        format!(r"{}\{key}", self.user_base)
    }
}

/// `Ok(None)` for a key that is not there.
#[cfg(windows)]
fn open_or_none(root: &winreg::RegKey, path: &str, flags: u32) -> io::Result<Option<winreg::RegKey>> {
    match root.open_subkey_with_flags(path, flags) {
        Ok(k) => Ok(Some(k)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(windows)]
impl Store for Registry {
    fn user_get(&self, key: &str, name: &str) -> io::Result<Option<String>> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        let root = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let Some(k) = open_or_none(&root, &self.user_path(key), KEY_READ)? else { return Ok(None) };
        match k.get_value::<String, _>(name) {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn user_set(&self, key: &str, name: &str, value: &str) -> io::Result<()> {
        use winreg::enums::HKEY_CURRENT_USER;
        let (k, _) = winreg::RegKey::predef(HKEY_CURRENT_USER).create_subkey(self.user_path(key))?;
        k.set_value(name, &value.to_string())
    }

    fn user_delete_value(&self, key: &str, name: &str) -> io::Result<()> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_SET_VALUE};
        let root = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let Some(k) = open_or_none(&root, &self.user_path(key), KEY_SET_VALUE)? else { return Ok(()) };
        match k.delete_value(name) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    fn user_prune(&self, key: &str) -> io::Result<bool> {
        use winreg::enums::{HKEY_CURRENT_USER, KEY_READ};
        let root = winreg::RegKey::predef(HKEY_CURRENT_USER);
        let path = self.user_path(key);
        let Some(k) = open_or_none(&root, &path, KEY_READ)? else { return Ok(false) };
        let info = k.query_info()?;
        if info.sub_keys != 0 || info.values != 0 {
            return Ok(false);
        }
        drop(k);
        match root.delete_subkey(&path) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn machine_has(&self, key: &str) -> io::Result<bool> {
        use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ};
        let root = winreg::RegKey::predef(if self.machine_in_user_hive { HKEY_CURRENT_USER } else { HKEY_LOCAL_MACHINE });
        Ok(open_or_none(&root, &format!(r"{}\{key}", self.machine_base), KEY_READ)?.is_some())
    }

    fn location(&self) -> String {
        format!(r"HKEY_CURRENT_USER\{}", self.user_base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STABLE: Channel = Channel::Stable;
    const BETA: Channel = Channel::Beta;

    #[test]
    fn each_channel_has_its_own_three_keys_and_text() {
        assert_eq!(
            verb_keys(STABLE),
            [r"Directory\shell\CodeShellManager", r"Directory\Background\shell\CodeShellManager", r"Drive\shell\CodeShellManager"]
        );
        assert_eq!(
            verb_keys(BETA),
            [r"Directory\shell\CodeShellManagerBeta", r"Directory\Background\shell\CodeShellManagerBeta", r"Drive\shell\CodeShellManagerBeta"]
        );
        assert_eq!(verb_text(STABLE), "Open in CodeShellManager");
        assert_eq!(verb_text(BETA), "Open in CodeShellManager (Beta)");
    }

    #[test]
    fn hiding_writes_legacy_disable_in_all_three_user_keys_and_showing_removes_every_trace() {
        let s = MemoryStore::default();
        s.install(STABLE);
        let st = status(&s, STABLE).unwrap();
        assert!(st.installed && st.shown && st.supported);
        assert!(s.user_keys().is_empty(), "reading writes nothing");

        let st = set(&s, STABLE, false).unwrap();
        assert!(!st.shown && st.installed);
        for k in verb_keys(STABLE) {
            assert_eq!(s.user_value(&k, DISABLE_VALUE).as_deref(), Some(""), "{k}");
        }
        // Idempotent.
        assert!(!set(&s, STABLE, false).unwrap().shown);

        let st = set(&s, STABLE, true).unwrap();
        assert!(st.shown);
        assert!(s.user_keys().is_empty(), "left behind: {:?}", s.user_keys());
        assert!(set(&s, STABLE, true).unwrap().shown, "showing twice is fine");
    }

    #[test]
    fn showing_leaves_alone_what_something_else_put_in_those_keys() {
        let s = MemoryStore::default();
        s.install(STABLE);
        set(&s, STABLE, false).unwrap();
        // Another program's value in the verb key, and a sibling verb under the same `shell`.
        s.put_user(r"Directory\shell\CodeShellManager", "MUIVerb", "mine");
        s.put_user(r"Directory\Background\shell\Other", "x", "y");
        set(&s, STABLE, true).unwrap();
        assert_eq!(s.user_value(r"Directory\shell\CodeShellManager", "MUIVerb").as_deref(), Some("mine"));
        assert!(s.user_value(r"Directory\shell\CodeShellManager", DISABLE_VALUE).is_none());
        assert_eq!(s.user_value(r"Directory\Background\shell\Other", "x").as_deref(), Some("y"));
        let keys = s.user_keys();
        assert!(keys.contains(&r"directory\background\shell".to_string()), "{keys:?}");
        assert!(!keys.contains(&r"drive".to_string()) && !keys.contains(&r"drive\shell\codeshellmanager".to_string()), "{keys:?}");
    }

    #[test]
    fn a_beta_switch_does_not_touch_the_stable_entry_and_the_reverse() {
        let s = MemoryStore::default();
        s.install(STABLE);
        s.install(BETA);
        set(&s, BETA, false).unwrap();
        assert!(!status(&s, BETA).unwrap().shown);
        assert!(status(&s, STABLE).unwrap().shown);
        set(&s, STABLE, false).unwrap();
        set(&s, BETA, true).unwrap();
        assert!(status(&s, BETA).unwrap().shown);
        assert!(!status(&s, STABLE).unwrap().shown);
    }

    #[test]
    fn a_build_without_the_installers_entry_says_so() {
        let s = MemoryStore::default();
        assert!(!status(&s, STABLE).unwrap().installed);
    }

    /// The real registry code, against a scratch key under HKCU that the test creates and deletes.
    /// Never HKLM, never `Software\Classes`.
    #[cfg(windows)]
    #[test]
    fn the_registry_writer_hides_and_shows_under_a_scratch_key() {
        use winreg::RegKey;
        use winreg::enums::HKEY_CURRENT_USER;

        struct Scratch(String);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = RegKey::predef(HKEY_CURRENT_USER).delete_subkey_all(&self.0);
            }
        }
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.subsec_nanos()).unwrap_or(0);
        let root = format!(r"Software\umage-csm-test-{}-{nanos}", std::process::id());
        let scratch = Scratch(root.clone());
        assert!(!scratch.0.is_empty() && scratch.0.starts_with(r"Software\umage-csm-test-"));
        let reg = Registry::scratch(&format!(r"{root}\user"), &format!(r"{root}\machine"));

        // The "installer" side is a key in the scratch machine base.
        assert!(!status(&reg, STABLE).unwrap().installed);
        RegKey::predef(HKEY_CURRENT_USER).create_subkey(format!(r"{root}\machine\Directory\shell\CodeShellManager\command")).unwrap();
        let st = status(&reg, STABLE).unwrap();
        assert!(st.installed && st.shown);

        let st = set(&reg, STABLE, false).unwrap();
        assert!(!st.shown);
        for k in verb_keys(STABLE) {
            assert_eq!(reg.user_get(&k, DISABLE_VALUE).unwrap().as_deref(), Some(""), "{k}");
        }
        // The raw value is a REG_SZ named LegacyDisable on the real key path.
        let raw = RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!(r"{root}\user\Directory\Background\shell\CodeShellManager")).unwrap();
        let v: String = raw.get_value("LegacyDisable").unwrap();
        assert_eq!(v, "");

        // Something else in a key keeps that key (and its parents) on showing.
        reg.user_set(r"Drive\shell\CodeShellManager", "Other", "keep").unwrap();
        let st = set(&reg, STABLE, true).unwrap();
        assert!(st.shown);
        assert_eq!(reg.user_get(r"Drive\shell\CodeShellManager", "Other").unwrap().as_deref(), Some("keep"));
        assert!(reg.user_get(r"Directory\shell\CodeShellManager", DISABLE_VALUE).unwrap().is_none());
        let gone = RegKey::predef(HKEY_CURRENT_USER).open_subkey(format!(r"{root}\user\Directory"));
        assert!(gone.is_err(), "the emptied Directory branch is removed");

        // A missing key reads as not hidden, and pruning it is not an error.
        assert!(!reg.user_prune(r"Nothing\here").unwrap());
        drop(scratch);
        assert!(RegKey::predef(HKEY_CURRENT_USER).open_subkey(&root).is_err(), "the scratch key is gone");
    }
}
