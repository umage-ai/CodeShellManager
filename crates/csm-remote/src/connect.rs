//! Opening a remote window from what this laptop keeps in its client folder (`peer-client/`): the
//! paired host in `hosts.json`, this app's device key, and the window's own view state for that
//! host in `view-<host id>.json` (layout, focus, rail, theme: per client, design section 2.3).
//!
//! Nothing here creates a device key or dials: a window connects only to a host the app paired
//! with, from the app's own window. A missing key or host is an error the window shows.

use std::path::{Path, PathBuf};

use csm_peer::client::ClientConfig;
use csm_peer::client_host::{ClientPeer, DEVICE_KEY, HOSTS_FILE};
use csm_peer::fsperm::write_private;
use csm_peer::hosts::HostStore;
use csm_peer::identity::Identity;
use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::ids::KEY_LEN;
use csm_ui::theme::ThemeKind;
use csm_ui::view_overlay::{ThemeFact, ViewOverlay};

use crate::backend::ConnectParams;

/// The longest host id a view file name may use (host ids are 26 characters).
const MAX_HOST_ID: usize = 64;

/// What a window shows of its host: the stored name and the address it dials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostWindow {
    pub name: String,
    /// `10.8.0.2:47300`, without the `tcp://` of the stored form.
    pub address: String,
}

/// One paired office, as the client folder lists it: what `csm attach --offices` prints and what a
/// window can connect to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedOffice {
    pub host_id: String,
    pub name: String,
    /// `10.8.0.2:47300`, without the `tcp://` of the stored form.
    pub address: String,
}

/// What a client needs to dial one paired office.
pub struct Paired {
    pub window: HostWindow,
    pub addr: PeerAddr,
    /// The relay route, when the office has one (tried after the direct address).
    pub relay: Option<csm_peer_proto::pairing::RelayRoute>,
    pub host_public: [u8; KEY_LEN],
    pub identity: Identity,
    pub cfg: ClientConfig,
}

/// The file that keeps one host's view state. `None` for a host id that is not a plain name (the id
/// comes from a file, so it is never trusted to build a path).
pub fn view_path(dir: &Path, host_id: &str) -> Option<PathBuf> {
    let plain = !host_id.is_empty() && host_id.len() <= MAX_HOST_ID && host_id.chars().all(|c| c.is_ascii_alphanumeric());
    plain.then(|| dir.join(format!("view-{host_id}.json")))
}

/// One host's saved view state. A missing or damaged file is no state: the window starts from the
/// host's snapshot instead.
pub fn load_view(path: &Path) -> Option<ViewOverlay> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// Saves one host's view state (private to this user, like the other client files).
pub fn save_view(path: &Path, overlay: &ViewOverlay) -> std::io::Result<()> {
    let bytes = serde_json::to_vec_pretty(overlay).map_err(std::io::Error::other)?;
    write_private(path, &bytes)
}

/// The offices this laptop is paired with, in the order the host list keeps them. A missing list is
/// no offices; a damaged one is an error (it is never replaced).
pub fn paired_offices(dir: &Path) -> Result<Vec<PairedOffice>, String> {
    let store = HostStore::load(&dir.join(HOSTS_FILE)).map_err(|e| format!("could not read the paired offices: {e}"))?;
    Ok(store
        .list()
        .iter()
        .map(|h| PairedOffice { host_id: h.host_id.clone(), name: h.name.clone(), address: h.address.trim_start_matches("tcp://").to_owned() })
        .collect())
}

/// What is needed to connect to `host_id`, read from the client folder `dir`. Nothing is made here:
/// a missing device key or an unpaired host is an error.
pub fn paired(dir: &Path, host_id: &str) -> Result<Paired, String> {
    let key = dir.join(DEVICE_KEY);
    if !key.exists() {
        return Err("This laptop has no device key yet. Pair with the office from CodeShellManager first.".into());
    }
    let store = HostStore::load(&dir.join(HOSTS_FILE)).map_err(|e| format!("could not read the paired offices: {e}"))?;
    let host = store.find(host_id).ok_or_else(|| "This office is not paired with this laptop any more.".to_owned())?;
    let addr = host.peer_addr().ok_or_else(|| "The address stored for this office is damaged. Forget it and pair again.".to_owned())?;
    let host_public = host.public_key().ok_or_else(|| "The key stored for this office is damaged. Forget it and pair again.".to_owned())?;
    let identity = Identity::load_or_create(&key).map_err(|e| format!("this app's device key: {e}"))?;
    let device = ClientPeer::new(dir.to_path_buf()).device_name().to_owned();
    let window = HostWindow { name: host.name.clone(), address: host.address.trim_start_matches("tcp://").to_owned() };
    Ok(Paired { window, addr, relay: host.relay.clone(), host_public, identity, cfg: ClientConfig::new(&device) })
}

/// What a remote window needs to connect to `host_id`, read from the client folder `dir`. `themes` is
/// the list of themes this window can draw; `theme` is its own mode.
pub fn window_params(dir: &Path, host_id: &str, theme: ThemeKind, themes: Vec<ThemeFact>) -> Result<(ConnectParams, HostWindow), String> {
    let p = paired(dir, host_id)?;
    let overlay = view_path(dir, host_id).as_deref().and_then(load_view);
    Ok((ConnectParams { addr: p.addr, relay: p.relay, host_public: p.host_public, identity: p.identity, cfg: p.cfg, theme, themes, overlay }, p.window))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_view_file_is_named_only_by_a_plain_host_id() {
        let dir = Path::new("/state/peer-client");
        assert_eq!(view_path(dir, "ABC234"), Some(dir.join("view-ABC234.json")));
        assert_eq!(view_path(dir, "../evil"), None);
        assert_eq!(view_path(dir, "a/b"), None);
        assert_eq!(view_path(dir, ""), None);
        assert_eq!(view_path(dir, &"a".repeat(65)), None);
    }

    #[test]
    fn a_window_with_no_device_key_is_refused_and_writes_nothing() {
        let dir = std::env::temp_dir().join(format!("csm-remote-connect-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let err = window_params(&dir, "ABC234", ThemeKind::Mocha, Vec::new()).err().expect("no key, no window");
        assert!(err.contains("no device key"), "{err}");
        assert!(!dir.join(DEVICE_KEY).exists(), "a window never makes a device key");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_saved_view_comes_back_and_a_damaged_one_is_no_state() {
        let dir = std::env::temp_dir().join(format!("csm-remote-view-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = view_path(&dir, "HOST1").expect("a plain id");
        let overlay = ViewOverlay { focused: Some("s1".into()), theme_dark: Some("Mocha".into()), ..ViewOverlay::default() };
        save_view(&path, &overlay).expect("the view is saved");
        assert_eq!(load_view(&path), Some(overlay));
        std::fs::write(&path, b"{ not json").expect("write");
        assert_eq!(load_view(&path), None, "a damaged file is no state");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_folder_has_no_offices_and_nothing_is_made() {
        let dir = std::env::temp_dir().join(format!("csm-remote-offices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(paired_offices(&dir), Ok(Vec::new()));
        assert!(!dir.exists(), "listing creates no folder");
        assert!(paired(&dir, "ABC234").is_err(), "an unpaired host is refused");
        assert!(!dir.exists());
    }
}
