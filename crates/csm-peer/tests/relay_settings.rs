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
const OTHER_URL: &str = "ws://127.0.0.1:11";

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
    assert!(info.access_set, "the status says a token is saved, never what it is");
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
        // Saved while remote access is off: it must not start now.
        let s = server(tmp.path(), store.clone());
        s.relay_set(RelaySetRequest { url: URL.into(), access: Some(TOKEN.into()) }, "test").unwrap();
    }
    // A new run: the saved relay is listed, not connected, and nothing runs until remote access is on.
    let s = server(tmp.path(), store.clone());
    let saved = PeerHost::relay_status(&s).expect("the saved relay is listed");
    assert_eq!((saved.url.as_str(), saved.connected, saved.access_set), (URL, false, true));
    assert!(saved.last_error.as_deref().is_some_and(|e| e.contains("remote access is off")), "{saved:?}");
    s.enable(EnableRequest { addr: "127.0.0.1:0".parse().unwrap(), allow_all_interfaces: false }, "test").unwrap();
    let back = PeerHost::relay_status(&s).expect("the saved relay is back");
    assert_eq!(back.url, URL);

    s.disable("test");
    let off = PeerHost::relay_status(&s).expect("still saved after the stop");
    assert!(!off.connected, "turning remote access off stops the relay");
    // The saved address is still there for the next time.
    assert!(tmp.path().join("peer").join("relay.json").exists());
}

#[test]
fn a_token_left_out_is_kept_for_the_same_address_only() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::default());
    let s = server(tmp.path(), store.clone());
    s.relay_set(RelaySetRequest { url: URL.into(), access: Some(TOKEN.into()) }, "test").unwrap();

    // Saving the same address again without a token keeps the saved one (the settings page sends
    // no token when the field is empty).
    let same = s.relay_set(RelaySetRequest { url: URL.into(), access: None }, "test").unwrap().expect("on");
    assert!(same.access_set);
    assert_eq!(store.get(SERVICE, "relay.enrol-token").unwrap().as_deref(), Some(TOKEN));

    // A different address must not receive the token: it is removed instead.
    let other = s.relay_set(RelaySetRequest { url: OTHER_URL.into(), access: None }, "test").unwrap().expect("on");
    assert!(!other.access_set, "the token of one relay is not sent to another");
    assert_eq!(store.get(SERVICE, "relay.enrol-token").unwrap(), None);
}

#[test]
fn an_empty_token_removes_the_saved_one_and_a_given_one_replaces_it() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Arc::new(MemoryStore::default());
    let s = server(tmp.path(), store.clone());
    s.relay_set(RelaySetRequest { url: URL.into(), access: Some(TOKEN.into()) }, "test").unwrap();

    let cleared = s.relay_set(RelaySetRequest { url: URL.into(), access: Some(String::new()) }, "test").unwrap().expect("on");
    assert!(!cleared.access_set, "an empty token removes the saved one");
    assert_eq!(store.get(SERVICE, "relay.enrol-token").unwrap(), None);

    let replaced = s.relay_set(RelaySetRequest { url: URL.into(), access: Some("another-enrolment-token-0123".into()) }, "test").unwrap();
    assert!(replaced.expect("on").access_set);
    assert_eq!(store.get(SERVICE, "relay.enrol-token").unwrap().as_deref(), Some("another-enrolment-token-0123"));
}

#[test]
fn the_token_never_shows_in_debug_output() {
    let req = RelaySetRequest { url: URL.into(), access: Some(TOKEN.into()) };
    let shown = format!("{req:?}");
    assert!(!shown.contains(TOKEN), "{shown}");
    assert!(shown.contains(URL), "the address may show: {shown}");
    let cfg = csm_peer::RelayConfig { url: URL.into(), host_secret: [7; 32], access: Some(TOKEN.into()) };
    let shown = format!("{cfg:?}");
    assert!(!shown.contains(TOKEN) && !shown.contains("[7, 7"), "{shown}");
}

#[test]
fn without_a_keychain_the_relay_is_refused_not_faked() {
    let tmp = tempfile::tempdir().unwrap();
    let s = PeerServer::with_config(tmp.path().join("peer"), ServerConfig::default());
    let r = s.relay_set(RelaySetRequest { url: URL.into(), access: None }, "test");
    assert!(matches!(r, Err(ToolError::Failed(_))), "{r:?}");
}
