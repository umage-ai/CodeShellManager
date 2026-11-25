//! The in-process office and laptop shared by the remote integration tests: a real engine with a
//! fake PTY, the real `csm-peer` server on 127.0.0.1, and a TCP proxy the test can cut. Only the PTY
//! is fake. Each test binary uses the part it needs.

#![allow(dead_code, unused_imports, clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io::{self, Read, Write};

use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, PeerHost, State, StateOptions, TerminalHandle, ToolError};
use csm_peer::client::{self, ClientConfig};
use csm_peer::identity::Identity;
use csm_peer::{PeerServer, ServerConfig};
use csm_peer_proto::addr::{Host, PeerAddr};
use csm_peer_proto::pairing::PeerOffer;
use csm_pty::{Backend as PtyBackend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_remote::{ConnectParams, LinkState, RemoteBackend};
use csm_term::GridSize;
use csm_term::replay;
use csm_ui::backend::{Backend as UiBackend, ToolPort};
use csm_ui::terminal::{SourceState, TerminalSource};
use csm_ui::theme::ThemeKind;
use serde_json::{Value, json};

/// Scrollback lines both emulators are compared with.
pub const HISTORY: usize = 2000;

// ---- a fake PTY ---------------------------------------------------------------------------------

pub struct FakePty {
    pub size: Mutex<PtySize>,
    pub sink: Mutex<Option<OutputSink>>,
    pub backlog: Mutex<Vec<u8>>,
    pub written: Mutex<Vec<u8>>,
    pub exited: AtomicBool,
}

impl FakePty {
    /// Output as the program would print it (kept if nobody is reading yet).
    pub fn emit(&self, bytes: &[u8]) {
        let mut sink = self.sink.lock().unwrap();
        match sink.as_mut() {
            Some(s) => s(bytes),
            None => self.backlog.lock().unwrap().extend_from_slice(bytes),
        }
    }

    pub fn written(&self) -> Vec<u8> {
        self.written.lock().unwrap().clone()
    }

    pub fn size(&self) -> (u16, u16) {
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
    fn backend(&self) -> PtyBackend {
        PtyBackend::BundledConPty
    }
    fn pid(&self) -> Option<u32> {
        Some(1)
    }
}

// ---- the office: an engine and its peer server ----------------------------------------------------

pub struct Office {
    /// The folder's owner, when this office made it (a restart hands it on).
    pub _tmp: Option<tempfile::TempDir>,
    pub dir: PathBuf,
    /// The port the server listens on (the same after a restart).
    pub port: u16,
    pub server: PeerServer,
    pub engine: Engine,
    pub folder: PathBuf,
    pub ptys: Arc<Mutex<Vec<Arc<FakePty>>>>,
}

pub fn quick() -> ServerConfig {
    ServerConfig { handshake_timeout: Duration::from_secs(5), ..ServerConfig::default() }
}

/// A port nothing listens on now (the office is asked to use it, so a restart finds it again).
pub fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

impl Office {
    /// An office in a fresh folder, listening on a free port.
    pub fn new() -> Office {
        let tmp = tempfile::tempdir().unwrap();
        let mut office = Office::boot(tmp.path().to_path_buf(), free_port(), false);
        office._tmp = Some(tmp);
        office
    }

    /// Starts an office in `dir`. A first start enables remote access on `port`; a `restore` start
    /// (after a restart) restores the sessions and resumes the saved choice, which keeps `port`.
    pub fn boot(dir: PathBuf, port: u16, restore: bool) -> Office {
        Office::boot_with(dir, port, restore, quick())
    }

    /// An office in a fresh folder whose publisher can be told to send one bad delta (`corrupt`).
    pub fn new_corruptible(corrupt: Arc<AtomicBool>) -> Office {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = ServerConfig { corrupt_next_delta: corrupt, ..quick() };
        let mut office = Office::boot_with(tmp.path().to_path_buf(), free_port(), false, cfg);
        office._tmp = Some(tmp);
        office
    }

    /// [`Office::boot`] with the server's config given.
    pub fn boot_with(dir: PathBuf, port: u16, restore: bool, cfg: ServerConfig) -> Office {
        let peer_dir = dir.join("peer");
        let folder = dir.join("work");
        std::fs::create_dir_all(&folder).unwrap();
        let server = PeerServer::with_config(peer_dir, cfg);
        let host: Arc<dyn PeerHost> = Arc::new(server.clone());
        let ptys: Arc<Mutex<Vec<Arc<FakePty>>>> = Arc::new(Mutex::new(Vec::new()));
        let list = ptys.clone();
        let options = StateOptions {
            save_path: Some(dir.join("state.json")),
            restore_on_start: restore,
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
        let office = Office { _tmp: None, dir, port, server, engine, folder, ptys };
        if restore {
            office.server.resume();
        } else {
            office.enable();
        }
        office
    }

    /// Stops this office and starts it again in the same folder, as after a restart of the app: the
    /// sessions are restored (new processes, so new instances) and the server listens on the same port.
    pub fn restart(mut self) -> Office {
        let tmp = self._tmp.take();
        let (dir, port) = (self.dir.clone(), self.port);
        drop(self);
        let mut next = Office::boot(dir, port, true);
        next._tmp = tmp;
        next
    }

    pub fn ui(&self, tool: &str, params: Value) -> Result<csm_engine::ToolOutput, ToolError> {
        self.engine.call(Caller::Ui, tool, params)
    }

    pub fn status(&self) -> Value {
        self.engine.call(Caller::Ui, "peer_status", json!({})).unwrap().data
    }

    pub fn enable(&self) -> SocketAddr {
        self.ui("peer_server_enable", json!({"address": "127.0.0.1", "port": self.port})).unwrap();
        self.server.local_addr().unwrap()
    }

    /// A live session on a fake PTY: its id, the PTY and the handle the server streams from.
    pub fn session(&self) -> (String, Arc<FakePty>, TerminalHandle) {
        let before = self.ptys.lock().unwrap().len();
        let out = self.ui("session_new", json!({"folder": self.folder.to_string_lossy(), "command": "fake"})).unwrap();
        let id = out.data["id"].as_str().unwrap().to_owned();
        let pty = wait_for("the fake PTY", || self.ptys.lock().unwrap().get(before).cloned());
        let handle = wait_for("the terminal", || self.engine.terminal(&id));
        (id, pty, handle)
    }

    /// Pairs a laptop with the office (the person answers Allow), and returns its identity and the
    /// pinned host key and address.
    pub fn pair(&self, name: &str) -> (Identity, client::PairedHost) {
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

    /// The emulator the office draws for `handle`, as the replay serializer reads it.
    pub fn screen_of(&self, handle: &TerminalHandle) -> Vec<u8> {
        let mut t = handle.terminal().lock().unwrap();
        replay::serialize(&mut t, HISTORY)
    }
}

impl Drop for Office {
    fn drop(&mut self) {
        self.server.shutdown();
        self.engine.shutdown();
    }
}

pub fn wait_for<T>(what: &str, mut f: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(v) = f() {
            return v;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---- a TCP proxy the test can cut ----------------------------------------------------------------

/// Forwards connections to the office. `cut` closes every one, so the laptop sees a dropped link;
/// the listener stays up, so the laptop's retry gets through.
pub struct Proxy {
    pub addr: SocketAddr,
    pub conns: Arc<Mutex<Vec<TcpStream>>>,
}

pub fn pipe(mut from: TcpStream, mut to: TcpStream) {
    let mut buf = [0u8; 8192];
    loop {
        match from.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if to.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    let _ = to.shutdown(Shutdown::Both);
}

impl Proxy {
    pub fn start(target: SocketAddr) -> Proxy {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let conns: Arc<Mutex<Vec<TcpStream>>> = Arc::new(Mutex::new(Vec::new()));
        let kept = conns.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(laptop) = stream else { continue };
                let Ok(office) = TcpStream::connect(target) else { continue };
                {
                    let mut all = kept.lock().unwrap();
                    all.push(laptop.try_clone().unwrap());
                    all.push(office.try_clone().unwrap());
                }
                let (l_read, o_write) = (laptop.try_clone().unwrap(), office.try_clone().unwrap());
                std::thread::spawn(move || pipe(l_read, o_write));
                std::thread::spawn(move || pipe(office, laptop));
            }
        });
        Proxy { addr, conns }
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    pub fn cut(&self) {
        for s in self.conns.lock().unwrap().drain(..) {
            let _ = s.shutdown(Shutdown::Both);
        }
    }
}

// ---- the laptop ----------------------------------------------------------------------------------

/// The address a laptop dials: the office, or the proxy in front of it.
pub fn dial(port: u16) -> PeerAddr {
    PeerAddr { host: Host::Ip("127.0.0.1".parse().unwrap()), port }
}

pub fn connect(office: &Office, port: Option<u16>, name: &str) -> (RemoteBackend, Identity) {
    let (me, host) = office.pair(name);
    let addr = match port {
        Some(p) => dial(p),
        None => host.address.clone(),
    };
    let backend = RemoteBackend::connect(ConnectParams {
        addr,
        relay: None,
        host_public: host.host_public,
        identity: me.clone(),
        cfg: ClientConfig::new(name),
        theme: ThemeKind::Mocha,
        themes: Vec::new(),
        overlay: None,
    })
    .unwrap();
    (backend, me)
}

/// Polls the backend until `ok` holds (the UI does this once a frame).
pub fn settle(b: &mut RemoteBackend, what: &str, mut ok: impl FnMut(&RemoteBackend) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let _ = b.poll();
        if ok(b) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; link is {:?}", b.link_state());
        std::thread::sleep(Duration::from_millis(5));
    }
}

pub fn live(b: &RemoteBackend) -> bool {
    matches!(b.link_state(), LinkState::Live { .. })
}

/// Submits a call and polls until its reply comes back.
pub fn call(b: &mut RemoteBackend, tool: &str, params: Value) -> Result<csm_engine::ToolOutput, ToolError> {
    b.submit(tool, params);
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (replies, _) = b.poll();
        if let Some((_, reply)) = replies.into_iter().find(|(c, _)| c.id == tool) {
            return reply;
        }
        assert!(Instant::now() < deadline, "no reply to {tool}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The laptop's emulator for a pane, as the replay serializer reads it.
pub fn screen_of_pane(term: &dyn TerminalSource) -> Vec<u8> {
    let t = term.terminal().expect("an emulator");
    let mut g = t.lock().unwrap();
    replay::serialize(&mut g, HISTORY)
}

pub fn pane_size(term: &dyn TerminalSource) -> (u16, u16) {
    let t = term.terminal().expect("an emulator");
    let g = t.lock().unwrap();
    (g.size().cols, g.size().rows)
}

/// Waits until the laptop's pane shows what the office's emulator shows.
pub fn assert_lockstep(b: &mut RemoteBackend, office: &Office, handle: &TerminalHandle, id: &str, what: &str) {
    settle(b, what, |b| {
        let Some(term) = b.terminal(id) else { return false };
        office.screen_of(handle) == screen_of_pane(term.as_ref())
    });
}

pub fn text_of(vt: &[u8]) -> String {
    String::from_utf8_lossy(vt).into_owned()
}

// ---- tests ------------------------------------------------------------------------------------------
