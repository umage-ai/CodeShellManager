//! The device list: who may connect. Public keys only. The file is untrusted input on load (a
//! person or a tool may have edited it): every entry is checked, and a bad one is dropped.

use std::path::{Path, PathBuf};

use csm_peer_proto::ids::{KEY_LEN, b64_decode_fixed, b64_encode, device_id, is_valid_id};
use csm_peer_proto::msg::{Level, valid_name};
use serde::{Deserialize, Serialize};

/// Most devices one host keeps.
pub const MAX_DEVICES: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub id: String,
    pub name: String,
    /// base64url of the device's static public key.
    pub public: String,
    pub level: Level,
    pub paired_at: u64,
    #[serde(default)]
    pub last_seen: Option<u64>,
    /// The person chose "no prompts" for this device: its calls that type into a session or start
    /// a process run without a consent card. A record without the field (paired before this
    /// existed) is trusted, so an existing pairing keeps working; a new pairing is not, unless
    /// the person ticked the box.
    #[serde(default = "trusted_by_default")]
    pub trusted: bool,
}

fn trusted_by_default() -> bool {
    true
}

impl Device {
    pub fn public_key(&self) -> Option<[u8; KEY_LEN]> {
        b64_decode_fixed(&self.public)
    }

    /// An entry that passes every check: a name, a key, and the id the key gives.
    fn sound(&self) -> bool {
        valid_name(&self.name) && is_valid_id(&self.id) && self.public_key().is_some_and(|k| device_id(&k) == self.id)
    }
}

pub fn level_name(l: Level) -> &'static str {
    match l {
        Level::Viewer => "viewer",
        Level::Operator => "operator",
        Level::Full => "full",
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    v: u8,
    devices: Vec<Device>,
}

/// The list and its file.
#[derive(Debug, Clone)]
pub struct DeviceStore {
    path: PathBuf,
    devices: Vec<Device>,
}

impl DeviceStore {
    /// Loads `path`. A missing file is an empty list; an unreadable or unparsable one is an error
    /// (it is not replaced: that would drop every pairing without anyone deciding to).
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let devices = match std::fs::read_to_string(path) {
            Ok(text) => {
                let f: File = serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is damaged: {e}", path.display())))?;
                if f.v != 1 {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is a newer version", path.display())));
                }
                let before = f.devices.len();
                let mut kept: Vec<Device> = Vec::new();
                for d in f.devices {
                    if d.sound() && !kept.iter().any(|k| k.id == d.id) && kept.len() < MAX_DEVICES {
                        kept.push(d);
                    }
                }
                if kept.len() != before {
                    log::warn!("remote access: dropped {} unusable entries from {}", before - kept.len(), path.display());
                }
                kept
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path: path.to_owned(), devices })
    }

    pub fn list(&self) -> &[Device] {
        &self.devices
    }

    pub fn find_by_key(&self, key: &[u8; KEY_LEN]) -> Option<&Device> {
        let want = b64_encode(key);
        self.devices.iter().find(|d| d.public == want)
    }

    pub fn find(&self, id: &str) -> Option<&Device> {
        self.devices.iter().find(|d| d.id == id)
    }

    /// Adds a device (or replaces the one with the same key: pairing again renames and renews it).
    pub fn add(&mut self, name: &str, key: &[u8; KEY_LEN], level: Level, trusted: bool, now: u64) -> std::io::Result<Device> {
        let id = device_id(key);
        let d = Device { id: id.clone(), name: name.trim().to_owned(), public: b64_encode(key), level, paired_at: now, last_seen: None, trusted };
        let mut next = self.devices.clone();
        next.retain(|x| x.id != id);
        if next.len() >= MAX_DEVICES {
            return Err(std::io::Error::other(format!("{MAX_DEVICES} devices are paired already; revoke one first")));
        }
        next.push(d.clone());
        self.save_list(&next)?;
        self.devices = next;
        Ok(d)
    }

    /// Sets whether a device is trusted; `false` if there is no such device.
    pub fn set_trusted(&mut self, id: &str, trusted: bool) -> std::io::Result<bool> {
        let mut next = self.devices.clone();
        let Some(d) = next.iter_mut().find(|d| d.id == id) else { return Ok(false) };
        d.trusted = trusted;
        self.save_list(&next)?;
        self.devices = next;
        Ok(true)
    }

    /// Removes one device; `true` if it was there.
    pub fn remove(&mut self, id: &str) -> std::io::Result<bool> {
        let next: Vec<Device> = self.devices.iter().filter(|d| d.id != id).cloned().collect();
        if next.len() == self.devices.len() {
            return Ok(false);
        }
        self.save_list(&next)?;
        self.devices = next;
        Ok(true)
    }

    /// Removes every device; how many there were.
    pub fn clear(&mut self) -> std::io::Result<usize> {
        let n = self.devices.len();
        self.save_list(&[])?;
        self.devices.clear();
        Ok(n)
    }

    /// Notes a connection. Best effort: a failed save only loses the timestamp.
    pub fn touch(&mut self, id: &str, now: u64) {
        let Some(d) = self.devices.iter_mut().find(|d| d.id == id) else { return };
        d.last_seen = Some(now);
        let snapshot = self.devices.clone();
        if let Err(e) = self.save_list(&snapshot) {
            log::warn!("remote access: could not save the device list: {e}");
        }
    }

    fn save_list(&self, list: &[Device]) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&File { v: 1, devices: list.to_vec() }).map_err(std::io::Error::other)?;
        crate::fsperm::write_private(&self.path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn add_find_remove_survive_a_reload() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("devices.json");
        let mut s = DeviceStore::load(&p).unwrap();
        assert!(s.list().is_empty());
        let a = s.add("Laptop", &[1; 32], Level::Full, true, 100).unwrap();
        s.add("Phone", &[2; 32], Level::Full, true, 101).unwrap();
        assert_eq!(s.find_by_key(&[1; 32]).unwrap().id, a.id);
        s.touch(&a.id, 200);
        let s2 = DeviceStore::load(&p).unwrap();
        assert_eq!(s2.list().len(), 2);
        assert_eq!(s2.find(&a.id).unwrap().last_seen, Some(200));
        let mut s2 = s2;
        assert!(s2.remove(&a.id).unwrap());
        assert!(!s2.remove(&a.id).unwrap());
        assert_eq!(DeviceStore::load(&p).unwrap().list().len(), 1);
        assert_eq!(s2.clear().unwrap(), 1);
        assert!(DeviceStore::load(&p).unwrap().list().is_empty());
    }

    #[test]
    fn a_record_without_the_trusted_field_is_trusted() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("d.json");
        let key = [7u8; 32];
        let old = format!(r#"{{"v":1,"devices":[{{"id":"{}","name":"Laptop","public":"{}","level":"full","paired_at":5}}]}}"#, device_id(&key), b64_encode(&key));
        std::fs::write(&p, old).unwrap();
        let mut s = DeviceStore::load(&p).unwrap();
        assert!(s.list()[0].trusted);
        let id = s.list()[0].id.clone();
        assert!(s.set_trusted(&id, false).unwrap());
        assert!(!DeviceStore::load(&p).unwrap().list()[0].trusted);
        assert!(!s.set_trusted("nope", true).unwrap());
        let added = s.add("New", &[8; 32], Level::Full, false, 9).unwrap();
        assert!(!DeviceStore::load(&p).unwrap().find(&added.id).unwrap().trusted);
    }

    #[test]
    fn pairing_the_same_key_again_replaces_it() {
        let d = tempfile::tempdir().unwrap();
        let mut s = DeviceStore::load(&d.path().join("d.json")).unwrap();
        s.add("Old", &[1; 32], Level::Full, true, 1).unwrap();
        s.add("New", &[1; 32], Level::Full, true, 2).unwrap();
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.list()[0].name, "New");
    }

    #[test]
    fn a_hand_edited_file_cannot_smuggle_a_bad_entry() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("d.json");
        let good = Device { id: device_id(&[3; 32]), name: "Good".into(), public: b64_encode(&[3; 32]), level: Level::Full, paired_at: 1, last_seen: None, trusted: true };
        let wrong_id = Device { id: device_id(&[9; 32]), name: "Liar".into(), public: b64_encode(&[4; 32]), level: Level::Full, paired_at: 1, last_seen: None, trusted: true };
        let bad_name = Device { id: device_id(&[5; 32]), name: "a\u{202E}b".into(), public: b64_encode(&[5; 32]), level: Level::Full, paired_at: 1, last_seen: None, trusted: true };
        let dup = good.clone();
        std::fs::write(&p, serde_json::to_string(&File { v: 1, devices: vec![good.clone(), wrong_id, bad_name, dup] }).unwrap()).unwrap();
        let s = DeviceStore::load(&p).unwrap();
        assert_eq!(s.list(), &[good]);
        std::fs::write(&p, "nope").unwrap();
        assert!(DeviceStore::load(&p).is_err());
    }

    #[test]
    fn the_list_has_a_cap() {
        let d = tempfile::tempdir().unwrap();
        let mut s = DeviceStore::load(&d.path().join("d.json")).unwrap();
        for i in 0..MAX_DEVICES {
            s.add("D", &[u8::try_from(i).unwrap(); 32], Level::Full, true, 1).unwrap();
        }
        assert!(s.add("One more", &[200; 32], Level::Full, true, 1).is_err());
    }
}
