//! The other end: pair with a host from its offer link, then connect as a paired device.
//!
//! Everything here blocks, so it runs on a worker thread, never the UI thread. The host's answers
//! are untrusted input like any other: every message goes through `csm-peer-proto`'s strict
//! decoders, and a handshake with a key other than the one the offer or the saved host named fails
//! before a single message is read.

use std::collections::HashMap;
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use csm_peer_proto::addr::{Host, PeerAddr};
use csm_peer_proto::codec::Frame;
use csm_peer_proto::ids::{KEY_LEN, host_id};
use csm_peer_proto::msg::{ByeReason, Level, Msg, Outcome, PROTOCOL_VERSION};
use csm_peer_proto::noise::{Preamble, check_code};
use csm_peer_proto::pairing::{DenyReason, OfferError, PairMsg, PeerOffer, RelayRoute};
use serde_json::Value;

use crate::TERM_VERSION;
use crate::identity::Identity;
use crate::relay_pump::{self, ClientCred, device_relay_token};
use crate::transport::Stream;
use crate::link::{Established, Io, Link, LinkError, Reader, initiate};

/// What the client says about itself.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// The device name the host shows ("Laptop"); at most 64 characters, no control characters.
    pub device_name: String,
    pub app_version: String,
    pub term_version: String,
    /// TCP connect and handshake time.
    pub connect_timeout: Duration,
    /// How long pairing waits for the person at the host to answer.
    pub pair_wait: Duration,
    pub ping_every: Duration,
    pub dead_after: Duration,
}

impl ClientConfig {
    pub fn new(device_name: &str) -> Self {
        Self {
            device_name: device_name.to_owned(),
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            term_version: TERM_VERSION.to_owned(),
            connect_timeout: Duration::from_secs(10),
            pair_wait: Duration::from_secs(130),
            ping_every: Duration::from_secs(15),
            dead_after: Duration::from_secs(45),
        }
    }
}

/// Why a pairing or a connection did not happen.
#[derive(Debug)]
pub enum ClientError {
    /// The offer link is not valid.
    Offer(OfferError),
    /// The host could not be reached.
    Connect(String),
    /// The host closed the connection without a word: no open offer (expired, used, a wrong id),
    /// or this device is not on its list. The host says nothing more on purpose.
    Refused,
    /// The person at the host denied the device (or did not answer in time).
    Denied(DenyReason),
    /// The host answered Bye.
    Bye(ByeReason),
    /// The person cancelled while the host had not answered.
    Cancelled,
    /// Something else went wrong on the wire.
    Link(String),
    Timeout,
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offer(e) => write!(f, "{e}"),
            Self::Connect(e) => write!(f, "could not reach the host: {e}"),
            Self::Refused => f.write_str("the host refused the connection: the pairing offer may have expired or been used, or this device is not paired"),
            Self::Denied(DenyReason::Denied) => f.write_str("the host denied this device"),
            Self::Denied(DenyReason::Expired) => f.write_str("nobody answered at the host in time"),
            Self::Denied(DenyReason::BadRequest) => f.write_str("the host did not accept the request"),
            Self::Bye(r) => write!(f, "the host ended the connection ({r:?})"),
            Self::Link(e) => write!(f, "{e}"),
            Self::Timeout => f.write_str("timed out"),
            Self::Cancelled => f.write_str("cancelled"),
        }
    }
}

impl std::error::Error for ClientError {}

fn link_failed(e: LinkError) -> ClientError {
    match e {
        // A silent close during the handshake is how the host says no.
        LinkError::Closed | LinkError::Refused => ClientError::Refused,
        LinkError::Timeout => ClientError::Timeout,
        other => ClientError::Link(other.to_string()),
    }
}

/// Connects to the first target that answers. `cancel` is checked before each attempt, so a
/// cancelled pairing does not start the next one. (One attempt can still take up to `timeout`.)
fn dial(addr: &PeerAddr, timeout: Duration, cancel: &AtomicBool) -> Result<TcpStream, ClientError> {
    let targets: Vec<SocketAddr> = match &addr.host {
        Host::Ip(ip) => vec![SocketAddr::new(*ip, addr.port)],
        Host::Name(n) => (n.as_str(), addr.port).to_socket_addrs().map_err(|e| ClientError::Connect(e.to_string()))?.collect(),
    };
    let mut last = "no address".to_owned();
    for t in targets {
        if cancel.load(Ordering::Acquire) {
            return Err(ClientError::Cancelled);
        }
        match TcpStream::connect_timeout(&t, timeout) {
            Ok(s) => return Ok(s),
            Err(e) => last = e.to_string(),
        }
    }
    Err(ClientError::Connect(last))
}

/// A host this device is now paired with: what to save to connect again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairedHost {
    /// The relay route, when the host offered one; the direct `address` is tried first.
    pub relay: Option<RelayRoute>,
    pub address: PeerAddr,
    pub host_id: String,
    pub host_public: [u8; KEY_LEN],
    /// The id the host lists this device under.
    pub device_id: String,
    pub level: Level,
}

/// Pairs with the host in `offer`. `on_check_code` is called with the four digits as soon as the
/// handshake is done; the person compares them with the host's screen and clicks Allow there.
/// Returns when the host answers.
pub fn pair(offer: &PeerOffer, me: &Identity, cfg: &ClientConfig, on_check_code: &mut dyn FnMut(&str)) -> Result<PairedHost, ClientError> {
    pair_cancellable(offer, me, cfg, Arc::new(AtomicBool::new(false)), on_check_code)
}

/// [`pair`], which gives up with [`ClientError::Cancelled`] once `cancel` is set. A cancel only
/// stops the wait for the answer (within about a quarter of a second); an answer that has already
/// arrived is taken. The connection is closed, so the host withdraws the request.
pub fn pair_cancellable(offer: &PeerOffer, me: &Identity, cfg: &ClientConfig, cancel: Arc<AtomicBool>, on_check_code: &mut dyn FnMut(&str)) -> Result<PairedHost, ClientError> {
    offer.validate().map_err(ClientError::Offer)?;
    let (addr, host_pub, id, psk) = match (offer.addr(), offer.host_pub(), offer.pairing_id(), offer.psk()) {
        (Some(a), Some(k), Some(i), Some(p)) => (a, k, i, p),
        _ => return Err(ClientError::Offer(OfferError::Format)),
    };
    let stream = open_route(Some(&addr), offer.r.as_ref(), ClientCred::Pairing(id), cfg.connect_timeout, &cancel)?;
    // The handshake's reads give up within a quarter of a second of a cancel, not at the deadline.
    let io = Io::new(stream).map_err(|e| ClientError::Connect(e.to_string()))?.cancel_on(cancel.clone());
    let deadline = Instant::now() + cfg.connect_timeout;
    let Established { link, mut reader, hash, .. } = initiate(io, me, &host_pub, &offer.h, Some(&psk), &Preamble::Pairing(id).encode(), deadline)
        .map_err(|e| match e {
            LinkError::Cancelled => ClientError::Cancelled,
            other => link_failed(other),
        })?;
    on_check_code(&check_code(&hash));
    if cancel.load(Ordering::Acquire) {
        link.close();
        return Err(ClientError::Cancelled);
    }
    let hello = PairMsg::Hello { v: PROTOCOL_VERSION, name: cfg.device_name.clone(), rt: true }.encode().map_err(|_| ClientError::Link("the device name is not allowed".into()))?;
    link.send_plains(&[hello]).map_err(link_failed)?;
    let until = Instant::now() + cfg.pair_wait;
    let answer = loop {
        if let Some(plain) = reader.recv_plain(Duration::from_millis(250)).map_err(link_failed)? {
            break PairMsg::decode(&plain).map_err(|e| ClientError::Link(format!("bad answer: {e:?}")))?;
        }
        if cancel.load(Ordering::Acquire) {
            link.close();
            return Err(ClientError::Cancelled);
        }
        if Instant::now() >= until {
            link.close();
            return Err(ClientError::Timeout);
        }
    };
    link.close();
    match answer {
        PairMsg::Accept { device_id, level, relay_token } if device_id == me.device_id() => {
            // The token comes from the host's answer, never from the link: a link cannot choose it.
            let relay = offer.r.clone().map(|mut r| {
                r.token = relay_token.filter(|t| csm_peer_proto::ids::b64_decode_fixed::<32>(t).is_some());
                r
            });
            Ok(PairedHost { address: addr, host_id: host_id(&host_pub), host_public: host_pub, device_id, level, relay })
        }
        PairMsg::Accept { .. } => Err(ClientError::Link("the host named another device id".into())),
        PairMsg::Deny { reason } => Err(ClientError::Denied(reason)),
        PairMsg::Hello { .. } => Err(ClientError::Link("unexpected message".into())),
    }
}

/// What the host said about itself in its `Welcome`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Welcome {
    pub host_name: String,
    pub server_version: String,
    pub term_version: String,
    pub level: Level,
    pub device_id: String,
}

/// What a connection delivers besides call replies.
#[derive(Debug)]
pub enum Incoming {
    /// Any frame that is not a reply, ping or bye (events, consent cards, terminal data...).
    Frame(Frame),
    /// The connection ended: with the host's reason if it sent one.
    Closed(Option<ByeReason>),
}

type Waiting = Arc<Mutex<HashMap<u64, Sender<Outcome>>>>;

/// A live connection to a host.
pub struct Connection {
    pub welcome: Welcome,
    link: Arc<Link>,
    rx: Receiver<Incoming>,
    waiting: Waiting,
    next_id: AtomicU64,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection").field("welcome", &self.welcome).field("closed", &self.link.is_closed()).finish()
    }
}

/// Connects to a host this device is paired with. The host's key is the one saved at pairing: a
/// handshake with any other key fails.
pub fn connect(addr: &PeerAddr, host_public: &[u8; KEY_LEN], me: &Identity, cfg: &ClientConfig) -> Result<Connection, ClientError> {
    connect_saved(Some(addr), None, host_public, me, cfg)
}

/// [`connect`] with a route: the direct address first, then the relay (when the host has one).
/// Through the relay the device presents its routing token: the random one the host gave at
/// pairing (kept in `relay.token`), or, for a pairing made before those existed, the one derived
/// from its own key.
pub fn connect_saved(direct: Option<&PeerAddr>, relay: Option<&RelayRoute>, host_public: &[u8; KEY_LEN], me: &Identity, cfg: &ClientConfig) -> Result<Connection, ClientError> {
    let cred = ClientCred::Token(relay_token_for(relay, me.public()));
    let stream = open_route(direct, relay, cred, cfg.connect_timeout, &AtomicBool::new(false))?;
    let io = Io::new(stream).map_err(|e| ClientError::Connect(e.to_string()))?;
    let deadline = Instant::now() + cfg.connect_timeout;
    let Established { link, mut reader, .. } =
        initiate(io, me, host_public, &host_id(host_public), None, &Preamble::Session.encode(), deadline).map_err(link_failed)?;
    link.send_msg(Msg::Hello {
        protocol: PROTOCOL_VERSION,
        client: cfg.device_name.clone(),
        app_version: cfg.app_version.clone(),
        term_version: cfg.term_version.clone(),
    })
    .map_err(link_failed)?;
    let welcome = match reader.recv(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(1))) {
        Ok(Some(Frame::Control(Msg::Welcome { host_name, server_version, term_version, level, device_id, .. }))) => {
            Welcome { host_name, server_version, term_version, level, device_id }
        }
        Ok(Some(Frame::Control(Msg::Bye { reason }))) => return Err(ClientError::Bye(reason)),
        Ok(Some(_)) => return Err(ClientError::Link("the host spoke before welcoming".into())),
        Ok(None) => return Err(ClientError::Timeout),
        Err(e) => return Err(link_failed(e)),
    };
    let (tx, rx) = mpsc::channel();
    let waiting: Waiting = Arc::new(Mutex::new(HashMap::new()));
    let (l2, w2, (ping_every, dead_after)) = (link.clone(), waiting.clone(), (cfg.ping_every, cfg.dead_after));
    std::thread::Builder::new()
        .name("csm-peer-client".into())
        .spawn(move || client_loop(&mut reader, &l2, &w2, &tx, ping_every, dead_after))
        .map_err(|e| ClientError::Link(e.to_string()))?;
    Ok(Connection { welcome, link, rx, waiting, next_id: AtomicU64::new(1) })
}

fn client_loop(reader: &mut Reader, link: &Arc<Link>, waiting: &Waiting, tx: &Sender<Incoming>, ping_every: Duration, dead_after: Duration) {
    let mut last_rx = Instant::now();
    let mut last_ping = Instant::now();
    let mut ping_n = 0u64;
    let tick = crate::server::tick_of(ping_every, dead_after);
    let bye = loop {
        match reader.recv(tick) {
            Ok(Some(Frame::Control(Msg::Reply { id, outcome }))) => {
                last_rx = Instant::now();
                let sender = waiting.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
                if let Some(s) = sender {
                    let _ = s.send(outcome);
                }
            }
            Ok(Some(Frame::Control(Msg::Ping { n }))) => {
                last_rx = Instant::now();
                let _ = link.send_msg(Msg::Pong { n });
            }
            Ok(Some(Frame::Control(Msg::Pong { .. }))) => last_rx = Instant::now(),
            Ok(Some(Frame::Control(Msg::Bye { reason }))) => break Some(reason),
            Ok(Some(frame)) => {
                last_rx = Instant::now();
                if tx.send(Incoming::Frame(frame)).is_err() {
                    break None;
                }
            }
            Ok(None) => {}
            Err(_) => break None,
        }
        if link.is_closed() || last_rx.elapsed() > dead_after {
            break None;
        }
        if last_ping.elapsed() >= ping_every {
            ping_n += 1;
            if link.send_msg(Msg::Ping { n: ping_n }).is_err() {
                break None;
            }
            last_ping = Instant::now();
        }
    };
    link.close();
    // Calls still waiting will never be answered: dropping their senders tells them.
    waiting.lock().unwrap_or_else(PoisonError::into_inner).clear();
    let _ = tx.send(Incoming::Closed(bye));
}

/// Why a call returned no outcome.
#[derive(Debug, PartialEq, Eq)]
pub enum CallError {
    /// The connection ended before the reply.
    Closed,
    Timeout,
}

impl Connection {
    /// Sends a tool call; the receiver gets the outcome (or disconnects if the link ends).
    pub fn call(&self, tool: &str, params: Value) -> Result<Receiver<Outcome>, CallError> {
        let id = self.next_id.fetch_add(1, Ordering::AcqRel);
        let (tx, rx) = mpsc::channel();
        self.waiting.lock().unwrap_or_else(PoisonError::into_inner).insert(id, tx);
        if self.link.send_msg(Msg::Call { id, tool: tool.to_owned(), params }).is_err() {
            self.waiting.lock().unwrap_or_else(PoisonError::into_inner).remove(&id);
            return Err(CallError::Closed);
        }
        Ok(rx)
    }

    /// [`Connection::call`], waiting for the outcome.
    pub fn call_wait(&self, tool: &str, params: Value, timeout: Duration) -> Result<Outcome, CallError> {
        match self.call(tool, params)?.recv_timeout(timeout) {
            Ok(o) => Ok(o),
            Err(RecvTimeoutError::Timeout) => Err(CallError::Timeout),
            Err(RecvTimeoutError::Disconnected) => Err(CallError::Closed),
        }
    }

    /// Answers a consent card shown by the host (the first answer anywhere wins).
    pub fn answer_consent(&self, request_id: u64, allow: bool) -> Result<(), CallError> {
        self.link.send_msg(Msg::ConsentAnswer { request_id, allow }).map_err(|_| CallError::Closed)
    }

    /// Sends any client message.
    pub fn send(&self, msg: Msg) -> Result<(), CallError> {
        self.link.send_msg(msg).map_err(|_| CallError::Closed)
    }

    /// Sends a client frame that is not a control message (an upload chunk, or terminal input).
    pub fn send_frame(&self, frame: &Frame) -> Result<(), CallError> {
        self.link.send_frame(frame).map_err(|_| CallError::Closed)
    }

    /// The next thing the host sent that is not a reply, or `None` after `wait`.
    pub fn recv_timeout(&self, wait: Duration) -> Option<Incoming> {
        self.rx.recv_timeout(wait).ok()
    }

    pub fn is_closed(&self) -> bool {
        self.link.is_closed()
    }

    /// Says goodbye and closes.
    pub fn close(&self) {
        self.link.try_bye(ByeReason::ClientQuit);
        self.link.close();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.link.close();
    }
}

// ---- terminal streams -------------------------------------------------------------------------

impl Connection {
    /// Asks for a session's terminal on `stream` (any id the client picks). With `resume` =
    /// `(instance, offset)` of what this client already has, the host continues from there when it
    /// still can; otherwise it sends a fresh attach.
    pub fn subscribe(&self, stream: u32, session: &str, resume: Option<(u64, u64)>) -> Result<(), CallError> {
        self.send(Msg::TermSubscribe { stream, session: session.to_owned(), instance: resume.map(|r| r.0), from_offset: resume.map(|r| r.1) })
    }

    /// The pane is hidden.
    pub fn unsubscribe(&self, stream: u32) -> Result<(), CallError> {
        self.send(Msg::TermUnsubscribe { stream })
    }

    /// Asks for a fresh attach (after [`TermUpdate::Gap`]).
    pub fn resync(&self, stream: u32) -> Result<(), CallError> {
        self.send(Msg::TermResync { stream })
    }

    /// This pane's size. `driver` says this client is the one typing: the PTY follows it.
    pub fn resize(&self, stream: u32, cols: u16, rows: u16, driver: bool) -> Result<(), CallError> {
        self.send(Msg::TermResize { stream, cols, rows, driver })
    }

    /// Typed input for the session on `stream`.
    pub fn input(&self, stream: u32, input: csm_peer_proto::codec::Input) -> Result<(), CallError> {
        self.link.send_frame(&Frame::TermInput { stream, input }).map_err(|_| CallError::Closed)
    }
}

/// What one frame meant for a followed terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermUpdate {
    /// A whole new screen: replace the emulator with a fresh one of this size and feed it `vt`.
    Attached(csm_peer_proto::codec::Attach),
    /// Bytes for the emulator, in order.
    Data(Vec<u8>),
    /// The PTY changed size at this point of the stream: resize the emulator.
    Resized { cols: u16, rows: u16 },
    /// The stream ended.
    Closed(csm_peer_proto::msg::TermClosedReason),
    /// Bytes arrived that do not follow on from what the client has: it must `resync`. Further
    /// data is ignored until the attach arrives.
    Gap,
    /// Not for this stream, or a part of an attach not complete yet, or stale data.
    Ignored,
}

/// Follows one terminal stream's frames: joins attach chunks, checks that data continues exactly
/// where the last byte ended, and remembers `(instance, offset)` for a resume after a reconnect.
/// Pure: the emulator is the caller's.
#[derive(Debug)]
pub struct TermFollower {
    stream: u32,
    attach: csm_peer_proto::codec::AttachAssembler,
    instance: Option<u64>,
    /// The offset of the next byte expected.
    offset: Option<u64>,
    /// A gap was reported: ignore data until an attach arrives.
    lost: bool,
}

impl TermFollower {
    pub fn new(stream: u32) -> Self {
        Self { stream, attach: csm_peer_proto::codec::AttachAssembler::new(), instance: None, offset: None, lost: false }
    }

    /// A follower for a stream that resumes at `(instance, offset)` (after a reconnect).
    pub fn resuming(stream: u32, instance: u64, offset: u64) -> Self {
        Self { instance: Some(instance), offset: Some(offset), ..Self::new(stream) }
    }

    /// What to pass to `Connection::subscribe` to continue where this left off.
    ///
    /// `None` while the follower is lost (a gap was reported and no attach has come since): the
    /// bytes it holds are not a screen to continue from, so the next subscribe asks for a fresh
    /// attach instead of resuming a stream the emulator never followed.
    pub fn resume_point(&self) -> Option<(u64, u64)> {
        if self.lost {
            return None;
        }
        self.instance.zip(self.offset)
    }

    /// A new connection starts: half an attach from the old one is dropped, so it is never glued
    /// to the first chunks of the next. Call before subscribing on the new link.
    pub fn new_link(&mut self) {
        self.attach.cancel(self.stream);
    }

    pub fn on_frame(&mut self, frame: &Frame) -> Result<TermUpdate, csm_peer_proto::codec::CodecError> {
        match frame {
            Frame::TermAttach { stream, .. } if *stream == self.stream => match self.attach.push(frame)? {
                Some(a) => {
                    self.instance = Some(a.instance);
                    self.offset = Some(a.offset);
                    self.lost = false;
                    Ok(TermUpdate::Attached(a))
                }
                None => Ok(TermUpdate::Ignored),
            },
            Frame::TermData { stream, instance, offset, bytes } if *stream == self.stream => {
                if self.lost {
                    return Ok(TermUpdate::Ignored);
                }
                if self.instance != Some(*instance) || self.offset != Some(*offset) {
                    self.lost = true;
                    return Ok(TermUpdate::Gap);
                }
                self.offset = Some(offset.saturating_add(bytes.len() as u64));
                Ok(TermUpdate::Data(bytes.clone()))
            }
            Frame::Control(Msg::TermResized { stream, offset, cols, rows }) if *stream == self.stream => {
                if self.lost {
                    return Ok(TermUpdate::Ignored);
                }
                if self.offset != Some(*offset) {
                    self.lost = true;
                    return Ok(TermUpdate::Gap);
                }
                Ok(TermUpdate::Resized { cols: *cols, rows: *rows })
            }
            Frame::Control(Msg::TermClosed { stream, reason }) if *stream == self.stream => {
                self.attach.cancel(self.stream);
                Ok(TermUpdate::Closed(*reason))
            }
            _ => Ok(TermUpdate::Ignored),
        }
    }
}

/// The stream to a host: the direct address first (when there is one), then the relay. `cancel`
/// stops the direct attempts; a relay attempt is not interrupted once it starts.
fn open_route(direct: Option<&PeerAddr>, relay: Option<&RelayRoute>, cred: ClientCred, timeout: Duration, cancel: &AtomicBool) -> Result<Stream, ClientError> {
    let mut direct_err = None;
    if let Some(addr) = direct {
        match dial(addr, timeout, cancel) {
            Ok(s) => return Ok(Stream::Tcp(s)),
            Err(ClientError::Cancelled) => return Err(ClientError::Cancelled),
            Err(e) => direct_err = Some(e),
        }
    }
    if let Some(r) = relay {
        if cancel.load(Ordering::Acquire) {
            return Err(ClientError::Cancelled);
        }
        return relay_pump::dial(&r.url, &r.host_id, cred).map_err(|e| ClientError::Connect(e.to_string()));
    }
    Err(direct_err.unwrap_or_else(|| ClientError::Connect("no route to the host".to_owned())))
}

/// The routing token to present on `relay`: the one saved at pairing, else the legacy derivation.
fn relay_token_for(relay: Option<&RelayRoute>, device_public: &[u8; KEY_LEN]) -> [u8; csm_remote_proto::ids::TOKEN_LEN] {
    relay.and_then(|r| r.token.as_deref()).and_then(csm_peer_proto::ids::b64_decode_fixed::<{ csm_remote_proto::ids::TOKEN_LEN }>).unwrap_or_else(|| device_relay_token(device_public))
}

#[cfg(test)]
mod relay_token_tests {
    use super::*;

    #[test]
    fn the_saved_token_is_used_and_an_older_pairing_falls_back_to_the_derived_one() {
        let key = [3u8; KEY_LEN];
        let route = |token: Option<String>| RelayRoute { url: "wss://relay.example".into(), host_id: host_id(&[2; 32]), token };
        let saved = [8u8; 32];
        assert_eq!(relay_token_for(Some(&route(Some(csm_peer_proto::ids::b64_encode(&saved)))), &key), saved);
        assert_eq!(relay_token_for(Some(&route(None)), &key), device_relay_token(&key), "paired before tokens were random");
        assert_eq!(relay_token_for(None, &key), device_relay_token(&key));
        assert_eq!(relay_token_for(Some(&route(Some("garbage".into()))), &key), device_relay_token(&key), "a damaged token is not used");
    }
}
