#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The relay settings: the address in a file in the peer folder, the secrets in the keychain (here a
//! memory store), a relay that comes back when remote access is turned on, and one that stops when
//! it is turned off. Nothing here needs a relay to answer: the status shows what was saved.

use std::sync::Arc;

use csm_engine::{EnableRequest, PeerHost, RelaySetRequest, ToolError};
use csm_peer::{PeerServer, ServerConfig};
use csm_services::secrets::{MemoryStore, SecretStore};

const SERVICE: &str = "codeshell-peer";
const TOKEN: &str = "enrolment-token-0123456789";
/// A loopback address nothing listens on: the relay host keeps retrying quietly.
const URL: &str = "ws://127.0.0.1:9";

fn server(dir: &std::path::Path, store: Arc<MemoryStore>) -> PeerServer {
    let s = PeerServer::with_config(dir.join("peer"), ServerConfig::default());
    s.use_secrets(store);
    s
}

#[test]
fn the_token_goes_to_the_keychain_and_the_address_to_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::default());
    let s = server(tmp.path(), store.clone());

    let info = s.relay_set(RelaySetRequest { url: URL.into(), access: Some(TOKEN.into()) }, "test").unwrap().expect("the relay is on");
    assert_eq!(info.url, URL);
    assert_eq!(info.host_id.len(), 26, "the relay's id for this office");
    assert_eq!(store.get(SERVICE, "relay.enrol-token").unwrap().as_deref(), Some(TOKEN));
    let secret = store.get(SERVICE, "relay.host-secret").unwrap().expect("the host secret is kept");

    let file = std::fs::read_to_string(tmp.path().join("peer").join("relay.json")).unwrap();
    assert!(file.contains(URL));
    assert!(!file.contains(TOKEN), "the token is never in a file");
    assert!(!file.contains(&secret), "the host secret is never in a file");

    // Turning it off removes the address and the token; the host secret stays, so the relay id stays.
    assert_eq!(s.relay_set(RelaySetRequest { url: String::new(), access: None }, "test").unwrap(), None);
    assert!(!tmp.path().join("peer").join("relay.json").exists());
    assert_eq!(store.get(SERVICE, "relay.enrol-token").unwrap(), None);
    assert_eq!(store.get(SERVICE, "relay.host-secret").unwrap().as_deref(), Some(secret.as_str()));
}

#[test]
fn a_bad_relay_address_is_refused_before_anything_is_saved() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::default());
    let s = server(tmp.path(), store.clone());
    for url in ["http://relay.example", "ws://relay.example", "wss://", "relay.example"] {
        let r = s.relay_set(RelaySetRequest { url: url.into(), access: Some(TOKEN.into()) }, "test");
        assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{url}: {r:?}");
    }
    assert!(!tmp.path().join("peer").join("relay.json").exists());
    assert_eq!(store.get(SERVICE, "relay.enrol-token").unwrap(), None);
}

#[test]
fn a_saved_relay_comes_back_when_remote_access_is_turned_on_and_stops_when_it_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::default());
    {
        let s = server(tmp.path(), store.clone());
        s.relay_set(RelaySetRequest { url: URL.into(), access: Some(TOKEN.into()) }, "test").unwrap();
    }
    // A new run: nothing runs until remote access is turned on.
    let s = server(tmp.path(), store.clone());
    assert_eq!(PeerHost::relay_status(&s), None, "the relay does not run while remote access is off");
    s.enable(EnableRequest { addr: "127.0.0.1:0".parse().unwrap(), allow_all_interfaces: false }, "test").unwrap();
    let back = PeerHost::relay_status(&s).expect("the saved relay is back");
    assert_eq!(back.url, URL);

    s.disable("test");
    assert_eq!(PeerHost::relay_status(&s), None, "turning remote access off stops the relay");
    // The saved address is still there for the next time.
    assert!(tmp.path().join("peer").join("relay.json").exists());
}

#[test]
fn without_a_keychain_the_relay_is_refused_not_faked() {
    let tmp = tempfile::tempdir().unwrap();
    let s = PeerServer::with_config(tmp.path().join("peer"), ServerConfig::default());
    let r = s.relay_set(RelaySetRequest { url: URL.into(), access: None }, "test");
    assert!(matches!(r, Err(ToolError::Failed(_))), "{r:?}");
}
