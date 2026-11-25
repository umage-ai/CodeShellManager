#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! A CSM peer through a relay, in one process: the real relay binary on 127.0.0.1, the real peer
//! server (the office) and the real client (the laptop). Both reach the relay outbound only. The
//! direct address in the offer is made unreachable, so the laptop can only get in through the relay.
//! The terminal is a fake PTY (as in csm-peer's stream tests); everything else is the real thing.
//!
//! The relay is `csm-relay`, built from the private repository `umage-ai/csm-relay`. It is not a
//! dependency of CSM: point `CSM_RELAY_BIN` at its binary. The tests are `#[ignore]`d, so the
//! normal `cargo test` never needs it; run them with `--ignored` (see `docs/design/remote-csm.md`).

use std::io;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, PeerHost, State, StateOptions};
use csm_peer::client::{self, ClientConfig, Connection, Incoming, PairedHost};
use csm_peer::identity::Identity;
use csm_peer::{PeerServer, RelayConfig, ServerConfig};
use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::codec::{Frame, Input};
use csm_peer_proto::ids::{hex, sha256};
use csm_peer_proto::limits::{UPLOAD_CHUNK, UPLOAD_CREDIT};
use csm_peer_proto::msg::{Level, Msg, Outcome, UploadKind};
use csm_peer_proto::pairing::{PeerOffer, RelayRoute};
use csm_pty::{Backend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);
const STREAM: u32 = 1;
/// The office's relay identity: its secret proves the relay host id.
const OFFICE_RELAY_SECRET: [u8; 32] = [0x42; 32];
/// A port nothing listens on: the direct address the laptop cannot reach.
const DEAD_DIRECT: &str = "tcp://127.0.0.1:9";
/// The environment variable that names the csm-relay binary.
const RELAY_BIN_VAR: &str = "CSM_RELAY_BIN";

/// The dev configuration of the relay: loopback, admits every host, no push. The limits are raised
/// so a test's connections are never refused by the rate limits.
fn dev_config(port: u16) -> String {
    format!(
        "bind = \"127.0.0.1:{port}\"\n\
         [access]\nopen = true\n\
         [limits]\nnew_connections_per_ip_per_minute = 1000\nmax_sockets_per_ip = 1000\n\
         [push]\nenabled = false\n"
    )
}

/// The relay binary from `CSM_RELAY_BIN`. Panics with the fix when it is not set.
fn relay_bin() -> PathBuf {
    match std::env::var_os(RELAY_BIN_VAR) {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => panic!(
            "{RELAY_BIN_VAR} is not set: build csm-relay (repository umage-ai/csm-relay, `cargo build -p csm-relay`) \
             and set {RELAY_BIN_VAR} to the csm-relay binary, then run these tests with --ignored"
        ),
    }
}

/// A free loopback port: bind port 0, read it, release it.
fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

/// The relay process on a loopback port, started from the dev config and stopped on drop.
struct RelayRig {
    addr: SocketAddr,
    child: Child,
    _tmp: tempfile::TempDir,
}

impl Drop for RelayRig {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_relay() -> RelayRig {
    let bin = relay_bin();
    let tmp = tempfile::tempdir().unwrap();
    let port = free_port();
    let config = tmp.path().join("relay.toml");
    std::fs::write(&config, dev_config(port)).unwrap();
    let child = Command::new(&bin)
        .arg("--config")
        .arg(&config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap_or_else(|e| panic!("could not start {}: {e}", bin.display()));
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut rig = RelayRig { addr, child, _tmp: tmp };
    // The relay is up once its port accepts a connection (it exits at once if the config is bad).
    let end = Instant::now() + WAIT;
    loop {
        if TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok() {
            break;
        }
        if let Ok(Some(status)) = rig.child.try_wait() {
            panic!("csm-relay exited before it listened: {status}");
        }
        assert!(Instant::now() < end, "csm-relay did not listen on {addr}");
        std::thread::sleep(Duration::from_millis(50));
    }
    rig
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
        self.engine.call(Caller::Ui, "peer_status", json!({})).unwrap().data
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
#[ignore = "needs CSM_RELAY_BIN: the csm-relay binary (umage-ai/csm-relay)"]
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
#[ignore = "needs CSM_RELAY_BIN: the csm-relay binary (umage-ai/csm-relay)"]
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
#[ignore = "needs CSM_RELAY_BIN: the csm-relay binary (umage-ai/csm-relay)"]
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
#[ignore = "needs CSM_RELAY_BIN: the csm-relay binary (umage-ai/csm-relay)"]
fn a_direct_address_that_answers_is_used_before_the_relay() {
    let relay = start_relay();
    let office = office(&relay);
    let (me, host) = pair_through_relay(&office);
    // The relay route here is dead: if the laptop tried the relay first, or only the relay, this
    // would fail. The direct address answers, so it is the route taken.
    let dead_relay = RelayRoute { url: "ws://127.0.0.1:9".to_owned(), host_id: route(&host).host_id, token: None };
    let direct = PeerAddr::parse(&format!("tcp://{}", office.server.local_addr().unwrap())).unwrap();
    let conn = client::connect_saved(Some(&direct), Some(&dead_relay), &host.host_public, &me, &ClientConfig::new("Laptop")).expect("the direct route answers");
    assert_eq!(conn.welcome.device_id, host.device_id);
    // With no direct answer, the same dead relay is the only route, and it fails.
    let dead_direct = PeerAddr::parse(DEAD_DIRECT).unwrap();
    assert!(client::connect_saved(Some(&dead_direct), Some(&dead_relay), &host.host_public, &me, &ClientConfig::new("Laptop")).is_err());
    // And the live relay carries it when the direct address is dead.
    assert!(client::connect_saved(Some(&dead_direct), Some(&route(&host)), &host.host_public, &me, &ClientConfig::new("Laptop")).is_ok());
}

#[test]
#[ignore = "needs CSM_RELAY_BIN: the csm-relay binary (umage-ai/csm-relay)"]
fn a_slow_laptop_reader_keeps_the_link_and_the_final_screen() {
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
    // The laptop stops reading while the office writes 4 MiB: more than the window (2 MiB), so the
    // office waits on the laptop's acks instead of the relay dropping the channel for a full queue.
    // The writes happen on their own thread, so a waiting office cannot hold the test.
    let writer_pty = pty.clone();
    let writer = std::thread::spawn(move || {
        let chunk = vec![b'y'; 16 * 1024];
        for _ in 0..256 {
            writer_pty.emit(&chunk);
        }
    });
    std::thread::sleep(Duration::from_millis(800));
    assert!(!conn.is_closed(), "a slow reader does not end the channel");
    // The laptop reads again: the link is live, and the final screen has the marker.
    let marker_pty = pty.clone();
    let marker = std::thread::spawn(move || marker_pty.emit(b"\r\nFINAL-MARK\r\n"));
    assert!(read_until(&conn, "FINAL-MARK").is_some(), "the final output arrives after the slow reader catches up");
    writer.join().unwrap();
    marker.join().unwrap();
    assert!(!conn.is_closed(), "the channel survived the backlog");
    assert!(matches!(conn.call_wait("session_list", json!({}), Duration::from_secs(10)).unwrap(), Outcome::Ok { .. }));
}

// ---- uploads through the relay (design: "Uploads", section 10, step 2) ------------------------

/// A file's bytes, in a pattern that is not all one value.
fn pattern(n: usize) -> Vec<u8> {
    (0..n).map(|i| u8::try_from(i % 251).unwrap_or(0).wrapping_mul(7)).collect()
}

/// One file of a drop, as the laptop sends it.
struct Upload {
    id: u64,
    name: &'static str,
    data: Vec<u8>,
    digest: String,
    sent: usize,
    acked: usize,
    accepted: bool,
    ended: bool,
    done: bool,
}

/// Sends a drop over `conn` the way the laptop does: the files begin together, their chunks take
/// turns, each file stays inside its credit, and each ends with its digest. Terminal output goes on
/// meanwhile, and the largest gap between two terminal frames is returned. Panics on a refusal or a
/// closed channel.
fn upload_drop_while_streaming(conn: &Connection, session: &str, pty: &FakePty, files: &[(&'static str, Vec<u8>)]) -> Duration {
    let mut ups: Vec<Upload> = files
        .iter()
        .enumerate()
        .map(|(i, (name, data))| Upload { id: i as u64 + 1, name, data: data.clone(), digest: hex(&sha256(data)), sent: 0, acked: 0, accepted: false, ended: false, done: false })
        .collect();
    for u in &ups {
        conn.send(Msg::UploadBegin { id: u.id, session: session.to_owned(), name: u.name.to_owned(), size: u.data.len() as u64, kind: UploadKind::File }).unwrap();
    }
    let end = Instant::now() + Duration::from_secs(180);
    let (mut last, mut max_gap) = (Instant::now(), Duration::ZERO);
    let (mut next_line, mut lines) = (Instant::now(), 0usize);
    while !ups.iter().all(|u| u.done) {
        assert!(Instant::now() < end, "the drop did not finish");
        assert!(!conn.is_closed(), "the relay closed the channel during the upload");
        for u in ups.iter_mut().filter(|u| u.accepted && !u.done) {
            let chunks = u.data.len().div_ceil(UPLOAD_CHUNK);
            if u.sent < chunks && u.sent.saturating_sub(u.acked) < UPLOAD_CREDIT {
                let from = u.sent * UPLOAD_CHUNK;
                let to = (from + UPLOAD_CHUNK).min(u.data.len());
                conn.send_frame(&Frame::UploadChunk { id: u.id, offset: from as u64, bytes: u.data[from..to].to_vec() }).unwrap();
                u.sent += 1;
            } else if u.sent == chunks && !u.ended {
                conn.send(Msg::UploadEnd { id: u.id, sha256: u.digest.clone() }).unwrap();
                u.ended = true;
            }
        }
        if Instant::now() >= next_line && lines < 300 {
            pty.emit(format!("tick {lines}\r\n").as_bytes());
            lines += 1;
            next_line = Instant::now() + Duration::from_millis(50);
        }
        match conn.recv_timeout(Duration::from_millis(2)) {
            Some(Incoming::Frame(Frame::TermData { .. })) => {
                let now = Instant::now();
                max_gap = max_gap.max(now.duration_since(last));
                last = now;
            }
            Some(Incoming::Frame(Frame::Control(m))) => match m {
                Msg::UploadAccepted { id } => {
                    if let Some(u) = ups.iter_mut().find(|u| u.id == id) {
                        u.accepted = true;
                    }
                }
                Msg::UploadAck { id, received } => {
                    if let Some(u) = ups.iter_mut().find(|u| u.id == id) {
                        u.acked = u.acked.max(usize::try_from(received).unwrap_or(usize::MAX).div_ceil(UPLOAD_CHUNK));
                    }
                }
                Msg::UploadDone { id, .. } => {
                    if let Some(u) = ups.iter_mut().find(|u| u.id == id) {
                        u.done = true;
                    }
                }
                Msg::UploadRefused { id, reason } => panic!("upload {id} was refused: {reason:?}"),
                _ => {}
            },
            Some(Incoming::Closed(r)) => panic!("the link closed ({r:?}) during the upload"),
            _ => {}
        }
    }
    max_gap
}

/// A 24 MiB file and a small one, through the real relay. The first is more than the relay's
/// 16 MiB burst for a peer channel (8 MiB/s, burst of 2 seconds), so a laptop that sent it as fast as
/// the office acknowledged would get the channel closed with `RateLimited`. The laptop paces its bulk
/// frames, so the channel stays open, and the terminal keeps streaming while the drop goes.
#[test]
#[ignore = "needs CSM_RELAY_BIN: the csm-relay binary (umage-ai/csm-relay)"]
fn a_big_relayed_upload_keeps_the_channel_and_the_terminal_stream() {
    let relay = start_relay();
    let office = office(&relay);
    let (me, host) = pair_through_relay(&office);
    let (id, pty) = office.session();
    let r = route(&host);
    let direct = PeerAddr::parse(DEAD_DIRECT).unwrap();
    let conn = client::connect_saved(Some(&direct), Some(&r), &host.host_public, &me, &ClientConfig::new("Laptop")).unwrap();
    conn.subscribe(STREAM, &id, None).unwrap();
    pty.emit(b"ready\r\n");
    assert!(read_until(&conn, "ready").is_some(), "the attach arrives");
    let files = [("big.bin", pattern(24 << 20)), ("small.txt", pattern(1 << 20))];
    let max_gap = upload_drop_while_streaming(&conn, &id, &pty, &files);
    assert!(!conn.is_closed(), "the channel survived the upload");
    assert!(max_gap < Duration::from_secs(3), "terminal output stalled for {max_gap:?} during the upload");
    pty.emit(b"after the upload\r\n");
    assert!(read_until(&conn, "after the upload").is_some(), "the terminal still streams after the upload");
    assert!(matches!(conn.call_wait("session_list", json!({}), Duration::from_secs(10)).unwrap(), Outcome::Ok { .. }), "calls still work");
}
