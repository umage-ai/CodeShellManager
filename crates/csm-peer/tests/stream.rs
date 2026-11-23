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
use csm_peer_proto::msg::{ByeReason, ConsentOutcome, Msg, Outcome, TermClosedReason};
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
        self.ui("peer_pair_answer", json!({"allow": true, "check_code": code})).unwrap();
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
