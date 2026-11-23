//! Opening a remote window from the laptop's client folder: the paired host and this app's device key
//! are read, and nothing is made. A host that was never paired, or a damaged list, is refused.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use csm_peer::client_host::{DEVICE_KEY, HOSTS_FILE};
use csm_peer::hosts::HostStore;
use csm_peer::identity::Identity;
use csm_peer_proto::addr::{Host, PeerAddr};
use csm_ui::theme::ThemeKind;

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("csm-remote-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A client folder with this app's key and one paired office, as the pairing would leave it.
fn paired(dir: &Path) -> String {
    let me = Identity::load_or_create(&dir.join(DEVICE_KEY)).unwrap();
    let office = Identity::generate().unwrap();
    let addr = PeerAddr { host: Host::Ip(IpAddr::V4(Ipv4Addr::new(10, 8, 0, 2))), port: 47300 };
    let mut store = HostStore::load(&dir.join(HOSTS_FILE)).unwrap();
    let host = store.add("Office", &addr, None, office.public(), 1_700_000_000).unwrap();
    let _ = me;
    host.host_id
}

#[test]
fn a_window_reads_the_paired_office_and_its_own_key() {
    let dir = scratch("paired");
    let host_id = paired(&dir);
    let key_before = std::fs::read(dir.join(DEVICE_KEY)).unwrap();

    let (params, host) = csm_remote::window_params(&dir, &host_id, ThemeKind::Latte, Vec::new()).expect("the office is paired");
    assert_eq!(host.name, "Office");
    assert_eq!(host.address, "10.8.0.2:47300", "the stored form loses its tcp:// prefix");
    assert_eq!(params.theme, ThemeKind::Latte);
    assert_eq!(params.overlay, None, "no saved view yet: the window starts from the host's snapshot");
    assert_eq!(std::fs::read(dir.join(DEVICE_KEY)).unwrap(), key_before, "the window does not change this laptop's key");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_host_that_was_never_paired_is_refused_and_nothing_is_made() {
    let dir = scratch("unpaired");
    let _ = paired(&dir);
    let err = csm_remote::window_params(&dir, "NOTPAIRED", ThemeKind::Mocha, Vec::new()).err().expect("refused");
    assert!(err.contains("not paired"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_saved_view_is_seeded_into_the_window() {
    let dir = scratch("view");
    let host_id = paired(&dir);
    let path = csm_remote::connect::view_path(&dir, &host_id).expect("a plain id");
    let overlay = csm_ui::view_overlay::ViewOverlay { focused: Some("session-1".into()), ..Default::default() };
    csm_remote::connect::save_view(&path, &overlay).unwrap();

    let (params, _) = csm_remote::window_params(&dir, &host_id, ThemeKind::Mocha, Vec::new()).expect("paired");
    assert_eq!(params.overlay, Some(overlay), "the window keeps its own focus and layout");
    let _ = std::fs::remove_dir_all(&dir);
}
