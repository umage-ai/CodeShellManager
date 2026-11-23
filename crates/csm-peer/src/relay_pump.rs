//! The relay's two sides, as threads. A laptop's peer connection is one relay channel: its own
//! WebSocket to `/v1/peer-client`, pumped by [`dial`]. The office is one WebSocket to
//! `/v1/peer-host` for all its channels: [`RelayHost`] opens a [`Stream`] for each channel the
//! relay announces and hands it to the server, and reconnects with backoff when the relay goes
//! away. Nothing in here understands the Noise session; the bytes are opaque.
//!
//! The office's writes are bounded and paced. Each channel's writer waits when the office's queue
//! is full (backpressure to the terminal), and the pump sends a channel's frames no faster than the
//! relay's per-channel caps, so a burst waits here instead of tripping the relay's rate limit, which
//! closes the channel. Inbound data that the reader does not take in time drops the channel instead
//! of stalling the thread that serves every channel.
//!
//! Connections are made with a timeout (the TCP connect and the WebSocket handshake, TLS included),
//! so an unreachable relay cannot hold the caller for the operating system's full connect timeout.
//!
//! The relay URL must be `wss://`, or `ws://` to a loopback address (tests, a relay on this PC).

use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{TOKEN_LEN, b64_encode, sha256, token_hash};
use csm_remote_proto::limits::{BURST_FACTOR, PEER_BYTES_PER_SECOND_PER_CHANNEL, PEER_FRAMES_PER_SECOND_PER_CHANNEL};
use csm_remote_proto::relay::{ClientToRelay, FrameKind, HostToRelay, OpenInfo, RelayToClient, RelayToHost, close_payload, decode_frame, encode_frame, to_text};
use tungstenite::handshake::HandshakeError;
use tungstenite::handshake::client::ClientHandshake;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::transport::{Out, QueueSink, RELAY_QUEUE, RelayStream, Sink, Stream};

/// How long a relay WebSocket waits for one read before the pump looks at its queue again.
const POLL: Duration = Duration::from_millis(5);
/// A Ping this often keeps the relay from calling an idle host silent.
const PING_EVERY: Duration = Duration::from_secs(20);
/// How long the TCP connect to the relay may take, per address.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the WebSocket handshake (TLS included) may take.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// Frames a channel's writer may queue before it waits.
const FRAME_QUEUE: usize = 256;
/// Frames the office holds for pacing before it stops taking more from the writers.
const OUTBOX_MAX: usize = 128;
/// Inbound frames a channel's reader may fall behind before the channel is dropped.
const INBOX_MAX: usize = 1024;

type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

/// Why a relay connection failed: the relay refused (a stable code) or the link broke.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelayErr {
    Refused(RefusalCode),
    Failed(String),
}

impl std::fmt::Display for RelayErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(c) => write!(f, "the relay refused the connection ({})", c.as_str()),
            Self::Failed(m) => write!(f, "the relay connection failed: {m}"),
        }
    }
}

impl std::error::Error for RelayErr {}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Whether `url` may be used (see `csm_peer_proto::pairing::relay_url_allowed`).
pub fn url_allowed(url: &str) -> bool {
    csm_peer_proto::pairing::relay_url_allowed(url)
}

/// The routing token a device presents to the relay: derived from its own key, so the laptop and
/// the office compute the same value without another message.
pub fn device_relay_token(device_public: &[u8; 32]) -> [u8; TOKEN_LEN] {
    let mut input = b"csm-peer relay token".to_vec();
    input.extend_from_slice(device_public);
    sha256(&input)
}

/// What the relay's allow list holds for a device: `token_hash` of its routing token.
pub fn device_relay_hash(device_public: &[u8; 32]) -> [u8; 32] {
    token_hash(&device_relay_token(device_public))
}

/// Opens the relay WebSocket at `path` below the configured base address (`wss://relay.example`).
fn connect(base: &str, path: &str) -> Result<Ws, RelayErr> {
    if !url_allowed(base) {
        return Err(RelayErr::Failed("the relay address must be wss://, or ws:// to this PC".to_owned()));
    }
    let url = format!("{}{path}", base.trim_end_matches('/'));
    let uri: tungstenite::http::Uri = url.parse().map_err(|_| RelayErr::Failed("the relay address is not a valid URL".to_owned()))?;
    let secure = uri.scheme_str() == Some("wss");
    let host = uri.host().ok_or_else(|| RelayErr::Failed("the relay address names no host".to_owned()))?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let port = uri.port_u16().unwrap_or(if secure { 443 } else { 80 });
    let tcp = dial_tcp(host, port)?;
    tcp.set_nodelay(true).map_err(|e| RelayErr::Failed(e.to_string()))?;
    tcp.set_read_timeout(Some(HANDSHAKE_TIMEOUT)).map_err(|e| RelayErr::Failed(e.to_string()))?;
    let (ws, _) = if secure {
        tungstenite::client_tls(url.as_str(), tcp).map_err(handshake_failed)?
    } else {
        tungstenite::client(url.as_str(), MaybeTlsStream::Plain(tcp)).map_err(handshake_failed)?
    };
    set_poll(&ws);
    Ok(ws)
}

/// A TCP connection to `host:port`, each address tried with [`CONNECT_TIMEOUT`].
fn dial_tcp(host: &str, port: u16) -> Result<TcpStream, RelayErr> {
    let addrs = (host, port).to_socket_addrs().map_err(|e| RelayErr::Failed(format!("could not look up the relay: {e}")))?;
    let mut last = None;
    for addr in addrs {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(s) => return Ok(s),
            Err(e) => last = Some(e),
        }
    }
    Err(RelayErr::Failed(match last {
        Some(e) => format!("could not reach the relay: {e}"),
        None => "the relay address has no usable address".to_owned(),
    }))
}

fn handshake_failed<S: io::Read + io::Write>(e: HandshakeError<ClientHandshake<S>>) -> RelayErr {
    match e {
        HandshakeError::Failure(f) => RelayErr::Failed(f.to_string()),
        HandshakeError::Interrupted(_) => RelayErr::Failed("the relay did not answer".to_owned()),
    }
}

fn set_poll(ws: &Ws) {
    let tcp = match ws.get_ref() {
        MaybeTlsStream::Plain(s) => Some(s),
        MaybeTlsStream::Rustls(s) => Some(s.get_ref()),
        _ => None,
    };
    if let Some(s) = tcp {
        let _ = s.set_read_timeout(Some(POLL));
        let _ = s.set_nodelay(true);
    }
}

fn is_idle(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io) if matches!(io.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted))
}

/// Sends the Hello and waits for the relay's answer.
fn hello_client(ws: &mut Ws, hello: &ClientToRelay) -> Result<(), RelayErr> {
    ws.send(Message::Text(to_text(hello).into())).map_err(|e| RelayErr::Failed(e.to_string()))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                return match serde_json::from_str::<RelayToClient>(t.as_str()) {
                    Ok(RelayToClient::Ready { .. }) => Ok(()),
                    Ok(RelayToClient::Error { code }) => Err(RelayErr::Refused(code)),
                    Err(_) => Err(RelayErr::Failed("the relay answered in a form this app does not read".to_owned())),
                };
            }
            Ok(Message::Close(_)) => return Err(RelayErr::Failed("the relay closed the connection".to_owned())),
            Ok(_) => {}
            Err(e) if is_idle(&e) => {
                if Instant::now() >= deadline {
                    return Err(RelayErr::Failed("the relay did not answer".to_owned()));
                }
            }
            Err(e) => return Err(RelayErr::Failed(e.to_string())),
        }
    }
}

/// A laptop's relay channel to the peer host `host_id` (the relay's host id, which the offer or the
/// saved host carries). `cred` is the pairing id while pairing, the routing token afterwards.
pub fn dial(url: &str, host_id: &str, cred: ClientCred) -> Result<Stream, RelayErr> {
    let mut ws = connect(url, "/v1/peer-client")?;
    let hello = match cred {
        ClientCred::Pairing(id) => ClientToRelay::Hello { v: csm_remote_proto::PROTOCOL_VERSION, host_id: host_id.to_owned(), token: None, pairing_id: Some(b64_encode(&id)) },
        ClientCred::Token(t) => ClientToRelay::Hello { v: csm_remote_proto::PROTOCOL_VERSION, host_id: host_id.to_owned(), token: Some(b64_encode(&t)), pairing_id: None },
    };
    hello_client(&mut ws, &hello)?;

    let (out_tx, out_rx) = mpsc::sync_channel::<Out>(RELAY_QUEUE);
    let (in_tx, in_rx) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new()
        .name("csm-peer-relay-client".into())
        .spawn(move || client_pump(ws, &out_rx, &in_tx))
        .map_err(|e| RelayErr::Failed(e.to_string()))?;
    Ok(Stream::Relay(RelayStream::new(in_rx, Arc::new(QueueSink(out_tx)))))
}

/// The credential a laptop presents on its relay channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientCred {
    Pairing([u8; 16]),
    Token([u8; TOKEN_LEN]),
}

fn client_pump(mut ws: Ws, out_rx: &Receiver<Out>, in_tx: &Sender<Vec<u8>>) {
    let mut last_ping = Instant::now();
    loop {
        loop {
            match out_rx.try_recv() {
                Ok(Out::Data(d)) => {
                    if ws.send(Message::Binary(d.into())).is_err() {
                        return;
                    }
                }
                Ok(Out::Close) => {
                    let _ = ws.close(None);
                    let _ = ws.flush();
                    return;
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => return,
            }
        }
        if last_ping.elapsed() >= PING_EVERY {
            last_ping = Instant::now();
            if ws.send(Message::Ping(Vec::new().into())).is_err() {
                return;
            }
        }
        match ws.read() {
            Ok(Message::Binary(b)) => {
                if in_tx.send(b.to_vec()).is_err() {
                    return;
                }
            }
            Ok(Message::Close(_)) => return,
            Ok(Message::Text(_)) => return,
            Ok(_) => {}
            Err(e) if is_idle(&e) => {}
            Err(_) => return,
        }
    }
}

// ---- the office side ------------------------------------------------------------------------

/// Called for each channel the relay opens: the stream and what the relay says opened it.
pub type OnChannel = Box<dyn FnMut(Stream, OpenInfo) + Send>;

/// Supplies the allow list (`token_hash` of each paired device's routing token) when the relay
/// connects, and again when [`RelayHost::resync`] is called.
pub type AllowList = Arc<dyn Fn() -> Vec<[u8; 32]> + Send + Sync>;

/// What the server asks of the relay thread. Control messages and frames share ONE queue, so they
/// reach the relay in the order the server sent them (a pairing's answer must leave before the
/// pairing slot closes, or the relay closes the laptop's channel first). The queue is unbounded
/// for control, which is small and never waited for; frames are bounded by [`Gate`].
enum Cmd {
    Control(HostToRelay),
    Resync,
    Frame(Frame),
    Stop,
}

/// One write from a channel's writer. `epoch` tells this incarnation of the channel number from
/// an earlier one, so a writer still holding a closed channel cannot write into a new one.
struct Frame {
    channel: u32,
    epoch: u64,
    out: Out,
}

/// Bounds the frames waiting for the relay thread: a writer waits while [`FRAME_QUEUE`] frames are
/// queued (backpressure to the terminal), and is released with an error once the thread is gone.
struct Gate {
    state: Mutex<GateState>,
    room: Condvar,
}

#[derive(Default)]
struct GateState {
    queued: usize,
    closed: bool,
}

/// How often a waiting writer looks again whether the relay thread has gone.
const GATE_WAIT: Duration = Duration::from_millis(100);

impl Gate {
    fn new() -> Self {
        Self { state: Mutex::new(GateState::default()), room: Condvar::new() }
    }

    fn enter(&self) -> io::Result<()> {
        let mut s = lock(&self.state);
        loop {
            if s.closed {
                return Err(gone());
            }
            if s.queued < FRAME_QUEUE {
                s.queued += 1;
                return Ok(());
            }
            s = self.room.wait_timeout(s, GATE_WAIT).unwrap_or_else(PoisonError::into_inner).0;
        }
    }

    fn leave(&self) {
        let mut s = lock(&self.state);
        s.queued = s.queued.saturating_sub(1);
        self.room.notify_one();
    }

    fn close(&self) {
        let mut s = lock(&self.state);
        s.closed = true;
        self.room.notify_all();
    }
}

fn gone() -> io::Error {
    io::Error::new(io::ErrorKind::BrokenPipe, "the relay connection is gone")
}

/// The sink for one channel of the office's relay socket: its writes become frames in the thread's
/// queue, and wait at the gate when the queue is full.
struct HostSink {
    cmds: Sender<Cmd>,
    gate: Arc<Gate>,
    channel: u32,
    epoch: u64,
}

impl Sink for HostSink {
    fn send(&self, out: Out) -> io::Result<()> {
        self.gate.enter()?;
        if self.cmds.send(Cmd::Frame(Frame { channel: self.channel, epoch: self.epoch, out })).is_err() {
            self.gate.leave();
            return Err(gone());
        }
        Ok(())
    }
}

/// The office's relay connection. Dropping it stops the thread and closes its channels.
pub struct RelayHost {
    cmds: Sender<Cmd>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    /// The last status the relay gave, for the settings page.
    status: Arc<Mutex<HostStatus>>,
}

/// What the office last saw from its relay.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HostStatus {
    pub connected: bool,
    pub last_error: Option<String>,
}

/// What the host thread needs to dial the relay.
pub struct HostRoute {
    pub url: String,
    pub host_secret: [u8; 32],
    /// The operator's enrolment token, when the relay needs one.
    pub access: Option<String>,
}

impl RelayHost {
    /// Starts the thread. It connects at once, reconnects with backoff (1, 2, 5, 10, then 30 s),
    /// and keeps trying until dropped.
    pub fn start(route: HostRoute, allow: AllowList, on_channel: OnChannel) -> Result<RelayHost, RelayErr> {
        if !url_allowed(&route.url) {
            return Err(RelayErr::Failed("the relay address must be wss://, or ws:// to this PC".to_owned()));
        }
        let (cmds, rx) = mpsc::channel::<Cmd>();
        let status = Arc::new(Mutex::new(HostStatus::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let mut state = HostState {
            cmds: cmds.clone(),
            rx,
            gate: Arc::new(Gate::new()),
            pending: VecDeque::new(),
            channels: HashMap::new(),
            open: HashMap::new(),
            pacers: HashMap::new(),
            epoch: 0,
            on_channel,
            allow,
            status: status.clone(),
            slot: None,
            stop: stop.clone(),
        };
        let thread = std::thread::Builder::new()
            .name("csm-peer-relay-host".into())
            .spawn(move || state.run(&route))
            .map_err(|e| RelayErr::Failed(e.to_string()))?;
        Ok(RelayHost { cmds, stop, thread: Some(thread), status })
    }

    /// Sends a control message (`PairingOpen`, `PairingClose`, `Allow`). Never blocks.
    pub fn control(&self, msg: HostToRelay) {
        let _ = self.cmds.send(Cmd::Control(msg));
    }

    /// Sends the current allow list again (after a device was added or removed).
    pub fn resync(&self) {
        let _ = self.cmds.send(Cmd::Resync);
    }

    pub fn status(&self) -> HostStatus {
        lock(&self.status).clone()
    }
}

impl Drop for RelayHost {
    /// Detached, not joined: a connect or a welcome wait can take seconds, and the caller may hold
    /// the server's locks. The thread sees the stop at its next step and exits by itself.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = self.cmds.send(Cmd::Stop);
        let _ = self.thread.take();
    }
}

/// An inbound channel: the reader's queue and the epoch it was opened under.
struct Chan {
    tx: SyncSender<Vec<u8>>,
    epoch: u64,
}

struct HostState {
    /// Handed to the channels' sinks.
    cmds: Sender<Cmd>,
    rx: Receiver<Cmd>,
    gate: Arc<Gate>,
    /// Taken from the queue, in order, and waiting to be sent (paced frames wait here).
    pending: VecDeque<Cmd>,
    /// Channels the relay has open, by number: where their inbound data goes.
    channels: HashMap<u32, Chan>,
    /// The epoch of each channel this office may still write to (opened, not yet closed).
    open: HashMap<u32, u64>,
    /// The per-channel pacing of the office's writes.
    pacers: HashMap<u32, Pacer>,
    epoch: u64,
    on_channel: OnChannel,
    allow: AllowList,
    status: Arc<Mutex<HostStatus>>,
    /// The pairing slot open now, replayed after a reconnect.
    slot: Option<HostToRelay>,
    stop: Arc<AtomicBool>,
}

impl Drop for HostState {
    /// Wakes any writer still waiting at the gate: the thread is gone, so its frames never go out.
    fn drop(&mut self) {
        self.gate.close();
    }
}

const BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];

impl HostState {
    fn run(&mut self, route: &HostRoute) {
        let mut attempt = 0usize;
        loop {
            if self.stop.load(Ordering::Acquire) {
                return;
            }
            let started = Instant::now();
            let result = self.session(route);
            lock(&self.status).connected = false;
            self.forget_session();
            match result {
                Ok(SessionEnd::Stopped) => return,
                Ok(SessionEnd::Lost) => {
                    lock(&self.status).last_error = None;
                }
                Err(e) => {
                    lock(&self.status).last_error = Some(e.to_string());
                }
            }
            if started.elapsed() > Duration::from_secs(60) {
                attempt = 0;
            }
            let wait = Duration::from_secs(BACKOFF.get(attempt).copied().unwrap_or(30));
            attempt = (attempt + 1).min(BACKOFF.len());
            let until = Instant::now() + wait;
            while Instant::now() < until {
                match self.rx.recv_timeout(Duration::from_millis(20)) {
                    Ok(cmd) => {
                        if self.while_away(cmd) {
                            return;
                        }
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
                if self.stop.load(Ordering::Acquire) {
                    return;
                }
            }
        }
    }

    /// A command taken while the relay is away. Frames are for channels that are gone, so they are
    /// dropped (which also releases the writers). Returns true when the thread must stop.
    fn while_away(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Stop => true,
            Cmd::Control(msg) => {
                self.remember(msg);
                false
            }
            Cmd::Resync => false,
            Cmd::Frame(_) => {
                self.gate.leave();
                false
            }
        }
    }

    /// Drops every channel of the lost session: the relay has forgotten them. Control messages that
    /// were not sent are remembered, so a pairing slot still open is replayed after a reconnect.
    fn forget_session(&mut self) {
        self.channels.clear();
        self.open.clear();
        self.pacers.clear();
        while let Some(cmd) = self.pending.pop_front() {
            if let Cmd::Control(msg) = cmd {
                self.remember(msg);
            }
        }
    }

    fn remember(&mut self, msg: HostToRelay) {
        match msg {
            HostToRelay::PairingOpen { .. } => self.slot = Some(msg),
            HostToRelay::PairingClose => self.slot = None,
            _ => {}
        }
    }

    fn session(&mut self, route: &HostRoute) -> Result<SessionEnd, RelayErr> {
        let mut ws = connect(&route.url, "/v1/peer-host")?;
        let hello = HostToRelay::Hello {
            v: csm_remote_proto::PROTOCOL_VERSION,
            host_id: csm_remote_proto::ids::host_id(&route.host_secret),
            host_secret: b64_encode(&route.host_secret),
            access: route.access.clone(),
        };
        ws.send(Message::Text(to_text(&hello).into())).map_err(|e| RelayErr::Failed(e.to_string()))?;
        wait_welcome(&mut ws)?;
        lock(&self.status).connected = true;
        lock(&self.status).last_error = None;
        self.send_control(&mut ws, &HostToRelay::Allow { tokens: (self.allow)().iter().map(|h| b64_encode(h)).collect() })?;
        if let Some(slot) = self.slot.clone() {
            self.send_control(&mut ws, &slot)?;
        }
        let mut last_ping = Instant::now();
        loop {
            if self.stop.load(Ordering::Acquire) {
                let _ = ws.close(None);
                return Ok(SessionEnd::Stopped);
            }
            // Take the commands in the order they were given. Stops once the pending queue is full,
            // so the writers wait at the gate instead of the queue growing without bound.
            while self.pending.len() < OUTBOX_MAX {
                match self.rx.try_recv() {
                    Ok(Cmd::Stop) => {
                        let _ = ws.close(None);
                        return Ok(SessionEnd::Stopped);
                    }
                    Ok(Cmd::Frame(f)) => {
                        self.gate.leave();
                        self.pending.push_back(Cmd::Frame(f));
                    }
                    Ok(cmd) => self.pending.push_back(cmd),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(SessionEnd::Stopped),
                }
            }
            self.pump(&mut ws)?;
            if last_ping.elapsed() >= PING_EVERY {
                last_ping = Instant::now();
                ws.send(Message::Ping(Vec::new().into())).map_err(|e| RelayErr::Failed(e.to_string()))?;
            }
            match ws.read() {
                Ok(Message::Binary(b)) => self.incoming_frame(&b),
                Ok(Message::Text(t)) => {
                    if let Ok(RelayToHost::Error { code }) = serde_json::from_str::<RelayToHost>(t.as_str()) {
                        return Err(RelayErr::Refused(code));
                    }
                }
                Ok(Message::Close(_)) => return Ok(SessionEnd::Lost),
                Ok(_) => {}
                Err(e) if is_idle(&e) => {}
                Err(e) => return Err(RelayErr::Failed(e.to_string())),
            }
        }
    }

    fn send_control(&self, ws: &mut Ws, msg: &HostToRelay) -> Result<(), RelayErr> {
        ws.send(Message::Text(to_text(msg).into())).map_err(|e| RelayErr::Failed(e.to_string()))
    }

    fn is_open(&self, channel: u32, epoch: u64) -> bool {
        self.open.get(&channel) == Some(&epoch)
    }

    /// Sends the pending commands in order. A data frame goes only when its channel's pacing has
    /// room; the first one that must wait stops the pump, so a channel's bytes keep their order.
    fn pump(&mut self, ws: &mut Ws) -> Result<(), RelayErr> {
        let now = Instant::now();
        while let Some(cmd) = self.pending.pop_front() {
            match cmd {
                Cmd::Control(msg) => {
                    self.remember(msg.clone());
                    self.send_control(ws, &msg)?;
                }
                Cmd::Resync => {
                    let tokens = (self.allow)().iter().map(|h| b64_encode(h)).collect();
                    self.send_control(ws, &HostToRelay::Allow { tokens })?;
                }
                Cmd::Frame(f) => {
                    if !self.is_open(f.channel, f.epoch) {
                        continue;
                    }
                    match f.out {
                        Out::Data(d) => {
                            let pacer = self.pacers.entry(f.channel).or_insert_with(|| Pacer::new(now));
                            if !pacer.try_take(d.len(), now) {
                                self.pending.push_front(Cmd::Frame(Frame { channel: f.channel, epoch: f.epoch, out: Out::Data(d) }));
                                break;
                            }
                            ws.send(Message::Binary(encode_frame(FrameKind::Data, f.channel, &d).into())).map_err(|e| RelayErr::Failed(e.to_string()))?;
                        }
                        Out::Close => {
                            self.open.remove(&f.channel);
                            self.channels.remove(&f.channel);
                            self.pacers.remove(&f.channel);
                            ws.send(Message::Binary(encode_frame(FrameKind::Close, f.channel, &close_payload(None)).into())).map_err(|e| RelayErr::Failed(e.to_string()))?;
                        }
                    }
                }
                Cmd::Stop => {}
            }
        }
        Ok(())
    }

    /// One binary frame from the relay: a new channel, its data, or its close.
    fn incoming_frame(&mut self, bytes: &[u8]) {
        let Ok(frame) = decode_frame(bytes) else { return };
        match frame.kind {
            FrameKind::Open => {
                let Some(info) = OpenInfo::decode(frame.payload) else { return };
                if self.stop.load(Ordering::Acquire) {
                    return;
                }
                // A new incarnation of this channel number: nothing from an older one may go out.
                self.epoch += 1;
                let epoch = self.epoch;
                self.pacers.remove(&frame.channel);
                let (in_tx, in_rx) = mpsc::sync_channel::<Vec<u8>>(INBOX_MAX);
                let sink = Arc::new(HostSink { cmds: self.cmds.clone(), gate: self.gate.clone(), channel: frame.channel, epoch });
                self.channels.insert(frame.channel, Chan { tx: in_tx, epoch });
                self.open.insert(frame.channel, epoch);
                (self.on_channel)(Stream::Relay(RelayStream::new(in_rx, sink)), info);
            }
            FrameKind::Data => {
                let Some(chan) = self.channels.get(&frame.channel) else { return };
                let epoch = chan.epoch;
                match chan.tx.try_send(frame.payload.to_vec()) {
                    Ok(()) => {}
                    // The reader is far behind (or gone): the channel ends rather than stalling the
                    // thread that serves every channel. The relay is told by the close queued here.
                    Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => {
                        self.channels.remove(&frame.channel);
                        self.pending.push_back(Cmd::Frame(Frame { channel: frame.channel, epoch, out: Out::Close }));
                    }
                }
            }
            FrameKind::Close => {
                // Dropping the sender ends the stream: its reader sees end of file.
                self.channels.remove(&frame.channel);
                self.open.remove(&frame.channel);
                self.pacers.remove(&frame.channel);
            }
        }
    }
}

/// A token bucket: `rate` units a second, holding at most [`BURST_FACTOR`] seconds of them.
struct Bucket {
    tokens: f64,
    cap: f64,
    rate: f64,
    last: Instant,
}

impl Bucket {
    fn new(rate: u32, now: Instant) -> Self {
        let rate = f64::from(rate);
        let cap = rate * f64::from(BURST_FACTOR);
        Self { tokens: cap, cap, rate, last: now }
    }

    fn refill(&mut self, now: Instant) {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + dt * self.rate).min(self.cap);
    }
}

/// The office's pacing for one channel: the relay's per-channel caps (frames and bytes a second),
/// so the office never sends faster than the relay lets a channel through.
struct Pacer {
    frames: Bucket,
    bytes: Bucket,
}

impl Pacer {
    fn new(now: Instant) -> Self {
        Self { frames: Bucket::new(PEER_FRAMES_PER_SECOND_PER_CHANNEL, now), bytes: Bucket::new(PEER_BYTES_PER_SECOND_PER_CHANNEL, now) }
    }

    /// Takes one frame with `len` payload bytes if both caps have room. Takes nothing otherwise.
    fn try_take(&mut self, len: usize, now: Instant) -> bool {
        self.frames.refill(now);
        self.bytes.refill(now);
        let n = f64::from(u32::try_from(len).unwrap_or(u32::MAX));
        if self.frames.tokens >= 1.0 && self.bytes.tokens >= n {
            self.frames.tokens -= 1.0;
            self.bytes.tokens -= n;
            true
        } else {
            false
        }
    }
}

enum SessionEnd {
    Stopped,
    Lost,
}

fn wait_welcome(ws: &mut Ws) -> Result<(), RelayErr> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match ws.read() {
            Ok(Message::Text(t)) => {
                return match serde_json::from_str::<RelayToHost>(t.as_str()) {
                    Ok(RelayToHost::Welcome { .. }) => Ok(()),
                    Ok(RelayToHost::Error { code }) => Err(RelayErr::Refused(code)),
                    _ => Err(RelayErr::Failed("the relay answered in a form this app does not read".to_owned())),
                };
            }
            Ok(Message::Close(_)) => return Err(RelayErr::Failed("the relay closed the connection".to_owned())),
            Ok(_) => {}
            Err(e) if is_idle(&e) => {
                if Instant::now() >= deadline {
                    return Err(RelayErr::Failed("the relay did not answer".to_owned()));
                }
            }
            Err(e) => return Err(RelayErr::Failed(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    #[test]
    fn pacer_holds_a_burst_to_the_frame_cap_then_refills_at_the_rate() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        // Tiny frames: the frame cap binds (2000 a second, twice that as burst).
        let first = (0..5000).filter(|_| p.try_take(1, t0)).count();
        assert_eq!(first, usize::try_from(PEER_FRAMES_PER_SECOND_PER_CHANNEL * BURST_FACTOR).unwrap());
        let later = (0..5000).filter(|_| p.try_take(1, t0 + Duration::from_secs(1))).count();
        assert_eq!(later, usize::try_from(PEER_FRAMES_PER_SECOND_PER_CHANNEL).unwrap());
    }

    #[test]
    fn pacer_keeps_a_large_stream_under_the_byte_cap() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        let chunk = 64 * 1024;
        let burst = (0..10_000).filter(|_| p.try_take(chunk, t0)).count();
        // 8 MiB a second with twice that as burst: 256 chunks at once.
        assert_eq!(burst, 256);
        let one_second = (0..10_000).filter(|_| p.try_take(chunk, t0 + Duration::from_secs(1))).count();
        assert_eq!(one_second, 128, "8 MiB a second of 64 KiB chunks");
    }

    #[test]
    fn a_refused_frame_takes_nothing() {
        let t0 = Instant::now();
        let mut p = Pacer::new(t0);
        while p.try_take(1024, t0) {}
        let frames_left = p.frames.tokens;
        assert!(!p.try_take(1024, t0));
        assert!((p.frames.tokens - frames_left).abs() < f64::EPSILON);
    }

    #[test]
    fn office_urls_are_checked_before_any_connection() {
        assert!(url_allowed("ws://127.0.0.1:9"));
        assert!(!url_allowed("ws://127.0.0.1:80@evil.example/x"));
        match connect("ws://127.0.0.1:80@evil.example", "/v1/peer-host") {
            Err(RelayErr::Failed(m)) => assert!(m.contains("wss://"), "{m}"),
            _ => panic!("a userinfo URL must be refused before a connection"),
        }
    }
}
