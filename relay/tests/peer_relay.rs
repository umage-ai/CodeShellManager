#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A CSM peer through the relay, in one process: the real relay on 127.0.0.1, the real peer server
//! (the office) and the real client (the laptop). Both reach the relay outbound only. The direct
//! address in the offer is made unreachable, so the laptop can only get in through the relay.
//! The terminal is a fake PTY (as in csm-peer's stream tests); everything else is the real thing.

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, PeerHost, State, StateOptions};
use csm_peer::client::{self, ClientConfig, Connection, Incoming, PairedHost};
use csm_peer::identity::Identity;
use csm_peer::{PeerServer, RelayConfig, ServerConfig};
use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::codec::{Frame, Input};
use csm_peer_proto::msg::{Level, Outcome};
use csm_peer_proto::pairing::{PeerOffer, RelayRoute};
use csm_pty::{Backend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_relay::config::Config;
use csm_relay::{Relay, serve};
use csm_remote_proto::relay::{ClientToRelay, HostToRelay, RelayToClient, RelayToHost, to_text};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio_tungstenite::tungstenite::Message;

const WAIT: Duration = Duration::from_secs(20);
const STREAM: u32 = 1;
/// The office's relay identity: its secret proves the relay host id.
const OFFICE_RELAY_SECRET: [u8; 32] = [0x42; 32];
/// A port nothing listens on: the direct address the laptop cannot reach.
const DEAD_DIRECT: &str = "tcp://127.0.0.1:9";

/// The relay on an ephemeral loopback port, served on its own runtime thread.
struct RelayRig {
    addr: SocketAddr,
    _stop: mpsc::Sender<()>,
    _relay: Arc<Relay>,
}

fn start_relay() -> RelayRig {
    let mut c = Config::default();
    c.access.open = true;
    c.limits.new_connections_per_ip_per_minute = 1000;
    c.limits.max_sockets_per_ip = 1000;
    let relay = Relay::new(c.into_settings().unwrap(), None);
    let (addr_tx, addr_rx) = mpsc::channel();
    let (stop, stop_rx) = mpsc::channel::<()>();
    let r = relay.clone();
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            addr_tx.send(listener.local_addr().unwrap()).unwrap();
            let _ = serve(listener, r, async move {
                let _ = tokio::task::spawn_blocking(move || stop_rx.recv()).await;
            })
            .await;
        });
    });
    RelayRig { addr: addr_rx.recv().unwrap(), _stop: stop, _relay: relay }
}

// ---- a fake PTY (the terminal under test) ---------------------------------------------------------

struct FakePty {
    size: Mutex<PtySize>,
    sink: Mutex<Option<OutputSink>>,
    backlog: Mutex<Vec<u8>>,
    written: Mutex<Vec<u8>>,
    exited: AtomicBool,
}

impl FakePty {
    fn emit(&self, bytes: &[u8]) {
        let mut sink = self.sink.lock().unwrap();
        match sink.as_mut() {
            Some(s) => s(bytes),
            None => self.backlog.lock().unwrap().extend_from_slice(bytes),
        }
    }
    fn written(&self) -> Vec<u8> {
        self.written.lock().unwrap().clone()
    }
}

impl Pty for FakePty {
    fn subscribe(&self, mut sink: OutputSink) {
        let backlog = std::mem::take(&mut *self.backlog.lock().unwrap());
        if !backlog.is_empty() {
            sink(&backlog);
        }
        *self.sink.lock().unwrap() = Some(sink);
    }
    fn on_exit(&self, _cb: ExitCallback) {}
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.written.lock().unwrap().extend_from_slice(bytes);
        Ok(())
    }
    fn resize(&self, size: PtySize) -> io::Result<()> {
        *self.size.lock().unwrap() = size;
        Ok(())
    }
    fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
    fn exit_code(&self) -> Option<i32> {
        None
    }
    fn kill(&self) {
        self.exited.store(true, Ordering::SeqCst);
    }
    fn backend(&self) -> Backend {
        Backend::BundledConPty
    }
    fn pid(&self) -> Option<u32> {
        Some(1)
    }
}

/// The office: a real engine with a real peer server, listening on loopback and reachable through
/// the relay as well.
struct Office {
    _tmp: tempfile::TempDir,
    folder: PathBuf,
    server: PeerServer,
    engine: Engine,
    ptys: Arc<Mutex<Vec<Arc<FakePty>>>>,
}

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + WAIT;
    while Instant::now() < end {
        if let Some(v) = f() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("timed out waiting for {what}");
}

fn office(relay: &RelayRig) -> Office {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("work");
    std::fs::create_dir_all(&folder).unwrap();
    let cfg = ServerConfig { handshake_timeout: Duration::from_secs(5), ..ServerConfig::default() };
    let server = PeerServer::with_config(tmp.path().join("peer"), cfg);
    let host: Arc<dyn PeerHost> = Arc::new(server.clone());
    let ptys: Arc<Mutex<Vec<Arc<FakePty>>>> = Arc::new(Mutex::new(Vec::new()));
    let list = ptys.clone();
    let options = StateOptions {
        restore_on_start: false,
        peer_host: host,
        pty: Arc::new(move |opts: SpawnOptions| {
            let p = Arc::new(FakePty {
                size: Mutex::new(opts.size),
                sink: Mutex::new(None),
                backlog: Mutex::new(Vec::new()),
                written: Mutex::new(Vec::new()),
                exited: AtomicBool::new(false),
            });
            list.lock().unwrap().push(p.clone());
            Ok(p as Arc<dyn Pty>)
        }),
        first_size_timeout: Duration::from_millis(30),
        save_debounce: Duration::from_millis(10),
        exit_wait: Duration::from_millis(500),
        shutdown_budget: Duration::from_millis(500),
        ..StateOptions::default()
    };
    let (state, _report) = State::load(options);
    let engine = Engine::start(state, vec![]).unwrap();
    server.attach(engine.clone());
    engine.call(Caller::Ui, "peer_server_enable", json!({"address": "127.0.0.1", "port": 0})).unwrap();
    let relay_cfg = RelayConfig { url: format!("ws://{}", relay.addr), host_secret: OFFICE_RELAY_SECRET, access: None };
    server.set_relay(Some(relay_cfg)).unwrap();
    wait_for("the office connected to the relay", || server.relay_status().filter(|s| s.connected).map(|_| ()));
    Office { _tmp: tmp, folder, server, engine, ptys }
}

impl Office {
    fn status(&self) -> Value {
        self.engine.call(Caller::Cli, "peer_status", json!({})).unwrap().data
    }

    /// A live session on a fake PTY: its id and the PTY.
    fn session(&self) -> (String, Arc<FakePty>) {
        let before = self.ptys.lock().unwrap().len();
        let out = self.engine.call(Caller::Ui, "session_new", json!({"folder": self.folder.to_string_lossy(), "command": "fake"})).unwrap();
        let id = out.data["id"].as_str().unwrap().to_owned();
        let pty = wait_for("the fake PTY", || self.ptys.lock().unwrap().get(before).cloned());
        (id, pty)
    }
}

impl Drop for Office {
    fn drop(&mut self) {
        self.server.shutdown();
        self.engine.shutdown();
    }
}

/// The whole pairing from a laptop that can reach only the relay.
fn pair_through_relay(office: &Office) -> (Identity, PairedHost) {
    let out = office.engine.call(Caller::Ui, "peer_pair_start", json!({})).unwrap();
    let mut offer = PeerOffer::decode(out.data["link"].as_str().unwrap()).unwrap();
    assert!(offer.r.is_some(), "the offer names the relay");
    offer.a = DEAD_DIRECT.to_owned();
    let me = Identity::generate().unwrap();
    let (tx, rx) = mpsc::channel::<String>();
    let cfg = ClientConfig { pair_wait: Duration::from_secs(20), ..ClientConfig::new("Laptop") };
    let me2 = me.clone();
    let handle = std::thread::spawn(move || client::pair(&offer, &me2, &cfg, &mut |c: &str| drop(tx.send(c.to_owned()))));
    let code = rx.recv_timeout(WAIT).expect("the laptop reached the check code through the relay");
    wait_for("the office shows the waiting device", || (office.status()["pending_pair"]["check_code"].as_str() == Some(code.as_str())).then_some(()));
    office.engine.call(Caller::Ui, "peer_pair_answer", json!({"allow": true, "check_code": code})).unwrap();
    let host = handle.join().unwrap().expect("the pairing finished");
    (me, host)
}

fn route(host: &PairedHost) -> RelayRoute {
    host.relay.clone().expect("the paired host has a relay route")
}

fn closed_by(conn: &Connection) -> bool {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if let Some(Incoming::Closed(_)) = conn.recv_timeout(Duration::from_millis(100)) {
            return true;
        }
    }
    false
}

/// Terminal text the laptop has received on `conn` so far, read until `marker` is in it.
fn read_until(conn: &Connection, marker: &str) -> Option<String> {
    let end = Instant::now() + WAIT;
    let mut text = String::new();
    while Instant::now() < end {
        match conn.recv_timeout(Duration::from_millis(100)) {
            Some(Incoming::Frame(Frame::TermData { bytes, .. })) => text.push_str(&String::from_utf8_lossy(&bytes)),
            Some(Incoming::Frame(Frame::TermAttach { vt, .. })) => text.push_str(&String::from_utf8_lossy(&vt)),
            Some(Incoming::Closed(_)) => return None,
            _ => {}
        }
        if text.contains(marker) {
            return Some(text);
        }
    }
    None
}

#[test]
fn a_laptop_pairs_and_works_through_the_relay_only() {
    let relay = start_relay();
    let office = office(&relay);
    let (me, host) = pair_through_relay(&office);
    assert_eq!(host.level, Level::Full);
    let r = route(&host);
    assert_eq!(r.url, format!("ws://{}", relay.addr));
    assert_eq!(r.host_id, csm_remote_proto::ids::host_id(&OFFICE_RELAY_SECRET));

    // A later connection: the direct address is unreachable, so the relay carries it.
    let direct = PeerAddr::parse(DEAD_DIRECT).unwrap();
    let cfg = ClientConfig::new("Laptop");
    let conn = client::connect_saved(Some(&direct), Some(&r), &host.host_public, &me, &cfg).expect("connected through the relay");
    assert_eq!(conn.welcome.device_id, host.device_id);
    let t = Duration::from_secs(10);
    assert!(matches!(conn.call_wait("session_list", json!({}), t).unwrap(), Outcome::Ok { .. }));

    // Revoking the device closes its relay channel and keeps it out.
    office.engine.call(Caller::Ui, "peer_device_revoke", json!({"id": host.device_id})).unwrap();
    assert!(closed_by(&conn), "the revoked connection ends");
    assert!(client::connect_saved(None, Some(&r), &host.host_public, &me, &cfg).is_err(), "a revoked device cannot come back");
}

#[test]
fn attach_and_typing_work_through_the_relay() {
    let relay = start_relay();
    let office = office(&relay);
    let (me, host) = pair_through_relay(&office);
    let (id, pty) = office.session();
    // Output from before the laptop looks: it comes with the attach.
    pty.emit(b"before the attach\r\n");
    let r = route(&host);
    let direct = PeerAddr::parse(DEAD_DIRECT).unwrap();
    let conn = client::connect_saved(Some(&direct), Some(&r), &host.host_public, &me, &ClientConfig::new("Laptop")).unwrap();
    conn.subscribe(STREAM, &id, None).unwrap();
    assert!(read_until(&conn, "before the attach").is_some(), "the attach arrives through the relay");
    // Live output after it.
    pty.emit(b"live through the relay\r\n");
    assert!(read_until(&conn, "live through the relay").is_some(), "live bytes arrive through the relay");
    // Typing reaches the session. Bracketed paste is on, so pasted text is wrapped.
    pty.emit(b"\x1b[?2004h$ ");
    assert!(read_until(&conn, "$ ").is_some(), "the prompt arrives");
    conn.input(STREAM, Input::Key(b"ls\r".to_vec())).unwrap();
    wait_for("the keys reach the PTY", || (pty.written() == b"ls\r").then_some(()));
    // Untrusted paste is bracketed by the office, whatever the text says.
    conn.input(STREAM, Input::Paste("a\x1b[201~b".to_owned())).unwrap();
    wait_for("the paste reaches the PTY", || String::from_utf8_lossy(&pty.written()).ends_with("\x1b[201~").then_some(()));
    let written = String::from_utf8_lossy(&pty.written()).into_owned();
    assert_eq!(written.matches("\x1b[201~").count(), 1, "only the office's own closing bracket: {written:?}");
}

#[test]
fn a_burst_of_output_crosses_the_relay_without_ending_the_channel() {
    let relay = start_relay();
    let office = office(&relay);
    let (me, host) = pair_through_relay(&office);
    let (id, pty) = office.session();
    let r = route(&host);
    let direct = PeerAddr::parse(DEAD_DIRECT).unwrap();
    let conn = client::connect_saved(Some(&direct), Some(&r), &host.host_public, &me, &ClientConfig::new("Laptop")).unwrap();
    conn.subscribe(STREAM, &id, None).unwrap();
    pty.emit(b"ready\r\n");
    assert!(read_until(&conn, "ready").is_some());
    // Two megabytes at once (a `cat` of a big file), then a marker. The office paces the writes to
    // the relay's per-channel cap, so the burst waits rather than tripping the relay's limit.
    let chunk = vec![b'x'; 16 * 1024];
    for _ in 0..128 {
        pty.emit(&chunk);
    }
    pty.emit(b"\r\nEND-OF-BURST\r\n");
    assert!(read_until(&conn, "END-OF-BURST").is_some(), "the marker after the burst arrives: nothing was dropped at the tail");
    assert!(!conn.is_closed(), "the channel survived the burst");
    assert!(matches!(conn.call_wait("session_list", json!({}), Duration::from_secs(10)).unwrap(), Outcome::Ok { .. }));
}

#[test]
fn a_direct_address_that_answers_is_used_before_the_relay() {
    let relay = start_relay();
    let office = office(&relay);
    let (me, host) = pair_through_relay(&office);
    // The relay route here is dead: if the laptop tried the relay first, or only the relay, this
    // would fail. The direct address answers, so it is the route taken.
    let dead_relay = RelayRoute { url: "ws://127.0.0.1:9".to_owned(), host_id: route(&host).host_id };
    let direct = PeerAddr::parse(&format!("tcp://{}", office.server.local_addr().unwrap())).unwrap();
    let conn = client::connect_saved(Some(&direct), Some(&dead_relay), &host.host_public, &me, &ClientConfig::new("Laptop")).expect("the direct route answers");
    assert_eq!(conn.welcome.device_id, host.device_id);
    // With no direct answer, the same dead relay is the only route, and it fails.
    let dead_direct = PeerAddr::parse(DEAD_DIRECT).unwrap();
    assert!(client::connect_saved(Some(&dead_direct), Some(&dead_relay), &host.host_public, &me, &ClientConfig::new("Laptop")).is_err());
    // And the live relay carries it when the direct address is dead.
    assert!(client::connect_saved(Some(&dead_direct), Some(&route(&host)), &host.host_public, &me, &ClientConfig::new("Laptop")).is_ok());
}

// ---- phone and peer channels are separate -----------------------------------------------------------

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open(addr: SocketAddr, path: &str) -> Ws {
    let (ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}{path}")).await.unwrap();
    ws
}

async fn reply_to_client(ws: &mut Ws) -> Option<RelayToClient> {
    let next = tokio::time::timeout(WAIT, ws.next()).await.ok()??;
    match next.ok()? {
        Message::Text(t) => serde_json::from_str(t.as_str()).ok(),
        _ => None,
    }
}

async fn send_client_hello(ws: &mut Ws, host_id: &str, token: Option<[u8; 32]>) {
    let hello = ClientToRelay::Hello {
        v: csm_remote_proto::PROTOCOL_VERSION,
        host_id: host_id.to_owned(),
        token: token.map(|t| csm_remote_proto::ids::b64_encode(&t)),
        pairing_id: None,
    };
    ws.send(Message::Text(to_text(&hello).into())).await.unwrap();
}

#[test]
fn a_phone_cannot_open_a_peer_channel_and_a_peer_cannot_open_a_phone_channel() {
    let relay = start_relay();
    let office = office(&relay);
    let office_id = csm_remote_proto::ids::host_id(&OFFICE_RELAY_SECRET);
    // A phone host on the phone namespace, with its own id.
    let phone_secret = [0x24u8; 32];
    let phone_id = csm_remote_proto::ids::host_id(&phone_secret);
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let mut phone_host = open(relay.addr, "/v1/host").await;
        let hello = HostToRelay::Hello { v: csm_remote_proto::PROTOCOL_VERSION, host_id: phone_id.clone(), host_secret: csm_remote_proto::ids::b64_encode(&phone_secret), access: None };
        phone_host.send(Message::Text(to_text(&hello).into())).await.unwrap();
        let welcome = tokio::time::timeout(WAIT, phone_host.next()).await.unwrap().unwrap().unwrap();
        assert!(matches!(welcome, Message::Text(t) if serde_json::from_str::<RelayToHost>(t.as_str()).is_ok_and(|m| matches!(m, RelayToHost::Welcome { .. }))));

        // The office's id is a peer host: a phone (`/v1/client`) asking for it is refused.
        let mut phone = open(relay.addr, "/v1/client").await;
        send_client_hello(&mut phone, &office_id, Some([7; 32])).await;
        assert!(matches!(reply_to_client(&mut phone).await, Some(RelayToClient::Error { .. })), "a phone cannot reach the office's peer host");

        // The phone host's id is a phone host: a peer client (`/v1/peer-client`) asking for it is refused.
        let mut peer = open(relay.addr, "/v1/peer-client").await;
        send_client_hello(&mut peer, &phone_id, Some([7; 32])).await;
        assert!(matches!(reply_to_client(&mut peer).await, Some(RelayToClient::Error { .. })), "a peer cannot reach a phone host");
        drop(phone_host);
    });
    drop(office);
}
