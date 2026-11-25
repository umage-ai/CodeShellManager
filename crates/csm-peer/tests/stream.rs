//! Serving sessions to a device (design 3 and 5): the snapshot and its deltas, terminal streams on
//! a fake PTY (attach, live bytes, typing, size, resume, overflow and resync), and consent cards
//! answered from a device. Real TCP on 127.0.0.1 with ephemeral ports, a real engine, the real
//! server and client; only the PTY is fake.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, PeerHost, State, StateOptions, TerminalHandle};
use csm_peer::client::{self, ClientConfig, Connection, Incoming, PairedHost, TermFollower, TermUpdate};
use csm_peer::identity::Identity;
use csm_peer::{PeerServer, ServerConfig};
use csm_peer_proto::Snapshot as WireSnapshot;
use csm_peer_proto::codec::{Frame, Input};
use csm_peer_proto::ids::{hex, sha256};
use csm_peer_proto::limits::{MAX_UPLOAD_FILE, UPLOAD_CHUNK, UPLOAD_CREDIT};
use csm_peer_proto::msg::{ByeReason, ConsentOutcome, Msg, Outcome, TermClosedReason, UploadKind, UploadRefused as UploadRefusedKind};
use csm_peer_proto::pairing::PeerOffer;
use csm_pty::{Backend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_term::replay;
use csm_term::{GridSize, ReplySink, TermConfig, Terminal};
use serde_json::{Value, json};

// ---- a fake PTY -------------------------------------------------------------------------------

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
    fn size(&self) -> (u16, u16) {
        let s = *self.size.lock().unwrap();
        (s.cols, s.rows)
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

// ---- the rig ----------------------------------------------------------------------------------

struct Rig {
    _tmp: tempfile::TempDir,
    peer_dir: PathBuf,
    folder: PathBuf,
    server: PeerServer,
    engine: Engine,
    ptys: Arc<Mutex<Vec<Arc<FakePty>>>>,
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
        let peer_dir = tmp.path().join("peer");
        let folder = tmp.path().join("work");
        std::fs::create_dir_all(&folder).unwrap();
        let server = PeerServer::with_config(peer_dir.clone(), cfg);
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
        let (state, _) = State::load(options);
        let engine = Engine::start(state, vec![]).unwrap();
        server.attach(engine.clone());
        Rig { _tmp: tmp, peer_dir, folder, server, engine, ptys }
    }

    fn ui(&self, tool: &str, params: Value) -> Result<csm_engine::ToolOutput, csm_engine::ToolError> {
        self.engine.call(Caller::Ui, tool, params)
    }

    fn status(&self) -> Value {
        self.engine.call(Caller::Cli, "peer_status", json!({})).unwrap().data
    }

    fn enable(&self) -> SocketAddr {
        self.ui("peer_server_enable", json!({"address": "127.0.0.1", "port": 0})).unwrap();
        self.server.local_addr().unwrap()
    }

    /// A live session on a fake PTY: its id, the PTY and the handle the server streams from.
    fn session(&self) -> (String, Arc<FakePty>, TerminalHandle) {
        let before = self.ptys.lock().unwrap().len();
        let out = self.ui("session_new", json!({"folder": self.folder.to_string_lossy(), "command": "fake"})).unwrap();
        let id = out.data["id"].as_str().unwrap().to_owned();
        let pty = wait_for("the fake PTY", || self.ptys.lock().unwrap().get(before).cloned());
        let handle = wait_for("the terminal", || self.engine.terminal(&id));
        (id, pty, handle)
    }

    fn pair(&self, name: &str) -> (Identity, PairedHost) {
        let out = self.ui("peer_pair_start", json!({})).unwrap();
        let offer = PeerOffer::decode(out.data["link"].as_str().unwrap()).unwrap();
        let me = Identity::generate().unwrap();
        let (tx, rx) = mpsc::channel();
        let cfg = ClientConfig { pair_wait: Duration::from_secs(20), ..ClientConfig::new(name) };
        let me2 = me.clone();
        let handle = std::thread::spawn(move || client::pair(&offer, &me2, &cfg, &mut |c| drop(tx.send(c.to_owned()))));
        let code = wait_for("a device waiting to pair", || self.status()["pending_pair"]["check_code"].as_str().map(str::to_owned));
        assert_eq!(code, rx.recv_timeout(Duration::from_secs(10)).unwrap());
        self.ui("peer_pair_answer", json!({"allow": true, "check_code": code, "trusted": true})).unwrap();
        (me, handle.join().unwrap().unwrap())
    }

    /// A paired device, connected, with the first snapshot read.
    fn device(&self, name: &str) -> Device {
        let (me, host) = self.pair(name);
        Device { me, host, name: name.to_owned() }
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.server.shutdown();
        self.engine.shutdown();
    }
}

struct Device {
    me: Identity,
    host: PairedHost,
    name: String,
}

impl Device {
    fn connect(&self) -> View {
        let conn = client::connect(&self.host.address, &self.host.host_public, &self.me, &ClientConfig::new(&self.name)).unwrap();
        View::new(conn)
    }
}

fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < end, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ---- the client's side of a connection --------------------------------------------------------

struct Discard;
impl ReplySink for Discard {
    fn send(&self, _bytes: &[u8]) {}
}

/// A connection plus what a client does with it: keeps the snapshot current, and follows stream 1
/// into its own emulator (replies discarded, as the design says).
struct View {
    conn: Connection,
    snapshot: Option<WireSnapshot>,
    follower: TermFollower,
    term: Option<Terminal>,
    attaches: usize,
    resizes: Vec<(u16, u16)>,
    closed: Vec<TermClosedReason>,
    gaps: usize,
    msgs: Vec<Msg>,
    byes: Vec<Option<ByeReason>>,
    data_frames: usize,
}

const STREAM: u32 = 1;

impl View {
    fn new(conn: Connection) -> View {
        View {
            conn,
            snapshot: None,
            follower: TermFollower::new(STREAM),
            term: None,
            attaches: 0,
            resizes: vec![],
            closed: vec![],
            gaps: 0,
            msgs: vec![],
            byes: vec![],
            data_frames: 0,
        }
    }

    /// Reconnects the same way an app would: a new connection, a follower that resumes.
    fn resumed(conn: Connection, point: (u64, u64), term: Terminal) -> View {
        let mut v = View::new(conn);
        v.follower = TermFollower::resuming(STREAM, point.0, point.1);
        v.term = Some(term);
        v
    }

    fn step(&mut self, wait: Duration) -> bool {
        let Some(incoming) = self.conn.recv_timeout(wait) else { return false };
        match incoming {
            Incoming::Closed(r) => self.byes.push(r),
            Incoming::Frame(frame) => {
                if let Frame::Control(Msg::Snapshot { snapshot }) = &frame {
                    self.snapshot = Some((**snapshot).clone());
                } else if let Frame::Control(Msg::Delta { delta }) = &frame {
                    delta.apply(self.snapshot.as_mut().expect("a delta before the snapshot")).unwrap();
                } else if let Frame::Control(m) = &frame
                    && !matches!(m, Msg::TermResized { .. } | Msg::TermClosed { .. })
                {
                    self.msgs.push(m.clone());
                }
                if let Frame::Control(Msg::TermClosed { reason, .. }) = &frame {
                    self.closed.push(*reason);
                }
                if matches!(frame, Frame::TermData { .. }) {
                    self.data_frames += 1;
                }
                match self.follower.on_frame(&frame).unwrap() {
                    TermUpdate::Attached(a) => {
                        self.attaches += 1;
                        let mut t = Terminal::new(GridSize::new(a.cols, a.rows), TermConfig::default(), Arc::new(Discard));
                        t.advance(&a.vt);
                        self.term = Some(t);
                    }
                    TermUpdate::Data(b) => self.term.as_mut().expect("data before an attach").advance(&b),
                    TermUpdate::Resized { cols, rows } => {
                        self.resizes.push((cols, rows));
                        self.term.as_mut().expect("a resize before an attach").resize(GridSize::new(cols, rows));
                    }
                    TermUpdate::Closed(_) => {}
                    TermUpdate::Gap => {
                        self.gaps += 1;
                        self.conn.resync(STREAM).unwrap();
                    }
                    TermUpdate::Ignored => {}
                }
            }
        }
        true
    }

    fn until(&mut self, what: &str, mut cond: impl FnMut(&mut View) -> bool) {
        let end = Instant::now() + Duration::from_secs(15);
        while !cond(self) {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            self.step(Duration::from_millis(20));
        }
    }

    /// Everything from the host for a moment (to prove that something does not arrive).
    fn settle(&mut self, ms: u64) {
        let end = Instant::now() + Duration::from_millis(ms);
        while Instant::now() < end {
            self.step(Duration::from_millis(10));
        }
    }

    /// The client's emulator has caught up with the server's and shows the same thing.
    fn in_lockstep(&mut self, server: &TerminalHandle) -> bool {
        let caught_up = self.follower.resume_point().is_some_and(|(_, o)| o == server.processed_offset());
        let Some(ours) = self.term.as_mut() else { return false };
        if !caught_up {
            return false;
        }
        let mut theirs = server.terminal().lock().unwrap();
        replay::snapshot(&mut theirs, 1000) == replay::snapshot(ours, 1000)
    }

    fn text(&mut self) -> String {
        let t = self.term.as_mut().expect("no terminal yet");
        let s = replay::snapshot(t, 1000);
        s.primary.screen.iter().map(|row| row.iter().map(|c| c.c).collect::<String>().trim_end().to_owned()).collect::<Vec<_>>().join("\n")
    }
}

fn lines(n: usize, tag: &str) -> Vec<u8> {
    (0..n).flat_map(|i| format!("{tag} line {i}\r\n").into_bytes()).collect()
}

// ---- snapshot and deltas ----------------------------------------------------------------------

#[test]
fn a_device_gets_the_snapshot_then_deltas_that_keep_it_equal() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (first, _, _) = rig.session();
    let mut v = dev.connect();
    v.until("the snapshot", |v| v.snapshot.is_some());
    assert!(v.snapshot.as_ref().unwrap().session(&first).is_some(), "the session that existed is in the snapshot");
    // Something changes on the office machine: a new session, a rename.
    let (second, _, _) = rig.session();
    rig.ui("session_rename", json!({"session_id": second, "name": "Renamed on the office machine"})).unwrap();
    v.until("the delta with the rename", |v| v.snapshot.as_ref().unwrap().session(&second).is_some_and(|s| s.name == "Renamed on the office machine"));
    // Once the publisher is quiet, the device's copy is the engine's view of the same version.
    v.until("the same version", |v| {
        let mine = v.snapshot.as_ref().unwrap();
        let theirs = WireSnapshot::from(&*rig.engine.view());
        mine.version == theirs.version && *mine == theirs
    });
}

#[test]
fn a_toast_reaches_the_device_but_the_offices_own_events_do_not() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let mut v = dev.connect();
    v.until("the snapshot", |v| v.snapshot.is_some());
    rig.ui("toast_show", json!({"message": "hello from the office"})).ok();
    rig.ui("window_show", json!({})).ok();
    // Whatever the tool names are in this build, nothing but toasts, errors and screens is forwarded.
    v.settle(300);
    for m in &v.msgs {
        if let Msg::Event { event: Some(e) } = m {
            assert!(matches!(e, csm_peer_proto::Event::Toast { .. } | csm_peer_proto::Event::Error { .. } | csm_peer_proto::Event::ScreenChanged { .. }), "{e:?}");
        }
    }
}

#[test]
fn a_tool_call_from_a_device_runs_on_the_office_engine() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, _) = rig.session();
    let v = dev.connect();
    let out = v.conn.call_wait("session_send_text", json!({"session_id": id, "text": "echo hi", "submit": true}), Duration::from_secs(10)).unwrap();
    assert!(matches!(out, Outcome::Ok { .. }), "{out:?}");
    wait_for("the text in the PTY", || String::from_utf8_lossy(&pty.written()).contains("echo hi").then_some(()));
    // The device cannot turn server mode on, whatever it says.
    let out = v.conn.call_wait("peer_server_enable", json!({"address": "127.0.0.1", "port": 0}), Duration::from_secs(10)).unwrap();
    assert!(matches!(out, Outcome::Err { .. }));
}

// ---- terminal streams -------------------------------------------------------------------------

#[test]
fn a_subscription_attaches_the_screen_then_streams_live_bytes() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    // Output from before the device looked: it must come with the attach.
    pty.emit(b"\x1b[1;31mred\x1b[0m before\r\n");
    wait_for("the emulator", || (handle.processed_offset() > 0).then_some(()));
    let mut v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("the attach", |v| v.attaches == 1);
    assert!(v.text().contains("red before"));
    // Live bytes after it.
    pty.emit(&lines(5, "live"));
    v.until("the live bytes", |v| v.in_lockstep(&handle));
    assert!(v.text().contains("live line 4"), "{}", v.text());
    assert_eq!(v.attaches, 1, "no second attach for plain output");
    assert!(v.data_frames > 0);
    // A scrollback worth of output, then a client that joins late gets all of it in one attach.
    pty.emit(&lines(300, "old"));
    wait_for("the emulator", || (handle.processed_offset() >= handle.output_end()).then_some(()));
    let mut late = dev.connect(); // replaces the first connection of the same device
    late.conn.subscribe(STREAM, &id, None).unwrap();
    late.until("the late attach", |v| v.attaches == 1 && v.in_lockstep(&handle));
    assert!(late.text().contains("old line 299"));
}

#[test]
fn typed_input_reaches_the_session_and_pasted_text_is_bracketed() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    pty.emit(b"\x1b[?2004h$ ");
    wait_for("bracketed paste mode", || (handle.processed_offset() > 0).then_some(()));
    let mut v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("the attach", |v| v.attaches == 1);
    v.conn.input(STREAM, Input::Key(b"ls\r".to_vec())).unwrap();
    wait_for("the keys", || (pty.written() == b"ls\r").then_some(()));
    v.conn.input(STREAM, Input::Paste("a\nb".to_owned())).unwrap();
    wait_for("the paste", || (pty.written().ends_with(b"\x1b[200~a\rb\x1b[201~")).then_some(()));
    // Text that tries to close the bracket early cannot: the terminal handle strips every ESC.
    v.conn.input(STREAM, Input::Paste("x\x1b[201~rm -rf /".to_owned())).unwrap();
    wait_for("the hostile paste", || String::from_utf8_lossy(&pty.written()).contains("rm -rf /").then_some(()));
    let w = String::from_utf8_lossy(&pty.written()).into_owned();
    assert_eq!(w.matches("\x1b[201~").count(), 2, "only the app's own closing brackets: {w:?}");
    v.conn.input(STREAM, Input::Mouse(b"\x1b[<0;1;1M".to_vec())).unwrap();
    wait_for("the mouse report", || String::from_utf8_lossy(&pty.written()).ends_with("\x1b[<0;1;1M").then_some(()));
    // Typing is audited as a byte count, never as content.
    v.conn.close();
    wait_for("the audit line", || {
        let log = std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).ok()?;
        log.contains("\"event\":\"input\"").then_some(log)
    });
    let log = std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).unwrap();
    assert!(!log.contains("rm -rf"), "typed content must never be logged");
}

#[test]
fn the_device_that_types_decides_the_pty_size_and_the_other_follows_in_stream_order() {
    let rig = Rig::new();
    rig.enable();
    let (laptop, tablet) = (rig.device("Laptop"), rig.device("Tablet"));
    let (id, pty, handle) = rig.session();
    pty.emit(b"before the resize\r\n");
    let (mut a, mut b) = (laptop.connect(), tablet.connect());
    a.conn.subscribe(STREAM, &id, None).unwrap();
    b.conn.subscribe(STREAM, &id, None).unwrap();
    a.until("laptop attach", |v| v.attaches == 1);
    b.until("tablet attach", |v| v.attaches == 1);
    // The laptop says it drives: the PTY follows, and both are told at the same byte.
    a.conn.resize(STREAM, 100, 30, true).unwrap();
    wait_for("the PTY at 100x30", || (pty.size() == (100, 30)).then_some(()));
    pty.emit(b"after the first resize\r\n");
    a.until("laptop lockstep", |v| v.in_lockstep(&handle));
    b.until("tablet lockstep", |v| v.in_lockstep(&handle));
    assert_eq!(b.resizes, vec![(100, 30)]);
    // The tablet only asks: the PTY does not move.
    b.conn.resize(STREAM, 60, 20, false).unwrap();
    b.settle(200);
    assert_eq!(pty.size(), (100, 30), "a viewer that does not drive does not resize the PTY");
    // Once the tablet types, it drives, and gets the size it asked for.
    b.conn.input(STREAM, Input::Key(b"x".to_vec())).unwrap();
    wait_for("the PTY at 60x20", || (pty.size() == (60, 20)).then_some(()));
    pty.emit(b"after the second resize\r\n");
    a.until("laptop lockstep 2", |v| v.in_lockstep(&handle));
    b.until("tablet lockstep 2", |v| v.in_lockstep(&handle));
    assert_eq!(a.resizes, vec![(100, 30), (60, 20)]);
    assert_eq!(handle.grid_size().cols, 60);
    assert_eq!(a.gaps + b.gaps, 0, "marks always came at the byte the data had reached");
}

#[test]
fn a_dropped_connection_resumes_from_its_offset_without_a_new_attach() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    let mut v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("attach", |v| v.attaches == 1);
    pty.emit(&lines(20, "one"));
    v.until("lockstep", |v| v.in_lockstep(&handle));
    let point = v.follower.resume_point().unwrap();
    // The link drops (the laptop sleeps). Output goes on at the office, and the PTY resizes.
    v.conn.close();
    drop(v.conn);
    let term = v.term.take().unwrap();
    pty.emit(&lines(20, "two"));
    handle.resize_cells(90, 25);
    pty.emit(&lines(5, "three"));
    wait_for("the emulator", || (handle.processed_offset() >= handle.output_end()).then_some(()));
    // Reconnect with (instance, offset): no attach, the bytes in between, the resize in order.
    let mut again = View::resumed(dev.connect().conn, point, term);
    again.conn.subscribe(STREAM, &id, Some(point)).unwrap();
    again.until("lockstep after the resume", |v| v.in_lockstep(&handle));
    assert_eq!(again.attaches, 0, "the ring still held everything: a plain resume");
    assert_eq!(again.resizes, vec![(90, 25)]);
    assert!(again.text().contains("three line 4"));
}

#[test]
fn an_offset_the_ring_no_longer_holds_gets_a_fresh_attach() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    let mut v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("attach", |v| v.attaches == 1);
    pty.emit(&lines(10, "kept"));
    v.until("lockstep", |v| v.in_lockstep(&handle));
    let point = v.follower.resume_point().unwrap();
    v.conn.close();
    drop(v.conn);
    let term = v.term.take().unwrap();
    // More than the ring (1 MiB) while the device is away.
    let chunk = lines(2000, "flood");
    for _ in 0..40 {
        pty.emit(&chunk);
    }
    wait_for("the emulator", || (handle.processed_offset() >= handle.output_end()).then_some(()));
    assert!(!handle.can_resume(point.0, point.1), "the ring has moved on");
    let mut again = View::resumed(dev.connect().conn, point, term);
    again.conn.subscribe(STREAM, &id, Some(point)).unwrap();
    again.until("a fresh attach that matches", |v| v.attaches == 1 && v.in_lockstep(&handle));
    assert!(again.text().contains("flood line"));
    assert_eq!(again.gaps, 0);
}

#[test]
fn a_stream_that_falls_behind_resyncs_instead_of_queueing() {
    let rig = Rig::with(ServerConfig { stream_backlog_bytes: 4 * 1024, ..quick() });
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    let mut v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("attach", |v| v.attaches == 1);
    // 200 KiB in one burst: far more than the backlog the server is willing to queue.
    pty.emit(&lines(8000, "burst"));
    v.until("a resync and lockstep", |v| v.attaches >= 2 && v.in_lockstep(&handle));
    assert!(v.text().contains("burst line 7999"));
    assert_eq!(v.gaps, 0, "the server resynced by itself; the client never saw a hole");
    // And the stream still works afterwards.
    pty.emit(b"still alive\r\n");
    v.until("lockstep", |v| v.in_lockstep(&handle));
    assert!(v.text().contains("still alive"));
}

#[test]
fn a_client_can_ask_for_a_resync() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    let mut v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("attach", |v| v.attaches == 1);
    pty.emit(b"one\r\n");
    v.until("lockstep", |v| v.in_lockstep(&handle));
    v.conn.resync(STREAM).unwrap();
    v.until("the second attach", |v| v.attaches == 2 && v.in_lockstep(&handle));
    pty.emit(b"two\r\n");
    v.until("lockstep", |v| v.in_lockstep(&handle));
    assert!(v.text().contains("one") && v.text().contains("two"));
}

#[test]
fn unknown_sessions_limits_unsubscribe_and_a_closed_session_end_the_stream() {
    let rig = Rig::with(ServerConfig { max_streams: 2, ..quick() });
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, _pty, _handle) = rig.session();
    let (id2, _, _) = rig.session();
    let (id3, _, _) = rig.session();
    let mut v = dev.connect();
    v.conn.subscribe(7, "no-such-session", None).unwrap();
    v.until("unknown session", |v| v.closed == vec![TermClosedReason::UnknownSession]);
    v.closed.clear();
    // The stream limit: the third distinct stream is refused.
    v.conn.subscribe(1, &id, None).unwrap();
    v.conn.subscribe(2, &id2, None).unwrap();
    v.conn.subscribe(3, &id3, None).unwrap();
    v.until("the limit", |v| v.closed.contains(&TermClosedReason::Forbidden));
    // Unsubscribing answers, and frees a slot.
    v.closed.clear();
    v.conn.unsubscribe(2).unwrap();
    v.until("unsubscribed", |v| v.closed.contains(&TermClosedReason::Unsubscribed));
    // A closed session ends its stream with SessionGone.
    v.closed.clear();
    rig.ui("session_close", json!({"session_id": id})).unwrap();
    v.until("session gone", |v| v.closed.contains(&TermClosedReason::SessionGone) || v.closed.contains(&TermClosedReason::NotRunning));
}

#[test]
fn a_dead_connection_stops_its_streams_and_gives_up_the_size() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    let v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.conn.resize(STREAM, 77, 33, true).unwrap();
    wait_for("the size", || (pty.size() == (77, 33)).then_some(()));
    let rx_before = handle.output_end();
    drop(v);
    wait_for("the device to disconnect", || (rig.status()["connected"] == 0).then_some(()));
    // The PTY keeps the size it has; nothing crashes, and output still flows for the office.
    pty.emit(b"still going\r\n");
    wait_for("output", || (handle.output_end() > rx_before).then_some(()));
    assert_eq!(pty.size(), (77, 33));
}

// ---- consent ----------------------------------------------------------------------------------

#[test]
fn a_device_answers_a_consent_card_and_the_office_card_closes() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let mut v = dev.connect();
    v.until("the snapshot", |v| v.snapshot.is_some());
    let office_ui = rig.engine.subscribe();
    let path = rig.peer_dir.parent().unwrap().join("agent.csm");
    let result = rig.engine.submit(Caller::External { client: "an-agent".into(), session_id: None }, "state_export", json!({"path": path.to_string_lossy()}));
    // The office UI gets the card (as an event) and so does the device.
    let ui_card = loop {
        match office_ui.recv_timeout(Duration::from_secs(5)) {
            Ok(csm_engine::Event::ConsentRequested { request }) => break request["request_id"].as_u64().unwrap(),
            Ok(_) => {}
            Err(e) => panic!("the office UI never saw the card: {e}"),
        }
    };
    v.until("the card on the device", |v| v.msgs.iter().any(|m| matches!(m, Msg::ConsentCard { request } if request["request_id"].as_u64() == Some(ui_card))));
    assert_eq!(rig.ui("consent_list", json!({})).unwrap().data.as_array().unwrap().len(), 1, "waiting on the office screen too");
    // The device answers; the office card is resolved and the agent's call runs.
    v.conn.answer_consent(ui_card, true).unwrap();
    v.until("the ack", |v| v.msgs.iter().any(|m| matches!(m, Msg::ConsentAck { outcome: ConsentOutcome::Applied, .. })));
    assert!(result.recv_timeout(Duration::from_secs(10)).unwrap().is_ok());
    loop {
        match office_ui.recv_timeout(Duration::from_secs(5)) {
            Ok(csm_engine::Event::ConsentResolved { request_id }) if request_id == ui_card => break,
            Ok(_) => {}
            Err(e) => panic!("the office card was not closed: {e}"),
        }
    }
    assert!(rig.ui("consent_list", json!({})).unwrap().data.as_array().unwrap().is_empty());
}

// ---- the follower, alone ----------------------------------------------------------------------

#[test]
fn the_follower_reports_a_hole_once_and_waits_for_the_attach() {
    let mut f = TermFollower::new(5);
    let attach = Frame::TermAttach { stream: 5, instance: 9, cols: 80, rows: 24, offset: 100, more: false, vt: b"x".to_vec() };
    assert!(matches!(f.on_frame(&attach).unwrap(), TermUpdate::Attached(_)));
    assert_eq!(f.resume_point(), Some((9, 100)));
    let data = |offset: u64| Frame::TermData { stream: 5, instance: 9, offset, bytes: b"abc".to_vec() };
    assert_eq!(f.on_frame(&data(100)).unwrap(), TermUpdate::Data(b"abc".to_vec()));
    assert_eq!(f.resume_point(), Some((9, 103)));
    // A hole: reported once, then ignored until the attach.
    assert_eq!(f.on_frame(&data(200)).unwrap(), TermUpdate::Gap);
    assert_eq!(f.on_frame(&data(203)).unwrap(), TermUpdate::Ignored);
    assert!(matches!(f.on_frame(&attach).unwrap(), TermUpdate::Attached(_)));
    assert_eq!(f.on_frame(&data(100)).unwrap(), TermUpdate::Data(b"abc".to_vec()));
    // Another instance (the session restarted) is a hole too; other streams are not this one's.
    let other = Frame::TermData { stream: 5, instance: 10, offset: 103, bytes: b"z".to_vec() };
    assert_eq!(f.on_frame(&other).unwrap(), TermUpdate::Gap);
    let elsewhere = Frame::TermData { stream: 6, instance: 9, offset: 0, bytes: b"z".to_vec() };
    assert_eq!(f.on_frame(&elsewhere).unwrap(), TermUpdate::Ignored);
    // A resize must land exactly where the bytes have reached.
    let mut g = TermFollower::resuming(5, 9, 50);
    assert_eq!(g.on_frame(&Frame::Control(Msg::TermResized { stream: 5, offset: 50, cols: 90, rows: 25 })).unwrap(), TermUpdate::Resized { cols: 90, rows: 25 });
    assert_eq!(g.on_frame(&Frame::Control(Msg::TermResized { stream: 5, offset: 51, cols: 90, rows: 25 })).unwrap(), TermUpdate::Gap);
}

// ---- replaced streams, viewers, revoking ------------------------------------------------------

#[test]
fn replacing_a_stream_during_a_multi_chunk_attach_never_mixes_two_attaches() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    // An idle session with far more than one chunk of screen and scrollback.
    pty.emit(&lines(1500, "hist"));
    wait_for("the emulator", || (handle.processed_offset() >= handle.output_end()).then_some(()));
    let mut v = dev.connect();
    for _ in 0..4 {
        // Subscribe, unsubscribe and subscribe the same id again, and resync twice, back to back.
        v.conn.subscribe(STREAM, &id, None).unwrap();
        v.conn.unsubscribe(STREAM).unwrap();
        v.conn.subscribe(STREAM, &id, None).unwrap();
        v.conn.resync(STREAM).unwrap();
        v.conn.resync(STREAM).unwrap();
        v.settle(60);
    }
    // The follower panics on a malformed attach (the helper unwraps); reaching lockstep with no
    // gap means every attach on the wire was whole.
    v.until("lockstep", |v| v.attaches >= 1 && v.in_lockstep(&handle));
    assert_eq!(v.gaps, 0);
    assert!(v.text().contains("hist line 1499"));
    pty.emit(b"after\r\n");
    v.until("lockstep", |v| v.in_lockstep(&handle));
    assert!(v.text().contains("after"));
}

/// A server that lists the one paired device as a viewer, with a session on it.
fn viewer_rig() -> (Rig, Device) {
    let first = Rig::new();
    first.enable();
    let (me, host) = first.pair("Viewer");
    first.server.shutdown();
    let path = first.peer_dir.join("devices.json");
    let text = std::fs::read_to_string(&path).unwrap().replace("\"full\"", "\"viewer\"");
    std::fs::write(&path, text).unwrap();
    let again = Rig::with(quick());
    std::fs::create_dir_all(&again.peer_dir).unwrap();
    for f in ["host.key", "devices.json", "server.json"] {
        std::fs::copy(first.peer_dir.join(f), again.peer_dir.join(f)).unwrap();
    }
    again.server.resume();
    let addr = wait_for("the listener", || again.server.local_addr());
    let mut host = host;
    host.address = csm_peer_proto::addr::PeerAddr::parse(&format!("tcp://{addr}")).unwrap();
    (again, Device { me, host, name: "Viewer".to_owned() })
}

#[test]
fn a_viewer_can_watch_but_its_input_size_and_focus_are_ignored() {
    let (rig, dev) = viewer_rig();
    let (id, pty, handle) = rig.session();
    pty.emit(b"hello\r\n");
    wait_for("the emulator", || (handle.processed_offset() > 0).then_some(()));
    let before = pty.size();
    let mut v = dev.connect();
    assert_eq!(v.conn.welcome.level, csm_peer_proto::msg::Level::Viewer);
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("the attach", |v| v.attaches == 1);
    assert!(v.text().contains("hello"));
    v.conn.input(STREAM, Input::Key(b"rm\r".to_vec())).unwrap();
    v.conn.input(STREAM, Input::Paste("pasted".to_owned())).unwrap();
    v.conn.input(STREAM, Input::Mouse(b"\x1b[<0;1;1M".to_vec())).unwrap();
    v.conn.input(STREAM, Input::Focus(true)).unwrap();
    v.conn.resize(STREAM, 91, 37, true).unwrap();
    v.settle(300);
    assert!(pty.written().is_empty(), "a viewer typed into the session: {:?}", pty.written());
    assert_eq!(pty.size(), before, "a viewer resized the PTY");
    // It still sees live output.
    pty.emit(b"live\r\n");
    v.until("lockstep", |v| v.in_lockstep(&handle));
}

#[test]
fn revoking_a_device_with_live_streams_stops_them_and_writes_nothing_more() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    let mut v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("the attach", |v| v.attaches == 1);
    rig.ui("peer_device_revoke", json!({"id": dev.host.device_id})).unwrap();
    v.until("the goodbye", |v| !v.byes.is_empty());
    let frames = v.data_frames;
    let before = handle.output_end();
    pty.emit(&lines(50, "late"));
    wait_for("output", || (handle.output_end() > before).then_some(()));
    v.settle(300);
    assert_eq!(v.data_frames, frames, "data was written to a revoked device");
    assert_eq!(rig.status()["connected"], 0);
}

// ---- uploads (design: "Uploads: file drops and image paste", section 10, step 1) -----------------

/// Waits for a control message that matches, on a connection. Other frames are skipped.
fn expect_msg(conn: &Connection, what: &str, mut pred: impl FnMut(&Msg) -> bool) -> Msg {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        match conn.recv_timeout(Duration::from_millis(100)) {
            Some(Incoming::Frame(Frame::Control(m))) if pred(&m) => return m,
            Some(Incoming::Closed(r)) => panic!("the link closed ({r:?}) while waiting for {what}"),
            _ => {}
        }
    }
    panic!("no {what} arrived");
}

/// Waits briefly for a control message that matches; `None` if none did.
fn maybe_msg(conn: &Connection, wait: Duration, mut pred: impl FnMut(&Msg) -> bool) -> Option<Msg> {
    let end = Instant::now() + wait;
    while Instant::now() < end {
        if let Some(Incoming::Frame(Frame::Control(m))) = conn.recv_timeout(Duration::from_millis(50))
            && pred(&m)
        {
            return Some(m);
        }
    }
    None
}

fn accepted(id: u64) -> impl Fn(&Msg) -> bool {
    move |m| matches!(m, Msg::UploadAccepted { id: i } if *i == id)
}

fn refused(id: u64) -> impl Fn(&Msg) -> bool {
    move |m| matches!(m, Msg::UploadRefused { id: i, .. } if *i == id)
}

fn payload(n: usize) -> Vec<u8> {
    (0..n).map(|i| u8::try_from(i % 251).unwrap_or(0).wrapping_mul(13)).collect()
}

fn digest(bytes: &[u8]) -> String {
    hex(&sha256(bytes))
}

/// The names in a session's upload folder (partial and finished).
fn upload_files(rig: &Rig, session: &str) -> Vec<String> {
    let dir = rig.peer_dir.join("uploads").join(session);
    let mut names: Vec<String> = std::fs::read_dir(&dir).map(|d| d.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect()).unwrap_or_default();
    names.sort();
    names
}

fn begin(conn: &Connection, id: u64, session: &str, name: &str, size: u64, kind: UploadKind) {
    conn.send(Msg::UploadBegin { id, session: session.to_owned(), name: name.to_owned(), size, kind }).unwrap();
}

fn chunk_frame(id: u64, offset: usize, bytes: &[u8]) -> Frame {
    Frame::UploadChunk { id, offset: offset as u64, bytes: bytes.to_vec() }
}

#[test]
fn a_file_goes_in_chunks_and_lands_in_the_session_folder_with_a_sanitised_name() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    let data = payload(70_000);
    begin(&conn, 1, &session, "..\\..\\reports\\Q3 report.txt", data.len() as u64, UploadKind::File);
    expect_msg(&conn, "the accept", accepted(1));
    for (i, part) in data.chunks(UPLOAD_CHUNK).enumerate() {
        conn.send_frame(&chunk_frame(1, i * UPLOAD_CHUNK, part)).unwrap();
    }
    conn.send(Msg::UploadEnd { id: 1, sha256: digest(&data) }).unwrap();
    let Msg::UploadDone { path, .. } = expect_msg(&conn, "the done", |m| matches!(m, Msg::UploadDone { id: 1, .. })) else { panic!("not a done") };
    let placed = std::path::PathBuf::from(&path);
    assert_eq!(placed.parent().unwrap(), rig.peer_dir.join("uploads").join(&session), "under the session's folder");
    let name = placed.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.ends_with("-Q3 report.txt"), "the laptop's folders are stripped: {name}");
    assert_eq!(std::fs::read(&placed).unwrap(), data, "the bytes are the bytes that were sent");
    assert!(upload_files(&rig, &session).iter().all(|n| !n.ends_with(".part")), "no partial file is left");
    assert!(!rig.folder.join("Q3 report.txt").exists(), "never in the project folder");
    let log = std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).unwrap();
    assert!(log.contains("\"event\":\"upload_begin\"") && log.contains("\"event\":\"upload_done\""), "{log}");
}

#[test]
fn the_office_acknowledges_every_four_chunks_so_the_laptop_has_credit() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    let chunk = UPLOAD_CHUNK;
    let data = payload(chunk * 6);
    begin(&conn, 2, &session, "big.bin", data.len() as u64, UploadKind::File);
    expect_msg(&conn, "the accept", accepted(2));
    for i in 0..4 {
        conn.send_frame(&chunk_frame(2, i * chunk, &data[i * chunk..(i + 1) * chunk])).unwrap();
    }
    let ack = expect_msg(&conn, "the ack after four chunks", |m| matches!(m, Msg::UploadAck { id: 2, .. }));
    assert_eq!(ack, Msg::UploadAck { id: 2, received: (4 * chunk) as u64 });
    for i in 4..6 {
        conn.send_frame(&chunk_frame(2, i * chunk, &data[i * chunk..(i + 1) * chunk])).unwrap();
    }
    conn.send(Msg::UploadEnd { id: 2, sha256: digest(&data) }).unwrap();
    expect_msg(&conn, "the done", |m| matches!(m, Msg::UploadDone { id: 2, .. }));
}

#[test]
fn an_image_is_named_by_what_it_really_is_and_a_non_image_is_refused() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    png.extend_from_slice(&payload(300));
    begin(&conn, 3, &session, "../../evil.exe", png.len() as u64, UploadKind::Image);
    expect_msg(&conn, "the accept", accepted(3));
    conn.send_frame(&chunk_frame(3, 0, &png)).unwrap();
    conn.send(Msg::UploadEnd { id: 3, sha256: digest(&png) }).unwrap();
    let Msg::UploadDone { path, .. } = expect_msg(&conn, "the done", |m| matches!(m, Msg::UploadDone { id: 3, .. })) else { panic!("not a done") };
    assert!(path.ends_with("-clipboard.png"), "{path}");

    // Not an image: refused on its first chunk, and nothing is kept.
    begin(&conn, 4, &session, "x.png", 12, UploadKind::Image);
    expect_msg(&conn, "the accept", accepted(4));
    conn.send_frame(&chunk_frame(4, 0, b"%PDF-1.7 hi")).unwrap();
    assert_eq!(expect_msg(&conn, "the refusal", refused(4)), Msg::UploadRefused { id: 4, reason: UploadRefusedKind::NotAnImage });
    wait_for("the partial file to go", || upload_files(&rig, &session).iter().all(|n| !n.ends_with(".part")).then_some(()));
    assert_eq!(upload_files(&rig, &session).len(), 1, "only the finished image is there");
}

#[test]
fn hostile_uploads_are_refused_cleanly_and_the_link_stays_up() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    let file = UploadKind::File;

    // A session that does not exist, and folder names that are not plain ids.
    for (id, bad) in [(10, "nope"), (11, "../escape"), (12, "a\\b"), (13, "é")] {
        begin(&conn, id, bad, "x.txt", 1, file);
        assert_eq!(expect_msg(&conn, "a refusal", refused(id)), Msg::UploadRefused { id, reason: UploadRefusedKind::NoSession }, "{bad:?}");
    }

    // A digest that does not match the bytes: nothing is kept.
    let data = payload(1000);
    begin(&conn, 20, &session, "a.bin", data.len() as u64, file);
    expect_msg(&conn, "the accept", accepted(20));
    conn.send_frame(&chunk_frame(20, 0, &data)).unwrap();
    conn.send(Msg::UploadEnd { id: 20, sha256: digest(b"something else") }).unwrap();
    assert_eq!(expect_msg(&conn, "the refusal", refused(20)), Msg::UploadRefused { id: 20, reason: UploadRefusedKind::Corrupt });

    // Fewer bytes than declared at the end: corrupt, not done.
    begin(&conn, 21, &session, "short.bin", 500, file);
    expect_msg(&conn, "the accept", accepted(21));
    conn.send_frame(&chunk_frame(21, 0, &data[..100])).unwrap();
    conn.send(Msg::UploadEnd { id: 21, sha256: digest(&data[..100]) }).unwrap();
    assert_eq!(expect_msg(&conn, "the refusal", refused(21)), Msg::UploadRefused { id: 21, reason: UploadRefusedKind::Corrupt });

    // More bytes than declared.
    begin(&conn, 22, &session, "long.bin", 10, file);
    expect_msg(&conn, "the accept", accepted(22));
    conn.send_frame(&chunk_frame(22, 0, &data[..50])).unwrap();
    assert_eq!(expect_msg(&conn, "the refusal", refused(22)), Msg::UploadRefused { id: 22, reason: UploadRefusedKind::TooLarge });

    // A chunk at the wrong offset.
    begin(&conn, 23, &session, "gap.bin", 1000, file);
    expect_msg(&conn, "the accept", accepted(23));
    conn.send_frame(&chunk_frame(23, 5, &data[..10])).unwrap();
    assert_eq!(expect_msg(&conn, "the refusal", refused(23)), Msg::UploadRefused { id: 23, reason: UploadRefusedKind::Overrun });

    // A chunk for an upload that was never begun.
    conn.send_frame(&chunk_frame(99, 0, &[1])).unwrap();
    assert_eq!(expect_msg(&conn, "the refusal", refused(99)), Msg::UploadRefused { id: 99, reason: UploadRefusedKind::Overrun });

    // Nothing was kept, and the link still answers calls.
    wait_for("the partial files to go", || upload_files(&rig, &session).is_empty().then_some(()));
    let out = conn.call_wait("session_list", json!({}), Duration::from_secs(10)).unwrap();
    assert!(matches!(out, Outcome::Ok { .. }));
    // A declared size over the limit is refused by the office, and the link stays up.
    let too_big = MAX_UPLOAD_FILE + 1;
    begin(&conn, 30, &session, "big.bin", too_big, file);
    assert_eq!(expect_msg(&conn, "the refusal", refused(30)), Msg::UploadRefused { id: 30, reason: UploadRefusedKind::TooLarge });
    let out = conn.call_wait("session_list", json!({}), Duration::from_secs(10)).unwrap();
    assert!(matches!(out, Outcome::Ok { .. }));
}

#[test]
fn a_cancel_deletes_the_partial_file_and_sends_nothing_back() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    begin(&conn, 40, &session, "gone.bin", 9000, UploadKind::File);
    expect_msg(&conn, "the accept", accepted(40));
    conn.send_frame(&chunk_frame(40, 0, &payload(4000))).unwrap();
    wait_for("the partial file", || upload_files(&rig, &session).iter().any(|n| n.ends_with(".part")).then_some(()));
    conn.send(Msg::UploadCancel { id: 40 }).unwrap();
    wait_for("the partial file to go", || upload_files(&rig, &session).is_empty().then_some(()));
    assert!(maybe_msg(&conn, Duration::from_millis(300), refused(40)).is_none(), "a cancel is silent");
    let log = wait_for("the cancel in the log", || std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).ok().filter(|l| l.contains("upload_cancelled")));
    assert!(log.contains("\"reason\":\"cancel\""), "{log}");
}

#[test]
fn a_chunk_beyond_the_credit_is_refused_overrun_and_the_partial_file_goes() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    let data = payload(UPLOAD_CHUNK);
    begin(&conn, 90, &session, "flood.bin", (UPLOAD_CHUNK * 10) as u64, UploadKind::File);
    expect_msg(&conn, "the accept", accepted(90));
    // Nothing is acknowledged yet, so the window is the first eight chunks: offset 8 is the ninth.
    conn.send_frame(&chunk_frame(90, UPLOAD_CREDIT * UPLOAD_CHUNK, &data)).unwrap();
    assert_eq!(expect_msg(&conn, "the refusal", refused(90)), Msg::UploadRefused { id: 90, reason: UploadRefusedKind::Overrun });
    wait_for("the partial file to go", || upload_files(&rig, &session).is_empty().then_some(()));
    // The link stays up for the next upload.
    begin(&conn, 91, &session, "after.bin", 3, UploadKind::File);
    expect_msg(&conn, "the accept", accepted(91));
}

#[test]
fn a_terminal_stream_keeps_its_frames_during_a_large_upload() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (id, pty, handle) = rig.session();
    // The upload and the terminal share one connection, as they do in a remote window.
    let mut v = dev.connect();
    v.conn.subscribe(STREAM, &id, None).unwrap();
    v.until("the attach", |v| v.attaches == 1);
    let data = payload(16 * 1024 * 1024);
    let chunks = data.len() / UPLOAD_CHUNK;
    begin(&v.conn, 100, &id, "big.bin", data.len() as u64, UploadKind::File);
    let (mut sent, mut accepted_seen, mut ended) = (0usize, false, false);
    let (mut last_data, mut max_gap, mut seen) = (Instant::now(), Duration::ZERO, v.data_frames);
    let (mut next_line, mut lines) = (Instant::now(), 0usize);
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        assert!(Instant::now() < deadline, "the upload did not finish");
        v.step(Duration::from_millis(2));
        if v.data_frames != seen {
            seen = v.data_frames;
            let now = Instant::now();
            max_gap = max_gap.max(now.duration_since(last_data));
            last_data = now;
        }
        if v.msgs.iter().any(|m| matches!(m, Msg::UploadDone { id: 100, .. })) {
            break;
        }
        accepted_seen = accepted_seen || v.msgs.iter().any(|m| matches!(m, Msg::UploadAccepted { id: 100 }));
        let acked_bytes = v.msgs.iter().filter_map(|m| if let Msg::UploadAck { id: 100, received } = m { Some(*received) } else { None }).max().unwrap_or(0);
        let acked_chunks = usize::try_from(acked_bytes).unwrap_or(usize::MAX).div_ceil(UPLOAD_CHUNK);
        if accepted_seen && !ended {
            while sent < chunks && sent.saturating_sub(acked_chunks) < UPLOAD_CREDIT {
                let from = sent * UPLOAD_CHUNK;
                v.conn.send_frame(&chunk_frame(100, from, &data[from..from + UPLOAD_CHUNK])).unwrap();
                sent += 1;
            }
            if sent == chunks {
                v.conn.send(Msg::UploadEnd { id: 100, sha256: digest(&data) }).unwrap();
                ended = true;
            }
        }
        if Instant::now() >= next_line && lines < 200 {
            pty.emit(format!("line {lines}\r\n").as_bytes());
            lines += 1;
            next_line = Instant::now() + Duration::from_millis(20);
        }
    }
    assert!(max_gap < Duration::from_secs(2), "terminal output stalled for {max_gap:?} during the upload");
    v.until("the last line", |v| v.text().contains(&format!("line {}", lines - 1)));
    wait_for("the emulator", || (handle.processed_offset() >= handle.output_end()).then_some(()));
}

#[test]
fn revoking_a_device_mid_upload_deletes_the_partial_file_and_logs_the_end() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    begin(&conn, 61, &session, "revoked.bin", 9000, UploadKind::File);
    expect_msg(&conn, "the accept", accepted(61));
    conn.send_frame(&chunk_frame(61, 0, &payload(4000))).unwrap();
    wait_for("the partial file", || upload_files(&rig, &session).iter().any(|n| n.ends_with(".part")).then_some(()));
    rig.ui("peer_device_revoke", json!({"id": dev.host.device_id})).unwrap();
    wait_for("the partial file to go", || upload_files(&rig, &session).is_empty().then_some(()));
    let log = wait_for("the end in the log", || std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).ok().filter(|l| l.contains("upload_cancelled")));
    assert!(log.contains("link_closed"), "{log}");
}

#[test]
fn files_cannot_be_uploaded_into_an_ssh_session_yet() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    // The shell runs on another machine, so a path on the office would mean nothing there.
    let out = rig.ui("session_new", json!({"ssh": {"host": "example-host"}, "command": "fake"})).unwrap();
    let id = out.data["id"].as_str().unwrap().to_owned();
    let conn = dev.connect().conn;
    begin(&conn, 110, &id, "x.txt", 1, UploadKind::File);
    assert_eq!(expect_msg(&conn, "the refusal", refused(110)), Msg::UploadRefused { id: 110, reason: UploadRefusedKind::NotSupported });
    assert!(upload_files(&rig, &id).is_empty(), "nothing is written for an SSH session");
}

#[test]
fn a_device_below_full_cannot_upload() {
    let (rig, dev) = viewer_rig();
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    begin(&conn, 50, &session, "x.txt", 1, UploadKind::File);
    assert_eq!(expect_msg(&conn, "the refusal", refused(50)), Msg::UploadRefused { id: 50, reason: UploadRefusedKind::Forbidden });
    assert!(upload_files(&rig, &session).is_empty());
}

#[test]
fn turning_remote_access_off_mid_upload_deletes_the_partial_file() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    begin(&conn, 60, &session, "half.bin", 9000, UploadKind::File);
    expect_msg(&conn, "the accept", accepted(60));
    conn.send_frame(&chunk_frame(60, 0, &payload(4000))).unwrap();
    wait_for("the partial file", || upload_files(&rig, &session).iter().any(|n| n.ends_with(".part")).then_some(()));
    rig.ui("peer_server_disable", json!({})).unwrap();
    wait_for("the partial file to go", || upload_files(&rig, &session).is_empty().then_some(()));
}

#[test]
fn an_upload_is_logged_by_name_and_size_and_never_by_its_contents() {
    let rig = Rig::new();
    rig.enable();
    let dev = rig.device("Laptop");
    let (session, _, _) = rig.session();
    let conn = dev.connect().conn;
    let data = payload(64);
    begin(&conn, 70, &session, "secret-plan.txt", data.len() as u64, UploadKind::File);
    expect_msg(&conn, "the accept", accepted(70));
    conn.send_frame(&chunk_frame(70, 0, &data)).unwrap();
    conn.send(Msg::UploadEnd { id: 70, sha256: digest(&data) }).unwrap();
    expect_msg(&conn, "the done", |m| matches!(m, Msg::UploadDone { id: 70, .. }));
    let log = std::fs::read_to_string(rig.peer_dir.join("remote-access.log")).unwrap();
    assert!(log.contains("\"size\":64") && log.contains("secret-plan.txt") && log.contains(&dev.host.device_id), "{log}");
    let leaked: String = data.iter().take(8).map(|b| format!("{b:02x}")).collect();
    assert!(!log.contains(&leaked), "no contents in the log");
}
