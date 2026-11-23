//! The hosts this app is paired with, on the laptop side: each one's id, name, address, the host
//! public key pinned at pairing, and when it was paired and last connected to.
//!
//! The file is untrusted input on load (a person may have edited it): every entry is checked (the
//! host id must be the one its key gives, the address must parse, the name must be a valid name),
//! and a bad entry is dropped. A damaged file is an error and is never replaced: a new empty list
//! would silently forget every host.

use std::path::{Path, PathBuf};

use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::ids::{KEY_LEN, b64_decode_fixed, b64_encode, host_id, is_valid_id};
use csm_peer_proto::msg::valid_name;
use serde::{Deserialize, Serialize};

/// Most hosts one app keeps.
pub const MAX_HOSTS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Host {
    pub host_id: String,
    pub name: String,
    /// `tcp://10.8.0.2:47300`.
    pub address: String,
    /// base64url of the host's static public key, pinned at pairing.
    pub public: String,
    pub paired_at: u64,
    #[serde(default)]
    pub last_connected: Option<u64>,
}

impl Host {
    pub fn public_key(&self) -> Option<[u8; KEY_LEN]> {
        b64_decode_fixed(&self.public)
    }

    pub fn peer_addr(&self) -> Option<PeerAddr> {
        PeerAddr::parse(&self.address).ok()
    }

    /// An entry that passes every check: a name, an address, and a key whose host id is the one listed.
    fn sound(&self) -> bool {
        valid_name(&self.name) && is_valid_id(&self.host_id) && self.peer_addr().is_some() && self.public_key().is_some_and(|k| host_id(&k) == self.host_id)
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    v: u8,
    hosts: Vec<Host>,
}

/// The list and its file.
#[derive(Debug, Clone)]
pub struct HostStore {
    path: PathBuf,
    hosts: Vec<Host>,
}

impl HostStore {
    /// Loads `path`. A missing file is an empty list; an unreadable or unparsable one is an error.
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let hosts = match std::fs::read_to_string(path) {
            Ok(text) => {
                let f: File = serde_json::from_str(&text).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is damaged: {e}", path.display())))?;
                if f.v != 1 {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{} is a newer version", path.display())));
                }
                let before = f.hosts.len();
                let mut kept: Vec<Host> = Vec::new();
                for h in f.hosts {
                    if h.sound() && !kept.iter().any(|k| k.host_id == h.host_id) && kept.len() < MAX_HOSTS {
                        kept.push(h);
                    }
                }
                if kept.len() != before {
                    log::warn!("remote hosts: dropped {} unusable entries from {}", before - kept.len(), path.display());
                }
                kept
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e),
        };
        Ok(Self { path: path.to_owned(), hosts })
    }

    pub fn list(&self) -> &[Host] {
        &self.hosts
    }

    pub fn find(&self, id: &str) -> Option<&Host> {
        self.hosts.iter().find(|h| h.host_id == id)
    }

    /// Stores a host after a pairing. Pairing with the same host again replaces its entry.
    pub fn add(&mut self, name: &str, address: &PeerAddr, key: &[u8; KEY_LEN], now: u64) -> std::io::Result<Host> {
        let id = host_id(key);
        let h = Host { host_id: id.clone(), name: name.trim().to_owned(), address: address.to_url(), public: b64_encode(key), paired_at: now, last_connected: None };
        if !h.sound() {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "the host's name or address is not usable"));
        }
        let mut next: Vec<Host> = self.hosts.iter().filter(|x| x.host_id != id).cloned().collect();
        if next.len() >= MAX_HOSTS {
            return Err(std::io::Error::other(format!("{MAX_HOSTS} hosts are stored already; forget one first")));
        }
        next.push(h.clone());
        self.save_list(&next)?;
        self.hosts = next;
        Ok(h)
    }

    /// Removes one host and its pinned key; `true` if it was there.
    pub fn remove(&mut self, id: &str) -> std::io::Result<bool> {
        let next: Vec<Host> = self.hosts.iter().filter(|h| h.host_id != id).cloned().collect();
        if next.len() == self.hosts.len() {
            return Ok(false);
        }
        self.save_list(&next)?;
        self.hosts = next;
        Ok(true)
    }

    /// Notes a connection. `false` if no such host.
    pub fn touch(&mut self, id: &str, now: u64) -> std::io::Result<bool> {
        let mut next = self.hosts.clone();
        let Some(h) = next.iter_mut().find(|h| h.host_id == id) else { return Ok(false) };
        h.last_connected = Some(now);
        self.save_list(&next)?;
        self.hosts = next;
        Ok(true)
    }

    fn save_list(&self, list: &[Host]) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(&File { v: 1, hosts: list.to_vec() }).map_err(std::io::Error::other)?;
        crate::fsperm::write_private(&self.path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> PeerAddr {
        PeerAddr::parse(s).unwrap()
    }

    #[test]
    fn add_find_touch_remove_survive_a_reload() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("hosts.json");
        let mut s = HostStore::load(&p).unwrap();
        assert!(s.list().is_empty());
        let h = s.add("Office", &addr("tcp://10.8.0.2:47300"), &[5; 32], 100).unwrap();
        s.add("Shed", &addr("tcp://10.8.0.9:47300"), &[6; 32], 101).unwrap();
        assert_eq!(s.find(&h.host_id).unwrap().name, "Office");
        assert!(s.touch(&h.host_id, 200).unwrap());
        assert!(!s.touch("nobody", 200).unwrap());
        let s2 = HostStore::load(&p).unwrap();
        assert_eq!(s2.list().len(), 2);
        let back = s2.find(&h.host_id).unwrap();
        assert_eq!(back.last_connected, Some(200));
        assert_eq!(back.public_key(), Some([5; 32]));
        assert_eq!(back.peer_addr().unwrap().to_url(), "tcp://10.8.0.2:47300");
        let mut s2 = s2;
        assert!(s2.remove(&h.host_id).unwrap());
        assert!(!s2.remove(&h.host_id).unwrap());
        assert_eq!(HostStore::load(&p).unwrap().list().len(), 1);
    }

    #[test]
    fn pairing_the_same_host_again_replaces_its_entry() {
        let d = tempfile::tempdir().unwrap();
        let mut s = HostStore::load(&d.path().join("h.json")).unwrap();
        s.add("Old", &addr("tcp://10.8.0.2:47300"), &[5; 32], 1).unwrap();
        s.add("New", &addr("tcp://10.8.0.3:47300"), &[5; 32], 2).unwrap();
        assert_eq!(s.list().len(), 1);
        assert_eq!(s.list()[0].name, "New");
        assert_eq!(s.list()[0].address, "tcp://10.8.0.3:47300");
    }

    #[test]
    fn a_hand_edited_file_cannot_smuggle_a_bad_entry() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("h.json");
        let good = Host {
            host_id: host_id(&[7; 32]),
            name: "Good".into(),
            address: "tcp://10.8.0.2:47300".into(),
            public: b64_encode(&[7; 32]),
            paired_at: 1,
            last_connected: None,
        };
        let wrong_id = Host { host_id: host_id(&[9; 32]), public: b64_encode(&[8; 32]), ..good.clone() };
        let bad_addr = Host { host_id: host_id(&[4; 32]), public: b64_encode(&[4; 32]), address: "tcp://0.0.0.0:47300".into(), ..good.clone() };
        let bad_name = Host { host_id: host_id(&[3; 32]), public: b64_encode(&[3; 32]), name: "a\u{202E}b".into(), ..good.clone() };
        let dup = good.clone();
        std::fs::write(&p, serde_json::to_string(&File { v: 1, hosts: vec![good.clone(), wrong_id, bad_addr, bad_name, dup] }).unwrap()).unwrap();
        let s = HostStore::load(&p).unwrap();
        assert_eq!(s.list(), std::slice::from_ref(&good));
        std::fs::write(&p, "not json").unwrap();
        assert!(HostStore::load(&p).is_err(), "a damaged file is an error, not an empty list");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "not json", "and it is left alone");
    }

    #[test]
    fn the_list_has_a_cap() {
        let d = tempfile::tempdir().unwrap();
        let mut s = HostStore::load(&d.path().join("h.json")).unwrap();
        for i in 0..MAX_HOSTS {
            let mut key = [0u8; 32];
            key[0] = u8::try_from(i).unwrap();
            key[1] = 1;
            s.add("H", &addr("tcp://10.8.0.2:47300"), &key, 1).unwrap();
        }
        assert!(s.add("One more", &addr("tcp://10.8.0.2:47300"), &[250; 32], 1).is_err());
    }
}
