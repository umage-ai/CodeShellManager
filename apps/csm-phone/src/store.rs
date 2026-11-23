//! The add-on's files in its data folder: the host key, the device list and the config (design
//! note, section 5). Damaged files are never replaced silently: a damaged device list is an error
//! that stops serving, and a damaged config is renamed aside before the first new write.
//!
//! Writes go to a temporary file in the same folder and are then renamed over the old file. The
//! previous device list is copied to `devices.json.bak` before each change.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use csm_remote_proto::ids::{b64_decode_fixed, b64_encode, token_hash};
use csm_remote_proto::msg::Level;
use csm_remote_proto::pairing::PairMsg;
use serde::{Deserialize, Serialize};

pub const HOST_KEY_FILE: &str = "host-key";
pub const DEVICES_FILE: &str = "devices.json";
pub const CONFIG_FILE: &str = "config.json";
/// The host secret the relay checks (it hashes to the host id). A raw file beside the host key:
/// not the OS keychain yet (see the design note, section 18.1).
pub const SECRET_FILE: &str = "relay-secret";
pub const DEVICES_VERSION: u32 = 1;
pub const MAX_DEVICES: usize = 64;

/// Web Push subscription of a device (section 9). Keys are base64url, as the phone sent them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PushSub {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

/// One paired phone. The routing token itself is never stored: only its hash (the relay's allow
/// list is built from hashes too). The phone's static public key is what its handshake must show.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Device {
    pub device_id: String,
    #[serde(default = "phone_kind")]
    pub kind: String,
    pub name: String,
    pub level: Level,
    pub token_hash: String,
    pub phone_pub: String,
    pub paired_at: u64,
    #[serde(default)]
    pub last_seen: u64,
    #[serde(default)]
    pub hide_names: bool,
    #[serde(default = "yes")]
    pub notify: bool,
    #[serde(default)]
    pub push: Option<PushSub>,
}

fn phone_kind() -> String {
    "phone".to_owned()
}

fn yes() -> bool {
    true
}

#[derive(Debug, Serialize, Deserialize)]
struct DeviceFile {
    version: u32,
    devices: Vec<Device>,
}

/// Why the data folder cannot serve. Each variant names the file and what the screen should say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    /// A file exists but cannot be used. Nothing was changed.
    Damaged { file: String, detail: String },
    /// The host key is gone but phones are paired. Nothing was created.
    KeyMissing,
    /// The host key exists but the device list is gone. Nothing was created.
    ListMissing,
    /// The folder could not be read or written.
    Io(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Damaged { file, detail } => write!(f, "{file} is damaged ({detail}); the file was not changed"),
            Self::KeyMissing => f.write_str("the host key is missing but phones are paired; the file was not changed"),
            Self::ListMissing => f.write_str("the device list is missing; the file was not changed"),
            Self::Io(m) => write!(f, "the data folder could not be used: {m}"),
        }
    }
}

/// What [`open`] found, ready to serve.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opened {
    pub host_key: [u8; 32],
    pub devices: Vec<Device>,
    /// True when this run created the key and the empty list.
    pub first_run: bool,
}

/// The host secret, created on first use. Losing it would change the host id that every paired
/// phone holds, so a missing secret with paired phones is an error, and nothing is created.
pub fn host_secret(dir: &Path, phones_paired: bool) -> Result<[u8; 32], StoreError> {
    let path = dir.join(SECRET_FILE);
    match fs::read(&path) {
        Ok(bytes) => <[u8; 32]>::try_from(bytes.as_slice()).map_err(|_| StoreError::Damaged { file: SECRET_FILE.into(), detail: "not 32 bytes".into() }),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            if phones_paired {
                return Err(StoreError::Damaged { file: SECRET_FILE.into(), detail: "missing but phones are paired".into() });
            }
            let secret = random32()?;
            write_atomic(&path, &secret, true)?;
            Ok(secret)
        }
        Err(e) => Err(StoreError::Io(e.to_string())),
    }
}

/// The first-run table of section 5, decided by which of the two files exist.
pub fn open(dir: &Path) -> Result<Opened, StoreError> {
    let key = read_key(&dir.join(HOST_KEY_FILE));
    let list = read_devices(&dir.join(DEVICES_FILE));
    match (key, list) {
        (Ok(None), Ok(None)) => {
            let host_key = random32()?;
            write_atomic(&dir.join(HOST_KEY_FILE), &host_key, true)?;
            save_devices(dir, &[])?;
            Ok(Opened { host_key, devices: Vec::new(), first_run: true })
        }
        (Ok(Some(host_key)), Ok(Some(devices))) => Ok(Opened { host_key, devices, first_run: false }),
        (Ok(None), Ok(Some(_))) => Err(StoreError::KeyMissing),
        (Ok(Some(_)), Ok(None)) => Err(StoreError::ListMissing),
        (Err(e), _) | (_, Err(e)) => Err(e),
    }
}

/// The explicit recovery of section 5: keeps what is there (renamed `<name>.damaged-<time>`),
/// creates a new key and an empty list. Every pairing is removed. Only the person asks for it.
pub fn start_new(dir: &Path, stamp: u64) -> Result<Opened, StoreError> {
    for name in [HOST_KEY_FILE, DEVICES_FILE] {
        let p = dir.join(name);
        if p.exists() {
            fs::rename(&p, dir.join(format!("{name}.damaged-{stamp}"))).map_err(|e| StoreError::Io(e.to_string()))?;
        }
    }
    let host_key = random32()?;
    write_atomic(&dir.join(HOST_KEY_FILE), &host_key, true)?;
    save_devices(dir, &[])?;
    Ok(Opened { host_key, devices: Vec::new(), first_run: true })
}

/// Writes the device list: the previous file goes to `.bak` first, then the new one replaces it.
pub fn save_devices(dir: &Path, devices: &[Device]) -> Result<(), StoreError> {
    let path = dir.join(DEVICES_FILE);
    if path.exists() {
        fs::copy(&path, dir.join(format!("{DEVICES_FILE}.bak"))).map_err(|e| StoreError::Io(e.to_string()))?;
    }
    let file = DeviceFile { version: DEVICES_VERSION, devices: devices.to_vec() };
    let text = serde_json::to_vec_pretty(&file).map_err(|e| StoreError::Io(e.to_string()))?;
    write_atomic(&path, &text, true)
}

/// Writes `config.json`. When the file on disk was damaged, it is renamed to
/// `config.json.damaged-<stamp>` first, so the new file never silently replaces it.
pub fn save_config(dir: &Path, text: &str, was_damaged: bool, stamp: u64) -> Result<(), StoreError> {
    let path = dir.join(CONFIG_FILE);
    if was_damaged && path.exists() {
        fs::rename(&path, dir.join(format!("{CONFIG_FILE}.damaged-{stamp}"))).map_err(|e| StoreError::Io(e.to_string()))?;
    }
    write_atomic(&path, text.as_bytes(), true)
}

/// Checks a device list read from disk. Anything wrong makes the whole file damaged.
pub fn validate(devices: &[Device]) -> Result<(), String> {
    if devices.len() > MAX_DEVICES {
        return Err(format!("more than {MAX_DEVICES} devices"));
    }
    let mut ids = std::collections::BTreeSet::new();
    let mut hashes = std::collections::BTreeSet::new();
    for d in devices {
        if d.device_id.is_empty() || d.device_id.len() > 64 || !d.device_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
            return Err("a device id is malformed".into());
        }
        if !ids.insert(d.device_id.as_str()) {
            return Err("two devices share an id".into());
        }
        if b64_decode_fixed::<32>(&d.token_hash).is_none() || !hashes.insert(d.token_hash.as_str()) {
            return Err("a token hash is malformed or repeated".into());
        }
        if b64_decode_fixed::<32>(&d.phone_pub).is_none() {
            return Err("a phone key is malformed".into());
        }
        if !PairMsg::valid_name(&d.name) {
            return Err("a device name is out of range".into());
        }
        if let Some(p) = &d.push
            && (!p.endpoint.starts_with("https://") || p.endpoint.len() > 1024 || p.p256dh.len() > 128 || p.auth.len() > 64)
        {
            return Err("a push subscription is out of range".into());
        }
    }
    Ok(())
}

/// The token hash a routing token gives, base64url (what the relay's allow list carries).
pub fn token_hash_b64(token: &[u8; 32]) -> String {
    b64_encode(&token_hash(token))
}

fn read_key(path: &Path) -> Result<Option<[u8; 32]>, StoreError> {
    match fs::read(path) {
        Ok(bytes) => match <[u8; 32]>::try_from(bytes.as_slice()) {
            Ok(k) => Ok(Some(k)),
            Err(_) => Err(StoreError::Damaged { file: HOST_KEY_FILE.into(), detail: "not 32 bytes".into() }),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(StoreError::Io(e.to_string())),
    }
}

fn read_devices(path: &Path) -> Result<Option<Vec<Device>>, StoreError> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(StoreError::Io(e.to_string())),
    };
    let damaged = |detail: &str| StoreError::Damaged { file: DEVICES_FILE.into(), detail: detail.to_owned() };
    let file: DeviceFile = serde_json::from_slice(&bytes).map_err(|_| damaged("does not parse"))?;
    if file.version != DEVICES_VERSION {
        return Err(damaged("unknown version"));
    }
    validate(&file.devices).map_err(|m| damaged(&m))?;
    Ok(Some(file.devices))
}

fn random32() -> Result<[u8; 32], StoreError> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| StoreError::Io(e.to_string()))?;
    Ok(b)
}

/// Writes a temporary file, makes it durable (`sync_all` before the rename, so a crash cannot leave
/// an empty list in place of the old one), then renames it over the target.
fn write_atomic(path: &Path, bytes: &[u8], private: bool) -> Result<(), StoreError> {
    use std::io::Write;
    let tmp: PathBuf = path.with_extension("tmp");
    let mut file = fs::File::create(&tmp).map_err(|e| StoreError::Io(e.to_string()))?;
    file.write_all(bytes).and_then(|()| file.sync_all()).map_err(|e| StoreError::Io(e.to_string()))?;
    drop(file);
    if private {
        // Before the rename: the file never appears under its name with a wider access list.
        restrict(&tmp)?;
    }
    fs::rename(&tmp, path).map_err(|e| StoreError::Io(e.to_string()))
}

/// Owner-only access. Unix: mode 0600. Windows: the access list of `acl.rs` (current user only).
/// A failure is an error, so a secret is never written into a file other users can read.
#[cfg(unix)]
fn restrict(path: &Path) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|e| StoreError::Io(format!("the file could not be made private: {e}")))
}

#[cfg(not(unix))]
fn restrict(path: &Path) -> Result<(), StoreError> {
    crate::acl::restrict_to_current_user(path).map_err(|e| StoreError::Io(format!("the file could not be made private: {e}")))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn dir() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn device(id: &str, name: &str, seed: u8) -> Device {
        Device {
            device_id: id.into(),
            kind: "phone".into(),
            name: name.into(),
            level: Level::Viewer,
            token_hash: b64_encode(&[seed; 32]),
            phone_pub: b64_encode(&[seed.wrapping_add(1); 32]),
            paired_at: 1,
            last_seen: 0,
            hide_names: false,
            notify: true,
            push: None,
        }
    }

    #[test]
    fn first_run_creates_key_and_empty_list() {
        let d = dir();
        let o = open(d.path()).unwrap();
        assert!(o.first_run);
        assert!(o.devices.is_empty());
        assert!(d.path().join(HOST_KEY_FILE).exists());
        assert!(d.path().join(DEVICES_FILE).exists());
        let again = open(d.path()).unwrap();
        assert!(!again.first_run);
        assert_eq!(again.host_key, o.host_key);
    }

    #[test]
    fn key_gone_but_phones_paired_is_an_error_and_creates_nothing() {
        let d = dir();
        open(d.path()).unwrap();
        save_devices(d.path(), &[device("d1", "Pixel", 7)]).unwrap();
        fs::remove_file(d.path().join(HOST_KEY_FILE)).unwrap();
        assert_eq!(open(d.path()).unwrap_err(), StoreError::KeyMissing);
        assert!(!d.path().join(HOST_KEY_FILE).exists(), "nothing is created");
    }

    #[test]
    fn list_gone_with_a_key_is_an_error_and_creates_nothing() {
        let d = dir();
        open(d.path()).unwrap();
        fs::remove_file(d.path().join(DEVICES_FILE)).unwrap();
        assert_eq!(open(d.path()).unwrap_err(), StoreError::ListMissing);
        assert!(!d.path().join(DEVICES_FILE).exists());
    }

    #[test]
    fn every_damaged_form_is_an_error_and_the_file_is_untouched() {
        let good = serde_json::to_string(&DeviceFile { version: 1, devices: vec![device("d1", "Pixel", 7)] }).unwrap();
        let cases: Vec<(&str, String)> = vec![
            ("not json", "{".into()),
            ("wrong version", good.replace("\"version\":1", "\"version\":2")),
            ("duplicate id", serde_json::to_string(&DeviceFile { version: 1, devices: vec![device("d1", "A", 1), device("d1", "B", 2)] }).unwrap()),
            ("duplicate token", serde_json::to_string(&DeviceFile { version: 1, devices: vec![device("d1", "A", 1), device("d2", "B", 1)] }).unwrap()),
            ("bad key length", good.replace(&b64_encode(&[8u8; 32]), "AAAA")),
            ("empty name", serde_json::to_string(&DeviceFile { version: 1, devices: vec![device("d1", "", 1)] }).unwrap()),
        ];
        for (what, text) in cases {
            let d = dir();
            open(d.path()).unwrap();
            fs::write(d.path().join(DEVICES_FILE), &text).unwrap();
            let before = fs::read(d.path().join(DEVICES_FILE)).unwrap();
            let r = open(d.path());
            assert!(matches!(r, Err(StoreError::Damaged { .. })), "{what}: {r:?}");
            assert_eq!(fs::read(d.path().join(DEVICES_FILE)).unwrap(), before, "{what}: the file is not changed");
        }
    }

    #[test]
    fn writes_keep_a_backup_of_the_previous_list() {
        let d = dir();
        open(d.path()).unwrap();
        save_devices(d.path(), &[device("d1", "Pixel", 7)]).unwrap();
        save_devices(d.path(), &[]).unwrap();
        let bak = fs::read_to_string(d.path().join("devices.json.bak")).unwrap();
        assert!(bak.contains("Pixel"), "the .bak holds the last good copy");
        assert!(open(d.path()).unwrap().devices.is_empty());
    }

    #[test]
    fn start_new_keeps_the_damaged_files_and_removes_pairings() {
        let d = dir();
        open(d.path()).unwrap();
        fs::write(d.path().join(DEVICES_FILE), "garbage").unwrap();
        let o = start_new(d.path(), 42).unwrap();
        assert!(o.devices.is_empty());
        assert!(d.path().join("devices.json.damaged-42").exists());
    }

    #[test]
    fn a_damaged_config_is_renamed_before_the_first_write() {
        let d = dir();
        open(d.path()).unwrap();
        fs::write(d.path().join(CONFIG_FILE), "{ broken").unwrap();
        save_config(d.path(), "{}", true, 9).unwrap();
        assert!(d.path().join("config.json.damaged-9").exists(), "the damaged file is kept");
        assert_eq!(fs::read_to_string(d.path().join(CONFIG_FILE)).unwrap(), "{}");
    }

    /// The secrets live only in their own files (`host-key`, `relay-secret`), as raw bytes. No other
    /// file in the data folder holds either of them, in base64 or as raw bytes, so a copy of the
    /// device list or the config leaks nothing that would let someone impersonate the PC.
    #[test]
    fn no_other_file_in_the_data_folder_holds_the_host_key_or_the_relay_secret() {
        use csm_remote_proto::ids::b64_encode;
        let d = dir();
        let opened = open(d.path()).unwrap();
        let secret = host_secret(d.path(), false).unwrap();
        save_devices(d.path(), &[device("d_1", "Pixel 9", 3)]).unwrap();
        save_config(d.path(), "{}", false, 1).unwrap();
        let secrets: [(&str, Vec<u8>); 2] = [("host key", opened.host_key.to_vec()), ("relay secret", secret.to_vec())];
        let b64s: Vec<String> = secrets.iter().map(|(_, s)| b64_encode(s)).collect();
        for entry in fs::read_dir(d.path()).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            if !path.is_file() || name == HOST_KEY_FILE || name == SECRET_FILE {
                continue;
            }
            let bytes = fs::read(&path).unwrap();
            let text = String::from_utf8_lossy(&bytes).into_owned();
            for ((what, raw), b64) in secrets.iter().zip(&b64s) {
                assert!(!text.contains(b64.as_str()), "{name} holds the {what} in base64");
                assert!(!bytes.windows(raw.len()).any(|w| w == raw.as_slice()), "{name} holds the {what} as raw bytes");
            }
        }
        assert_eq!(fs::read(d.path().join(SECRET_FILE)).unwrap(), secret.to_vec(), "the secret is in its own file");
    }
}
