//! The peer server and client over real TCP on 127.0.0.1, in one process: a real engine, the real
//! server as its `PeerHost`, the real client. Every test uses an ephemeral port.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, PeerHost, State, StateOptions};
use csm_peer::client::{self, ClientConfig, ClientError, Incoming, PairedHost};
use csm_peer::identity::Identity;
use csm_peer::{PeerServer, ServerConfig};
use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::codec::Frame;
use csm_peer_proto::Event;
use csm_peer_proto::msg::{ByeReason, ConsentOutcome, ErrorKind, Level, Msg, Outcome};
use csm_peer_proto::pairing::{DenyReason, PeerOffer};
use serde_json::{Value, json};

struct Rig {
    loaded: Option<csm_core::state::LoadSource>,
    _tmp: tempfile::TempDir,
    peer_dir: PathBuf,
    server: PeerServer,
    engine: Engine,
}

fn quick() -> ServerConfig {
    ServerConfig { handshake_timeout: Duration::from_secs(5), ..ServerConfig::default() }
}

impl Rig {
    fn new() -> Rig {
        Rig::with(quick())
    }

    fn with(cfg: ServerConfig) -> Rig {
        Rig::start(cfg, |_| {})
    }

    fn start(cfg: ServerConfig, tweak: impl FnOnce(&mut StateOptions)) -> Rig {
        let tmp = tempfile::tempdir().unwrap();
        let peer_dir = tmp.path().join("peer");
        let server = PeerServer::with_config(peer_dir.clone(), cfg);
        let host: Arc<dyn PeerHost> = Arc::new(server.clone());
        let mut options = StateOptions { restore_on_start: false, peer_host: host, ..StateOptions::default() };
        tweak(&mut options);
        // The way apps/codeshell builds it: a state file is really read when there is one.
        let (state, report) = State::load(options);
        let engine = Engine::start(state, vec![]).unwrap();
        server.attach(engine.clone());
        Rig { _tmp: tmp, peer_dir, server, engine, loaded: report.source }
    }

    fn ui(&self, tool: &str, params: Value) -> Result<csm_engine::ToolOutput, csm_engine::ToolError> {
        self.engine.call(Caller::Ui, tool, params)
    }

    fn status(&self) -> Value {
        self.engine.call(Caller::Cli, "peer_status", json!({})).unwrap().data
    }

    /// Turns remote access on at 127.0.0.1 with a free port the system picks.
    fn enable(&self) -> SocketAddr {
        self.ui("peer_server_enable", json!({"address": "127.0.0.1", "port": 0})).unwrap();
        self.server.local_addr().unwrap()
    }

    fn open_offer(&self) -> PeerOffer {
        let out = self.ui("peer_pair_start", json!({})).unwrap();
        PeerOffer::decode(out.data["link"].as_str().unwrap()).unwrap()
    }

    /// The whole pairing, with the person comparing the codes and allowing.
    fn pair(&self, name: &str) -> (Identity, PairedHost) {
        let offer = self.open_offer();
        let me = Identity::generate().unwrap();
        let p = Pairing::start(offer, me.clone(), name);
        let code = p.wait_pending(self);
        assert_eq!(code, p.client_code(), "both screens show the same four digits");
        self.ui("peer_pair_answer", json!({"allow": true, "check_code": code})).unwrap();
        (me, p.finish().unwrap())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.shutdown();
        self.engine.shutdown();
    }
}

/// A client pairing on its own thread, the way an app would run it.
struct Pairing {
    handle: std::thread::JoinHandle<Result<PairedHost, ClientError>>,
    code: mpsc::Receiver<String>,
    seen: std::cell::RefCell<Option<String>>,
}

impl Pairing {
    fn start(offer: PeerOffer, me: Identity, name: &str) -> Pairing {
        let (tx, rx) = mpsc::channel();
        let cfg = ClientConfig { pair_wait: Duration::from_secs(20), ..ClientConfig::new(name) };
        let handle = std::thread::spawn(move || client::pair(&offer, &me, &cfg, &mut |c| drop(tx.send(c.to_owned()))));
        Pairing { handle, code: rx, seen: std::cell::RefCell::new(None) }
    }

    fn client_code(&self) -> String {
        let mut seen = self.seen.borrow_mut();
        if seen.is_none() {
            *seen = Some(self.code.recv_timeout(Duration::from_secs(10)).expect("the client reached the check code"));
        }
        seen.clone().unwrap()
    }

    /// Waits until the server shows the device waiting, and returns the server's code.
    fn wait_pending(&self, rig: &Rig) -> String {
        wait_for("a device waiting to pair", || rig.status()["pending_pair"]["check_code"].as_str().map(str::to_owned))
    }

    fn finish(self) -> Result<PairedHost, ClientError> {
        self.handle.join().unwrap()
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

/// Tries to connect, which is the only way to know nothing listens.
fn refuses_connections(addr: SocketAddr) -> bool {
    (0..3).all(|_| TcpStream::connect_timeout(&addr, Duration::from_millis(700)).is_err())
}

fn peer_addr(a: SocketAddr) -> PeerAddr {
    PeerAddr::parse(&format!("tcp://{a}")).unwrap()
}

fn connect(host: &PairedHost, me: &Identity) -> Result<client::Connection, ClientError> {
    client::connect(&host.address, &host.host_public, me, &ClientConfig::new("Laptop"))
}

fn wait_closed(c: &client::Connection) -> Option<ByeReason> {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        match c.recv_timeout(Duration::from_millis(100)) {
            Some(Incoming::Closed(r)) => return r,
            Some(Incoming::Frame(_)) | None => {}
        }
    }
    panic!("the connection did not close");
}

// ---- default off ------------------------------------------------------------------------

/// Writes the keys a hostile or confused file might use to ask for a listener at `port`.
fn hostile_keys(port: u16) -> Value {
    json!({
        "PeerServerEnabled": true,
        "PeerServer": {"enabled": true, "address": "127.0.0.1", "port": port},
        "RemoteAccess": {"Enabled": true, "Address": "127.0.0.1", "Port": port},
        "peer_server_enabled": true
    })
}

fn assert_off(rig: &Rig, port: u16) {
    // A restart's worth of start-up: the app would call this, and it must find nothing to resume.
    rig.server.resume();
    let s = rig.status();
    assert_eq!(s["available"], true);
    assert_eq!(s["enabled"], false, "{s}");
    assert_eq!(s["listening"], Value::Null);
    assert!(rig.server.local_addr().is_none());
    assert!(refuses_connections(SocketAddr::from(([127, 0, 0, 1], port))), "something is listening on {port}");
    assert!(!rig.peer_dir.join("server.json").exists(), "no saved choice");
    assert!(!rig.peer_dir.join("host.key").exists(), "no key was made: nothing was turned on");
}

#[test]
fn a_fresh_state_binds_nothing() {
    let rig = Rig::new();
    assert_off(&rig, free_port());
    assert!(!rig.peer_dir.exists() || std::fs::read_dir(&rig.peer_dir).unwrap().all(|e| e.unwrap().file_name() == "remote-access.log"));
}

#[test]
fn an_upgraded_state_file_with_remote_access_keys_binds_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let port = free_port();
    let mut state = json!({"Sessions": [{"Id": "u1", "Name": "upgraded", "Command": "pwsh", "WorkingFolder": "C:\\src"}], "Groups": [], "Settings": {"Theme": "light"}});
    for (k, v) in hostile_keys(port).as_object().unwrap() {
        state[k] = v.clone();
        state["Settings"][k] = v.clone();
    }
    let path = tmp.path().join("state.json");
    std::fs::write(&path, serde_json::to_string(&state).unwrap()).unwrap();
    let rig = Rig::start(quick(), |o| o.save_path = Some(path.clone()));
    assert!(matches!(rig.loaded, Some(csm_core::state::LoadSource::Primary)), "the file was read: {:?}", rig.loaded);
    assert!(rig.engine.view().sessions.iter().any(|s| s.id == "u1"), "the hostile file really was loaded");
    assert_off(&rig, port);
}

#[test]
fn a_migrated_wpf_state_binds_nothing() {
    let rig = Rig::new();
    let port = free_port();
    let tmp = tempfile::tempdir().unwrap();
    let mut settings = json!({"Theme": "light", "LaunchCommands": ["pwsh"]});
    for (k, v) in hostile_keys(port).as_object().unwrap() {
        settings[k] = v.clone();
    }
    let mut wpf = json!({"Sessions": [{"Id": "w1", "Name": "one", "Command": "pwsh", "WorkingFolder": "C:\\src"}], "Groups": [], "Settings": settings});
    for (k, v) in hostile_keys(port).as_object().unwrap() {
        wpf[k] = v.clone();
    }
    let path = tmp.path().join("wpf-state.json");
    std::fs::write(&path, serde_json::to_string(&wpf).unwrap()).unwrap();
    rig.ui("state_import_wpf", json!({"path": path.to_string_lossy()})).unwrap();
    assert!(rig.engine.view().sessions.iter().any(|s| s.id == "w1"), "the import did happen");
    assert_off(&rig, port);
}

#[test]
fn an_imported_csm_backup_binds_nothing() {
    let rig = Rig::new();
    let port = free_port();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("backup.csm");
    rig.ui("state_export", json!({"path": path.to_string_lossy()})).unwrap();
    let mut backup: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (k, v) in hostile_keys(port).as_object().unwrap() {
        backup[k] = v.clone();
        if let Some(s) = backup.get_mut("Settings") {
            s[k] = v.clone();
        }
    }
    std::fs::write(&path, serde_json::to_string(&backup).unwrap()).unwrap();
    rig.ui("state_import", json!({"path": path.to_string_lossy(), "force": true})).unwrap();
    assert_off(&rig, port);
    // Even a backup that came from a machine with remote access on cannot carry the choice: the
    // server's own files are not part of a backup, and the engine never reads them.
    assert!(!rig.peer_dir.join("server.json").exists());
}

#[test]
fn resume_only_replays_a_choice_the_enable_tool_saved() {
    let rig = Rig::new();
    let addr = rig.enable();
    assert!(rig.peer_dir.join("server.json").exists());
    rig.server.shutdown();
    assert!(refuses_connections(addr), "the app closing closes the listener");
    // A new run of the app, same folder: the saved choice comes back on the same address.
    let again = Rig::with(quick());
    let saved = std::fs::read(rig.peer_dir.join("server.json")).unwrap();
    std::fs::create_dir_all(&again.peer_dir).unwrap();
    std::fs::write(again.peer_dir.join("server.json"), &saved).unwrap();
    again.server.resume();
    wait_for("the saved listener", || again.server.local_addr());
    assert_eq!(again.server.local_addr().unwrap(), addr);
    assert!(TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok());
    assert_eq!(again.status()["enabled"], true);
    // Disabling saves the choice off: the next start finds nothing.
    again.ui("peer_server_disable", json!({})).unwrap();
    assert!(refuses_connections(addr));
    again.server.resume();
    assert!(again.server.local_addr().is_none());
}

#[test]
fn a_doctored_choice_cannot_open_every_interface() {
    let rig = Rig::new();
    std::fs::create_dir_all(&rig.peer_dir).unwrap();
    for addr in ["0.0.0.0", "::"] {
        let choice = json!({"v": 1, "enabled": true, "address": addr, "port": free_port(), "all_interfaces": false});
        std::fs::write(rig.peer_dir.join("server.json"), choice.to_string()).unwrap();
        rig.server.resume();
        assert!(rig.server.local_addr().is_none(), "{addr}");
    }
    std::fs::write(rig.peer_dir.join("server.json"), "{ not json").unwrap();
    rig.server.resume();
    assert!(rig.server.local_addr().is_none());
}

#[test]
fn an_address_that_is_not_there_yet_waits_and_never_listens_elsewhere() {
    let rig = Rig::new();
    std::fs::create_dir_all(&rig.peer_dir).unwrap();
    // 203.0.113.0/24 is documentation space: not an address of this machine.
    let choice = json!({"v": 1, "enabled": true, "address": "203.0.113.7", "port": 47999, "all_interfaces": false});
    std::fs::write(rig.peer_dir.join("server.json"), choice.to_string()).unwrap();
    rig.server.resume();
    let s = rig.status();
    assert_eq!(s["enabled"], true);
    assert_eq!(s["listening"], Value::Null);
    assert!(s["waiting"].as_str().unwrap().contains("203.0.113.7"), "{s}");
    assert!(refuses_connections(SocketAddr::from(([127, 0, 0, 1], 47999))), "not on loopback either");
    rig.ui("peer_server_disable", json!({})).unwrap();
    assert_eq!(rig.status()["waiting"], Value::Null);
}

// ---- the listener -----------------------------------------------------------------------

#[test]
fn it_binds_only_the_chosen_address() {
    let rig = Rig::new();
    let addr = rig.enable();
    assert_eq!(addr.ip().to_string(), "127.0.0.1");
    assert!(TcpStream::connect_timeout(&addr, Duration::from_secs(2)).is_ok());
    let other = SocketAddr::from(([127, 0, 0, 2], addr.port()));
    assert!(refuses_connections(other), "another loopback address is not served");
    assert_eq!(rig.status()["listening"], addr.to_string());
}

#[test]
fn every_interface_needs_to_be_asked_for_on_purpose() {
    let rig = Rig::new();
    for a in ["0.0.0.0", "::"] {
        let r = rig.ui("peer_server_enable", json!({"address": a, "port": 0}));
        assert!(matches!(r, Err(csm_engine::ToolError::Refused(_))), "{a}: {r:?}");
        assert!(rig.server.local_addr().is_none());
    }
    // The host refuses it too if the tool layer were skipped.
    let direct = rig.server.enable(csm_engine::EnableRequest { addr: "0.0.0.0:0".parse().unwrap(), allow_all_interfaces: false }, "test");
    assert!(matches!(direct, Err(csm_engine::ToolError::Refused(_))));
    assert!(rig.server.local_addr().is_none());
}

#[test]
fn files_are_created_only_when_it_is_turned_on() {
    let rig = Rig::new();
    assert!(!rig.peer_dir.join("host.key").exists());
    rig.enable();
    for f in ["host.key", "server.json"] {
        assert!(rig.peer_dir.join(f).exists(), "{f}");
    }
    let key = std::fs::read_to_string(rig.peer_dir.join("host.key")).unwrap();
    assert!(key.contains("private"), "the key file holds the secret half");
    assert!(rig.status()["private_files_restricted"].is_boolean());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(rig.peer_dir.join("host.key")).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(std::fs::metadata(&rig.peer_dir).unwrap().permissions().mode() & 0o777, 0o700);
        assert_eq!(rig.status()["private_files_restricted"], true);
    }
}

// ---- pairing ----------------------------------------------------------------------------

#[test]
fn pairing_then_connecting_then_calling_tools_as_a_peer() {
    let rig = Rig::new();
    let addr = rig.enable();
    let (me, host) = rig.pair("Laptop");
    assert_eq!(host.level, Level::Full);
    assert_eq!(host.address.socket_addr(), Some(addr));
    let devices = rig.ui("peer_device_list", json!({})).unwrap().data;
    assert_eq!(devices[0]["name"], "Laptop");
    assert_eq!(devices[0]["id"], host.device_id);
    assert_eq!(devices[0]["connected"], false);
    assert_eq!(rig.status()["pending_pair"], Value::Null);
    assert_eq!(rig.status()["devices"], 1);

    let conn = connect(&host, &me).unwrap();
    assert_eq!(conn.welcome.device_id, host.device_id);
    assert_eq!(conn.welcome.level, Level::Full);
    wait_for("the device connected", || (rig.status()["connected"] == 1).then_some(()));

    // A read, a change (which the engine logs), and the tools a peer can never use.
    let t = Duration::from_secs(10);
    assert!(matches!(conn.call_wait("session_list", json!({}), t).unwrap(), Outcome::Ok { .. }));
    let made = conn.call_wait("group_new", json!({"name": "From the laptop"}), t).unwrap();
    assert!(matches!(made, Outcome::Ok { .. }));
    assert!(rig.engine.view().groups.iter().any(|g| g.name == "From the laptop"));
    for tool in ["peer_server_enable", "peer_pair_start", "peer_pair_answer"] {
        match conn.call_wait(tool, json!({"address": "127.0.0.1", "allow": true}), t).unwrap() {
            Outcome::Err { kind: ErrorKind::Refused, .. } => {}
            other => panic!("{tool} from a paired device must be refused, got {other:?}"),
        }
    }
    assert_eq!(rig.status()["listening"], addr.to_string(), "still the same listener");
    match conn.call_wait("no_such_tool", json!({}), t).unwrap() {
        Outcome::Err { kind: ErrorKind::UnknownTool, .. } => {}
        other => panic!("{other:?}"),
    }
    // A peer may close doors: this one revokes nothing it does not own, and can list.
    assert!(matches!(conn.call_wait("peer_device_list", json!({}), t).unwrap(), Outcome::Ok { .. }));

    // The log has the story, and never a key or a link.
    let log = std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).unwrap();
    for event in ["enabled", "pair_started", "pair_requested", "paired", "connected", "refused", "call"] {
        assert!(log.contains(&format!("\"event\":\"{event}\"")), "{event} is logged:\n{log}");
    }
    assert!(log.contains("group_new"), "a non-read call is logged with its tool");
    assert!(!log.contains("csm://pair") && !log.contains("session_list"), "no link, and reads are not logged");
    let key = std::fs::read_to_string(rig.peer_dir.join("host.key")).unwrap();
    let secret = serde_json::from_str::<Value>(&key).unwrap()["private"].as_str().unwrap().to_owned();
    assert!(!log.contains(&secret));
}

#[test]
fn an_unpaired_key_is_refused_in_silence() {
    let rig = Rig::new();
    let addr = rig.enable();
    let (_me, host) = rig.pair("Laptop");
    let stranger = Identity::generate().unwrap();
    // The stranger knows the host's public key (it is in every offer) but is not on the list.
    let r = client::connect(&peer_addr(addr), &host.host_public, &stranger, &ClientConfig::new("Stranger"));
    assert!(matches!(r, Err(ClientError::Refused)), "{r:?}");
    wait_for("the refusal in the log", || std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).ok().filter(|l| l.contains("unknown key")));
    assert_eq!(rig.status()["connected"], 0);
}

#[test]
fn strangers_get_nothing_back_whatever_they_send() {
    let rig = Rig::new();
    let addr = rig.enable();
    let probes: Vec<Vec<u8>> = vec![
        vec![],
        b"GET / HTTP/1.1\r\n\r\n".to_vec(),
        vec![0, 0, 0, 1, 0x01],                       // a session selector, then nothing
        vec![0, 0, 0, 17, 0x02, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16], // a pairing selector, no slot
        vec![0xff; 64],
        vec![0, 0, 0xff, 0xff, 1],                    // a length far beyond a frame
        vec![0, 0, 0, 0],                             // an empty frame
    ];
    for p in probes {
        let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
        let _ = s.write_all(&p);
        let _ = s.shutdown(std::net::Shutdown::Write);
        let mut buf = [0u8; 64];
        match s.read(&mut buf) {
            Ok(0) | Err(_) => {}
            Ok(n) => panic!("the server answered {n} bytes to {p:?}"),
        }
    }
    assert_eq!(rig.status()["connected"], 0);
    assert_eq!(rig.status()["listening"], addr.to_string(), "still serving");
}

#[test]
fn a_connection_that_says_nothing_is_dropped() {
    let rig = Rig::with(ServerConfig { handshake_timeout: Duration::from_millis(400), ..ServerConfig::default() });
    let addr = rig.enable();
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(8))).unwrap();
    let mut buf = [0u8; 8];
    let started = Instant::now();
    assert!(matches!(s.read(&mut buf), Ok(0) | Err(_)));
    assert!(started.elapsed() < Duration::from_secs(5), "closed after the handshake time, not left hanging");
}

#[test]
fn a_wrong_check_code_is_not_an_answer() {
    let rig = Rig::new();
    rig.enable();
    let offer = rig.open_offer();
    let p = Pairing::start(offer, Identity::generate().unwrap(), "Laptop");
    let code = p.wait_pending(&rig);
    let wrong = if code == "0000" { "0001" } else { "0000" };
    for _ in 0..1 {
        let r = rig.ui("peer_pair_answer", json!({"allow": true, "check_code": wrong}));
        assert!(matches!(r, Err(csm_engine::ToolError::Refused(_))), "{r:?}");
        assert_eq!(rig.status()["pending_pair"]["check_code"], code, "still waiting");
        assert_eq!(rig.status()["devices"], 0, "nothing was added");
    }
    let none = rig.ui("peer_pair_answer", json!({"allow": true}));
    assert!(matches!(none, Err(csm_engine::ToolError::Refused(_))), "allowing needs the code: {none:?}");
    // The third wrong try denies the device for good.
    let r = rig.ui("peer_pair_answer", json!({"allow": true, "check_code": wrong}));
    assert!(matches!(r, Err(csm_engine::ToolError::Refused(m)) if m.contains("denied")));
    assert!(matches!(p.finish(), Err(ClientError::Denied(DenyReason::Denied))));
    assert_eq!(rig.status()["devices"], 0);
    assert_eq!(rig.status()["pending_pair"], Value::Null);
}

#[test]
fn the_person_can_deny() {
    let rig = Rig::new();
    rig.enable();
    let p = Pairing::start(rig.open_offer(), Identity::generate().unwrap(), "Laptop");
    p.wait_pending(&rig);
    rig.ui("peer_pair_answer", json!({"allow": false})).unwrap();
    assert!(matches!(p.finish(), Err(ClientError::Denied(DenyReason::Denied))));
    assert_eq!(rig.status()["devices"], 0);
    let none = rig.ui("peer_pair_answer", json!({"allow": false}));
    assert!(matches!(none, Err(csm_engine::ToolError::NotFound(_))), "nothing is waiting any more");
}

#[test]
fn nobody_answering_expires_the_request() {
    let rig = Rig::with(ServerConfig { pending_ttl: Duration::from_millis(600), ..quick() });
    rig.enable();
    let p = Pairing::start(rig.open_offer(), Identity::generate().unwrap(), "Laptop");
    p.wait_pending(&rig);
    assert!(matches!(p.finish(), Err(ClientError::Denied(DenyReason::Expired))));
    assert_eq!(rig.status()["devices"], 0);
}

#[test]
fn an_expired_offer_is_refused() {
    let rig = Rig::with(ServerConfig { pair_ttl: Duration::from_millis(300), ..quick() });
    rig.enable();
    let offer = rig.open_offer();
    assert!(rig.status()["pairing_open_secs"].is_u64());
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(rig.status()["pairing_open_secs"], Value::Null, "the offer is no longer open");
    let r = client::pair(&offer, &Identity::generate().unwrap(), &ClientConfig::new("Late"), &mut |_| {});
    assert!(matches!(r, Err(ClientError::Refused)), "{r:?}");
    assert_eq!(rig.status()["pending_pair"], Value::Null);
}

#[test]
fn an_offer_works_once() {
    let rig = Rig::new();
    rig.enable();
    let offer = rig.open_offer();
    let p = Pairing::start(offer.clone(), Identity::generate().unwrap(), "First");
    let code = p.wait_pending(&rig);
    rig.ui("peer_pair_answer", json!({"allow": true, "check_code": code})).unwrap();
    p.finish().unwrap();
    let r = client::pair(&offer, &Identity::generate().unwrap(), &ClientConfig::new("Second"), &mut |_| {});
    assert!(matches!(r, Err(ClientError::Refused)), "the slot is spent: {r:?}");
    assert_eq!(rig.status()["devices"], 1);
}

#[test]
fn a_wrong_offer_key_gives_three_tries_and_then_the_slot_is_dead() {
    let rig = Rig::new();
    rig.enable();
    let offer = rig.open_offer();
    // Same slot id, wrong secret: the handshake fails, and each try is counted.
    let mut forged = offer.clone();
    forged.p = csm_peer_proto::ids::b64_encode(&[7u8; 32]);
    for _ in 0..3 {
        let r = client::pair(&forged, &Identity::generate().unwrap(), &ClientConfig::new("Guesser"), &mut |_| {});
        assert!(matches!(r, Err(ClientError::Refused | ClientError::Link(_))), "{r:?}");
    }
    let r = client::pair(&offer, &Identity::generate().unwrap(), &ClientConfig::new("Real"), &mut |_| {});
    assert!(matches!(r, Err(ClientError::Refused)), "the real offer is dead after three bad tries: {r:?}");
    assert_eq!(rig.status()["devices"], 0);
    // A new offer works.
    rig.pair("Real");
    assert_eq!(rig.status()["devices"], 1);
}

#[test]
fn pairing_needs_the_listener() {
    let rig = Rig::new();
    let r = rig.ui("peer_pair_start", json!({}));
    assert!(matches!(r, Err(csm_engine::ToolError::Refused(_))), "{r:?}");
    let r = rig.ui("peer_pair_answer", json!({"allow": false}));
    assert!(matches!(r, Err(csm_engine::ToolError::NotFound(_))), "{r:?}");
}

// ---- revoking and turning off -----------------------------------------------------------

#[test]
fn revoking_a_device_closes_its_link_and_it_cannot_return() {
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let conn = connect(&host, &me).unwrap();
    wait_for("connected", || (rig.status()["connected"] == 1).then_some(()));
    let out = rig.engine.call(Caller::External { client: "an-agent".into(), session_id: None }, "peer_device_revoke", json!({"id": host.device_id})).unwrap();
    assert_eq!(out.data["revoked"], 1, "closing a door is allowed for an agent too");
    assert_eq!(wait_closed(&conn), Some(ByeReason::Revoked));
    assert!(conn.call_wait("session_list", json!({}), Duration::from_secs(2)).is_err());
    assert_eq!(rig.status()["devices"], 0);
    assert_eq!(rig.status()["connected"], 0);
    let r = connect(&host, &me);
    assert!(matches!(r, Err(ClientError::Refused)), "{r:?}");
    let missing = rig.ui("peer_device_revoke", json!({"id": host.device_id}));
    assert!(matches!(missing, Err(csm_engine::ToolError::NotFound(_))));
}

#[test]
fn revoke_all_closes_every_link_and_forgets_every_key() {
    let rig = Rig::new();
    rig.enable();
    let (a, ha) = rig.pair("Laptop");
    let (b, hb) = rig.pair("Tablet");
    let (ca, cb) = (connect(&ha, &a).unwrap(), connect(&hb, &b).unwrap());
    wait_for("both connected", || (rig.status()["connected"] == 2).then_some(()));
    let out = rig.ui("peer_device_revoke", json!({"all": true})).unwrap();
    assert_eq!(out.data["revoked"], 2);
    assert_eq!(wait_closed(&ca), Some(ByeReason::Revoked));
    assert_eq!(wait_closed(&cb), Some(ByeReason::Revoked));
    assert!(connect(&ha, &a).is_err() && connect(&hb, &b).is_err());
    let on_disk = std::fs::read_to_string(rig.peer_dir.join("devices.json")).unwrap();
    assert!(!on_disk.contains(&ha.device_id) && !on_disk.contains(&hb.device_id));
}

#[test]
fn revoking_one_leaves_the_other_connected() {
    let rig = Rig::new();
    rig.enable();
    let (a, ha) = rig.pair("Laptop");
    let (b, hb) = rig.pair("Tablet");
    let (ca, cb) = (connect(&ha, &a).unwrap(), connect(&hb, &b).unwrap());
    wait_for("both connected", || (rig.status()["connected"] == 2).then_some(()));
    rig.ui("peer_device_revoke", json!({"id": ha.device_id})).unwrap();
    assert_eq!(wait_closed(&ca), Some(ByeReason::Revoked));
    assert!(matches!(cb.call_wait("session_list", json!({}), Duration::from_secs(5)).unwrap(), Outcome::Ok { .. }));
}

#[test]
fn turning_it_off_closes_the_listener_and_every_connection() {
    let rig = Rig::new();
    let addr = rig.enable();
    let (me, host) = rig.pair("Laptop");
    let conn = connect(&host, &me).unwrap();
    let open_offer = rig.open_offer();
    wait_for("connected", || (rig.status()["connected"] == 1).then_some(()));
    // The person may turn it off (an agent may not: SEC-37).
    rig.engine.call(Caller::Cli, "peer_server_disable", json!({})).unwrap();
    assert_eq!(wait_closed(&conn), Some(ByeReason::ServerOff));
    assert!(refuses_connections(addr), "nothing listens any more");
    TcpListener::bind(addr).expect("the port is free again");
    let s = rig.status();
    assert_eq!((s["enabled"].clone(), s["listening"].clone(), s["pairing_open_secs"].clone()), (json!(false), Value::Null, Value::Null));
    assert_eq!(s["devices"], 1, "turning off does not unpair");
    let r = client::pair(&open_offer, &Identity::generate().unwrap(), &ClientConfig::new("Late"), &mut |_| {});
    assert!(matches!(r, Err(ClientError::Connect(_))), "{r:?}");
    // Turning it on again needs the person again, and the old offer is dead.
    let port = addr.port();
    rig.ui("peer_server_enable", json!({"address": "127.0.0.1", "port": port})).unwrap();
    let again = rig.server.local_addr().unwrap();
    let r = client::pair(&open_offer, &Identity::generate().unwrap(), &ClientConfig::new("Late"), &mut |_| {});
    assert!(matches!(r, Err(ClientError::Refused)), "{r:?}");
    assert!(connect(&PairedHost { address: peer_addr(again), ..host.clone() }, &me).is_ok(), "the paired device still works");
}

#[test]
fn the_app_closing_says_goodbye() {
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let conn = connect(&host, &me).unwrap();
    wait_for("connected", || (rig.status()["connected"] == 1).then_some(()));
    rig.server.shutdown();
    assert_eq!(wait_closed(&conn), Some(ByeReason::ServerQuit));
}

#[test]
fn a_second_connection_of_the_same_device_replaces_the_first() {
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let first = connect(&host, &me).unwrap();
    wait_for("connected", || (rig.status()["connected"] == 1).then_some(()));
    let second = connect(&host, &me).unwrap();
    assert_eq!(wait_closed(&first), Some(ByeReason::Replaced));
    assert!(matches!(second.call_wait("session_list", json!({}), Duration::from_secs(5)).unwrap(), Outcome::Ok { .. }));
    assert_eq!(rig.status()["connected"], 1);
}

// ---- the session link -------------------------------------------------------------------

#[test]
fn the_host_key_is_pinned() {
    let rig = Rig::new();
    let addr = rig.enable();
    let (me, host) = rig.pair("Laptop");
    // Another machine answering on the same address has another key: the handshake fails.
    let impostor = Identity::generate().unwrap();
    let r = client::connect(&peer_addr(addr), impostor.public(), &me, &ClientConfig::new("Laptop"));
    assert!(r.is_err(), "a host with another key is not talked to");
    assert!(connect(&host, &me).is_ok());
}

#[test]
fn a_different_terminal_version_is_told_so() {
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let cfg = ClientConfig { term_version: "99.0.0".into(), ..ClientConfig::new("Laptop") };
    let r = client::connect(&host.address, &host.host_public, &me, &cfg);
    assert!(matches!(r, Err(ClientError::Bye(ByeReason::TermMismatch))), "{r:?}");
}

#[test]
fn pings_keep_a_quiet_link_alive() {
    // The dead time is wide enough that a loaded machine running every test binary at once
    // does not starve a ping past it; the sleep is still more than twice that time.
    let rig = Rig::with(ServerConfig { ping_every: Duration::from_millis(150), dead_after: Duration::from_millis(2000), ..quick() });
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let cfg = ClientConfig { ping_every: Duration::from_millis(150), dead_after: Duration::from_millis(2000), ..ClientConfig::new("Laptop") };
    let conn = client::connect(&host.address, &host.host_public, &me, &cfg).unwrap();
    std::thread::sleep(Duration::from_millis(4500));
    assert!(!conn.is_closed(), "pings kept it open for twice the dead time");
    assert!(matches!(conn.call_wait("session_list", json!({}), Duration::from_secs(5)).unwrap(), Outcome::Ok { .. }));
}

#[test]
fn many_messages_cross_a_rekey() {
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let conn = connect(&host, &me).unwrap();
    // More than the rekey interval in each direction: the keys change under live traffic.
    let n = csm_peer_proto::noise::REKEY_INTERVAL + 50;
    // Eight at a time: the server runs at most 16 calls at once and refuses the rest.
    let mut ok = 0u64;
    let mut sent = 0u64;
    while sent < n {
        let batch = (n - sent).min(8);
        let waiting: Vec<_> = (0..batch).map(|_| conn.call("tool_list", json!({"toolset": "layout"})).unwrap()).collect();
        for rx in waiting {
            if let Outcome::Ok { .. } = rx.recv_timeout(Duration::from_secs(30)).unwrap() {
                ok += 1;
            }
        }
        sent += batch;
    }
    assert_eq!(ok, n);
}

#[test]
fn a_big_reply_is_split_and_a_huge_one_becomes_an_error() {
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let conn = connect(&host, &me).unwrap();
    // Enough groups that their list is bigger than one Noise message (65 535 bytes).
    for i in 0..700 {
        rig.ui("group_new", json!({"name": format!("A group with a long enough name to add up, number {i}")})).unwrap();
    }
    let out = conn.call_wait("group_list", json!({}), Duration::from_secs(20)).unwrap();
    match out {
        Outcome::Ok { data, .. } => {
            assert!(data.to_string().len() > 65_535, "this reply needed fragments, it was {} bytes", data.to_string().len());
            assert!(data.to_string().contains("number 699"), "and arrived whole");
        }
        other => panic!("{other:?}"),
    }
    // The link still works after fragments.
    assert!(matches!(conn.call_wait("session_list", json!({}), Duration::from_secs(5)).unwrap(), Outcome::Ok { .. }));
}

#[test]
fn a_device_below_full_cannot_change_anything() {
    // Levels other than Full are not issued yet, but the server maps what the list says.
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let conn = connect(&host, &me).unwrap();
    // Edit the list on disk to make it a viewer, as a future pairing would: the engine's level gate holds.
    let path = rig.peer_dir.join("devices.json");
    let text = std::fs::read_to_string(&path).unwrap().replace("\"full\"", "\"viewer\"");
    std::fs::write(&path, text).unwrap();
    drop(conn);
    // A fresh server instance reads the list; the connection carries the viewer level.
    rig.server.shutdown();
    let again = Rig::with(quick());
    std::fs::create_dir_all(&again.peer_dir).unwrap();
    for f in ["host.key", "devices.json", "server.json"] {
        std::fs::copy(rig.peer_dir.join(f), again.peer_dir.join(f)).unwrap();
    }
    again.server.resume();
    wait_for("listener", || again.server.local_addr());
    let addr = again.server.local_addr().unwrap();
    let conn = client::connect(&peer_addr(addr), &host.host_public, &me, &ClientConfig::new("Laptop")).unwrap();
    assert_eq!(conn.welcome.level, Level::Viewer);
    let t = Duration::from_secs(5);
    assert!(matches!(conn.call_wait("session_list", json!({}), t).unwrap(), Outcome::Ok { .. }));
    assert!(matches!(conn.call_wait("group_new", json!({"name": "no"}), t).unwrap(), Outcome::Err { kind: ErrorKind::Refused, .. }));
}

// ---- consent over the link --------------------------------------------------------------

/// What one connection has delivered, kept so a search for one message never discards another.
struct Inbox<'a> {
    c: &'a client::Connection,
    seen: Vec<Msg>,
}

impl<'a> Inbox<'a> {
    fn new(c: &'a client::Connection) -> Self {
        Inbox { c, seen: Vec::new() }
    }

    fn find<T>(&mut self, what: &str, pick: impl Fn(&Msg) -> Option<T>) -> T {
        let end = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(i) = self.seen.iter().position(|m| pick(m).is_some()) {
                let m = self.seen.remove(i);
                return pick(&m).unwrap();
            }
            assert!(Instant::now() < end, "no {what} arrived");
            if let Some(Incoming::Frame(Frame::Control(m))) = self.c.recv_timeout(Duration::from_millis(100)) {
                self.seen.push(m);
            }
        }
    }

    fn card(&mut self) -> u64 {
        self.find("consent card", |m| match m {
            Msg::ConsentCard { request } => request["request_id"].as_u64(),
            _ => None,
        })
    }

    fn ack(&mut self) -> (u64, ConsentOutcome) {
        self.find("consent ack", |m| match m {
            Msg::ConsentAck { request_id, outcome } => Some((*request_id, *outcome)),
            _ => None,
        })
    }

    fn resolved(&mut self) -> u64 {
        self.find("card resolved", |m| match m {
            Msg::Event { event: Some(Event::ConsentResolved { request_id }) } => Some(*request_id),
            _ => None,
        })
    }
}

fn agent_asks(rig: &Rig) -> mpsc::Receiver<Result<csm_engine::ToolOutput, csm_engine::ToolError>> {
    let path = rig.peer_dir.parent().unwrap().join("agent.csm");
    rig.engine.submit(Caller::External { client: "an-agent".into(), session_id: None }, "state_export", json!({"path": path.to_string_lossy()}))
}

#[test]
fn a_consent_card_reaches_the_laptop_and_the_first_answer_wins() {
    let rig = Rig::new();
    rig.enable();
    let (a, ha) = rig.pair("Laptop");
    let (b, hb) = rig.pair("Tablet");
    let (ca, cb) = (connect(&ha, &a).unwrap(), connect(&hb, &b).unwrap());
    let (mut ia, mut ib) = (Inbox::new(&ca), Inbox::new(&cb));
    wait_for("both connected", || (rig.status()["connected"] == 2).then_some(()));
    let result = agent_asks(&rig);
    let (ida, idb) = (ia.card(), ib.card());
    assert_eq!(ida, idb, "the same card on both screens");
    // The laptop answers first and allows it; the tablet is too late.
    ca.answer_consent(ida, true).unwrap();
    assert_eq!(ia.ack(), (ida, ConsentOutcome::Applied));
    assert!(result.recv_timeout(Duration::from_secs(10)).unwrap().is_ok(), "the agent's call ran");
    cb.answer_consent(idb, false).unwrap();
    assert_eq!(ib.ack(), (idb, ConsentOutcome::AlreadyResolved));
    // The card is withdrawn everywhere.
    assert_eq!(ib.resolved(), idb, "the tablet is told the card is gone");
    assert_eq!(ia.resolved(), ida);
    let log = std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).unwrap();
    assert!(log.contains("consent_answered") && log.contains("Laptop"), "who answered is logged:\n{log}");
}

#[test]
fn a_denial_from_the_laptop_declines_the_agent() {
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let conn = connect(&host, &me).unwrap();
    let mut inbox = Inbox::new(&conn);
    wait_for("connected", || (rig.status()["connected"] == 1).then_some(()));
    let result = agent_asks(&rig);
    let id = inbox.card();
    conn.answer_consent(id, false).unwrap();
    assert_eq!(inbox.ack(), (id, ConsentOutcome::Applied));
    assert!(matches!(result.recv_timeout(Duration::from_secs(10)).unwrap(), Err(csm_engine::ToolError::Declined(_))));
}

#[test]
fn a_card_already_waiting_is_shown_to_a_device_that_connects_later() {
    let rig = Rig::new();
    rig.enable();
    let (me, host) = rig.pair("Laptop");
    let events = rig.engine.subscribe();
    let result = agent_asks(&rig);
    while !matches!(events.recv_timeout(Duration::from_secs(5)), Ok(csm_engine::Event::ConsentRequested { .. })) {}
    let conn = connect(&host, &me).unwrap();
    let mut inbox = Inbox::new(&conn);
    let id = inbox.card();
    rig.ui("consent_answer", json!({"request_id": id, "answer": "deny"})).unwrap();
    assert!(result.recv_timeout(Duration::from_secs(10)).unwrap().is_err());
    conn.answer_consent(id, true).unwrap();
    assert_eq!(inbox.ack(), (id, ConsentOutcome::AlreadyResolved));
}

// ---- fix round 1 ----------------------------------------------------------------------

#[test]
fn two_bad_tries_then_the_real_pairing_still_works_on_the_third() {
    let rig = Rig::new();
    rig.enable();
    let offer = rig.open_offer();
    let mut forged = offer.clone();
    forged.p = csm_peer_proto::ids::b64_encode(&[7u8; 32]);
    for _ in 0..2 {
        let r = client::pair(&forged, &Identity::generate().unwrap(), &ClientConfig::new("Guesser"), &mut |_| {});
        assert!(matches!(r, Err(ClientError::Refused | ClientError::Link(_))), "{r:?}");
    }
    let me = Identity::generate().unwrap();
    let p = Pairing::start(offer, me, "Third try");
    let code = p.wait_pending(&rig);
    rig.ui("peer_pair_answer", json!({"allow": true, "check_code": code})).unwrap();
    assert!(p.finish().is_ok(), "the third handshake is allowed");
    assert_eq!(rig.status()["devices"], 1);
}

#[test]
fn a_failed_re_enable_clears_the_saved_choice_so_a_restart_binds_nothing() {
    let rig = Rig::new();
    rig.enable();
    let r = rig.ui("peer_server_enable", json!({"address": "203.0.113.7", "port": 0}));
    assert!(r.is_err(), "not an address of this machine");
    let s = rig.status();
    assert_eq!(s["enabled"], false, "{s}");
    assert_eq!(s["listening"], Value::Null);
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(rig.peer_dir.join("server.json")).unwrap()).unwrap();
    assert_eq!(saved["enabled"], false, "what the person saw is what is saved: {saved}");
    let fresh = PeerServer::with_config(rig.peer_dir.clone(), quick());
    fresh.resume();
    assert!(fresh.local_addr().is_none(), "a fresh start listens nowhere");
    fresh.shutdown();
}

#[test]
fn a_refusal_before_remote_access_ever_existed_still_leaves_a_log_line() {
    let rig = Rig::new();
    assert!(!rig.peer_dir.exists());
    let r = rig.engine.call(Caller::Cli, "peer_server_enable", json!({"address": "127.0.0.1", "port": 0}));
    assert!(matches!(r, Err(csm_engine::ToolError::Refused(_))));
    let log = std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).unwrap();
    assert!(log.contains("\"refused\"") && log.contains("peer_server_enable"), "{log}");
}
