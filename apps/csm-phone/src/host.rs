//! The host's protocol core, with no sockets in it: relay frames in, relay frames and control
//! messages out. The relay client (`relay_client.rs`) moves bytes between this and a WebSocket;
//! the tests drive it directly with a test phone.
//!
//! What it does, per channel:
//! - a paired phone opens with `OpenInfo::Device`, runs `Noise_IK` (its static key must be the
//!   one saved at pairing), then sends requests that the [`Engine`] answers;
//! - a phone that scanned the offer opens with `OpenInfo::Pairing`, runs `Noise_IKpsk2` with the
//!   offer's PSK, then sends `PairMsg::Confirm` (key confirmation) and `PairMsg::Hello`. Nothing
//!   is shown to the person until the confirmation decrypts and matches: a phone without the PSK
//!   never gets past message 2, so it never appears in the pending list. The person then allows
//!   or denies the phone on the PC.
//!
//! Every reply is produced here; nothing is sent to a phone except through [`Out`].

use std::collections::BTreeMap;
use std::path::PathBuf;

use csm_remote_proto::codes::RefusalCode;
use csm_remote_proto::ids::{b64_decode, b64_decode_fixed, b64_encode, hex, host_id, token_hash};
use csm_remote_proto::limits::{MAX_APP_MESSAGE_BYTES, MAX_CHANNELS_PER_HOST, MAX_PUSH_BODY_BYTES, MAX_PUSH_ENDPOINT_BYTES};
use csm_remote_proto::msg::{self, ByeReason, ErrorCode, Event, Level, Op, PushKind, PushPayload, Request, Response, SessionInfo};
use csm_remote_proto::noise::{MAX_CHANNEL_SECS, MAX_NOISE_MESSAGE, PARAMS_PAIRING, PARAMS_SESSION, PSK_POSITION, REKEY_INTERVAL, check_code, prologue};
use csm_remote_proto::pairing::{DenyReason, PairMsg, PairingOffer};
use csm_remote_proto::relay::{FrameKind, HostToRelay, OpenInfo, PushRequest, RelayToHost, Urgency, close_payload, decode_frame, encode_frame};
use serde::Serialize;

use crate::config::Config;
use crate::csm::Csm;
use crate::ops::{Engine, push_host_allowed};
use crate::pairing::{Refusal, Slot};
use crate::settings::{self, SettingsChange};
use crate::push;
use crate::store::{self, Device, Opened};
use crate::tracker::{self, REFRESH_SECS, Tracker};
use crate::worker::{Done, Job, Refused, TRACK_CH, Worker};

/// Seconds between two web pushes to one device. A push that falls inside the window is held and
/// sent when the window opens, carrying the newest session (section 9).
pub const PUSH_WINDOW_SECS: u64 = 30;
/// Lifetime of a push at the push service, in seconds.
const PUSH_TTL_SECS: u32 = 3600;
/// Push requests the host keeps the device for, until the relay answers them.
const PUSH_PENDING_MAX: usize = 256;

/// Requests the host keeps in flight at once, across all channels (and per channel). A request
/// beyond it is answered `rate_limited` at once; it is not queued.
pub const MAX_REQUESTS_IN_FLIGHT: usize = 8;

/// Seconds a channel may take to finish its handshake before the host closes it.
pub const HANDSHAKE_SECS: u64 = 20;
/// Seconds a `remote_control` wait may last before it ends with a failure (section 4).
pub const REMOTE_WAIT_SECS: u64 = 120;
/// Waits one phone may have at once, and in all: a further `session.new` with `remote_control` is
/// refused before `session_new` is called (section 4).
pub const MAX_WAITS_PER_DEVICE: usize = 2;
pub const MAX_WAITS: usize = 8;

/// Where one `remote_control` wait is: launching (the `session.new` is with the worker), pending
/// (the session exists and the wait for it to be idle runs), or sending (the `/remote-control` is
/// with the worker).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RcState {
    Launching,
    Pending,
    Sending,
}

/// One `remote_control` wait. It holds no request id and no reply: its answer is the event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RcWait {
    pub token: u64,
    pub ch: u32,
    pub device_id: String,
    pub session: Option<String>,
    pub state: RcState,
    pub since: u64,
}
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
    /// The waiting phone left before the person decided (its channel closed): its check code and the
    /// Allow and Deny buttons go. The pairing slot may still be open for another phone.
    PendingCleared,
    /// The device list changed and was saved.
    DevicesChanged,
    /// Something the person should see, in plain words (never a secret).
    Problem(String),
    /// A plain answer for the person's last action ("Saved.", "Pairing denied."). Shown on the
    /// screen, not logged.
    Notice(String),
    /// The relay connection changed (connecting, connected, or lost with the reason).
    Link(Link),
}

/// A per-phone flag the devices screen sets: "notify me" or "hide session names".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceFlag {
    Notify(bool),
    HideNames(bool),
}

/// The relay connection, as the screens and the indicator show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Up,
    Down(String),
}

/// What the screens need from the host, copied on request (`Command::View`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostView {
    pub cfg: Config,
    pub devices: Vec<DeviceView>,
    /// When the open pairing offer expires (Unix seconds), if one is open.
    pub slot_expires: Option<u64>,
    /// The relay or the connect switch differs from what this run started with: a restart applies it.
    pub restart_needed: bool,
    /// This PC's host id: the name a self-hosted relay admits (its `access.allowed_hosts`). Shown read-only
    /// on the settings screen and in `phone-remote.status`. It is not a secret (the pairing offer carries it).
    pub host_id: String,
}

/// One paired device, as shown to the person.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DeviceView {
    pub device_id: String,
    pub name: String,
    pub level: Level,
    pub paired_at: u64,
    pub last_seen: u64,
    /// "Notify me about sessions that need me".
    pub notify: bool,
    /// Session names are left out of this phone's notifications.
    pub hide_names: bool,
    /// The phone has a push subscription (so a push can reach it).
    pub push: bool,
}

/// Where one channel is in its life.
pub enum ChanState {
    DevHandshake { hs: Box<snow::HandshakeState>, device_id: String },
    PairHandshake { hs: Box<snow::HandshakeState> },
    /// The host sent its message 2. The phone has not yet proven the PSK (no `Confirm` yet).
    PairConfirm { transport: Box<snow::TransportState>, phone_pub: [u8; 32], check: String },
    /// The phone proved the PSK; its `Hello` (the name) is what it waits for.
    PairHello { transport: Box<snow::TransportState>, phone_pub: [u8; 32], check: String },
    /// A phone finished the pairing exchange and waits for Allow, Deny or the expiry.
    PairWait { transport: Box<snow::TransportState>, name: String, phone_pub: [u8; 32], check: String },
    /// A paired phone. `view` is the session list the phone has (what the last snapshot or delta
    /// left it with) and `seq` the last sequence number sent (section 7.2).
    Session { transport: Box<snow::TransportState>, device_id: String, sent: u64, received: u64, view: BTreeMap<String, SessionInfo>, seq: u64 },
}

/// Web Push state (section 9): the request ids, when each device last got a push, the devices whose
/// push is held by the window (with the newest session they should be told about), and the request
/// ids waiting for the relay's answer.
#[derive(Debug, Default)]
pub struct PushState {
    next_id: u32,
    last_sent: BTreeMap<String, u64>,
    held: BTreeMap<String, Option<SessionInfo>>,
    pending: BTreeMap<u32, String>,
    /// Sessions that need the person now, from the last tracker read.
    needs_count: u32,
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
    /// Messages per direction between Noise rekeys ([`REKEY_INTERVAL`] in the product). A test
    /// lowers it to see a rekey happen inside a short exchange; the phone must use the same count.
    pub rekey_every: u64,
    /// The host-wide tracker (section 7.1): CSM's session list and the push episodes.
    pub tracker: Tracker,
    /// A tracker read is on the worker.
    pub track_inflight: bool,
    /// When the last tracker read was sent (Unix seconds).
    track_at: u64,
    /// The last tracker error, so a repeated one is reported once.
    track_error: Option<String>,
    /// The time of the last call into the host (`tick` or `on_frame`), for answers from the worker.
    clock: u64,
    pushes: PushState,
    /// The relay and the connect switch this run started with (the relay client dials with these).
    start_relay: String,
    start_connect: bool,
    /// The `remote_control` waits (section 4), in the order they started.
    waits: Vec<RcWait>,
    /// The next wait's token (a job for a wait carries it, so its answer finds the wait).
    next_token: u64,
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
        let start_relay = cfg.relay.clone();
        let start_connect = cfg.connect;
        Ok(Self {
            dir,
            start_relay,
            start_connect,
            waits: Vec::new(),
            next_token: 1,
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
            rekey_every: REKEY_INTERVAL,
            tracker: Tracker::default(),
            track_inflight: false,
            track_at: 0,
            track_error: None,
            clock: 0,
            pushes: PushState::default(),
        })
    }

    /// The paired devices as the screen and the tools show them: no keys and no token hashes.
    pub fn devices_view(&self) -> Vec<DeviceView> {
        self.devices
            .iter()
            .map(|d| DeviceView {
                device_id: d.device_id.clone(),
                name: d.name.clone(),
                level: d.level,
                paired_at: d.paired_at,
                last_seen: d.last_seen,
                notify: d.notify,
                hide_names: d.hide_names,
                push: d.push.is_some(),
            })
            .collect()
    }

    /// What the screens need: the config, the devices, when an open offer expires, and whether a
    /// restart applies the relay or connect switch.
    pub fn view(&self, now: u64) -> HostView {
        HostView {
            cfg: self.cfg.clone(),
            devices: self.devices_view(),
            slot_expires: self.slot.as_ref().filter(|s| !s.expired(now)).map(|s| now + u64::from(s.seconds_left(now))),
            restart_needed: self.restart_pending(),
            host_id: self.host_id.clone(),
        }
    }

    /// The relay or the connect switch differs from what this run started with.
    fn restart_pending(&self) -> bool {
        self.cfg.relay != self.start_relay || self.cfg.connect != self.start_connect
    }

    /// Applies a settings change. The result is checked first ([`settings::apply`]); a refused change
    /// writes nothing. Otherwise the new config is written (a damaged file is kept as
    /// `config.json.damaged-<time>` first) and becomes the live config.
    pub fn apply_settings(&mut self, change: &SettingsChange, now: u64) -> Vec<Out> {
        let next = match settings::apply(&self.cfg, change) {
            Ok(n) => n,
            Err(why) => return vec![Out::Notice(why)],
        };
        let text = match serde_json::to_string_pretty(&next) {
            Ok(t) => t,
            Err(e) => return vec![Out::Notice(format!("The settings were not saved: {e}"))],
        };
        if let Err(e) = store::save_config(&self.dir, &text, self.cfg.damaged, now) {
            return vec![Out::Notice(format!("The settings were not saved: {e}"))];
        }
        self.cfg = next;
        let restart = self.restart_pending();
        vec![Out::Notice(if restart { "Saved. The relay and connect changes apply when the add-on restarts.".into() } else { "Saved.".into() })]
    }

    /// Sets the notify or hide-names flag of one phone, or of every phone when `device_id` is `None`,
    /// and saves the device list. Only the flag changes: no channel is closed.
    pub fn set_device_flag(&mut self, device_id: Option<&str>, flag: DeviceFlag) -> Vec<Out> {
        let mut next = self.devices.clone();
        let mut hit = false;
        for d in next.iter_mut().filter(|d| device_id.is_none_or(|id| d.device_id == id)) {
            match flag {
                DeviceFlag::Notify(on) => d.notify = on,
                DeviceFlag::HideNames(on) => d.hide_names = on,
            }
            hit = true;
        }
        if !hit {
            return vec![Out::Notice("No such phone.".into())];
        }
        if let Err(e) = store::save_devices(&self.dir, &next) {
            return vec![Out::Notice(format!("The change was not saved: {e}"))];
        }
        self.devices = next;
        vec![Out::DevicesChanged]
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
        let pairing_channels: Vec<u32> = self.chans.iter().filter(|(_, c)| is_pairing(&c.state)).map(|(k, _)| *k).collect();
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
            let notice = match deny {
                Some(DenyReason::Expired) => "The pairing code expired. Start pairing again for a new code.",
                _ => "Pairing stopped.",
            };
            outs.push(Out::Notice(notice.into()));
        }
        outs
    }

    /// The person's decision on the waiting phone: `Some(level)` allows it at that level, `None`
    /// denies it. `check` is the code the person compared; when it is not the code of the phone that
    /// is waiting now, nothing changes (the phone on the screen may not be the one answering). Allow
    /// saves the device before the phone is told.
    pub fn decide(&mut self, now: u64, check: &str, allow: Option<Level>) -> Vec<Out> {
        let Some(ch) = self.waiting_channel() else {
            return vec![Out::Notice("No phone is waiting for an answer.".into())];
        };
        if self.waiting_check() != Some(check) {
            return vec![Out::Notice("The phone waiting now is not the one whose code was shown. Nothing was decided; check the code on the screen again.".into())];
        }
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
        let who = crate::policy::clean_name(&name, 64);
        let as_level = if level == Level::Operator { "an operator" } else { "a viewer" };
        outs.push(Out::Notice(format!("{who} is paired as {as_level}.")));
        outs
    }

    /// Time-driven work: the pairing slot's expiry and the channels' maximum age. A waiting phone whose
    /// channel is closed here is reported with [`Out::PendingCleared`] (see [`Self::settle_pending`]).
    pub fn tick(&mut self, now: u64) -> Vec<Out> {
        let before = self.waiting_check().is_some();
        let outs = self.tick_inner(now);
        self.settle_pending(before, outs)
    }

    /// Adds [`Out::PendingCleared`] when a phone was waiting before the event and none is now, unless the
    /// event already ended the pairing (which clears the code too). A phone that waits when the event
    /// starts and is replaced by another still shows its own code, through `Out::Pending`.
    fn settle_pending(&self, was_waiting: bool, mut outs: Vec<Out>) -> Vec<Out> {
        let ended = outs.iter().any(|o| matches!(o, Out::PairingEnded | Out::PendingCleared));
        if was_waiting && !ended && self.waiting_check().is_none() {
            outs.push(Out::PendingCleared);
        }
        outs
    }

    fn tick_inner(&mut self, now: u64) -> Vec<Out> {
        self.clock = now;
        let mut outs = Vec::new();
        while let Some(done) = self.worker.try_done() {
            outs.extend(self.finish(done));
        }
        // The tracker reads CSM's list on the worker: one read at a time, one every REFRESH_SECS.
        if !self.track_inflight && now.saturating_sub(self.track_at) >= REFRESH_SECS {
            let job = Job {
                ch: TRACK_CH,
                device_id: String::new(),
                req: Request { id: 0, op: Op::Ping },
                now,
                cfg: self.cfg.clone(),
                devices: self.devices.clone(),
                track: true,
                wait: None,
            };
            if self.worker.submit(job).is_ok() {
                self.track_inflight = true;
                self.track_at = now;
            }
        }
        outs.extend(self.send_held(now));
        outs.extend(self.wait_tick(now));
        if self.slot.as_ref().is_some_and(|s| s.expired(now)) {
            outs.extend(self.end_pairing(now, Some(DenyReason::Expired)));
        }
        // A channel that has not finished its handshake in time is closed, so a relay (or a phone)
        // that opens channels and goes silent cannot hold the channel budget for an hour.
        let stalled: Vec<u32> = self
            .chans
            .iter()
            .filter(|(_, c)| matches!(c.state, ChanState::DevHandshake { .. } | ChanState::PairHandshake { .. } | ChanState::PairConfirm { .. } | ChanState::PairHello { .. }) && now.saturating_sub(c.opened) >= HANDSHAKE_SECS)
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
        if level.is_none() {
            // A removed phone's waits end with it: no event is sent to a device that is gone.
            self.waits.retain(|w| w.device_id != device_id);
        }
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
        let was_waiting = self.waiting_check().is_some();
        let outs = self.frame_inner(bytes, now);
        self.settle_pending(was_waiting, outs)
    }

    fn frame_inner(&mut self, bytes: &[u8], now: u64) -> Vec<Out> {
        self.clock = now;
        let Ok(frame) = decode_frame(bytes) else { return Vec::new() };
        let ch = frame.channel;
        match frame.kind {
            FrameKind::Open => self.on_open(ch, frame.payload, now),
            FrameKind::Data => self.on_data(ch, frame.payload, now),
            // Peer channels only (CSM to CSM); a phone channel has no reader acks.
            FrameKind::Ack => Vec::new(),
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
                    Ok(transport) => {
                        let mut transport = Box::new(transport);
                        let (mut sent, mut view, mut seq) = (0u64, BTreeMap::new(), 0u64);
                        // The first thing a phone gets after its handshake is a snapshot of the list.
                        let list = self.tracker.last().map(<[SessionInfo]>::to_vec).unwrap_or_default();
                        let mut outs = vec![Out::Frame(encode_frame(FrameKind::Data, ch, &reply))];
                        outs.extend(resync(ch, &mut transport, &mut sent, &mut view, &mut seq, &list, self.rekey_every));
                        (Some(ChanState::Session { transport, device_id, sent, received: 0, view, seq }), outs)
                    }
                    Err(_) => (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]),
                }
            }
            ChanState::PairHandshake { mut hs } => {
                // Message 1 carries no payload. A phone without the PSK is not refused here: the
                // host cannot check the PSK before it writes message 2, and nothing is shown until
                // the phone confirms (see `PairConfirm`).
                let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
                let Ok(n) = hs.read_message(payload, &mut buf) else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                if n != 0 {
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
                let state = ChanState::PairConfirm { transport: Box::new(transport), phone_pub, check };
                (Some(state), vec![Out::Frame(encode_frame(FrameKind::Data, ch, &reply))])
            }
            ChanState::PairConfirm { mut transport, phone_pub, check } => {
                // The first transport message must decrypt (only a phone with the PSK can make it)
                // and carry this handshake's check code. Nothing is shown to the person yet.
                let Some(plain) = open_transport(&mut transport, payload) else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                match serde_json::from_slice::<PairMsg>(&plain) {
                    Ok(PairMsg::Confirm { check: theirs }) if theirs == check => (Some(ChanState::PairHello { transport, phone_pub, check }), Vec::new()),
                    _ => (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]),
                }
            }
            ChanState::PairHello { mut transport, phone_pub, check } => {
                let Some(plain) = open_transport(&mut transport, payload) else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                // The name is cleaned before the person sees it on the decision screen, so a name
                // with bidi or zero-width characters cannot disguise itself there. A name that
                // cleans to nothing is refused.
                let name = match serde_json::from_slice::<PairMsg>(&plain) {
                    Ok(PairMsg::Hello { v, name }) if v == csm_remote_proto::PROTOCOL_VERSION && PairMsg::valid_name(&name) => crate::policy::clean_name(&name, MAX_PAIR_NAME_CHARS),
                    _ => return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]),
                };
                if name.is_empty() {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                }
                let state = ChanState::PairWait { transport, name: name.clone(), phone_pub, check: check.clone() };
                (Some(state), vec![Out::Pending { check, name }])
            }
            ChanState::PairWait { transport, name, phone_pub, check } => {
                // The phone has nothing more to say until the person answers.
                (Some(ChanState::PairWait { transport, name, phone_pub, check }), Vec::new())
            }
            ChanState::Session { mut transport, device_id, sent, mut received, mut view, mut seq } => {
                let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
                let Ok(n) = transport.read_message(payload, &mut buf) else {
                    return (None, vec![self.close_ch_out(ch, Some(RefusalCode::BadRequest))]);
                };
                received += 1;
                if received.is_multiple_of(self.rekey_every) {
                    transport.rekey_incoming();
                }
                let plain = buf.get(..n).unwrap_or_default();
                let mut sent = sent;
                let every = self.rekey_every;
                let mut outs = Vec::new();
                if plain.len() > MAX_APP_MESSAGE_BYTES {
                    outs.extend(send_response(ch, &mut transport, &mut sent, &Response::err(0, ErrorCode::TooLarge, "the message is too large"), every));
                } else {
                    match Request::parse(plain) {
                        // The snapshot is answered from the tracker's last list: no CSM call, no worker.
                        Ok(req) if matches!(req.op, Op::Snapshot) => {
                            let list = self.tracker.last().map(<[SessionInfo]>::to_vec).unwrap_or_default();
                            outs.extend(send_response(ch, &mut transport, &mut sent, &Response::ok(req.id, serde_json::json!({})), every));
                            outs.extend(resync(ch, &mut transport, &mut sent, &mut view, &mut seq, &list, every));
                        }
                        // A parsed request goes to the worker and is answered later (`finish`). Only a
                        // refusal is answered here.
                        Ok(req) => {
                            if let Err(response) = self.submit(ch, &device_id, req, now) {
                                outs.extend(send_response(ch, &mut transport, &mut sent, &response, every));
                            }
                        }
                        Err(e) => outs.extend(send_response(ch, &mut transport, &mut sent, &Response::err(e.id.unwrap_or(0), e.code, "the request was refused"), every)),
                    }
                }
                (Some(ChanState::Session { transport, device_id, sent, received, view, seq }), outs)
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
        // A launch with Remote Control takes one of the waits, before session_new is called.
        let waits_launch = matches!(req.op, Op::SessionNew { remote_control: true, .. });
        if waits_launch {
            let (mine, all) = self.wait_counts(device_id);
            if mine >= MAX_WAITS_PER_DEVICE || all >= MAX_WAITS {
                return Err(Response::err(id, ErrorCode::RateLimited, "too many Remote Control waits are running; ask again in a moment"));
            }
        }
        let token = self.next_token;
        let wait = waits_launch.then_some(token);
        let job = Job { ch, device_id: device_id.to_owned(), req, now, cfg: self.cfg.clone(), devices: self.devices.clone(), track: false, wait };
        match self.worker.submit(job) {
            Ok(()) => {
                *self.inflight.entry(ch).or_insert(0) += 1;
                if waits_launch {
                    self.next_token = self.next_token.wrapping_add(1);
                    self.waits.push(RcWait { token, ch, device_id: device_id.to_owned(), session: None, state: RcState::Launching, since: now });
                }
                Ok(())
            }
            Err(Refused::Full) => Err(Response::err(id, ErrorCode::RateLimited, "too many requests are running; ask again in a moment")),
            Err(Refused::Gone) => Err(Response::err(id, ErrorCode::Unavailable, "the phone remote stopped its worker; restart CSM")),
        }
    }

    /// The waits on a device, and all of them.
    fn wait_counts(&self, device_id: &str) -> (usize, usize) {
        (self.waits.iter().filter(|w| w.device_id == device_id).count(), self.waits.len())
    }

    /// A worker answer for a `remote_control` wait (`done.wait`). A launch that answered `pending` makes
    /// the wait pending; any other launch answer ends it, and its response goes to the phone as usual.
    /// The `/remote-control` answer ends the wait and is sent as an event, never as a response.
    /// Returns the outputs and whether the normal response is still to be sent.
    fn finish_wait(&mut self, token: u64, response: Option<&Response>, ch: u32, device_id: &str) -> (Vec<Out>, bool) {
        let Some(state) = self.waits.iter().find(|w| w.token == token).map(|w| w.state) else { return (Vec::new(), true) };
        match state {
            RcState::Launching => {
                let session = response
                    .filter(|r| r.ok)
                    .and_then(|r| r.data.as_ref())
                    .filter(|d| d["remote_control"] == "pending")
                    .and_then(|d| d["id"].as_str())
                    .map(str::to_owned);
                match session {
                    Some(session) => {
                        let now = self.clock;
                        if let Some(w) = self.waits.iter_mut().find(|w| w.token == token) {
                            w.session = Some(session);
                            w.state = RcState::Pending;
                            w.since = now;
                        }
                    }
                    None => self.waits.retain(|w| w.token != token),
                }
                (Vec::new(), true)
            }
            // A pending wait has no job in the worker, so no answer can name it; kept as it is.
            RcState::Pending => (Vec::new(), true),
            RcState::Sending => {
                let session = self.waits.iter().find(|w| w.token == token).and_then(|w| w.session.clone()).unwrap_or_default();
                self.waits.retain(|w| w.token != token);
                let event = match response {
                    Some(r) if r.ok => Event::RemoteControl { session, ok: true, code: None, message: None },
                    Some(r) => {
                        let (code, message) = r.error.as_ref().map_or((ErrorCode::Failed, String::new()), |e| (e.code, e.message.clone()));
                        Event::RemoteControl { session, ok: false, code: Some(code), message: Some(crate::policy::cut(&message, 200)).filter(|m| !m.is_empty()) }
                    }
                    None => Event::RemoteControl { session, ok: false, code: Some(ErrorCode::Unavailable), message: Some("the command did not run".into()) },
                };
                (self.event_out(ch, device_id, &event), false)
            }
        }
    }

    /// The waits' time-driven work. A wait pending for two minutes ends with a failure event. A pending
    /// wait whose session is idle sends `/remote-control` once: one CSM call, one slot of the in-flight cap.
    fn wait_tick(&mut self, now: u64) -> Vec<Out> {
        let mut outs = Vec::new();
        let expired: Vec<u64> = self.waits.iter().filter(|w| w.state == RcState::Pending && now.saturating_sub(w.since) >= REMOTE_WAIT_SECS).map(|w| w.token).collect();
        for token in expired {
            let Some(w) = self.waits.iter().find(|w| w.token == token).cloned() else { continue };
            self.waits.retain(|x| x.token != token);
            let event = Event::RemoteControl {
                session: w.session.unwrap_or_default(),
                ok: false,
                code: Some(ErrorCode::Failed),
                message: Some("Remote Control did not start within two minutes. The session is as it was.".into()),
            };
            outs.extend(self.event_out(w.ch, &w.device_id, &event));
        }
        let list = self.tracker.last().map(<[SessionInfo]>::to_vec).unwrap_or_default();
        let ready: Vec<(u64, u32, String, String)> = self
            .waits
            .iter()
            .filter(|w| w.state == RcState::Pending)
            .filter_map(|w| {
                let session = w.session.clone()?;
                let idle = list.iter().any(|s| s.id == session && s.status == csm_remote_proto::msg::PhoneStatus::Idle);
                idle.then(|| (w.token, w.ch, w.device_id.clone(), session))
            })
            .collect();
        for (token, ch, device_id, session) in ready {
            let total: usize = self.inflight.values().sum();
            if total >= MAX_REQUESTS_IN_FLIGHT {
                break;
            }
            let op = Op::SessionCommand { session_id: session, command: "/remote-control".into(), force: false };
            let job = Job { ch, device_id, req: Request { id: 0, op }, now, cfg: self.cfg.clone(), devices: self.devices.clone(), track: false, wait: Some(token) };
            if self.worker.submit(job).is_ok() {
                *self.inflight.entry(ch).or_insert(0) += 1;
                if let Some(w) = self.waits.iter_mut().find(|w| w.token == token) {
                    w.state = RcState::Sending;
                }
            }
        }
        outs
    }

    /// One event on a phone's channel, if that channel is still open with that device.
    fn event_out(&mut self, ch: u32, device_id: &str, event: &Event) -> Vec<Out> {
        let every = self.rekey_every;
        let Ok(plain) = msg::encode(event) else { return Vec::new() };
        let Some(chan) = self.chans.get_mut(&ch) else { return Vec::new() };
        let ChanState::Session { transport, device_id: owner, sent, .. } = &mut chan.state else { return Vec::new() };
        if owner.as_str() != device_id {
            return Vec::new();
        }
        send_msg(ch, transport, sent, &plain, every).into_iter().collect()
    }

    /// One answer from the worker: the device fields it may have changed are merged, and the
    /// response is sealed into its channel if that channel is still open with the same device.
    fn finish(&mut self, done: Done) -> Vec<Out> {
        if done.ch == TRACK_CH {
            return self.finish_track(done.sessions);
        }
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
        let every = self.rekey_every;
        if let Some(token) = done.wait {
            let (events, answer) = self.finish_wait(token, done.response.as_ref(), done.ch, &done.device_id);
            outs.extend(events);
            if !answer {
                return outs;
            }
        }
        let Some(response) = done.response else { return outs };
        let Some(chan) = self.chans.get_mut(&done.ch) else { return outs };
        let ChanState::Session { transport, device_id, sent, .. } = &mut chan.state else { return outs };
        if *device_id != done.device_id {
            return outs;
        }
        outs.extend(send_response(done.ch, transport, sent, &response, every));
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
        while (!self.inflight.is_empty() || self.track_inflight) && started.elapsed() < std::time::Duration::from_secs(10) {
            if let Some(done) = self.worker.next_done(std::time::Duration::from_millis(20)) {
                outs.extend(self.finish(done));
            }
        }
        outs
    }

    /// One read of CSM's session list came back from the worker (or failed). The tracker takes it,
    /// each connected phone gets what changed, and the sessions that started to need the person are
    /// pushed. A failure is reported once until the list reads again.
    fn finish_track(&mut self, sessions: Option<Result<Vec<SessionInfo>, String>>) -> Vec<Out> {
        self.track_inflight = false;
        match sessions {
            Some(Ok(list)) => {
                self.track_error = None;
                self.pushes.needs_count = u32::try_from(list.iter().filter(|s| tracker::needs_you(s.status)).count()).unwrap_or(u32::MAX);
                let now = self.clock;
                let refresh = self.tracker.observe(list, now);
                let mut outs = self.track_outs();
                outs.extend(self.push_started(&refresh.started, now));
                outs
            }
            Some(Err(message)) => {
                if self.track_error.as_deref() == Some(message.as_str()) {
                    return Vec::new();
                }
                let shown = format!("CSM did not list its sessions: {message}");
                self.track_error = Some(message);
                vec![Out::Problem(shown)]
            }
            None => Vec::new(),
        }
    }

    /// The changes since each connected phone's last message (section 7.2): a delta with the next
    /// sequence number, split when it is large. A phone whose view is already the list gets nothing.
    fn track_outs(&mut self) -> Vec<Out> {
        let Some(list) = self.tracker.last().map(<[SessionInfo]>::to_vec) else { return Vec::new() };
        let every = self.rekey_every;
        let mut outs = Vec::new();
        for (ch, chan) in self.chans.iter_mut() {
            let ChanState::Session { transport, sent, view, seq, .. } = &mut chan.state else { continue };
            let upserts: Vec<SessionInfo> = list.iter().filter(|s| view.get(&s.id) != Some(*s)).cloned().collect();
            let removed: Vec<String> = view.keys().filter(|id| !list.iter().any(|s| &s.id == *id)).cloned().collect();
            if upserts.is_empty() && removed.is_empty() {
                continue;
            }
            let events = delta_events(*seq + 1, &upserts, &removed);
            *seq += u64::try_from(events.len()).unwrap_or(0);
            outs.extend(send_events(*ch, transport, sent, &events, every));
            for s in upserts {
                view.insert(s.id.clone(), s);
            }
            for id in removed {
                view.remove(&id);
            }
        }
        outs
    }

    /// Pushes for sessions that have just started to need the person: one to each device that
    /// asked for them. A device pushed within the window gets the push when the window opens.
    fn push_started(&mut self, started: &[SessionInfo], now: u64) -> Vec<Out> {
        let Some(newest) = started.last().cloned() else { return Vec::new() };
        let ids: Vec<String> = self.devices.iter().filter(|d| d.notify && d.push.is_some()).map(|d| d.device_id.clone()).collect();
        let mut outs = Vec::new();
        for id in ids {
            if self.push_window_open(&id, now) {
                outs.extend(self.push_device(&id, Some(&newest), now));
            } else {
                self.pushes.held.insert(id, Some(newest.clone()));
            }
        }
        outs
    }

    /// The held pushes whose window has opened by `now`.
    fn send_held(&mut self, now: u64) -> Vec<Out> {
        let due: Vec<String> = self.pushes.held.keys().filter(|id| self.push_window_open(id, now)).cloned().collect();
        let mut outs = Vec::new();
        for id in due {
            let newest = self.pushes.held.remove(&id).flatten();
            outs.extend(self.push_device(&id, newest.as_ref(), now));
        }
        outs
    }

    fn push_window_open(&self, device_id: &str, now: u64) -> bool {
        self.pushes.last_sent.get(device_id).is_none_or(|t| now.saturating_sub(*t) >= PUSH_WINDOW_SECS)
    }

    /// One `needs-you` push to one device, as a relay request. The payload names the newest session
    /// unless the device hides names; the body is encrypted to the device's key. Nothing is sent when
    /// the device no longer wants pushes.
    fn push_device(&mut self, device_id: &str, newest: Option<&SessionInfo>, now: u64) -> Vec<Out> {
        if !self.cfg.push {
            return Vec::new();
        }
        let Some(device) = self.devices.iter().find(|d| d.device_id == device_id) else { return Vec::new() };
        if !device.notify {
            return Vec::new();
        }
        let Some(sub) = device.push.clone() else { return Vec::new() };
        let hide = device.hide_names;
        if sub.endpoint.len() > MAX_PUSH_ENDPOINT_BYTES || !push_host_allowed(&self.cfg.push_hosts, &sub.endpoint) {
            return vec![Out::Problem("a push was not sent: its service is not on the list of push hosts".into())];
        }
        let (s, sid) = match newest {
            Some(session) if !hide => (Some(crate::policy::clean_name(&session.name, 80)), Some(session.id.clone())),
            _ => (None, None),
        };
        let payload = PushPayload { t: PushKind::NeedsYou, s, sid, n: self.pushes.needs_count.max(1) };
        let Ok(plain) = serde_json::to_vec(&payload) else { return Vec::new() };
        let body = match encrypt_push(&plain, &sub.p256dh, &sub.auth) {
            Ok(body) => body,
            Err(e) => return vec![Out::Problem(format!("a push was not sent: {e}"))],
        };
        if body.len() > MAX_PUSH_BODY_BYTES {
            return vec![Out::Problem("a push was not sent: the message is too large".into())];
        }
        let id = self.pushes.next_id.wrapping_add(1);
        self.pushes.next_id = id;
        if self.pushes.pending.len() >= PUSH_PENDING_MAX {
            self.pushes.pending.clear();
        }
        self.pushes.pending.insert(id, device_id.to_owned());
        self.pushes.last_sent.insert(device_id.to_owned(), now);
        let request = PushRequest { id, endpoint: sub.endpoint, ttl: PUSH_TTL_SECS, urgency: Some(Urgency::High), topic: Some("needs-you".into()), body: b64_encode(&body) };
        vec![Out::Control(HostToRelay::Push(request))]
    }

    /// What the relay says back on the host socket. A push the push service refuses as gone (404 or
    /// 410) drops that device's subscription; other answers are not recorded.
    pub fn on_relay(&mut self, msg: &RelayToHost) -> Vec<Out> {
        match msg {
            RelayToHost::PushResult { id, ok, status, .. } => {
                let Some(device_id) = self.pushes.pending.remove(id) else { return Vec::new() };
                if *ok || !matches!(status, Some(404 | 410)) {
                    return Vec::new();
                }
                let Some(device) = self.devices.iter_mut().find(|d| d.device_id == device_id) else { return Vec::new() };
                device.push = None;
                match store::save_devices(&self.dir, &self.devices) {
                    Ok(()) => vec![Out::DevicesChanged],
                    Err(e) => vec![Out::Problem(format!("the device list was not saved: {e}"))],
                }
            }
            RelayToHost::Error { code } => vec![Out::Problem(format!("the relay refused a request ({})", code.as_str()))],
            RelayToHost::Welcome { .. } | RelayToHost::AllowOk { .. } | RelayToHost::PairingOk { .. } => Vec::new(),
        }
    }

    fn waiting_channel(&self) -> Option<u32> {
        self.chans.iter().find(|(_, c)| matches!(c.state, ChanState::PairWait { .. })).map(|(k, _)| *k)
    }

    /// The check code of the phone that is waiting, if one is.
    fn waiting_check(&self) -> Option<&str> {
        self.chans.values().find_map(|c| match &c.state {
            ChanState::PairWait { check, .. } => Some(check.as_str()),
            _ => None,
        })
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
fn send_response(ch: u32, transport: &mut snow::TransportState, sent: &mut u64, response: &Response, rekey_every: u64) -> Option<Out> {
    let plain = encode_json_response(response)?;
    send_msg(ch, transport, sent, &plain, rekey_every)
}

/// One app message (a response or an event) on a channel's transport, framed. Every message counts
/// for the rekey schedule, in both directions, so the phone counts the same way.
fn send_msg(ch: u32, transport: &mut snow::TransportState, sent: &mut u64, plain: &[u8], rekey_every: u64) -> Option<Out> {
    let bytes = seal(transport, plain)?;
    *sent += 1;
    if sent.is_multiple_of(rekey_every) {
        transport.rekey_outgoing();
    }
    Some(Out::Frame(encode_frame(FrameKind::Data, ch, &bytes)))
}

/// The events, one message each, in order. An event that does not encode is skipped (it cannot
/// be made smaller by waiting; the phone's next snapshot brings it back).
fn send_events(ch: u32, transport: &mut snow::TransportState, sent: &mut u64, events: &[Event], rekey_every: u64) -> Vec<Out> {
    events.iter().filter_map(|e| msg::encode(e).ok()).filter_map(|plain| send_msg(ch, transport, sent, &plain, rekey_every)).collect()
}

/// A snapshot of `list` for one phone: a new sequence number, and the phone's view becomes the list
/// (section 7.2). The parts of a split snapshot all carry that number.
fn resync(
    ch: u32,
    transport: &mut snow::TransportState,
    sent: &mut u64,
    view: &mut BTreeMap<String, SessionInfo>,
    seq: &mut u64,
    list: &[SessionInfo],
    rekey_every: u64,
) -> Vec<Out> {
    *seq += 1;
    view.clear();
    for s in list {
        view.insert(s.id.clone(), s.clone());
    }
    let events = msg::split_snapshot(*seq, list, None, MAX_APP_MESSAGE_BYTES);
    send_events(ch, transport, sent, &events, rekey_every)
}

/// The delta messages for a change: the upserts and the removed ids, split so that each message
/// fits one app message. They take consecutive sequence numbers from `first_seq` (section 7.2
/// point 5: each one is its own step, so the phone applies them in order).
pub fn delta_events(first_seq: u64, upserts: &[SessionInfo], removed: &[String]) -> Vec<Event> {
    const BUDGET: usize = MAX_APP_MESSAGE_BYTES - 512;
    let mut parts: Vec<Event> = Vec::new();
    let mut up: Vec<SessionInfo> = Vec::new();
    let mut rm: Vec<String> = Vec::new();
    let mut used = 0usize;
    for s in upserts {
        let size = serde_json::to_vec(s).map_or(0, |v| v.len() + 1);
        if used + size > BUDGET && (!up.is_empty() || !rm.is_empty()) {
            parts.push(delta_of(next_seq(first_seq, parts.len()), &mut up, &mut rm));
            used = 0;
        }
        up.push(s.clone());
        used += size;
    }
    for r in removed {
        let size = r.len() + 4;
        if used + size > BUDGET && (!up.is_empty() || !rm.is_empty()) {
            parts.push(delta_of(next_seq(first_seq, parts.len()), &mut up, &mut rm));
            used = 0;
        }
        rm.push(r.clone());
        used += size;
    }
    if !up.is_empty() || !rm.is_empty() {
        parts.push(delta_of(next_seq(first_seq, parts.len()), &mut up, &mut rm));
    }
    parts
}

/// The sequence number of the `index`th message of a split change.
fn next_seq(first_seq: u64, index: usize) -> u64 {
    first_seq.saturating_add(u64::try_from(index).unwrap_or(u64::MAX))
}

/// One delta message with the given sequence number, taking what has been collected so far.
fn delta_of(seq: u64, up: &mut Vec<SessionInfo>, rm: &mut Vec<String>) -> Event {
    Event::Delta { seq, upserts: std::mem::take(up), removed: std::mem::take(rm) }
}

/// The RFC 8291 body for one push: a fresh key and salt from the OS random source.
fn encrypt_push(plain: &[u8], p256dh: &str, auth: &str) -> Result<Vec<u8>, String> {
    let ua = b64_decode(p256dh).ok_or_else(|| "the push key is not base64".to_owned())?;
    let auth = b64_decode(auth).ok_or_else(|| "the push auth secret is not base64".to_owned())?;
    let key: [u8; 32] = random()?;
    let salt: [u8; 16] = random()?;
    push::encrypt(plain, &ua, &auth, &key, &salt)
}

/// Encrypts one application message for a transport, or `None` when it does not fit.
fn seal(transport: &mut snow::TransportState, plain: &[u8]) -> Option<Vec<u8>> {
    let mut out = vec![0u8; plain.len() + csm_remote_proto::noise::TAG_LEN + 16];
    let n = transport.write_message(plain, &mut out).ok()?;
    out.truncate(n);
    Some(out)
}

/// Decrypts one transport message from the phone. `None` for anything that does not decrypt or does
/// not fit the buffer; nothing here panics on bytes.
fn open_transport(transport: &mut snow::TransportState, payload: &[u8]) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; MAX_NOISE_MESSAGE];
    let n = transport.read_message(payload, &mut buf).ok()?;
    buf.get(..n).map(<[u8]>::to_vec)
}

/// Whether a channel is a pairing channel (it holds the one pairing slot).
fn is_pairing(state: &ChanState) -> bool {
    matches!(state, ChanState::PairHandshake { .. } | ChanState::PairConfirm { .. } | ChanState::PairHello { .. } | ChanState::PairWait { .. })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;
    use csm_remote_proto::msg::{PhoneStatus, encode};

    fn big(i: usize) -> SessionInfo {
        SessionInfo {
            id: format!("s{i}"),
            name: "n".repeat(200),
            folder: "f".repeat(400),
            status: PhoneStatus::Working,
            branch: Some("b".repeat(200)),
            message: None,
            group: None,
            profile: None,
            since: Some(1),
        }
    }

    /// A change too big for one message is split into deltas with consecutive sequence numbers.
    /// Every upsert and every removal appears exactly once, and each message encodes.
    #[test]
    fn a_large_change_is_split_into_deltas_with_consecutive_numbers_and_every_item_once() {
        let upserts: Vec<SessionInfo> = (0..400).map(big).collect();
        let removed: Vec<String> = (0..300).map(|i| format!("gone-{i}-{}", "x".repeat(40))).collect();
        let events = delta_events(10, &upserts, &removed);
        assert!(events.len() > 2, "the change needs several messages, got {}", events.len());
        let mut got_up = Vec::new();
        let mut got_rm = Vec::new();
        for (i, ev) in events.iter().enumerate() {
            assert!(encode(ev).is_ok(), "message {i} fits one app message");
            let Event::Delta { seq, upserts, removed } = ev else { panic!("a delta") };
            assert_eq!(*seq, 10 + u64::try_from(i).unwrap(), "consecutive numbers");
            got_up.extend(upserts.iter().map(|s| s.id.clone()));
            got_rm.extend(removed.iter().cloned());
        }
        assert_eq!(got_up, upserts.iter().map(|s| s.id.clone()).collect::<Vec<_>>());
        assert_eq!(got_rm, removed);
    }

    #[test]
    fn a_small_change_is_one_delta_and_no_change_is_none() {
        let events = delta_events(3, &[big(1)], &["gone".to_owned()]);
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], Event::Delta { seq: 3, upserts, removed } if upserts.len() == 1 && removed.len() == 1));
        assert!(delta_events(3, &[], &[]).is_empty());
    }
}
