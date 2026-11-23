//! The host's protocol core, with no sockets in it: relay frames in, relay frames and control
//! messages out. The relay client (`relay_client.rs`) moves bytes between this and a WebSocket;
//! the tests drive it directly with a test phone.
//!
//! What it does, per channel:
//! - a paired phone opens with `OpenInfo::Device`, runs `Noise_IK` (its static key must be the
//!   one saved at pairing), then sends requests that the [`Engine`] answers;
//! - a phone that scanned the offer opens with `OpenInfo::Pairing`, runs `Noise_IKpsk2` with the
//!   offer's PSK, and sends `PairMsg::Hello`. The person then allows or denies it on the PC.
//!
//! Every reply is produced here; nothing is sent to a phone except through [`Out`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{b64_decode_fixed, b64_encode, hex, host_id, token_hash};
use csm_remote_proto::limits::{MAX_APP_MESSAGE_BYTES, MAX_CHANNELS_PER_HOST};
use csm_remote_proto::msg::{self, ByeReason, ErrorCode, Event, Level, Request, Response};
use csm_remote_proto::noise::{MAX_CHANNEL_SECS, MAX_NOISE_MESSAGE, PARAMS_PAIRING, PARAMS_SESSION, PSK_POSITION, REKEY_INTERVAL, check_code, prologue};
use csm_remote_proto::pairing::{DenyReason, PairMsg, PairingOffer};
use csm_remote_proto::relay::{FrameKind, HostToRelay, OpenInfo, close_payload, decode_frame, encode_frame};
use serde::Serialize;

use crate::config::Config;
use crate::csm::Csm;
use crate::ops::Engine;
use crate::pairing::{Refusal, Slot};
use crate::store::{self, Device, Opened};
use crate::worker::{Done, Job, Refused, Worker};

/// Requests the host keeps in flight at once, across all channels (and per channel). A request
/// beyond it is answered `rate_limited` at once; it is not queued.
pub const MAX_REQUESTS_IN_FLIGHT: usize = 8;

/// Seconds a channel may take to finish its handshake before the host closes it.
pub const HANDSHAKE_SECS: u64 = 20;
/// The longest device name a pairing may carry (the same limit as a saved device).
const MAX_PAIR_NAME_CHARS: usize = 64;

/// Something the host wants done, for the relay client or the screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Out {
    /// A binary host frame for the relay (a Noise message, a close).
    Frame(Vec<u8>),
    /// A text control message for the relay.
    Control(HostToRelay),
    /// A phone is waiting for the person's decision: its check code and the name it sent, cleaned
    /// of control and hidden format characters (the same text is what gets saved).
    Pending { check: String, name: String },
    /// The offer link for a new pairing (it carries the PSK: the pairing screen only, never a log).
    PairingUrl(String),
    /// The pairing ended: accepted, denied, expired or stopped.
    PairingEnded,
    /// The device list changed and was saved.
    DevicesChanged,
    /// Something the person should see, in plain words (never a secret).
    Problem(String),
}

/// One paired device, as shown to the person.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeviceView {
    pub device_id: String,
    pub name: String,
    pub level: Level,
    pub paired_at: u64,
    pub last_seen: u64,
}

/// Where one channel is in its life.
pub enum ChanState {
    DevHandshake { hs: Box<snow::HandshakeState>, device_id: String },
    PairHandshake { hs: Box<snow::HandshakeState> },
    /// A phone finished the pairing handshake and waits for Allow, Deny or the expiry.
    PairWait { transport: Box<snow::TransportState>, name: String, phone_pub: [u8; 32], check: String },
    Session { transport: Box<snow::TransportState>, device_id: String, sent: u64, received: u64 },
}

pub struct Chan {
    pub state: ChanState,
    pub opened: u64,
}

pub struct Host {
    pub dir: PathBuf,
    pub cfg: Config,
    pub devices: Vec<Device>,
    pub host_priv: [u8; 32],
    pub host_pub: [u8; 32],
    pub host_id: String,
    /// The ops run on this worker thread, never on the relay loop.
    pub worker: Worker,
    /// Requests sent to the worker and not yet answered, per channel.
    pub inflight: BTreeMap<u32, usize>,
    pub chans: BTreeMap<u32, Chan>,
    pub slot: Option<Slot>,
    /// Seconds an opened pairing offer stays valid: the spec's ten minutes. Tests set it shorter
    /// before the host starts, to see the expiry through the real relay.
    pub pairing_window: u64,
}

/// Random bytes from the OS.
pub fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| format!("no random bytes: {e}"))?;
    Ok(b)
}

/// The public key of an X25519 private key, through snow's own primitive.
pub fn public_of(private: &[u8; 32]) -> Option<[u8; 32]> {
    use snow::resolvers::{CryptoResolver, DefaultResolver};
    let mut dh = DefaultResolver.resolve_dh(&snow::params::DHChoice::Curve25519)?;
    dh.set(private);
    <[u8; 32]>::try_from(dh.pubkey()).ok()
}

fn encode_json<T: Serialize>(v: &T) -> Option<Vec<u8>> {
    serde_json::to_vec(v).ok()
}

impl Host {
    /// Builds the host from its saved files. `host_secret` (32 bytes, from the keychain) decides
    /// the host id the relay knows.
    pub fn new(dir: PathBuf, opened: Opened, cfg: Config, host_secret: &[u8; 32], csm: Box<dyn Csm>) -> Result<Self, String> {
        let host_pub = public_of(&opened.host_key).ok_or("the host key is not usable")?;
        let worker = Worker::start(Engine::new(csm))?;
        Ok(Self {
            dir,
            cfg,
            devices: opened.devices,
            host_priv: opened.host_key,
            host_pub,
            host_id: host_id(host_secret),
            worker,
            inflight: BTreeMap::new(),
            chans: BTreeMap::new(),
            slot: None,
            pairing_window: u64::from(csm_remote_proto::limits::PAIRING_SLOT_MAX_SECS),
        })
    }

    /// The paired devices as the screen and the tools show them: no keys and no token hashes.
    pub fn devices_view(&self) -> Vec<DeviceView> {
        self.devices.iter().map(|d| DeviceView { device_id: d.device_id.clone(), name: d.name.clone(), level: d.level, paired_at: d.paired_at, last_seen: d.last_seen }).collect()
    }

    /// The allow list the relay gets: the hash of each device's routing token.
    pub fn allow_tokens(&self) -> Vec<String> {
        self.devices.iter().map(|d| d.token_hash.clone()).collect()
    }

    fn allow_out(&self) -> Out {
        Out::Control(HostToRelay::Allow { tokens: self.allow_tokens() })
    }

    /// Opens the pairing slot and returns the offer URL for the pairing screen. The URL carries the
    /// PSK: it goes only into the screen tree, never into a log or a result.
    pub fn start_pairing(&mut self, now: u64, app_version: &str) -> Result<(String, Vec<Out>), String> {
        if !self.cfg.relay_allowed() || self.cfg.relay.is_empty() {
            return Err("no relay is set, so there is nothing to pair through".into());
        }
        if self.slot.as_ref().is_some_and(|s| !s.expired(now)) {
            return Err("a pairing is already open".into());
        }
        let pairing_id: [u8; 16] = random()?;
        let psk: [u8; 32] = random()?;
        let offer = PairingOffer::new(&self.cfg.relay, &self.host_id, &self.host_pub, &pairing_id, &psk);
        let url = offer.url(&self.cfg.app_origin, app_version);
        self.slot = Some(Slot::open_for(pairing_id, psk, now, self.pairing_window));
        let expires_in = self.slot.as_ref().map_or(0, |s| s.seconds_left(now));
        Ok((url, vec![Out::Control(HostToRelay::PairingOpen { pairing_id: b64_encode(&pairing_id), expires_in })]))
    }

    /// Stop pairing: a waiting phone is denied and the slot closes.
    pub fn stop_pairing(&mut self, now: u64) -> Vec<Out> {
        self.end_pairing(now, Some(DenyReason::Denied))
    }

    /// Ends the pairing slot. A phone waiting in it gets `deny` (when given) and its channel closes.
    fn end_pairing(&mut self, _now: u64, deny: Option<DenyReason>) -> Vec<Out> {
        let mut outs = Vec::new();
        let pairing_channels: Vec<u32> = self.chans.iter().filter(|(_, c)| matches!(c.state, ChanState::PairHandshake { .. } | ChanState::PairWait { .. })).map(|(k, _)| *k).collect();
        for ch in pairing_channels {
            if let Some(reason) = deny
                && let Some(frame) = self.send_pair(ch, &PairMsg::Deny { reason })
            {
                outs.push(frame);
            }
            outs.push(self.close(ch, None));
        }
        if self.slot.take().is_some() {
            outs.push(Out::Control(HostToRelay::PairingClose));
            outs.push(Out::PairingEnded);
        }
        outs
    }

    /// The person's decision on the waiting phone: `Some(level)` allows it at that level, `None`
    /// denies it. Allow saves the device before the phone is told.
    pub fn decide(&mut self, now: u64, allow: Option<Level>) -> Vec<Out> {
        let Some(ch) = self.waiting_channel() else {
            return vec![Out::Problem("no phone is waiting for an answer".into())];
        };
        let Some(level) = allow else {
            return self.end_pairing(now, Some(DenyReason::Denied));
        };
        let Some(chan) = self.chans.remove(&ch) else { return Vec::new() };
        let opened = chan.opened;
        let ChanState::PairWait { mut transport, name, phone_pub, check } = chan.state else {
            return vec![Out::Problem("no phone is waiting for an answer".into())];
        };
        // Built first, so that a failure below can put the waiting channel back unchanged.
        let routing_token: Result<[u8; 32], String> = random();
        let device_id = random::<8>().map(|b| format!("d_{}", hex(&b)));
        let (routing_token, device_id) = match (routing_token, device_id) {
            (Ok(t), Ok(d)) => (t, d),
            (Err(e), _) | (_, Err(e)) => {
                self.chans.insert(ch, Chan { state: ChanState::PairWait { transport, name, phone_pub, check }, opened });
                return vec![Out::Problem(e)];
            }
        };
        let device = Device {
            device_id: device_id.clone(),
            kind: "phone".into(),
            name: crate::policy::clean_name(&name, 64),
            level,
            token_hash: b64_encode(&token_hash(&routing_token)),
            phone_pub: b64_encode(&phone_pub),
            paired_at: now,
            last_seen: now,
            hide_names: false,
            notify: true,
            push: None,
        };
        let mut next = self.devices.clone();
        next.push(device);
        if let Err(e) = store::save_devices(&self.dir, &next) {
            self.chans.insert(ch, Chan { state: ChanState::PairWait { transport, name, phone_pub, check }, opened });
            return vec![Out::Problem(format!("the phone was not paired: {e}"))];
        }
        self.devices = next;
        let accept = PairMsg::Accept { routing_token: b64_encode(&routing_token), level, device_id };
        let mut outs = Vec::new();
        if let Some(bytes) = encode_json(&accept).and_then(|plain| seal(&mut transport, &plain)) {
            outs.push(Out::Frame(encode_frame(FrameKind::Data, ch, &bytes)));
        }
        outs.push(self.close(ch, None));
        self.slot = None;
        outs.push(Out::Control(HostToRelay::PairingClose));
        outs.push(self.allow_out());
        outs.push(Out::DevicesChanged);
        outs.push(Out::PairingEnded);
        outs
    }

    /// Time-driven work: the pairing slot's expiry and the channels' maximum age.
    pub fn tick(&mut self, now: u64) -> Vec<Out> {
        let mut outs = Vec::new();
        while let Some(done) = self.worker.try_done() {
            outs.extend(self.finish(done));
        }
        if self.slot.as_ref().is_some_and(|s| s.expired(now)) {
            outs.extend(self.end_pairing(now, Some(DenyReason::Expired)));
        }
        // A channel that has not finished its handshake in time is closed, so a relay (or a phone)
        // that opens channels and goes silent cannot hold the channel budget for an hour.
        let stalled: Vec<u32> = self
            .chans
            .iter()
            .filter(|(_, c)| matches!(c.state, ChanState::DevHandshake { .. } | ChanState::PairHandshake { .. }) && now.saturating_sub(c.opened) >= HANDSHAKE_SECS)
            .map(|(k, _)| *k)
            .collect();
        let old: Vec<u32> = self.chans.iter().filter(|(_, c)| now.saturating_sub(c.opened) >= MAX_CHANNEL_SECS).map(|(k, _)| *k).collect();
        for ch in stalled.into_iter().chain(old) {
            outs.push(self.close(ch, None));
        }
        outs
    }

    /// Removes a device: its channels get `Bye { revoked }` and are closed here, whatever the relay
    /// does with the new allow list.
    pub fn revoke(&mut self, device_id: &str) -> Vec<Out> {
        self.remove_or_change(device_id, None)
    }

    /// Changes a device's level: its channels get `Bye { level_changed }` and close; the phone
    /// reconnects at the new level.
    pub fn set_level(&mut self, device_id: &str, level: Level) -> Vec<Out> {
        self.remove_or_change(device_id, Some(level))
    }

    fn remove_or_change(&mut self, device_id: &str, level: Option<Level>) -> Vec<Out> {
        let Some(pos) = self.devices.iter().position(|d| d.device_id == device_id) else {
            return vec![Out::Problem("no such phone".into())];
        };
        let mut next = self.devices.clone();
        match level {
            Some(l) => {
                if let Some(d) = next.get_mut(pos) {
                    d.level = l;
                }
            }
            None => {
                next.remove(pos);
            }
        }
        if let Err(e) = store::save_devices(&self.dir, &next) {
            return vec![Out::Problem(format!("the change was not saved: {e}"))];
        }
        self.devices = next;
        let reason = if level.is_some() { ByeReason::LevelChanged } else { ByeReason::Revoked };
        let victims: Vec<u32> = self.chans.iter().filter(|(_, c)| matches!(&c.state, ChanState::Session { device_id: d, .. } if d.as_str() == device_id)).map(|(k, _)| *k).collect();
        let mut outs = Vec::new();
        for ch in victims {
            if let Ok(plain) = msg::encode(&Event::Bye { reason })
                && let Some(chan) = self.chans.get_mut(&ch)
                && let ChanState::Session { transport, .. } = &mut chan.state
                && let Some(bytes) = seal(transport, &plain)
            {
                outs.push(Out::Frame(encode_frame(FrameKind::Data, ch, &bytes)));
            }
            outs.push(self.close(ch, None));
        }
        outs.push(self.allow_out());
        outs.push(Out::DevicesChanged);
        outs
    }

    /// One binary frame from the relay. Garbage is ignored; nothing here panics on bytes.
    pub fn on_frame(&mut self, bytes: &[u8], now: u64) -> Vec<Out> {
        let Ok(frame) = decode_frame(bytes) else { return Vec::new() };
        let ch = frame.channel;
        match frame.kind {
            FrameKind::Open => self.on_open(ch, frame.payload, now),
            FrameKind::Data => self.on_data(ch, frame.payload, now),
            FrameKind::Close => {
                self.forget(ch);
                Vec::new()
            }
        }
    }

    fn on_open(&mut self, ch: u32, payload: &[u8], now: u64) -> Vec<Out> {
        if self.chans.contains_key(&ch) {
            return vec![self.close(ch, Some(RefusalCode::BadRequest))];
        }
        if self.chans.len() >= MAX_CHANNELS_PER_HOST {
            return vec![self.close(ch, Some(RefusalCode::RelayFull))];
        }
        let state = match OpenInfo::decode(payload) {
            Some(OpenInfo::Device(hash)) => {
                let found = self.devices.iter().find(|d| b64_decode_fixed::<32>(&d.token_hash) == Some(hash));
                let Some(device) = found else { return vec![self.close(ch, Some(RefusalCode::NotAllowed))] };
                let Ok(hs) = build(PARAMS_SESSION, &self.host_priv, &prologue(&self.host_id), None) else {
                    return vec![self.close(ch, Some(RefusalCode::BadRequest))];
                };
                ChanState::DevHandshake { hs: Box::new(hs), device_id: device.device_id.clone() }
            }
            Some(OpenInfo::Pairing(id)) => {
                let Some(slot) = self.slot.as_mut() else { return vec![self.close(ch, Some(RefusalCode::NotAllowed))] };
                if slot.admit(&id, now).is_err() {
                    return vec![self.close(ch, Some(RefusalCode::NotAllowed))];
                }
                let psk = slot.psk;
                let Ok(hs) = build(PARAMS_PAIRING, &self.host_priv, &prologue(&self.host_id), Some(&psk)) else {
                    self.free_slot();
                    return vec![self.close(ch, Some(RefusalCode::BadRequest))];
                };
                ChanState::PairHandshake { hs: Box::new(hs) }
            }
            None => return vec![self.close(ch, Some(RefusalCode::BadRequest))],
        };
        self.chans.insert(ch, Chan { state, opened: now });
        Vec::new()
    }

    fn on_data(&mut self, ch: u32, payload: &[u8], now: u64) -> Vec<Out> {
        let Some(chan) = self.chans.remove(&ch) else { return Vec::new() };
        let pairing = is_pairing(&chan.state);
        if payload.len() > MAX_NOISE_MESSAGE {
            self.end_channel(pairing);
            return vec![self.close_ch_out(ch, Some(RefusalCode::FrameTooLarge))];
        }
        let opened = chan.opened;
        let (next, outs) = self.step(ch, chan.state, payload, now);
        match next {
            Some(state) => {
                self.chans.insert(ch, Chan { state, opened });
            }
            None => self.end_channel(pairing),
        }
        outs
    }

    /// A channel that ended: the pairing slot is free again when it was the pairing channel.
    fn end_channel(&mut self, pairing: bool) {
        if pairing {
            self.free_slot();
        }
    }

    /// One Data frame on a channel. Returns the new state (`None` when the channel is closed here)
    /// and what to send.
    fn step(&mut self, ch: u32, state: ChanState, payload: &[u8], now: u64) -> (Option<ChanState>, Vec<Out>) {
        match state {
            ChanState::DevHandshake { mut hs, device_id } => {
                let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
                if hs.read_message(payload, &mut buf).is_err() {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                }
                let known = self.devices.iter().find(|d| d.device_id == device_id).and_then(|d| b64_decode_fixed::<32>(&d.phone_pub));
                let remote = hs.get_remote_static().and_then(|r| <[u8; 32]>::try_from(r).ok());
                if known.is_none() || known != remote {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::NotAllowed))]);
                }
                let Ok(n) = hs.write_message(&[], &mut buf) else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                let Some(reply) = buf.get(..n).map(<[u8]>::to_vec) else { return (None, Vec::new()) };
                match hs.into_transport_mode() {
                    Ok(transport) => (Some(ChanState::Session { transport: Box::new(transport), device_id, sent: 0, received: 0 }), vec![Out::Frame(encode_frame(FrameKind::Data, ch, &reply))]),
                    Err(_) => (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]),
                }
            }
            ChanState::PairHandshake { mut hs } => {
                let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
                let Ok(n) = hs.read_message(payload, &mut buf) else {
                    // A wrong PSK (a stale or forged offer) fails here, before any app message.
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                let hello = buf.get(..n).and_then(|p| serde_json::from_slice::<PairMsg>(p).ok());
                // The name is cleaned before the person sees it on the decision screen, so a name
                // with bidi or zero-width characters cannot disguise itself there. A name that
                // cleans to nothing is refused.
                let name = match hello {
                    Some(PairMsg::Hello { v, name }) if v == csm_remote_proto::PROTOCOL_VERSION && PairMsg::valid_name(&name) => crate::policy::clean_name(&name, MAX_PAIR_NAME_CHARS),
                    _ => return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]),
                };
                if name.is_empty() {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                }
                let Some(phone_pub) = hs.get_remote_static().and_then(|r| <[u8; 32]>::try_from(r).ok()) else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                let Ok(m) = hs.write_message(&[], &mut buf) else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                let Some(reply) = buf.get(..m).map(<[u8]>::to_vec) else { return (None, Vec::new()) };
                // The code is taken after msg 2: the hash covers the whole handshake, as the phone's does.
                let check = check_code(hs.get_handshake_hash());
                let Ok(transport) = hs.into_transport_mode() else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                let state = ChanState::PairWait { transport: Box::new(transport), name: name.clone(), phone_pub, check: check.clone() };
                (Some(state), vec![Out::Frame(encode_frame(FrameKind::Data, ch, &reply)), Out::Pending { check, name }])
            }
            ChanState::PairWait { transport, name, phone_pub, check } => {
                // The phone has nothing more to say until the person answers.
                (Some(ChanState::PairWait { transport, name, phone_pub, check }), Vec::new())
            }
            ChanState::Session { mut transport, device_id, sent, mut received } => {
                let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
                let Ok(n) = transport.read_message(payload, &mut buf) else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                received += 1;
                if received.is_multiple_of(REKEY_INTERVAL) {
                    transport.rekey_incoming();
                }
                let plain = buf.get(..n).unwrap_or_default();
                let mut sent = sent;
                // A parsed request goes to the worker and is answered later (`finish`). Only a
                // refusal, or a message that does not parse, is answered here.
                let answer_now: Option<Response> = if plain.len() > MAX_APP_MESSAGE_BYTES {
                    Some(Response::err(0, ErrorCode::TooLarge, "the message is too large"))
                } else {
                    match Request::parse(plain) {
                        Ok(req) => self.submit(ch, &device_id, req, now).err(),
                        Err(e) => Some(Response::err(e.id.unwrap_or(0), e.code, "the request was refused")),
                    }
                };
                let outs: Vec<Out> = answer_now.and_then(|response| send_response(ch, &mut transport, &mut sent, &response)).into_iter().collect();
                (Some(ChanState::Session { transport, device_id, sent, received }), outs)
            }
        }
    }

    /// Queues one parsed request for the worker. A refusal is the answer to send now: the caps
    /// (`rate_limited`), or a worker that has stopped (`unavailable`).
    fn submit(&mut self, ch: u32, device_id: &str, req: Request, now: u64) -> Result<(), Response> {
        let id = req.id;
        let on_channel = self.inflight.get(&ch).copied().unwrap_or(0);
        let total: usize = self.inflight.values().sum();
        if on_channel >= MAX_REQUESTS_IN_FLIGHT || total >= MAX_REQUESTS_IN_FLIGHT {
            return Err(Response::err(id, ErrorCode::RateLimited, "too many requests are running; ask again in a moment"));
        }
        let job = Job { ch, device_id: device_id.to_owned(), req, now, cfg: self.cfg.clone(), devices: self.devices.clone() };
        match self.worker.submit(job) {
            Ok(()) => {
                *self.inflight.entry(ch).or_insert(0) += 1;
                Ok(())
            }
            Err(Refused::Full) => Err(Response::err(id, ErrorCode::RateLimited, "too many requests are running; ask again in a moment")),
            Err(Refused::Gone) => Err(Response::err(id, ErrorCode::Unavailable, "the phone remote stopped its worker; restart CSM")),
        }
    }

    /// One answer from the worker: the device fields it may have changed are merged, and the
    /// response is sealed into its channel if that channel is still open with the same device.
    fn finish(&mut self, done: Done) -> Vec<Out> {
        let mut outs = Vec::new();
        if let Some(n) = self.inflight.get_mut(&done.ch) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.inflight.remove(&done.ch);
            }
        }
        if let Some(problem) = self.merge_devices(&done.devices, done.devices_changed) {
            outs.push(Out::Problem(problem));
        }
        let Some(chan) = self.chans.get_mut(&done.ch) else { return outs };
        let ChanState::Session { transport, device_id, sent, .. } = &mut chan.state else { return outs };
        if *device_id != done.device_id {
            return outs;
        }
        outs.extend(send_response(done.ch, transport, sent, &done.response));
        outs
    }

    /// Copies back the fields the ops change (`last_seen`, `hide_names`, `notify`, `push`) for each
    /// device still paired. A device revoked meanwhile is not brought back, and a level change is
    /// not undone. Saves the list when an op changed a setting. Returns a problem line on failure.
    fn merge_devices(&mut self, copy: &[Device], changed: bool) -> Option<String> {
        for mine in self.devices.iter_mut() {
            let Some(theirs) = copy.iter().find(|d| d.device_id == mine.device_id && d.token_hash == mine.token_hash) else { continue };
            mine.last_seen = theirs.last_seen;
            if changed {
                mine.hide_names = theirs.hide_names;
                mine.notify = theirs.notify;
                mine.push = theirs.push.clone();
            }
        }
        if changed {
            return store::save_devices(&self.dir, &self.devices).err().map(|e| format!("the device list was not saved: {e}"));
        }
        None
    }

    /// Waits until every request sent to the worker has been answered, and returns what to send.
    /// For tests; the relay loop calls [`Host::tick`], which never waits.
    pub fn wait_idle(&mut self) -> Vec<Out> {
        let started = std::time::Instant::now();
        let mut outs = Vec::new();
        while !self.inflight.is_empty() && started.elapsed() < std::time::Duration::from_secs(10) {
            if let Some(done) = self.worker.next_done(std::time::Duration::from_millis(20)) {
                outs.extend(self.finish(done));
            }
        }
        outs
    }

    fn waiting_channel(&self) -> Option<u32> {
        self.chans.iter().find(|(_, c)| matches!(c.state, ChanState::PairWait { .. })).map(|(k, _)| *k)
    }

    /// Encrypts a pairing-channel message (Deny) for `ch`, when that channel is waiting.
    fn send_pair(&mut self, ch: u32, msg: &PairMsg) -> Option<Out> {
        let plain = encode_json(msg)?;
        let chan = self.chans.get_mut(&ch)?;
        let ChanState::PairWait { transport, .. } = &mut chan.state else { return None };
        let bytes = seal(transport, &plain)?;
        Some(Out::Frame(encode_frame(FrameKind::Data, ch, &bytes)))
    }

    /// Closes a channel from the host's side: a close frame, and the channel forgotten.
    fn close(&mut self, ch: u32, code: Option<RefusalCode>) -> Out {
        self.forget(ch);
        self.close_ch_out(ch, code)
    }

    /// The close frame for a channel the caller has already taken out of the map.
    fn close_ch_out(&mut self, ch: u32, code: Option<RefusalCode>) -> Out {
        Out::Frame(encode_frame(FrameKind::Close, ch, &close_payload(code)))
    }

    /// The relay (or the host) ended a channel: forget it, and free the pairing slot when it was
    /// the pairing channel.
    fn forget(&mut self, ch: u32) {
        if let Some(c) = self.chans.remove(&ch) {
            self.end_channel(is_pairing(&c.state));
        }
    }

    fn free_slot(&mut self) {
        if let Some(s) = self.slot.as_mut() {
            s.active = false;
        }
    }

    /// Whether a pairing attempt is refused for this reason, as a readable line for the screen.
    pub fn refusal_text(r: Refusal) -> &'static str {
        match r {
            Refusal::NoSlot => "No pairing is open.",
            Refusal::Expired => "This code has expired.",
            Refusal::WrongSlot => "This code is not the open one.",
            Refusal::Busy => "Another phone is pairing now.",
            Refusal::AttemptsUsed => "Too many attempts on this code.",
        }
    }
}

fn build(params: &str, private: &[u8; 32], prologue: &[u8], psk: Option<&[u8; 32]>) -> Result<snow::HandshakeState, snow::Error> {
    let p: snow::params::NoiseParams = params.parse()?;
    let b = snow::Builder::new(p).local_private_key(private)?.prologue(prologue)?;
    match psk {
        Some(k) => b.psk(PSK_POSITION, k)?.build_responder(),
        None => b.build_responder(),
    }
}

fn encode_json_response(r: &Response) -> Option<Vec<u8>> {
    msg::encode(r).ok()
}

/// One response, encrypted on the channel's transport and framed. Counts the message for the
/// rekey schedule (the phone counts the same way).
fn send_response(ch: u32, transport: &mut snow::TransportState, sent: &mut u64, response: &Response) -> Option<Out> {
    let bytes = encode_json_response(response).and_then(|p| seal(transport, &p))?;
    *sent += 1;
    if sent.is_multiple_of(REKEY_INTERVAL) {
        transport.rekey_outgoing();
    }
    Some(Out::Frame(encode_frame(FrameKind::Data, ch, &bytes)))
}

/// Encrypts one application message for a transport, or `None` when it does not fit.
fn seal(transport: &mut snow::TransportState, plain: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0u8; plain.len() + csm_remote_proto::noise::TAG_LEN + 16];
    let n = transport.write_message(plain, &mut out).ok()?;
    out.truncate(n);
    Some(out)
}

/// Whether a channel is a pairing channel (it holds the one pairing slot).
fn is_pairing(state: &ChanState) -> bool {
    matches!(state, ChanState::PairHandshake { .. } | ChanState::PairWait { .. })
}
