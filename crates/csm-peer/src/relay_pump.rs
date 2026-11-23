//! The relay's two sides, as threads. A laptop's peer connection is one relay channel: its own
//! WebSocket to `/v1/peer-client`, pumped by [`dial`]. The office is one WebSocket to
//! `/v1/peer-host` for all its channels: [`RelayHost`] opens a [`Stream`] for each channel the
//! relay announces and hands it to the server, and reconnects with backoff when the relay goes
//! away. Nothing in here understands the Noise session; the bytes are opaque.
//!
//! The relay URL must be `wss://`, or `ws://` to a loopback address (tests, a relay on this PC).

use std::collections::HashMap;
use std::io;
use std::net::TcpStream;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{TOKEN_LEN, b64_encode, sha256, token_hash};
use csm_remote_proto::relay::{ClientToRelay, FrameKind, HostToRelay, OpenInfo, RelayToClient, RelayToHost, close_payload, decode_frame, encode_frame, to_text};
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

use crate::transport::{Out, QueueSink, RELAY_QUEUE, RelayStream, Sink, Stream};

/// How long a relay WebSocket waits for one read before the pump looks at its queue again.
const POLL: Duration = Duration::from_millis(5);
/// A Ping this often keeps the relay from calling an idle host silent.
const PING_EVERY: Duration = Duration::from_secs(20);

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
    let url = format!("{}{path}", base.trim_end_matches('/'));
    if !url_allowed(base) {
        return Err(RelayErr::Failed("the relay address must be wss://, or ws:// to this PC".to_owned()));
    }
    let (ws, _) = tungstenite::connect(url.as_str()).map_err(|e| RelayErr::Failed(e.to_string()))?;
    set_poll(&ws);
    Ok(ws)
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

enum Cmd {
    Control(HostToRelay),
    Frame(u32, Out),
    Resync,
    Stop,
}

/// The sink for one channel of the office's relay socket: its writes become frames.
struct HostSink {
    cmds: Sender<Cmd>,
    channel: u32,
}

impl Sink for HostSink {
    fn send(&self, out: Out) -> io::Result<()> {
        self.cmds.send(Cmd::Frame(self.channel, out)).map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "the relay connection is gone"))
    }
}

/// The office's relay connection. Dropping it stops the thread and closes its channels.
pub struct RelayHost {
    cmds: Sender<Cmd>,
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
        let mut state = HostState { cmds: cmds.clone(), rx, channels: HashMap::new(), on_channel, allow, status: status.clone(), slot: None };
        let thread = std::thread::Builder::new()
            .name("csm-peer-relay-host".into())
            .spawn(move || state.run(&route))
            .map_err(|e| RelayErr::Failed(e.to_string()))?;
        Ok(RelayHost { cmds, thread: Some(thread), status })
    }

    /// Sends a control message (`PairingOpen`, `PairingClose`, `Allow`).
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
    fn drop(&mut self) {
        let _ = self.cmds.send(Cmd::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct HostState {
    cmds: Sender<Cmd>,
    rx: Receiver<Cmd>,
    channels: HashMap<u32, SyncSender<Vec<u8>>>,
    on_channel: OnChannel,
    allow: AllowList,
    status: Arc<Mutex<HostStatus>>,
    /// The pairing slot open now, replayed after a reconnect.
    slot: Option<HostToRelay>,
}

const BACKOFF: [u64; 5] = [1, 2, 5, 10, 30];

impl HostState {
    fn run(&mut self, route: &HostRoute) {
        let mut attempt = 0usize;
        loop {
            let started = Instant::now();
            let result = self.session(route);
            lock(&self.status).connected = false;
            self.channels.clear();
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
                match self.rx.recv_timeout(Duration::from_millis(100)) {
                    Ok(Cmd::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
                    Ok(Cmd::Control(msg)) => self.remember(msg),
                    Ok(Cmd::Frame(..) | Cmd::Resync) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                }
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
            loop {
                match self.rx.try_recv() {
                    Ok(Cmd::Stop) => {
                        let _ = ws.close(None);
                        return Ok(SessionEnd::Stopped);
                    }
                    Ok(Cmd::Control(msg)) => {
                        self.remember(msg.clone());
                        self.send_control(&mut ws, &msg)?;
                    }
                    Ok(Cmd::Resync) => {
                        let tokens = (self.allow)().iter().map(|h| b64_encode(h)).collect();
                        self.send_control(&mut ws, &HostToRelay::Allow { tokens })?;
                    }
                    Ok(Cmd::Frame(ch, Out::Data(d))) => {
                        ws.send(Message::Binary(encode_frame(FrameKind::Data, ch, &d).into())).map_err(|e| RelayErr::Failed(e.to_string()))?;
                    }
                    Ok(Cmd::Frame(ch, Out::Close)) => {
                        self.channels.remove(&ch);
                        ws.send(Message::Binary(encode_frame(FrameKind::Close, ch, &close_payload(None)).into())).map_err(|e| RelayErr::Failed(e.to_string()))?;
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return Ok(SessionEnd::Stopped),
                }
            }
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

    /// One binary frame from the relay: a new channel, its data, or its close.
    fn incoming_frame(&mut self, bytes: &[u8]) {
        let Ok(frame) = decode_frame(bytes) else { return };
        match frame.kind {
            FrameKind::Open => {
                let Some(info) = OpenInfo::decode(frame.payload) else { return };
                let (in_tx, in_rx) = mpsc::sync_channel::<Vec<u8>>(RELAY_QUEUE);
                let sink = Arc::new(HostSink { cmds: self.cmds.clone(), channel: frame.channel });
                self.channels.insert(frame.channel, in_tx);
                (self.on_channel)(Stream::Relay(RelayStream::new(in_rx, sink)), info);
            }
            FrameKind::Data => {
                if let Some(tx) = self.channels.get(&frame.channel)
                    && tx.send(frame.payload.to_vec()).is_err()
                {
                    self.channels.remove(&frame.channel);
                }
            }
            FrameKind::Close => {
                // Dropping the sender ends the stream: its reader sees end of file.
                self.channels.remove(&frame.channel);
            }
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
