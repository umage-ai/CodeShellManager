//! The laptop's half of pairing, against the real server, over real TCP on 127.0.0.1 with an
//! ephemeral port. One engine plays both apps: its `peer_host` is the office's server and its
//! `peer_client` is this laptop's client, each with its own folder. The tools are the ones the
//! person and the agents see (`peer_client_*`, `peer_hosts_list`, `peer_host_forget`).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::{SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, PeerClientHost, PeerHost, State, StateOptions, ToolError, ToolOutput};
use csm_peer::client::{self, ClientConfig};
use csm_peer::hosts::HostStore;
use csm_peer::identity::Identity;
use csm_peer::{ClientPeer, PeerServer, ServerConfig};
use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::msg::Level;
use csm_peer_proto::pairing::PeerOffer;
use serde_json::{Value, json};

struct Rig {
    _tmp: tempfile::TempDir,
    server: PeerServer,
    client_dir: PathBuf,
    engine: Engine,
    /// The host ids the window launcher was asked to open.
    launched: Arc<std::sync::Mutex<Vec<String>>>,
}

fn quick() -> ServerConfig {
    ServerConfig { handshake_timeout: Duration::from_secs(5), ..ServerConfig::default() }
}

impl Rig {
    fn new() -> Rig {
        Rig::with(quick())
    }

    fn with(cfg: ServerConfig) -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let server = PeerServer::with_config(tmp.path().join("peer"), cfg);
        let client_dir = tmp.path().join("peer-client");
        let launched = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = launched.clone();
        let options = StateOptions {
            remote_window_launcher: Arc::new(move |id: &str| {
                seen.lock().unwrap().push(id.to_owned());
                Ok(())
            }),
            restore_on_start: false,
            peer_host: Arc::new(server.clone()) as Arc<dyn PeerHost>,
            peer_client: Arc::new(ClientPeer::new(client_dir.clone())) as Arc<dyn PeerClientHost>,
            ..StateOptions::default()
        };
        let (state, _report) = State::load(options);
        let engine = Engine::start(state, vec![]).unwrap();
        server.attach(engine.clone());
        Rig { _tmp: tmp, server, client_dir, engine, launched }
    }

    fn ui(&self, tool: &str, params: Value) -> Result<ToolOutput, ToolError> {
        self.engine.call(Caller::Ui, tool, params)
    }

    fn cli(&self, tool: &str, params: Value) -> Result<ToolOutput, ToolError> {
        self.engine.call(Caller::Cli, tool, params)
    }

    /// Turns the office's remote access on at 127.0.0.1 with a free port.
    fn enable(&self) -> SocketAddr {
        self.ui("peer_server_enable", json!({"address": "127.0.0.1", "port": 0})).unwrap();
        self.server.local_addr().unwrap()
    }

    /// The office opens a pairing offer (the app's own window does this).
    fn offer_link(&self) -> String {
        self.ui("peer_pair_start", json!({})).unwrap().data["link"].as_str().unwrap().to_owned()
    }

    /// The laptop's app starts a pairing from a link (the app's own window).
    fn start_pair(&self, link: &str, name: Option<&str>) -> Result<Value, ToolError> {
        let mut params = json!({"link": link});
        if let Some(n) = name {
            params["name"] = json!(n);
        }
        self.ui("peer_client_pair", params).map(|o| o.data)
    }

    fn client_status(&self) -> Value {
        self.ui("peer_client_pair_status", json!({})).unwrap().data
    }

    /// Polls the laptop's pairing until it reaches `state`; panics with the last status otherwise.
    fn wait_client(&self, state: &str) -> Value {
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            let s = self.client_status();
            if s["state"] == state {
                return s;
            }
            assert!(Instant::now() < end, "waiting for the laptop to be `{state}`; last status {s}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The office's screen: the code shown for the device that is waiting.
    fn wait_office_code(&self) -> String {
        wait_for("a device waiting at the office", || self.server_status()["pending_pair"]["check_code"].as_str().map(str::to_owned))
    }

    fn server_status(&self) -> Value {
        self.cli("peer_status", json!({})).unwrap().data
    }

    fn hosts(&self) -> Vec<Value> {
        self.cli("peer_hosts_list", json!({})).unwrap().data.as_array().unwrap().clone()
    }

    fn client_file(&self, name: &str) -> PathBuf {
        self.client_dir.join(name)
    }

    /// The whole pairing with the office allowing it, as the two people would do it.
    fn pair_and_allow(&self, name: &str) -> Value {
        let link = self.offer_link();
        self.start_pair(&link, Some(name)).unwrap();
        let laptop_code = self.wait_client("check_code")["check_code"].as_str().unwrap().to_owned();
        let office_code = self.wait_office_code();
        assert_eq!(laptop_code, office_code, "both screens show the same four digits");
        self.ui("peer_pair_answer", json!({"allow": true, "check_code": office_code})).unwrap();
        self.wait_client("paired")
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.shutdown();
        self.engine.shutdown();
    }
}

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

/// An offer for a host nobody listens on. Nothing dials it when the laptop's storage is the thing
/// under test.
fn offer_to_nowhere() -> String {
    let addr = PeerAddr::parse(&format!("tcp://127.0.0.1:{}", free_port())).unwrap();
    PeerOffer::new(&addr, &[2; 32], &[3; 16], &[4; 32]).link()
}

// ---- pairing ---------------------------------------------------------------------------------

#[test]
fn allow_stores_the_host_and_the_codes_agree_and_the_host_connects_after() {
    let rig = Rig::new();
    let addr = rig.enable();
    let status = rig.pair_and_allow("Office");
    assert_eq!(status["host_name"], "Office");
    assert!(status["check_code"].is_null(), "the code is not kept once the pairing is done");

    let hosts = rig.hosts();
    assert_eq!(hosts.len(), 1, "{hosts:?}");
    assert_eq!(hosts[0]["name"], "Office");
    assert_eq!(hosts[0]["address"], format!("tcp://{addr}"));
    assert_eq!(hosts[0]["host_id"], status["host_id"]);
    assert_eq!(rig.server_status()["devices"], 1, "the office lists the laptop");

    // The stored state is what a later connection needs: this app's key and the pinned host key.
    let me = Identity::load_or_create(&rig.client_file("device.key")).unwrap();
    let stored = HostStore::load(&rig.client_file("hosts.json")).unwrap();
    let host = stored.list().first().unwrap();
    let conn = client::connect(&host.peer_addr().unwrap(), &host.public_key().unwrap(), &me, &ClientConfig::new("Laptop")).unwrap();
    assert_eq!(conn.welcome.device_id, me.device_id());
    conn.close();
}

#[test]
fn a_name_is_optional_and_the_address_stands_in() {
    let rig = Rig::new();
    let addr = rig.enable();
    let link = rig.offer_link();
    rig.start_pair(&link, None).unwrap();
    rig.wait_client("check_code");
    let code = rig.wait_office_code();
    rig.ui("peer_pair_answer", json!({"allow": true, "check_code": code})).unwrap();
    rig.wait_client("paired");
    assert_eq!(rig.hosts()[0]["name"], format!("tcp://{addr}"));
}

#[test]
fn deny_stores_nothing_and_says_so() {
    let rig = Rig::new();
    rig.enable();
    let link = rig.offer_link();
    rig.start_pair(&link, Some("Office")).unwrap();
    rig.wait_client("check_code");
    rig.wait_office_code();
    rig.ui("peer_pair_answer", json!({"allow": false})).unwrap();
    let s = rig.wait_client("failed");
    assert!(s["message"].as_str().unwrap().contains("denied"), "{s}");
    assert!(rig.hosts().is_empty());
    assert_eq!(rig.server_status()["devices"], 0);
}

#[test]
fn a_wrong_code_at_the_office_is_not_an_answer_and_the_right_one_still_pairs() {
    let rig = Rig::new();
    rig.enable();
    let link = rig.offer_link();
    rig.start_pair(&link, Some("Office")).unwrap();
    let laptop_code = rig.wait_client("check_code")["check_code"].as_str().unwrap().to_owned();
    let office_code = rig.wait_office_code();
    let wrong = if office_code == "0000" { "0001" } else { "0000" };
    assert!(rig.ui("peer_pair_answer", json!({"allow": true, "check_code": wrong})).is_err());
    // The laptop still waits: a wrong code at the office is not visible to it.
    assert_eq!(rig.client_status()["state"], "check_code");
    assert_eq!(rig.client_status()["check_code"], laptop_code);
    rig.ui("peer_pair_answer", json!({"allow": true, "check_code": office_code})).unwrap();
    rig.wait_client("paired");
    assert_eq!(rig.hosts().len(), 1);
}

#[test]
fn an_expired_offer_fails_with_the_host_saying_so_and_stores_nothing() {
    let rig = Rig::with(ServerConfig { pair_ttl: Duration::from_millis(300), ..quick() });
    rig.enable();
    let link = rig.offer_link();
    std::thread::sleep(Duration::from_millis(500));
    rig.start_pair(&link, Some("Late")).unwrap();
    let s = rig.wait_client("failed");
    assert!(s["message"].as_str().unwrap().contains("expired"), "{s}");
    assert!(rig.hosts().is_empty());
}

#[test]
fn cancel_withdraws_the_request_at_the_office_and_a_new_pairing_can_start() {
    let rig = Rig::new();
    rig.enable();
    let link = rig.offer_link();
    rig.start_pair(&link, Some("Office")).unwrap();
    rig.wait_client("check_code");
    rig.wait_office_code();
    // One pairing at a time.
    let again = rig.start_pair(&offer_to_nowhere(), Some("Other"));
    assert!(matches!(again, Err(ToolError::Refused(_))), "{again:?}");

    rig.ui("peer_client_pair_cancel", json!({})).unwrap();
    rig.wait_client("cancelled");
    wait_for("the office to drop the request", || rig.server_status()["pending_pair"].is_null().then_some(()));
    assert!(rig.hosts().is_empty());
    assert_eq!(rig.server_status()["devices"], 0);

    // The cancelled slot is spent, so the office opens a new offer and the laptop pairs with it.
    let status = rig.pair_and_allow("Office");
    assert_eq!(status["state"], "paired");
    assert_eq!(rig.hosts().len(), 1);
}

#[test]
fn cancelling_with_nothing_running_is_harmless() {
    let rig = Rig::new();
    let s = rig.cli("peer_client_pair_cancel", json!({})).unwrap().data;
    assert_eq!(s["state"], "idle");
}

#[test]
fn forget_removes_the_host_and_its_pinned_key_and_keeps_this_apps_key() {
    let rig = Rig::new();
    rig.enable();
    let status = rig.pair_and_allow("Office");
    let host_id = status["host_id"].as_str().unwrap().to_owned();

    let out = rig.ui("peer_host_forget", json!({"host_id": host_id})).unwrap();
    assert_eq!(out.data["forgotten"], host_id);
    assert!(rig.hosts().is_empty());
    assert!(matches!(rig.ui("peer_host_forget", json!({"host_id": host_id})), Err(ToolError::NotFound(_))));
    // The list is a file: a new app over the same folder sees the same thing.
    let again = ClientPeer::new(rig.client_dir.clone());
    assert!(again.hosts().unwrap().is_empty());
    assert!(!std::fs::read_to_string(rig.client_file("hosts.json")).unwrap().contains("public"), "the pinned key is gone with the entry");
    assert!(rig.client_file("device.key").exists(), "this app's own key stays: other hosts may know it");
}

// ---- who may start a pairing -----------------------------------------------------------------

#[test]
fn only_the_apps_own_window_may_start_a_pairing() {
    let rig = Rig::new();
    let link = offer_to_nowhere();
    let callers = [
        Caller::Cli,
        Caller::External { client: "some-agent".into(), session_id: None },
        Caller::Peer { device_id: "abcdefghijklmnopqrstuvwxyz".into(), name: "Phone".into(), level: Level::Full },
    ];
    for caller in callers {
        let r = rig.engine.call(caller.clone(), "peer_client_pair", json!({"link": link}));
        assert!(matches!(r, Err(ToolError::Refused(_))), "{caller:?}: {r:?}");
    }
    // Refused before anything happens: no state, no key, no folder.
    assert_eq!(rig.client_status()["state"], "idle");
    assert!(!rig.client_dir.exists(), "a refused pairing leaves nothing behind");
    // Reading and cancelling are for the person and their agents, not for a paired device.
    let s = rig.engine.call(Caller::Cli, "peer_client_pair_status", json!({})).unwrap();
    assert_eq!(s.data["state"], "idle");
    assert!(rig.engine.call(Caller::Cli, "peer_client_pair_cancel", json!({})).is_ok());
    let peer = Caller::Peer { device_id: "abcdefghijklmnopqrstuvwxyz".into(), name: "Phone".into(), level: Level::Full };
    for id in ["peer_client_pair_status", "peer_client_pair_cancel", "peer_hosts_list", "peer_host_forget"] {
        let r = rig.engine.call(peer.clone(), id, json!({"host_id": "x"}));
        assert!(matches!(r, Err(ToolError::Refused(_))), "{id} for a paired device: {r:?}");
    }
    // A refused read changes nothing and lists nothing.
    assert!(rig.hosts().is_empty());
}

#[test]
fn bad_input_is_refused_before_anything_is_written() {
    let rig = Rig::new();
    let link = offer_to_nowhere();
    assert!(matches!(rig.start_pair("not a link", None), Err(ToolError::InvalidParams(_))));
    assert!(matches!(rig.start_pair("csm://pair#AAAA", None), Err(ToolError::InvalidParams(_))));
    assert!(matches!(rig.start_pair(&link, Some("a\nb")), Err(ToolError::InvalidParams(_))));
    assert!(matches!(rig.ui("peer_client_pair", json!({"link": link, "extra": 1})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(rig.ui("peer_client_pair", json!({})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(rig.ui("peer_host_forget", json!({"host_id": "  "})), Err(ToolError::InvalidParams(_))));
    assert_eq!(rig.client_status()["state"], "idle");
    assert!(!rig.client_dir.exists());
}

// ---- nothing on disk until pairing; damaged files are errors ----------------------------------

#[test]
fn a_fresh_laptop_has_no_hosts_and_no_files() {
    let rig = Rig::new();
    assert!(rig.hosts().is_empty());
    assert_eq!(rig.client_status()["state"], "idle");
    assert!(!rig.client_dir.exists(), "reading the list writes nothing");
}

#[test]
fn a_damaged_host_list_is_an_error_and_is_left_alone() {
    let rig = Rig::new();
    std::fs::create_dir_all(&rig.client_dir).unwrap();
    std::fs::write(rig.client_file("hosts.json"), "not json").unwrap();
    assert!(matches!(rig.cli("peer_hosts_list", json!({})), Err(ToolError::Failed(_))));
    assert!(matches!(rig.ui("peer_host_forget", json!({"host_id": "x"})), Err(ToolError::Failed(_))));
    assert_eq!(std::fs::read_to_string(rig.client_file("hosts.json")).unwrap(), "not json");
}

#[test]
fn a_damaged_device_key_fails_the_pairing_and_is_not_replaced() {
    let rig = Rig::new();
    std::fs::create_dir_all(&rig.client_dir).unwrap();
    std::fs::write(rig.client_file("device.key"), "{ not json").unwrap();
    rig.start_pair(&offer_to_nowhere(), Some("Office")).unwrap();
    let s = rig.wait_client("failed");
    assert!(s["message"].as_str().unwrap().contains("damaged"), "{s}");
    assert_eq!(std::fs::read_to_string(rig.client_file("device.key")).unwrap(), "{ not json");
}

#[test]
fn the_host_list_survives_a_restart_of_the_app() {
    let rig = Rig::new();
    rig.enable();
    rig.pair_and_allow("Office");
    let again = ClientPeer::new(rig.client_dir.clone());
    let hosts = again.hosts().unwrap();
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].name, "Office");
    assert!(hosts[0].last_connected.is_none());
}

// ---- a host that is not there -------------------------------------------------------------------

#[test]
fn a_host_that_is_not_there_fails_the_pairing_cleanly() {
    let rig = Rig::new();
    let addr = PeerAddr::parse(&format!("tcp://127.0.0.1:{}", free_port())).unwrap();
    let link = PeerOffer::new(&addr, &[2; 32], &[3; 16], &[4; 32]).link();
    rig.start_pair(&link, Some("Gone")).unwrap();
    let s = rig.wait_client("failed");
    assert!(s["message"].as_str().unwrap().contains("could not reach"), "{s}");
    assert!(rig.hosts().is_empty());
    // A new pairing may start after a failed one.
    assert!(rig.start_pair(&offer_to_nowhere(), Some("Again")).is_ok());
    rig.wait_client("failed");
}

#[test]
fn a_cancel_ends_a_pairing_that_waits_in_the_handshake_at_once() {
    // A host that takes the connection and never answers: the laptop waits in the handshake. The
    // cancel must end that wait, not the connect timeout (10 s).
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = PeerAddr::parse(&format!("tcp://{}", listener.local_addr().unwrap())).unwrap();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming().flatten() {
            held.push(stream);
        }
    });
    let rig = Rig::new();
    let link = PeerOffer::new(&addr, &[2; 32], &[3; 16], &[4; 32]).link();
    rig.start_pair(&link, Some("Silent")).unwrap();
    rig.wait_client("connecting");
    std::thread::sleep(Duration::from_millis(300));
    let asked = Instant::now();
    rig.ui("peer_client_pair_cancel", json!({})).unwrap();
    rig.wait_client("cancelled");
    assert!(asked.elapsed() < Duration::from_secs(3), "the cancel ends the handshake wait: {:?}", asked.elapsed());
    assert!(rig.hosts().is_empty());
    // And the next pairing is not blocked by the old one.
    assert!(rig.start_pair(&offer_to_nowhere(), Some("Next")).is_ok());
    rig.wait_client("failed");
}

#[test]
fn connect_starts_a_remote_window_only_for_a_paired_host_and_only_from_the_app() {
    let rig = Rig::new();
    rig.enable();
    let host = rig.pair_and_allow("Office");
    assert_eq!(host["state"], "paired");
    let id = rig.hosts()[0]["host_id"].as_str().unwrap().to_owned();

    // The CLI and a paired device cannot open a window (the app's own window does).
    assert!(matches!(rig.engine.call(Caller::Cli, "remote_window_open", json!({"host_id": id})), Err(ToolError::Refused(_))));
    assert!(rig.launched.lock().unwrap().is_empty());
    // An id that is not a paired host is refused before anything starts.
    assert!(matches!(rig.ui("remote_window_open", json!({"host_id": "nope"})), Err(ToolError::NotFound(_))));
    assert!(matches!(rig.ui("remote_window_open", json!({"host_id": "  "})), Err(ToolError::InvalidParams(_))));
    assert!(rig.launched.lock().unwrap().is_empty());
    // The app's Connect: the launcher gets the id, and the call says so.
    let out = rig.ui("remote_window_open", json!({"host_id": id})).unwrap();
    assert!(out.message.contains("remote window"), "{}", out.message);
    assert_eq!(rig.launched.lock().unwrap().as_slice(), [id]);
}

#[test]
fn the_check_code_is_shown_to_the_app_only() {
    let rig = Rig::new();
    rig.enable();
    let link = rig.offer_link();
    rig.start_pair(&link, Some("Office")).unwrap();
    let code = rig.wait_client("check_code")["check_code"].as_str().unwrap().to_owned();
    assert_eq!(code.len(), 4);
    // The CLI sees the state, never the digits.
    let cli = rig.engine.call(Caller::Cli, "peer_client_pair_status", json!({})).unwrap().data;
    assert_eq!(cli["state"], "check_code");
    assert!(cli["check_code"].is_null(), "{cli}");
    rig.ui("peer_client_pair_cancel", json!({})).unwrap();
    rig.wait_client("cancelled");
}
