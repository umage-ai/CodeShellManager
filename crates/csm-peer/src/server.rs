//! The peer server: the listener, the pairing slot, the connections, and the engine's
//! [`PeerHost`] over all of it.
//!
//! **Off until the person turns it on.** [`PeerServer::new`] touches no disk and no network. The
//! listener starts in `enable` (the engine allows that for `Caller::Ui` only) and in [`resume`]
//! (which only replays a choice `enable` saved). It binds the one address the person chose, never
//! `0.0.0.0` or `::` unless they said "all interfaces". `disable` closes the listener and every
//! connection before it returns.
//!
//! **Silence for strangers.** A connection begins with a selector frame. A pairing selector with no
//! open slot, a wrong id, an exhausted slot, a session handshake from a key that is not on the
//! device list, garbage: all end in a closed socket with nothing written, so a scan learns nothing
//! about which of these it hit.
//!
//! **Pairing** (design 4.1): `pair_start` opens a slot (random id and key, ten minutes, one use) and
//! returns the link. The other CSM connects with the `IKpsk2` handshake; the slot is consumed when
//! its first transport message decrypts (the key is only proven then). Both sides compute the
//! four-digit check code from the handshake hash. The device is added only when the person answers
//! Allow with that code (`pair_answer`), never by the handshake alone.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr, TcpListener};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use csm_engine::{
    AuditEvent, Caller, DeviceInfo, Engine, EnableRequest, PairAnswer, PairingStarted, PeerHost, PeerStatus, PendingPair, Revoke, ToolError,
};
use csm_peer_proto::addr::{PeerAddr, check_bind};
use csm_peer_proto::codec::{CodecError, Frame};
use csm_peer_proto::ids::{PAIRING_ID_LEN, PSK_LEN};
use csm_peer_proto::msg::{ByeReason, ConsentOutcome, ErrorKind, Level, Msg, Outcome, PROTOCOL_VERSION, check_term_versions, valid_name};
use csm_peer_proto::noise::{MAX_PAIR_ATTEMPTS, Route, check_code, route};
use csm_peer_proto::pairing::{DenyReason, PairMsg, PeerOffer, SLOT_SECS};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::audit::{Audit, clean};
use crate::devices::{Device, DeviceStore, level_name};
use crate::fsperm;
use crate::identity::Identity;
use crate::relay_pump::{AllowList, HostRoute, HostStatus, OnChannel, RelayHost, device_relay_hash};
use csm_engine::{RelayInfo, RelaySetRequest};
use csm_remote_proto::ids::{b64_decode_fixed, b64_encode};
use crate::transport::Stream;
use csm_peer_proto::pairing::RelayRoute;
use csm_remote_proto::relay::HostToRelay;
use crate::link::{Established, Io, Link, LinkError, Reader, respond};
use crate::serve::{Drivers, Knobs, Serving, Who};
use crate::{TERM_VERSION, unix_secs};

/// Knobs a test turns (short lifetimes); the defaults are the design's.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// What the client shows as the host's name.
    pub host_name: String,
    pub app_version: String,
    pub term_version: String,
    /// How long a pairing offer lives (design: 10 minutes).
    pub pair_ttl: Duration,
    /// How long a device that asked to pair waits for Allow or Deny.
    pub pending_ttl: Duration,
    /// Time a new connection has to finish its handshake and first message.
    pub handshake_timeout: Duration,
    /// Connections that have not finished authenticating at once; more are dropped unanswered.
    pub max_unauthenticated: usize,
    /// Tool calls from devices running at once.
    pub max_in_flight_calls: usize,
    pub ping_every: Duration,
    pub dead_after: Duration,
    /// A terminal stream this many bytes behind sends a fresh screen instead of the backlog.
    pub stream_backlog_bytes: usize,
    /// Scrollback lines an attach carries (the screen always comes whole).
    pub attach_history_lines: usize,
    /// Terminal streams one device may have open.
    pub max_streams: usize,
    /// How often the snapshot publisher looks for a new version.
    pub snapshot_every: Duration,
    /// Tests only: the next delta any device is sent has a base that device does not have, which
    /// makes it ask for the whole snapshot. Nothing else sets it.
    pub corrupt_next_delta: Arc<AtomicBool>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
        let host = host.trim().to_owned();
        Self {
            host_name: if valid_name(&host) { host } else { "CodeShellManager".to_owned() },
            app_version: env!("CARGO_PKG_VERSION").to_owned(),
            term_version: TERM_VERSION.to_owned(),
            pair_ttl: Duration::from_secs(u64::from(SLOT_SECS)),
            pending_ttl: Duration::from_secs(120),
            handshake_timeout: Duration::from_secs(10),
            max_unauthenticated: 8,
            max_in_flight_calls: 16,
            ping_every: Duration::from_secs(15),
            dead_after: Duration::from_secs(45),
            stream_backlog_bytes: 256 * 1024,
            attach_history_lines: 1000,
            max_streams: 16,
            snapshot_every: Duration::from_millis(50),
            corrupt_next_delta: Arc::new(AtomicBool::new(false)),
        }
    }
}

/// The choice `peer_server_enable` saved (`server.json`), replayed by [`PeerServer::resume`].
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Choice {
    v: u8,
    enabled: bool,
    address: String,
    port: u16,
    all_interfaces: bool,
}

struct Run {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

struct Slot {
    id: [u8; PAIRING_ID_LEN],
    psk: [u8; PSK_LEN],
    expires: Instant,
    attempts: u8,
    consumed: bool,
}

impl Slot {
    /// At Hello time the attempt was already counted when the selector was routed.
    fn open(&self) -> bool {
        !self.consumed && Instant::now() < self.expires
    }
    fn usable(&self) -> bool {
        !self.consumed && self.attempts < MAX_PAIR_ATTEMPTS && Instant::now() < self.expires
    }
}

enum Answer {
    Allow,
    Deny,
}

struct Pending {
    name: String,
    check_code: String,
    key: [u8; 32],
    since: Instant,
    wrong: u8,
    answer: Sender<Answer>,
}

struct Conn {
    device_id: String,
    link: Arc<Link>,
    level: Level,
}

struct St {
    prepared: bool,
    restricted: bool,
    host: Option<Identity>,
    devices: Option<DeviceStore>,
    /// The address the person chose, while enabled (it may not be bound yet: see `waiting`).
    choice: Option<(SocketAddr, bool)>,
    run: Option<Run>,
    waiting: Option<String>,
    /// Bumped by every enable and disable; a retry or a connection that started under an older
    /// one does nothing.
    generation: u64,
    slot: Option<Slot>,
    pending: Option<Pending>,
    conns: HashMap<u64, Conn>,
}

struct Inner {
    dir: PathBuf,
    cfg: ServerConfig,
    st: Mutex<St>,
    engine: OnceLock<Engine>,
    audit: Arc<Audit>,
    drivers: Drivers,
    dead: AtomicBool,
    next_conn: AtomicU64,
    unauth: AtomicUsize,
    in_flight: Arc<AtomicUsize>,
    /// The relay this server is also reachable through, when the person set one up.
    relay: Mutex<Option<RelayState>>,
    /// The keychain (or a test store): the relay secrets live here, never in a file.
    secrets: OnceLock<Arc<dyn csm_services::secrets::SecretStore>>,
}

/// The server. Cloning shares it. Dropping the last handle closes everything (the threads hold
/// the shared state, not a handle).
#[derive(Clone)]
pub struct PeerServer {
    inner: Arc<Inner>,
    token: Arc<()>,
}

impl PeerServer {
    /// A server whose files will live in `dir` (created, private, on first use). Does nothing else:
    /// no file is read, no socket is opened.
    pub fn new(dir: PathBuf) -> Self {
        Self::with_config(dir, ServerConfig::default())
    }

    pub fn with_config(dir: PathBuf, cfg: ServerConfig) -> Self {
        let audit = Arc::new(Audit::new(dir.join("remote-access.log")));
        let st = St {
            prepared: false,
            restricted: false,
            host: None,
            devices: None,
            choice: None,
            run: None,
            waiting: None,
            generation: 0,
            slot: None,
            pending: None,
            conns: HashMap::new(),
        };
        Self {
            token: Arc::new(()),
            inner: Arc::new(Inner {
                dir,
                cfg,
                st: Mutex::new(st),
                engine: OnceLock::new(),
                audit,
                drivers: Drivers::default(),
                dead: AtomicBool::new(false),
                next_conn: AtomicU64::new(1),
                unauth: AtomicUsize::new(0),
                in_flight: Arc::new(AtomicUsize::new(0)),
                relay: Mutex::new(None),
                secrets: OnceLock::new(),
            }),
        }
    }

    /// Gives the server the engine its devices' tool calls run on (as `Caller::Peer`), and starts
    /// forwarding consent cards to connected Full devices. Call once, after `Engine::start`.
    pub fn attach(&self, engine: Engine) {
        if self.inner.engine.set(engine.clone()).is_err() {
            return;
        }
        let inner = self.inner.clone();
        let rx = engine.subscribe();
        let spawned = std::thread::Builder::new().name("csm-peer-events".into()).spawn(move || forward_events(&inner, &rx));
        if let Err(e) = spawned {
            log::warn!("remote access: could not start the event forwarder: {e}");
        }
    }

    /// Replays the saved choice, if the person enabled remote access and has not disabled it. A
    /// fresh install, an upgrade, a migrated WPF state and an imported backup have no such choice
    /// (it is a file of this server's own, written only by `enable`), so nothing starts.
    /// If the chosen address is missing (a tunnel that is down) it keeps retrying in the
    /// background; it never listens anywhere else.
    pub fn resume(&self) {
        let path = self.inner.dir.join("server.json");
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(choice) = serde_json::from_str::<Choice>(&text) else {
            log::warn!("remote access: {} is not readable; remote access stays off", path.display());
            return;
        };
        if choice.v != 1 || !choice.enabled {
            return;
        }
        let Ok(ip) = choice.address.parse::<IpAddr>() else { return };
        let addr = SocketAddr::new(ip, choice.port);
        if check_bind(&addr, choice.all_interfaces).is_err() {
            log::warn!("remote access: the saved address {addr} is not allowed; remote access stays off");
            return;
        }
        if self.inner.dead.load(Ordering::Acquire) {
            return;
        }
        let gen_ = {
            let mut st = self.inner.lock();
            if st.run.is_some() || st.choice.is_some() {
                return;
            }
            // The key and the device list, so connections can be told apart once it listens.
            if let Err(e) = self.inner.prepare(&mut st) {
                log::warn!("remote access: {e}; remote access stays off");
                return;
            }
            st.generation += 1;
            st.choice = Some((addr, choice.all_interfaces));
            st.generation
        };
        self.inner.audit.record("resumed", json!({"address": addr.to_string()}));
        self.restore_relay();
        if let Err(e) = self.inner.try_listen(gen_) {
            self.inner.lock().waiting = Some(e);
            let inner = self.inner.clone();
            let spawned = std::thread::Builder::new().name("csm-peer-retry".into()).spawn(move || inner.retry_listen(gen_));
            if let Err(e) = spawned {
                log::warn!("remote access: could not start the retry thread: {e}");
            }
        }
    }

    /// The remote-access log, newest last (what Settings shows).
    pub fn audit_tail(&self, n: usize) -> Vec<Value> {
        self.inner.audit.tail(n)
    }

    /// The address the listener is bound to right now (tests and the indicator).
    pub fn local_addr(&self) -> Option<SocketAddr> {
        self.inner.lock().run.as_ref().map(|r| r.addr)
    }
}

impl Drop for PeerServer {
    fn drop(&mut self) {
        if Arc::strong_count(&self.token) == 1 {
            self.inner.stop_all(ByeReason::ServerQuit);
        }
    }
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, St> {
        self.st.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn io_failed(what: &str, e: &std::io::Error) -> ToolError {
        ToolError::Failed(format!("{what}: {e}"))
    }

    /// Creates the private folder, the host key and the device list, once.
    fn prepare(&self, st: &mut St) -> Result<(), ToolError> {
        if !st.prepared {
            st.restricted = fsperm::ensure_private_dir(&self.dir).map_err(|e| Self::io_failed("could not create the remote access folder", &e))?;
            st.prepared = true;
        }
        if st.host.is_none() {
            st.host = Some(Identity::load_or_create(&self.dir.join("host.key")).map_err(|e| Self::io_failed("the host key", &e))?);
        }
        self.load_devices(st)
    }

    fn load_devices(&self, st: &mut St) -> Result<(), ToolError> {
        if st.devices.is_none() {
            st.devices = Some(DeviceStore::load(&self.dir.join("devices.json")).map_err(|e| Self::io_failed("the device list", &e))?);
        }
        Ok(())
    }

    fn save_choice(&self, enabled: bool, addr: Option<SocketAddr>, all: bool) {
        let (address, port) = addr.map_or((String::new(), 0), |a| (a.ip().to_string(), a.port()));
        let c = Choice { v: 1, enabled, address, port, all_interfaces: all };
        let text = serde_json::to_string_pretty(&c).unwrap_or_default();
        if let Err(e) = fsperm::write_private(&self.dir.join("server.json"), text.as_bytes()) {
            log::warn!("remote access: could not save the choice: {e}");
        }
    }

    /// Binds the chosen address and starts accepting. Under the lock: the bind is quick.
    fn try_listen(self: &Arc<Self>, gen_: u64) -> Result<(), String> {
        let mut st = self.lock();
        if st.generation != gen_ || st.run.is_some() {
            return Ok(());
        }
        let Some((addr, all)) = st.choice else { return Ok(()) };
        check_bind(&SocketAddr::new(addr.ip(), if addr.port() == 0 { 1 } else { addr.port() }), all).map_err(|e| e.to_string())?;
        let listener = TcpListener::bind(addr).map_err(|e| format!("could not listen on {addr}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let bound = listener.local_addr().map_err(|e| e.to_string())?;
        let stop = Arc::new(AtomicBool::new(false));
        let (inner, flag) = (self.clone(), stop.clone());
        let thread = std::thread::Builder::new()
            .name("csm-peer-accept".into())
            .spawn(move || inner.accept_loop(&listener, &flag, gen_))
            .map_err(|e| e.to_string())?;
        st.run = Some(Run { addr: bound, stop, thread: Some(thread) });
        st.choice = Some((bound, all));
        st.waiting = None;
        drop(st);
        log::info!("remote access: listening on {bound}");
        Ok(())
    }

    fn retry_listen(self: Arc<Self>, gen_: u64) {
        let steps = [1u64, 2, 5, 10, 30];
        let mut i = 0usize;
        loop {
            let wait = Duration::from_secs(steps.get(i).copied().unwrap_or(30));
            let until = Instant::now() + wait;
            while Instant::now() < until {
                std::thread::sleep(Duration::from_millis(100));
                if self.dead.load(Ordering::Acquire) || self.lock().generation != gen_ {
                    return;
                }
            }
            match self.try_listen(gen_) {
                Ok(()) => {
                    if self.lock().generation == gen_ {
                        self.audit.record("listening", json!({"after_retry": true}));
                    }
                    return;
                }
                Err(e) => {
                    self.lock().waiting = Some(e);
                    i += 1;
                }
            }
        }
    }

    fn accept_loop(self: Arc<Self>, listener: &TcpListener, stop: &AtomicBool, gen_: u64) {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream, peer)) => {
                    if self.unauth.fetch_add(1, Ordering::AcqRel) >= self.cfg.max_unauthenticated {
                        self.unauth.fetch_sub(1, Ordering::AcqRel);
                        continue; // dropped unanswered
                    }
                    let inner = self.clone();
                    let spawned = std::thread::Builder::new().name("csm-peer-conn".into()).spawn(move || inner.serve(Stream::Tcp(stream), From::Addr(peer), gen_));
                    if spawned.is_err() {
                        self.unauth.fetch_sub(1, Ordering::AcqRel);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                Err(e) => {
                    log::debug!("remote access: accept: {e}");
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    /// Closes the listener and every connection. `join` waits for the accept thread, so the port
    /// is free when this returns. The saved choice is the caller's business.
    fn stop_all(&self, reason: ByeReason) {
        let (run, conns, pending) = {
            let mut st = self.lock();
            st.generation += 1;
            st.slot = None;
            self.relay_slot_close();
            st.waiting = None;
            (st.run.take(), std::mem::take(&mut st.conns), st.pending.take())
        };
        if let Some(mut run) = run {
            run.stop.store(true, Ordering::Release);
            if let Some(t) = run.thread.take() {
                let _ = t.join();
            }
        }
        if let Some(p) = pending {
            let _ = p.answer.send(Answer::Deny);
        }
        for (_, c) in conns {
            c.link.try_bye(reason);
            c.link.close();
        }
    }

    fn status(&self) -> PeerStatus {
        let mut st = self.lock();
        if st.devices.is_none() {
            // Reading a small file once; not an error when it is not there.
            let _ = self.load_devices(&mut st);
        }
        PeerStatus {
            available: true,
            enabled: st.choice.is_some(),
            listening: st.run.as_ref().map(|r| r.addr.to_string()),
            waiting: st.waiting.clone(),
            pairing_open_secs: st.slot.as_ref().filter(|s| s.usable()).map(|s| s.expires.saturating_duration_since(Instant::now()).as_secs()),
            pending_pair: st.pending.as_ref().map(|p| PendingPair {
                device_name: p.name.clone(),
                check_code: p.check_code.clone(),
                expires_in_secs: self.cfg.pending_ttl.saturating_sub(p.since.elapsed()).as_secs(),
            }),
            devices: st.devices.as_ref().map_or(0, |d| d.list().len()),
            connected: st.conns.len(),
            private_files_restricted: st.restricted,
        }
    }

    // ---- one connection ---------------------------------------------------------------------

    fn serve(self: Arc<Self>, stream: Stream, peer: From, gen_: u64) {
        let _ = stream.set_nonblocking(false);
        let mut guard = Unauth { inner: &self, held: true };
        let result = self.serve_inner(stream, peer, gen_, &mut guard);
        drop(guard);
        match result {
            Ok(()) | Err(LinkError::Closed) => {}
            Err(LinkError::Refused) => {}
            Err(e) => log::debug!("remote access: {peer}: {e}"),
        }
    }

    fn serve_inner(&self, stream: Stream, peer: From, gen_: u64, guard: &mut Unauth<'_>) -> Result<(), LinkError> {
        let mut io = Io::new(stream).map_err(|e| LinkError::Io(e.to_string()))?;
        let deadline = Instant::now() + self.cfg.handshake_timeout;
        let pre = io.read_frame_by(deadline)?;
        if pre.len() > 1 + PAIRING_ID_LEN {
            return Err(LinkError::Refused);
        }
        let (decision, psk, host) = {
            let mut st = self.lock();
            if st.generation != gen_ || st.run.is_none() {
                return Err(LinkError::Refused);
            }
            let open = st.slot.as_ref().filter(|s| s.usable()).map(|s| s.id);
            let decision = route(&pre, open.as_ref());
            let mut psk = None;
            if decision == Route::Pairing
                && let Some(slot) = st.slot.as_mut()
            {
                slot.attempts += 1;
                psk = Some(slot.psk);
            }
            (decision, psk, st.host.clone())
        };
        let Some(host) = host else { return Err(LinkError::Refused) };
        let arrival = Arrival { peer, gen_, deadline };
        match decision {
            Route::Refuse => Err(LinkError::Refused),
            Route::Session => self.serve_session(io, &host, &arrival, guard),
            Route::Pairing => match psk {
                Some(psk) => self.serve_pairing(io, &host, &psk, &arrival, guard),
                None => Err(LinkError::Refused),
            },
        }
    }

    fn serve_pairing(&self, io: Io, host: &Identity, psk: &[u8; PSK_LEN], arrival: &Arrival, guard: &mut Unauth<'_>) -> Result<(), LinkError> {
        let Arrival { peer, gen_, deadline } = *arrival;
        let Established { link, mut reader, remote_static, hash } = respond(io, host, Some(psk), &|_| true, deadline)?;
        let hello = loop {
            if Instant::now() >= deadline {
                return Err(LinkError::Timeout);
            }
            if let Some(plain) = reader.recv_plain(Duration::from_millis(250))? {
                break PairMsg::decode(&plain).map_err(|_| LinkError::Protocol("not a pairing hello"))?;
            }
        };
        let PairMsg::Hello { name, .. } = hello else { return Err(LinkError::Protocol("not a pairing hello")) };
        let (tx, rx) = mpsc::channel::<Answer>();
        let code = check_code(&hash);
        {
            let mut st = self.lock();
            let slot_ok = st.generation == gen_ && st.pending.is_none() && st.slot.as_ref().is_some_and(|s| s.open() && s.psk == *psk);
            if !slot_ok {
                return Err(LinkError::Refused);
            }
            // The key proved itself: the slot is spent, whatever the person answers.
            if let Some(s) = st.slot.as_mut() {
                s.consumed = true;
            }
            st.slot = None;
            st.pending = Some(Pending { name: clean(&name), check_code: code, key: remote_static, since: Instant::now(), wrong: 0, answer: tx });
        }
        guard.release();
        self.audit.record("pair_requested", json!({"device": clean(&name), "from": peer.to_string()}));
        // Wait for the person, and notice if the other side leaves.
        let give_up = Instant::now() + self.cfg.pending_ttl;
        let (mut answer, mut reason) = loop {
            match rx.try_recv() {
                Ok(a) => break (a, DenyReason::Denied),
                Err(TryRecvError::Disconnected) => break (Answer::Deny, DenyReason::Denied),
                Err(TryRecvError::Empty) => {}
            }
            if self.dead.load(Ordering::Acquire) || Instant::now() >= give_up {
                break (Answer::Deny, DenyReason::Expired);
            }
            match reader.recv_plain(Duration::from_millis(100)) {
                Ok(None) => {}
                Ok(Some(_)) | Err(_) => {
                    // It hung up, or spoke out of turn: the request is withdrawn.
                    self.clear_pending(&remote_static);
                    self.audit.record("pair_withdrawn", json!({"device": clean(&name)}));
                    return Err(LinkError::Closed);
                }
            }
        };
        self.clear_pending(&remote_static);
        // An Allow can land between the last poll and the give-up check. pair_answer decides under
        // the lock that clear_pending just took, so a device that was added is honoured here.
        if matches!(answer, Answer::Deny)
            && matches!(reason, DenyReason::Expired)
            && let Ok(a) = rx.try_recv()
        {
            answer = a;
            reason = DenyReason::Denied;
        }
        let reply = match answer {
            Answer::Allow => {
                let id = csm_peer_proto::ids::device_id(&remote_static);
                PairMsg::Accept { device_id: id, level: Level::Full }
            }
            Answer::Deny => PairMsg::Deny { reason },
        };
        if let Ok(bytes) = reply.encode() {
            let _ = link.send_plains(&[bytes]);
        }
        // The relay's slot closes after the answer, so the answer is queued before the close.
        self.relay_slot_close();
        // Give the answer a moment to leave before the socket closes.
        std::thread::sleep(Duration::from_millis(50));
        link.close();
        Ok(())
    }

    fn clear_pending(&self, key: &[u8; 32]) {
        let mut st = self.lock();
        if st.pending.as_ref().is_some_and(|p| &p.key == key) {
            st.pending = None;
        }
    }

    fn serve_session(&self, io: Io, host: &Identity, arrival: &Arrival, guard: &mut Unauth<'_>) -> Result<(), LinkError> {
        let Arrival { peer, gen_, deadline } = *arrival;
        let known = |k: &[u8; 32]| self.lock().devices.as_ref().is_some_and(|d| d.find_by_key(k).is_some());
        let Established { link, mut reader, remote_static, .. } = match respond(io, host, None, &known, deadline) {
            Ok(e) => e,
            Err(LinkError::Refused) => {
                self.audit.record("refused", json!({"why": "unknown key", "from": peer.to_string()}));
                return Err(LinkError::Refused);
            }
            Err(e) => return Err(e),
        };
        let first = loop {
            if Instant::now() >= deadline {
                return Err(LinkError::Timeout);
            }
            match reader.recv(Duration::from_millis(250)) {
                Ok(Some(f)) => break f,
                Ok(None) => {}
                Err(LinkError::Codec(CodecError::Version(_))) => {
                    link.try_bye(ByeReason::ProtocolMismatch);
                    link.close();
                    return Err(LinkError::Protocol("protocol version"));
                }
                Err(e) => return Err(e),
            }
        };
        let Frame::Control(Msg::Hello { term_version, client, .. }) = first else { return Err(LinkError::Protocol("expected hello")) };
        if let Err(reason) = check_term_versions(&self.cfg.term_version, &term_version) {
            link.try_bye(reason);
            link.close();
            return Err(LinkError::Protocol("terminal version"));
        }
        // Register: the device must still be paired, and this must still be the same listener.
        let conn_id = self.next_conn.fetch_add(1, Ordering::AcqRel);
        let (device, replaced) = {
            let mut st = self.lock();
            if st.generation != gen_ || st.run.is_none() {
                return Err(LinkError::Refused);
            }
            let Some(device) = st.devices.as_ref().and_then(|d| d.find_by_key(&remote_static)).cloned() else {
                return Err(LinkError::Refused);
            };
            let old: Vec<u64> = st.conns.iter().filter(|(_, c)| c.device_id == device.id).map(|(k, _)| *k).collect();
            let replaced: Vec<Conn> = old.iter().filter_map(|k| st.conns.remove(k)).collect();
            st.conns.insert(conn_id, Conn { device_id: device.id.clone(), link: link.clone(), level: device.level });
            if let Some(d) = st.devices.as_mut() {
                d.touch(&device.id, unix_secs());
            }
            (device, replaced)
        };
        for c in replaced {
            c.link.try_bye(ByeReason::Replaced);
            c.link.close();
        }
        guard.release();
        let welcome = Msg::Welcome {
            protocol: PROTOCOL_VERSION,
            host_name: self.cfg.host_name.clone(),
            server_version: self.cfg.app_version.clone(),
            term_version: self.cfg.term_version.clone(),
            level: device.level,
            device_id: device.id.clone(),
        };
        if link.send_msg(welcome).is_err() {
            self.drop_conn(conn_id);
            return Err(LinkError::Closed);
        }
        self.audit.record("connected", json!({"device_id": device.id, "device": clean(&device.name), "client": clean(&client), "from": peer.to_string()}));
        self.send_waiting_cards(&device, &link);
        let serving = self.engine.get().map(|engine| {
            let knobs = Knobs {
                backlog: self.cfg.stream_backlog_bytes,
                history: self.cfg.attach_history_lines,
                max_streams: self.cfg.max_streams,
                snapshot_every: self.cfg.snapshot_every,
                corrupt_next_delta: self.cfg.corrupt_next_delta.clone(),
            };
            Serving::new(engine.clone(), link.clone(), Who { conn_id, device_id: device.id.clone(), level: device.level }, knobs, self.drivers.clone(), self.audit.clone())
        });
        if let Some(s) = &serving
            && s.start().is_err()
        {
            s.stop();
            self.drop_conn(conn_id);
            return Err(LinkError::Closed);
        }
        let reason = self.session_loop(&mut reader, &link, &device, serving.as_ref());
        if let Some(s) = &serving {
            s.stop();
        }
        link.close();
        self.drop_conn(conn_id);
        self.audit.record("disconnected", json!({"device_id": device.id, "device": clean(&device.name), "reason": reason}));
        Ok(())
    }

    fn drop_conn(&self, conn_id: u64) {
        self.lock().conns.remove(&conn_id);
    }

    /// Reads frames until the connection ends. The reason is for the log.
    fn session_loop(&self, reader: &mut Reader, link: &Arc<Link>, device: &Device, serving: Option<&Arc<Serving>>) -> &'static str {
        let mut last_rx = Instant::now();
        let mut last_ping = Instant::now();
        let mut ping_n: u64 = 0;
        let tick = tick_of(self.cfg.ping_every, self.cfg.dead_after);
        loop {
            if link.is_closed() {
                return "closed";
            }
            match reader.recv(tick) {
                Ok(Some(frame)) => {
                    last_rx = Instant::now();
                    if let Some(why) = self.handle_frame(frame, link, device, serving) {
                        return why;
                    }
                }
                Ok(None) => {}
                Err(LinkError::Closed) => return "closed",
                Err(LinkError::Codec(_) | LinkError::Frame(_)) => {
                    link.try_bye(ByeReason::BadMessage);
                    return "bad message";
                }
                Err(_) => return "error",
            }
            if last_rx.elapsed() > self.cfg.dead_after {
                link.try_bye(ByeReason::Timeout);
                return "timeout";
            }
            if last_ping.elapsed() >= self.cfg.ping_every {
                ping_n += 1;
                if link.send_msg(Msg::Ping { n: ping_n }).is_err() {
                    return "closed";
                }
                last_ping = Instant::now();
            }
        }
    }

    /// `Some(reason)` ends the connection.
    fn handle_frame(&self, frame: Frame, link: &Arc<Link>, device: &Device, serving: Option<&Arc<Serving>>) -> Option<&'static str> {
        if let Some(s) = serving
            && s.on_frame(&frame)
        {
            return None;
        }
        match frame {
            Frame::Control(Msg::Ping { n }) => {
                let _ = link.send_msg(Msg::Pong { n });
            }
            Frame::Control(Msg::Bye { .. }) => return Some("client left"),
            Frame::Control(Msg::Call { id, tool, params }) => self.spawn_call(link, device, id, tool, params),
            Frame::Control(Msg::ConsentAnswer { request_id, allow }) => self.spawn_consent(link, device, request_id, allow),
            // Terminal messages reach here only when no engine is attached: say no, not nothing.
            Frame::Control(Msg::TermSubscribe { stream, .. }) => {
                let _ = link.send_msg(Msg::TermClosed { stream, reason: csm_peer_proto::msg::TermClosedReason::Forbidden });
            }
            _ => {}
        }
        None
    }

    fn caller(device: &Device) -> Caller {
        Caller::Peer { device_id: device.id.clone(), name: device.name.clone(), level: device.level }
    }

    /// A tool call runs on its own thread (it may wait for the engine), so pings keep flowing.
    fn spawn_call(&self, link: &Arc<Link>, device: &Device, id: u64, tool: String, params: Value) {
        let Some(engine) = self.engine.get().cloned() else {
            let _ = link.send_msg(Msg::Reply { id, outcome: err_outcome(ErrorKind::Refused, "this host's engine is not attached yet") });
            return;
        };
        if self.in_flight.fetch_add(1, Ordering::AcqRel) >= self.cfg.max_in_flight_calls {
            self.in_flight.fetch_sub(1, Ordering::AcqRel);
            self.audit.record("refused", json!({"why": "too many calls at once", "device_id": device.id, "tool": clean(&tool)}));
            let _ = link.send_msg(Msg::Reply { id, outcome: err_outcome(ErrorKind::Refused, "too many calls at once; wait for one to finish") });
            return;
        }
        let (link, caller) = (link.clone(), Self::caller(device));
        let counter = InFlight(self.in_flight.clone());
        let spawned = std::thread::Builder::new().name("csm-peer-call".into()).spawn(move || {
            let _counter = counter;
            // A revoke or turn-off closed the link while this call waited to start: it does not run.
            if link.is_closed() {
                return;
            }
            let outcome = match engine.call(caller, &tool, params) {
                Ok(o) => Outcome::Ok { message: o.message, data: o.data },
                Err(e) => outcome_of(&e),
            };
            send_reply(&link, id, outcome);
        });
        if let Err(e) = spawned {
            log::warn!("remote access: could not start a call: {e}");
        }
    }

    fn spawn_consent(&self, link: &Arc<Link>, device: &Device, request_id: u64, allow: bool) {
        let Some(engine) = self.engine.get().cloned() else { return };
        let (link, caller) = (link.clone(), Self::caller(device));
        let spawned = std::thread::Builder::new().name("csm-peer-consent".into()).spawn(move || {
            if link.is_closed() {
                return;
            }
            let answer = if allow { "allow_once" } else { "deny" };
            let outcome = match engine.call(caller, "consent_answer", json!({"request_id": request_id, "answer": answer})) {
                Ok(_) => ConsentOutcome::Applied,
                // The first answer won: the card is gone.
                Err(ToolError::NotFound(_)) => ConsentOutcome::AlreadyResolved,
                Err(_) => ConsentOutcome::Forbidden,
            };
            let _ = link.send_msg(Msg::ConsentAck { request_id, outcome });
        });
        if let Err(e) = spawned {
            log::warn!("remote access: could not start a consent answer: {e}");
        }
    }

    /// Cards that were already waiting when a Full device connected.
    fn send_waiting_cards(&self, device: &Device, link: &Arc<Link>) {
        if !device.level.allows(Level::Full) {
            return;
        }
        let Some(engine) = self.engine.get().cloned() else { return };
        let link = link.clone();
        let _ = std::thread::Builder::new().name("csm-peer-cards".into()).spawn(move || {
            if let Ok(list) = engine.call(Caller::Ui, "consent_list", Value::Null)
                && let Value::Array(cards) = list.data
            {
                for request in cards {
                    let _ = link.send_msg(Msg::ConsentCard { request });
                }
            }
        });
    }

    /// Sends `msg` to every connected device at `level` or above.
    fn broadcast(&self, level: Level, msg: &Msg) {
        let links: Vec<Arc<Link>> = self.lock().conns.values().filter(|c| c.level.allows(level)).map(|c| c.link.clone()).collect();
        for l in links {
            let _ = l.send_msg(msg.clone());
        }
    }
}


/// Where a connection came from, which listener run it belongs to, and when its handshake is due.
#[derive(Clone, Copy)]
struct Arrival {
    peer: From,
    gen_: u64,
    deadline: Instant,
}

/// Counts one unauthenticated connection until it is released (or dropped).
struct Unauth<'a> {
    inner: &'a Inner,
    held: bool,
}

impl Unauth<'_> {
    fn release(&mut self) {
        if self.held {
            self.held = false;
            self.inner.unauth.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

impl Drop for Unauth<'_> {
    fn drop(&mut self) {
        self.release();
    }
}

/// Counts one running tool call; the thread that owns it ends it by dropping it.
struct InFlight(Arc<AtomicUsize>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// How long a read waits before the loop checks its clocks: well inside both the ping interval and
/// the dead time, at most a second.
pub(crate) fn tick_of(ping_every: Duration, dead_after: Duration) -> Duration {
    ping_every.min(dead_after / 4).clamp(Duration::from_millis(10), Duration::from_secs(1))
}

fn err_outcome(kind: ErrorKind, message: &str) -> Outcome {
    Outcome::Err { kind, message: message.to_owned() }
}

/// The message of an error as the wire allows it: no control characters but newline and tab.
fn wire_text(s: &str) -> String {
    s.chars().map(|c| if c.is_control() && c != '\n' && c != '\t' { ' ' } else { c }).collect()
}

fn outcome_of(e: &ToolError) -> Outcome {
    let kind = match e {
        ToolError::UnknownTool(_) => ErrorKind::UnknownTool,
        ToolError::InvalidParams(_) => ErrorKind::InvalidParams,
        ToolError::NotFound(_) => ErrorKind::NotFound,
        ToolError::Refused(_) => ErrorKind::Refused,
        ToolError::Declined(_) => ErrorKind::Declined,
        ToolError::Failed(_) => ErrorKind::Failed,
        ToolError::TimedOut(_) => ErrorKind::TimedOut,
    };
    Outcome::Err { kind, message: wire_text(&e.to_string()) }
}

/// Sends a reply; a result too big for the wire becomes an error the caller can read.
fn send_reply(link: &Link, id: u64, outcome: Outcome) {
    if let Err(LinkError::Codec(_)) = link.send_msg(Msg::Reply { id, outcome }) {
        let _ = link.send_msg(Msg::Reply { id, outcome: err_outcome(ErrorKind::Failed, "the result is too large to send") });
    }
}

/// Engine events that belong on other screens: consent cards and their resolution.
fn forward_events(inner: &Arc<Inner>, rx: &Receiver<csm_engine::Event>) {
    use csm_engine::Event;
    loop {
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(Event::ConsentRequested { request }) => inner.broadcast(Level::Full, &Msg::ConsentCard { request }),
            Ok(ev @ Event::ConsentResolved { .. }) => {
                inner.broadcast(Level::Full, &Msg::Event { event: Some(csm_peer_proto::Event::from(&ev)) });
            }
            // What a screen shows without asking: toasts, errors and add-on screens. The snapshot
            // carries sessions, groups and the inbox; the rest of the events are the office's own.
            Ok(ev @ (Event::Toast { .. } | Event::Error { .. } | Event::ScreenChanged { .. })) => {
                inner.broadcast(Level::Viewer, &Msg::Event { event: Some(csm_peer_proto::Event::from(&ev)) });
            }
            Ok(_) | Err(mpsc::RecvTimeoutError::Timeout) => {
                if inner.dead.load(Ordering::Acquire) {
                    return;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

impl PeerHost for PeerServer {
    fn relay_set(&self, req: RelaySetRequest, by: &str) -> Result<Option<RelayInfo>, ToolError> {
        self.set_relay_from_request(req, by)
    }

    fn relay_status(&self) -> Option<RelayInfo> {
        self.relay_info()
    }

    fn status(&self) -> PeerStatus {
        self.inner.status()
    }

    fn enable(&self, req: EnableRequest, by: &str) -> Result<PeerStatus, ToolError> {
        let i = &self.inner;
        if i.dead.load(Ordering::Acquire) {
            return Err(ToolError::Failed("the app is closing".into()));
        }
        let probe = SocketAddr::new(req.addr.ip(), if req.addr.port() == 0 { 1 } else { req.addr.port() });
        check_bind(&probe, req.allow_all_interfaces).map_err(|e| ToolError::Refused(e.to_string()))?;
        // Turning it on again with another address replaces the listener.
        i.stop_all(ByeReason::ServerOff);
        let gen_ = {
            let mut st = i.lock();
            i.prepare(&mut st)?;
            st.generation += 1;
            st.choice = Some((req.addr, req.allow_all_interfaces));
            st.generation
        };
        if let Err(e) = i.try_listen(gen_) {
            i.lock().choice = None;
            // The old listener is gone, so the saved choice must say so: what the person sees is what starts up.
            i.save_choice(false, None, false);
            i.audit.record("enable_failed", json!({"by": clean(by), "address": req.addr.to_string(), "old_choice_cleared": true}));
            return Err(ToolError::Failed(format!("{e}. Check that the address belongs to this machine (for a WireGuard address, that the tunnel is up).")));
        }
        let bound = i.lock().run.as_ref().map(|r| r.addr);
        i.save_choice(true, bound, req.allow_all_interfaces);
        i.audit.record("enabled", json!({"by": clean(by), "address": bound.map(|a| a.to_string()), "all_interfaces": req.allow_all_interfaces}));
        self.restore_relay();
        Ok(i.status())
    }

    fn disable(&self, by: &str) -> PeerStatus {
        let i = &self.inner;
        i.stop_all(ByeReason::ServerOff);
        *lock_relay(i) = None;
        let was = i.lock().choice.take().is_some();
        if was || i.dir.join("server.json").exists() {
            i.save_choice(false, None, false);
        }
        if was {
            i.audit.record("disabled", json!({"by": clean(by)}));
        }
        i.status()
    }

    fn pair_start(&self, by: &str) -> Result<PairingStarted, ToolError> {
        let i = &self.inner;
        let id = crate::random::<PAIRING_ID_LEN>().map_err(|e| Inner::io_failed("random numbers", &e))?;
        let psk = crate::random::<PSK_LEN>().map_err(|e| Inner::io_failed("random numbers", &e))?;
        let (offer, secs) = {
            let mut st = i.lock();
            let Some(addr) = st.run.as_ref().map(|r| r.addr) else {
                return Err(ToolError::Refused("remote access is not listening; turn it on first".into()));
            };
            let Some(host) = st.host.as_ref().map(|h| *h.public()) else {
                return Err(ToolError::Failed("the host key is not loaded".into()));
            };
            let peer_addr = PeerAddr::from_socket(addr).map_err(|_| {
                ToolError::Refused("the server listens on every interface, so a pairing link has no one address to name; turn it on for one address instead".into())
            })?;
            st.slot = Some(Slot { id, psk, expires: Instant::now() + i.cfg.pair_ttl, attempts: 0, consumed: false });
            i.relay_open_slot(&id, i.cfg.pair_ttl.as_secs());
            let mut offer = PeerOffer::new(&peer_addr, &host, &id, &psk);
            if let Some(route) = i.relay_route() {
                offer = offer.with_relay(route);
            }
            (offer, i.cfg.pair_ttl.as_secs())
        };
        i.audit.record("pair_started", json!({"by": clean(by)}));
        Ok(PairingStarted { link: offer.link(), address: offer.a.clone(), expires_in_secs: secs })
    }

    fn pair_answer(&self, answer: PairAnswer, by: &str) -> Result<PeerStatus, ToolError> {
        let i = &self.inner;
        {
            let mut st = i.lock();
            let Some(p) = st.pending.as_mut() else {
                return Err(ToolError::NotFound("no device is waiting to pair".into()));
            };
            if p.since.elapsed() >= i.cfg.pending_ttl {
                st.pending = None;
                return Err(ToolError::NotFound("the pairing request expired".into()));
            }
            if answer.allow {
                // The person read the code on both screens: a wrong one is not an answer.
                if answer.check_code.as_deref().map(str::trim) != Some(p.check_code.as_str()) {
                    p.wrong += 1;
                    if p.wrong >= 3 {
                        let name = p.name.clone();
                        if let Some(done) = st.pending.take() {
                            let _ = done.answer.send(Answer::Deny);
                        }
                        drop(st);
                        i.audit.record("pair_denied", json!({"device": name, "by": clean(by), "why": "wrong check code three times"}));
                        return Err(ToolError::Refused("that is not the code on the other screen, three times: the device was denied".into()));
                    }
                    return Err(ToolError::Refused("that is not the code on the other screen; compare the four digits again".into()));
                }
                let (name, key) = (p.name.clone(), p.key);
                i.load_devices(&mut st)?;
                let added = st.devices.as_mut().map(|d| d.add(&name, &key, Level::Full, unix_secs()));
                i.relay_resync();
                match added {
                    Some(Ok(dev)) => {
                        if let Some(done) = st.pending.take() {
                            let _ = done.answer.send(Answer::Allow);
                        }
                        drop(st);
                        i.audit.record("paired", json!({"device_id": dev.id, "device": clean(&dev.name), "level": level_name(dev.level), "by": clean(by)}));
                    }
                    Some(Err(e)) => {
                        if let Some(done) = st.pending.take() {
                            let _ = done.answer.send(Answer::Deny);
                        }
                        return Err(Inner::io_failed("could not save the device", &e));
                    }
                    None => return Err(ToolError::Failed("the device list is not loaded".into())),
                }
            } else if let Some(done) = st.pending.take() {
                let name = done.name.clone();
                let _ = done.answer.send(Answer::Deny);
                drop(st);
                i.audit.record("pair_denied", json!({"device": name, "by": clean(by)}));
            }
        }
        Ok(i.status())
    }

    fn devices(&self) -> Vec<DeviceInfo> {
        let mut st = self.inner.lock();
        let _ = self.inner.load_devices(&mut st);
        let connected: Vec<String> = st.conns.values().map(|c| c.device_id.clone()).collect();
        st.devices
            .as_ref()
            .map(|d| {
                d.list()
                    .iter()
                    .map(|x| DeviceInfo {
                        id: x.id.clone(),
                        name: x.name.clone(),
                        level: level_name(x.level).to_owned(),
                        paired_at: x.paired_at,
                        last_seen: x.last_seen,
                        connected: connected.contains(&x.id),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn revoke(&self, target: Revoke, by: &str) -> Result<usize, ToolError> {
        let i = &self.inner;
        let (n, closing) = {
            let mut st = i.lock();
            i.load_devices(&mut st)?;
            let ids: Vec<String> = match &target {
                Revoke::One(id) => {
                    if st.devices.as_ref().and_then(|d| d.find(id)).is_none() {
                        return Err(ToolError::NotFound(format!("no paired device `{id}`; `peer_device_list` shows the ids")));
                    }
                    vec![id.clone()]
                }
                Revoke::All => st.devices.as_ref().map(|d| d.list().iter().map(|x| x.id.clone()).collect()).unwrap_or_default(),
            };
            let n = match (&target, st.devices.as_mut()) {
                (Revoke::One(id), Some(d)) => usize::from(d.remove(id).map_err(|e| Inner::io_failed("could not save the device list", &e))?),
                (Revoke::All, Some(d)) => d.clear().map_err(|e| Inner::io_failed("could not save the device list", &e))?,
                _ => 0,
            };
            i.relay_resync();
            let keys: Vec<u64> = st.conns.iter().filter(|(_, c)| ids.contains(&c.device_id)).map(|(k, _)| *k).collect();
            let closing: Vec<Conn> = keys.iter().filter_map(|k| st.conns.remove(k)).collect();
            // A device waiting to pair under a revoke-all is withdrawn as well.
            if matches!(target, Revoke::All)
                && let Some(p) = st.pending.take()
            {
                let _ = p.answer.send(Answer::Deny);
            }
            (n, closing)
        };
        for c in closing {
            c.link.try_bye(ByeReason::Revoked);
            c.link.close();
        }
        let what = match &target {
            Revoke::One(id) => json!({"device_id": id}),
            Revoke::All => json!({"all": true}),
        };
        i.audit.record("revoked", json!({"target": what, "count": n, "by": clean(by)}));
        Ok(n)
    }

    fn audit(&self, event: AuditEvent) {
        let a = &self.inner.audit;
        match event {
            AuditEvent::Refused { tool, caller } => a.record("refused", json!({"tool": clean(&tool), "caller": clean(&caller)})),
            AuditEvent::PeerCall { device_id, device_name, tool, session, effects } => a.record(
                "call",
                json!({"device_id": device_id, "device": clean(&device_name), "tool": clean(&tool), "session": session.as_deref().map(clean), "effects": format!("{effects:?}")}),
            ),
            AuditEvent::ConsentAnswered { by, request_id, allow, answer } => a.record("consent_answered", json!({"by": clean(&by), "request_id": request_id, "allow": allow, "answer": answer})),
        }
    }

    fn interfaces(&self) -> Vec<csm_engine::NetInterface> {
        crate::netif::list()
    }

    fn shutdown(&self) {
        self.inner.dead.store(true, Ordering::Release);
        self.inner.stop_all(ByeReason::ServerQuit);
    }
}

/// Where a connection came from, for the audit log: its address, or the relay that carried it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum From {
    Addr(SocketAddr),
    Relay,
}

impl std::fmt::Display for From {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Addr(a) => write!(f, "{a}"),
            Self::Relay => f.write_str("relay"),
        }
    }
}

/// The relay this server is reachable through: the running host, and the route the offer names.
struct RelayState {
    host: RelayHost,
    route: RelayRoute,
}

/// What the person set up for the relay: its address, the host secret (kept in the secret store,
/// never in the state file) and the operator's enrolment token, when the relay needs one.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub url: String,
    pub host_secret: [u8; 32],
    pub access: Option<String>,
}

impl PeerServer {
    /// Makes this server reachable through a relay as well (or, with `None`, stops that). The
    /// direct listener is unchanged. A bad address is refused before anything starts.
    pub fn set_relay(&self, cfg: Option<RelayConfig>) -> Result<(), String> {
        let i = &self.inner;
        let old = lock_relay(i).take();
        drop(old);
        let Some(cfg) = cfg else { return Ok(()) };
        let route = RelayRoute { url: cfg.url.clone(), host_id: csm_remote_proto::ids::host_id(&cfg.host_secret) };
        if !route.validate() {
            return Err("the relay address must be wss://, or ws:// to this PC".to_owned());
        }
        let for_allow = i.clone();
        let allow: AllowList = Arc::new(move || for_allow.relay_allow());
        let for_channels = i.clone();
        let on_channel: OnChannel = Box::new(move |stream, _info| for_channels.relay_channel(stream));
        let host = RelayHost::start(HostRoute { url: cfg.url, host_secret: cfg.host_secret, access: cfg.access }, allow, on_channel).map_err(|e| e.to_string())?;
        *lock_relay(i) = Some(RelayState { host, route });
        log::info!("remote access: also reachable through a relay");
        Ok(())
    }

    /// The relay route the offers name, when one is set.
    pub fn relay_route(&self) -> Option<RelayRoute> {
        self.inner.relay_route()
    }

    /// What the relay last said, when one is set.
    pub fn relay_status(&self) -> Option<HostStatus> {
        lock_relay(&self.inner).as_ref().map(|r| r.host.status())
    }
}

fn lock_relay(i: &Inner) -> MutexGuard<'_, Option<RelayState>> {
    i.relay.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Inner {
    fn relay_route(&self) -> Option<RelayRoute> {
        lock_relay(self).as_ref().map(|r| r.route.clone())
    }

    fn relay_open_slot(&self, id: &[u8; PAIRING_ID_LEN], secs: u64) {
        if let Some(r) = lock_relay(self).as_ref() {
            let expires_in = u32::try_from(secs).unwrap_or(SLOT_SECS).min(SLOT_SECS);
            r.host.control(HostToRelay::PairingOpen { pairing_id: b64_encode(id), expires_in });
        }
    }

    fn relay_slot_close(&self) {
        if let Some(r) = lock_relay(self).as_ref() {
            r.host.control(HostToRelay::PairingClose);
        }
    }

    fn relay_resync(&self) {
        if let Some(r) = lock_relay(self).as_ref() {
            r.host.resync();
        }
    }

    /// Every paired device's relay hash, as the relay's allow list wants them.
    fn relay_allow(&self) -> Vec<[u8; 32]> {
        let mut st = self.lock();
        let _ = self.load_devices(&mut st);
        st.devices.as_ref().map(|d| d.list().iter().filter_map(Device::public_key).map(|k| device_relay_hash(&k)).collect()).unwrap_or_default()
    }

    /// A relay channel from a paired device (or a pairing): served like a direct connection.
    fn relay_channel(self: &Arc<Self>, stream: Stream) {
        if self.unauth.fetch_add(1, Ordering::AcqRel) >= self.cfg.max_unauthenticated {
            self.unauth.fetch_sub(1, Ordering::AcqRel);
            let _ = stream.shutdown(std::net::Shutdown::Both);
            return;
        }
        let gen_ = self.lock().generation;
        let inner = self.clone();
        let spawned = std::thread::Builder::new().name("csm-peer-relay-conn".into()).spawn(move || inner.serve(stream, From::Relay, gen_));
        if spawned.is_err() {
            self.unauth.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// The keychain entries the relay uses (service `codeshell-peer`).
const RELAY_SERVICE: &str = "codeshell-peer";
const RELAY_SECRET_KEY: &str = "relay.host-secret";
const RELAY_TOKEN_KEY: &str = "relay.enrol-token";
/// The relay's address (not secret) in the peer folder; the secrets stay in the keychain.
const RELAY_FILE: &str = "relay.json";

#[derive(Serialize, Deserialize)]
struct RelayFile {
    v: u8,
    url: String,
}

fn read_secret(store: &dyn csm_services::secrets::SecretStore, key: &str) -> Option<String> {
    store.get(RELAY_SERVICE, key).ok().flatten()
}

impl PeerServer {
    /// Gives the server the keychain its relay secrets are kept in. Call once, before `resume`.
    pub fn use_secrets(&self, store: Arc<dyn csm_services::secrets::SecretStore>) {
        let _ = self.inner.secrets.set(store);
    }

    fn set_relay_from_request(&self, req: RelaySetRequest, by: &str) -> Result<Option<RelayInfo>, ToolError> {
        let i = &self.inner;
        let Some(store) = i.secrets.get().cloned() else {
            return Err(ToolError::Failed("no keychain is wired for the relay secrets".into()));
        };
        let url = req.url.trim().to_owned();
        let path = i.dir.join(RELAY_FILE);
        if url.is_empty() {
            *lock_relay(i) = None;
            let _ = std::fs::remove_file(&path);
            let _ = store.delete(RELAY_SERVICE, RELAY_TOKEN_KEY);
            i.audit.record("relay_off", json!({"by": clean(by)}));
            return Ok(None);
        }
        if !csm_peer_proto::pairing::relay_url_allowed(&url) || url.len() > 512 {
            return Err(ToolError::InvalidParams("the relay address must be wss://, or ws:// to this PC".into()));
        }
        if req.access.as_ref().is_some_and(|t| t.len() > 256) {
            return Err(ToolError::InvalidParams("an enrolment token is at most 256 characters".into()));
        }
        // The host secret is made once and kept: the same relay id survives a change of address.
        let secret = match read_secret(store.as_ref(), RELAY_SECRET_KEY).and_then(|s| b64_decode_fixed::<32>(&s)) {
            Some(s) => s,
            None => {
                let s = crate::random::<32>().map_err(|e| Inner::io_failed("random numbers", &e))?;
                store.set(RELAY_SERVICE, RELAY_SECRET_KEY, &b64_encode(&s)).map_err(|e| ToolError::Failed(format!("could not keep the relay secret: {e}")))?;
                s
            }
        };
        match &req.access {
            Some(t) => store.set(RELAY_SERVICE, RELAY_TOKEN_KEY, t).map_err(|e| ToolError::Failed(format!("could not keep the enrolment token: {e}")))?,
            None => {
                let _ = store.delete(RELAY_SERVICE, RELAY_TOKEN_KEY);
            }
        }
        self.set_relay(Some(RelayConfig { url: url.clone(), host_secret: secret, access: req.access.clone() })).map_err(ToolError::Refused)?;
        let text = serde_json::to_string(&RelayFile { v: 1, url: url.clone() }).unwrap_or_default();
        crate::fsperm::ensure_private_dir(&i.dir).map_err(|e| Inner::io_failed("could not save the relay address", &e))?;
        crate::fsperm::write_private(&path, text.as_bytes()).map_err(|e| Inner::io_failed("could not save the relay address", &e))?;
        i.audit.record("relay_set", json!({"by": clean(by)}));
        Ok(self.relay_info())
    }

    /// Starts the relay the person saved, when the keychain still has its secret. Called when remote
    /// access comes on, so a relay never runs while remote access is off.
    fn restore_relay(&self) {
        if lock_relay(&self.inner).is_some() {
            return;
        }
        let path = self.inner.dir.join(RELAY_FILE);
        let Ok(text) = std::fs::read_to_string(&path) else { return };
        let Ok(file) = serde_json::from_str::<RelayFile>(&text) else {
            log::warn!("remote access: {} is not readable; the relay stays off", path.display());
            return;
        };
        let Some(store) = self.inner.secrets.get().cloned() else { return };
        let Some(secret) = read_secret(store.as_ref(), RELAY_SECRET_KEY).and_then(|s| b64_decode_fixed::<32>(&s)) else {
            log::warn!("remote access: the relay secret is missing from the keychain; the relay stays off");
            return;
        };
        let access = read_secret(store.as_ref(), RELAY_TOKEN_KEY);
        if let Err(e) = self.set_relay(Some(RelayConfig { url: file.url, host_secret: secret, access })) {
            log::warn!("remote access: the saved relay is not used: {e}");
        }
    }

    fn relay_info(&self) -> Option<RelayInfo> {
        let guard = lock_relay(&self.inner);
        let r = guard.as_ref()?;
        let st = r.host.status();
        Some(RelayInfo { url: r.route.url.clone(), host_id: r.route.host_id.clone(), connected: st.connected, last_error: st.last_error })
    }
}
