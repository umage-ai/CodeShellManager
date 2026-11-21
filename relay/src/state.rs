//! The relay's whole state: live host sockets, their allow lists, pairing slots and channels, and
//! the rate-limit counters. All in memory; the hosts re-send everything when they reconnect.
//!
//! Every method takes the lock briefly and never awaits while holding it.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{PAIRING_ID_LEN, ct_eq};
use csm_remote_proto::relay::{FrameKind, OpenInfo, close_payload, encode_frame};
use tokio::sync::mpsc;

use crate::config::Settings;
use crate::limiter::{ChannelLimiter, Window};
use crate::push::PushSender;

pub const HOST_QUEUE: usize = 256;
pub const CLIENT_QUEUE: usize = 64;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the relay sends down a host socket.
#[derive(Debug)]
pub enum HostOut {
    Text(String),
    Binary(Vec<u8>),
    /// Close the socket (this connection was replaced, or the relay is shutting down).
    Shutdown(RefusalCode),
}

/// What the relay sends down a phone socket.
#[derive(Debug)]
pub enum ClientOut {
    Binary(Vec<u8>),
    /// Close with this code, or normally.
    Close(Option<RefusalCode>),
}

pub type HostTx = mpsc::Sender<HostOut>;

struct Slot {
    pairing_id: [u8; PAIRING_ID_LEN],
    expires: Instant,
    attempts: u8,
    /// A pairing channel is currently open on this slot.
    active: bool,
}

struct Chan {
    tx: mpsc::Sender<ClientOut>,
    device: Option<[u8; 32]>,
    pairing: bool,
    /// Host to phone direction.
    limiter: ChannelLimiter,
}

struct HostEntry {
    conn: u64,
    tx: HostTx,
    allow: HashSet<[u8; 32]>,
    slot: Option<Slot>,
    channels: HashMap<u32, Chan>,
    next_channel: u32,
}

#[derive(Default)]
struct IpEntry {
    sockets: usize,
    recent: Window,
}

#[derive(Default)]
struct PushCounters {
    per_host: HashMap<String, Window>,
    per_endpoint: HashMap<[u8; 32], Window>,
}

pub struct Relay {
    pub settings: Settings,
    hosts: Mutex<HashMap<String, HostEntry>>,
    ips: Mutex<HashMap<IpAddr, IpEntry>>,
    push_counters: Mutex<PushCounters>,
    pub push: Option<Arc<dyn PushSender>>,
    next_conn: AtomicU64,
}

/// Holds a socket slot for one IP; released on drop.
pub struct IpGuard {
    relay: Arc<Relay>,
    ip: IpAddr,
}

impl Drop for IpGuard {
    fn drop(&mut self) {
        let mut ips = lock(&self.relay.ips);
        if let Some(e) = ips.get_mut(&self.ip) {
            e.sockets = e.sockets.saturating_sub(1);
        }
    }
}

/// A channel the relay opened for a phone.
pub struct OpenedChannel {
    pub channel: u32,
    pub host_tx: HostTx,
    pub rx: mpsc::Receiver<ClientOut>,
}

pub enum Credential {
    Device([u8; 32]),
    Pairing([u8; PAIRING_ID_LEN]),
}

impl Relay {
    pub fn new(settings: Settings, push: Option<Arc<dyn PushSender>>) -> Arc<Relay> {
        Arc::new(Relay {
            settings,
            hosts: Mutex::new(HashMap::new()),
            ips: Mutex::new(HashMap::new()),
            push_counters: Mutex::new(PushCounters::default()),
            push,
            next_conn: AtomicU64::new(1),
        })
    }

    // ---- per-IP limits ----------------------------------------------------------------

    /// Counts a new connection from `ip`: refuses over the per-minute rate or the open-socket cap.
    pub fn admit_ip(self: &Arc<Self>, ip: IpAddr) -> Result<IpGuard, RefusalCode> {
        let l = &self.settings.limits;
        let mut ips = lock(&self.ips);
        let e = ips.entry(ip).or_default();
        if e.sockets >= l.max_sockets_per_ip || !e.recent.hit(Instant::now(), Duration::from_secs(60), l.new_connections_per_ip_per_minute) {
            return Err(RefusalCode::RateLimited);
        }
        e.sockets += 1;
        Ok(IpGuard { relay: self.clone(), ip })
    }

    // ---- hosts ------------------------------------------------------------------------

    /// Registers the live connection for `host_id`, replacing (and closing) an older one.
    pub fn register_host(&self, host_id: &str, tx: HostTx) -> Result<u64, RefusalCode> {
        let mut hosts = lock(&self.hosts);
        if !hosts.contains_key(host_id) && hosts.len() >= self.settings.limits.max_hosts {
            return Err(RefusalCode::RelayFull);
        }
        let conn = self.next_conn.fetch_add(1, Ordering::Relaxed);
        let entry = HostEntry { conn, tx, allow: HashSet::new(), slot: None, channels: HashMap::new(), next_channel: 1 };
        if let Some(old) = hosts.insert(host_id.to_owned(), entry) {
            let _ = old.tx.try_send(HostOut::Shutdown(RefusalCode::HostReplaced));
            for chan in old.channels.into_values() {
                let _ = chan.tx.try_send(ClientOut::Close(Some(RefusalCode::HostOffline)));
            }
        }
        Ok(conn)
    }

    /// Removes the host if `conn` is still its live connection; closes its phones.
    pub fn unregister_host(&self, host_id: &str, conn: u64) {
        let mut hosts = lock(&self.hosts);
        if hosts.get(host_id).is_some_and(|h| h.conn == conn)
            && let Some(old) = hosts.remove(host_id)
        {
            for chan in old.channels.into_values() {
                let _ = chan.tx.try_send(ClientOut::Close(Some(RefusalCode::HostOffline)));
            }
        }
    }

    /// Replaces the allow list; phones whose entry vanished are closed with `revoked`.
    pub fn set_allow(&self, host_id: &str, conn: u64, tokens: Vec<[u8; 32]>) -> Result<usize, RefusalCode> {
        if tokens.len() > self.settings.limits.max_allow_entries {
            return Err(RefusalCode::RelayFull);
        }
        let mut hosts = lock(&self.hosts);
        let h = live(&mut hosts, host_id, conn)?;
        h.allow = tokens.into_iter().collect();
        let revoked: Vec<u32> = h.channels.iter().filter(|(_, c)| c.device.is_some_and(|d| !h.allow.contains(&d))).map(|(id, _)| *id).collect();
        for id in revoked {
            if let Some(c) = h.channels.remove(&id) {
                let _ = c.tx.try_send(ClientOut::Close(Some(RefusalCode::Revoked)));
            }
        }
        Ok(h.allow.len())
    }

    pub fn open_slot(&self, host_id: &str, conn: u64, pairing_id: [u8; PAIRING_ID_LEN], secs: u32) -> Result<u32, RefusalCode> {
        let secs = secs.clamp(1, self.settings.limits.max_pairing_secs);
        let mut hosts = lock(&self.hosts);
        let h = live(&mut hosts, host_id, conn)?;
        h.slot = Some(Slot { pairing_id, expires: Instant::now() + Duration::from_secs(u64::from(secs)), attempts: 0, active: false });
        Ok(secs)
    }

    /// Drops the slot and any pairing channel still open on it.
    pub fn close_slot(&self, host_id: &str, conn: u64) -> Result<(), RefusalCode> {
        let mut hosts = lock(&self.hosts);
        let h = live(&mut hosts, host_id, conn)?;
        h.slot = None;
        let pairing: Vec<u32> = h.channels.iter().filter(|(_, c)| c.pairing).map(|(id, _)| *id).collect();
        for id in pairing {
            if let Some(c) = h.channels.remove(&id) {
                let _ = c.tx.try_send(ClientOut::Close(None));
            }
        }
        Ok(())
    }

    // ---- channels ---------------------------------------------------------------------

    /// A phone presents a credential for `host_id`. On success the host is told (`Open` frame).
    pub fn open_channel(&self, host_id: &str, cred: &Credential) -> Result<OpenedChannel, RefusalCode> {
        let l = &self.settings.limits;
        let mut hosts = lock(&self.hosts);
        let h = hosts.get_mut(host_id).ok_or(RefusalCode::HostOffline)?;
        let now = Instant::now();
        if h.channels.len() >= l.max_channels_per_host {
            return Err(RefusalCode::RelayFull);
        }
        let info = match cred {
            Credential::Device(hash) => {
                if !h.allow.contains(hash) {
                    return Err(RefusalCode::NotAllowed);
                }
                OpenInfo::Device(*hash)
            }
            Credential::Pairing(id) => {
                let slot = h.slot.as_mut().ok_or(RefusalCode::NotAllowed)?;
                if now >= slot.expires || !ct_eq(&slot.pairing_id, id) || slot.attempts >= l.max_pairing_attempts || slot.active {
                    return Err(RefusalCode::NotAllowed);
                }
                slot.attempts += 1;
                slot.active = true;
                OpenInfo::Pairing(*id)
            }
        };
        let channel = loop {
            let c = h.next_channel;
            h.next_channel = h.next_channel.wrapping_add(1).max(1);
            if !h.channels.contains_key(&c) {
                break c;
            }
        };
        let (tx, rx) = mpsc::channel(CLIENT_QUEUE);
        let device = match info {
            OpenInfo::Device(d) => Some(d),
            OpenInfo::Pairing(_) => None,
        };
        let pairing = device.is_none();
        let limiter = ChannelLimiter::new(l.frames_per_second_per_channel, l.bytes_per_second_per_channel, csm_remote_proto::limits::BURST_FACTOR, now);
        if h.tx.try_send(HostOut::Binary(encode_frame(FrameKind::Open, channel, &info.encode()))).is_err() {
            if let Some(slot) = h.slot.as_mut() {
                slot.active = false;
            }
            return Err(RefusalCode::RateLimited);
        }
        h.channels.insert(channel, Chan { tx, device, pairing, limiter });
        Ok(OpenedChannel { channel, host_tx: h.tx.clone(), rx })
    }

    /// The phone side of `channel` is gone: tell the host (unless the channel was already closed).
    pub fn channel_gone(&self, host_id: &str, channel: u32, code: Option<RefusalCode>) {
        let mut hosts = lock(&self.hosts);
        let Some(h) = hosts.get_mut(host_id) else { return };
        if let Some(c) = h.channels.remove(&channel) {
            if c.pairing
                && let Some(s) = h.slot.as_mut()
            {
                s.active = false;
            }
            let _ = h.tx.try_send(HostOut::Binary(encode_frame(FrameKind::Close, channel, &close_payload(code))));
        }
    }

    /// The host asked to close `channel` (for example a pairing it denied).
    pub fn host_closes_channel(&self, host_id: &str, conn: u64, channel: u32, code: Option<RefusalCode>) {
        let mut hosts = lock(&self.hosts);
        let Ok(h) = live(&mut hosts, host_id, conn) else { return };
        if let Some(c) = h.channels.remove(&channel) {
            if c.pairing
                && let Some(s) = h.slot.as_mut()
            {
                s.active = false;
            }
            let _ = c.tx.try_send(ClientOut::Close(code));
        }
    }

    /// A Noise message from the host for a phone. Over-rate or a stuck phone closes the channel.
    pub fn host_data(&self, host_id: &str, conn: u64, channel: u32, payload: &[u8]) {
        let mut hosts = lock(&self.hosts);
        let Ok(h) = live(&mut hosts, host_id, conn) else { return };
        let Some(c) = h.channels.get_mut(&channel) else { return };
        let now = Instant::now();
        let verdict = if !c.limiter.allow(payload.len(), now) {
            Some(RefusalCode::RateLimited)
        } else {
            match c.tx.try_send(ClientOut::Binary(payload.to_vec())) {
                Ok(()) => None,
                Err(mpsc::error::TrySendError::Full(_)) => Some(RefusalCode::RateLimited),
                Err(mpsc::error::TrySendError::Closed(_)) => Some(RefusalCode::HostOffline),
            }
        };
        if let Some(code) = verdict
            && let Some(c) = h.channels.remove(&channel)
        {
            let _ = c.tx.try_send(ClientOut::Close(Some(code)));
            let _ = h.tx.try_send(HostOut::Binary(encode_frame(FrameKind::Close, channel, &close_payload(Some(code)))));
        }
    }

    // ---- push -------------------------------------------------------------------------

    /// Charges one push against the per-host and per-endpoint hourly caps.
    pub fn push_allowed(&self, host_id: &str, endpoint_hash: [u8; 32]) -> bool {
        let hour = Duration::from_secs(3600);
        let now = Instant::now();
        let p = &self.settings.push;
        let mut c = lock(&self.push_counters);
        // Charge both or neither: a push refused by one cap must not use up the other's budget.
        let room = c.per_host.entry(host_id.to_owned()).or_default().has_room(now, hour, p.per_host_per_hour)
            && c.per_endpoint.entry(endpoint_hash).or_default().has_room(now, hour, p.per_endpoint_per_hour);
        if room {
            c.per_host.entry(host_id.to_owned()).or_default().record(now);
            c.per_endpoint.entry(endpoint_hash).or_default().record(now);
        }
        room
    }

    // ---- housekeeping and stats -------------------------------------------------------

    /// Drops idle bookkeeping and expired slots. Called periodically.
    pub fn sweep(&self) {
        let now = Instant::now();
        lock(&self.ips).retain(|_, e| {
            e.recent.prune(now, Duration::from_secs(60));
            e.sockets > 0 || !e.recent.is_empty()
        });
        let mut c = lock(&self.push_counters);
        let hour = Duration::from_secs(3600);
        c.per_host.retain(|_, w| {
            w.prune(now, hour);
            !w.is_empty()
        });
        c.per_endpoint.retain(|_, w| {
            w.prune(now, hour);
            !w.is_empty()
        });
        drop(c);
        for h in lock(&self.hosts).values_mut() {
            if h.slot.as_ref().is_some_and(|s| now >= s.expires && !s.active) {
                h.slot = None;
            }
        }
    }

    /// `(hosts, channels)` for the status log line.
    pub fn counts(&self) -> (usize, usize) {
        let hosts = lock(&self.hosts);
        (hosts.len(), hosts.values().map(|h| h.channels.len()).sum())
    }

    /// Asks every host socket to close (shutdown).
    pub fn shutdown_all(&self, code: RefusalCode) {
        for h in lock(&self.hosts).values() {
            let _ = h.tx.try_send(HostOut::Shutdown(code));
        }
    }

    /// Queue handle for a live host (for pushing results back).
    pub fn host_tx(&self, host_id: &str, conn: u64) -> Option<HostTx> {
        lock(&self.hosts).get(host_id).filter(|h| h.conn == conn).map(|h| h.tx.clone())
    }

    /// Pending (unconsumed) pairing slot, for tests.
    pub fn has_slot(&self, host_id: &str) -> bool {
        lock(&self.hosts).get(host_id).is_some_and(|h| h.slot.is_some())
    }
}

fn live<'a>(hosts: &'a mut HashMap<String, HostEntry>, host_id: &str, conn: u64) -> Result<&'a mut HostEntry, RefusalCode> {
    hosts.get_mut(host_id).filter(|h| h.conn == conn).ok_or(RefusalCode::HostReplaced)
}

