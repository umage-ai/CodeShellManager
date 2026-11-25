//! The connection worker: one thread per remote window. It owns the `csm-peer` connection, the
//! mirrored snapshot, every terminal stream's follower and emulator feed, and the calls in flight.
//! The UI thread talks to it through [`Cmd`] (never blocks: a channel send) and gets [`Up`] back
//! (polled once a frame).
//!
//! Lifecycle: connect, then serve until the link ends. A dropped link is retried with backoff
//! (1, 2, 5, 10, then 30 s), resuming each terminal from its `(instance, offset)`. A refusal (the
//! device was revoked, or the two versions do not speak the same protocol) ends the worker with
//! [`LinkState::Refused`] and the reason: retrying cannot help.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_peer::client::{self, ClientConfig, ClientError, Connection, Incoming, TermFollower, TermUpdate};
use csm_peer::identity::Identity;
use csm_peer_proto::Event as WireEvent;
use csm_peer_proto::Snapshot as WireSnapshot;
use csm_peer_proto::addr::PeerAddr;
use csm_peer_proto::codec::{Frame, Input};
use csm_peer_proto::ids::KEY_LEN;
use csm_peer_proto::msg::{ByeReason, ErrorKind, Level, Msg, Outcome};
use csm_term::{GridSize, ReplySink, TermConfig, Terminal};
use csm_ui::UploadEvent;
use csm_ui::backend::ToolInfo;
use serde_json::{Value, json};

use crate::map;
use crate::terminal::{ENDED, RUNNING, STALE, lock};
use crate::upload::{self, Pumped, Uploads, Wire};

/// The call key the worker uses for its own `tool_list` at connect. The UI's keys start at 1.
const TOOLS_KEY: u64 = 0;

/// Office notifications (toasts and the like) a window shows per second. The rest are dropped, and
/// one note says so. Consent cards and their answers are never limited: they wait for a person.
const NOTICES_PER_SEC: u32 = 10;

/// All mirror emulators of one link together, in cells (SEC-62): one 2000x2000 grid, or many
/// ordinary ones. An office cannot make this laptop allocate 64 maximum-size screens.
const MAX_MIRROR_CELLS: u64 = 4_000_000;

/// What the window shows of the connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkState {
    /// The first attempt is running.
    Connecting,
    /// Connected: the host's name and this device's level there.
    Live { host_name: String, level: Level },
    /// The link is down; the next attempt is in progress (`attempt` counts from 1).
    Reconnecting { attempt: u32, reason: String },
    /// The host will not take this device (revoked, or a version mismatch). Retrying cannot help.
    Refused { reason: String },
    /// The window closed.
    Stopped,
}

/// A command from the UI thread.
pub enum Cmd {
    /// A tool call; its outcome comes back as [`Up::Reply`] under `key`.
    Call { key: u64, tool: String, params: Value },
    /// Show a session's terminal on `stream`. The worker feeds `term`, and sets `mode` and `state`.
    Subscribe { stream: u32, session: String, term: Arc<Mutex<Terminal>>, mode: Arc<AtomicU32>, state: Arc<AtomicU8> },
    Unsubscribe { stream: u32 },
    /// The pane's size. `driver` says this client is the one typing.
    Resize { stream: u32, cols: u16, rows: u16, driver: bool },
    Input { stream: u32, input: Input },
    /// Upload local files, or the clipboard's image, into a session's folder on the office. The
    /// progress comes back as [`Up::Upload`] under the request's drop id.
    Upload(upload::Request),
    /// Cancel a drop: its files are removed on the office and nothing is pasted.
    UploadCancel { drop: u64 },
    /// Close the window's connection and end the worker.
    Stop,
}

/// What the worker tells the UI thread.
pub enum Up {
    State(LinkState),
    /// The host's `tool_list`, once per connection.
    Tools(Vec<ToolInfo>),
    /// The mirrored snapshot has changed: take it from the [`Latest`] slot the window was given.
    /// One wake is pending at a time, so a fast office cannot queue copies of its state.
    Snapshot,
    Event(WireEvent),
    Reply { key: u64, outcome: Outcome },
    /// An upload's progress, or how it ended.
    Upload(UploadEvent),
}

/// The newest mirrored snapshot, waiting for the window. Only the latest is kept: a chatty office
/// costs the laptop one copy, however many deltas it sends while the window is busy.
#[derive(Default)]
pub struct Latest {
    slot: Mutex<Option<Box<WireSnapshot>>>,
    /// A wake is in the window's queue and has not been handled yet.
    awake: AtomicBool,
}

impl Latest {
    /// Replaces the waiting snapshot. `true` when the window must be woken (none was pending).
    fn put(&self, snap: Box<WireSnapshot>) -> bool {
        *self.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = Some(snap);
        !self.awake.swap(true, Ordering::AcqRel)
    }

    /// The waiting snapshot, if any. Called by the window when it handles [`Up::Snapshot`].
    pub fn take(&self) -> Option<Box<WireSnapshot>> {
        // Cleared first: a snapshot put after this point sends a new wake.
        self.awake.store(false, Ordering::Release);
        self.slot.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take()
    }
}

/// Admits office notifications at a fixed rate per second.
struct Limiter {
    start: Instant,
    count: u32,
    noted: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    Pass,
    /// The first one dropped in this second: the window says that some were dropped.
    DropNoted,
    Drop,
}

impl Limiter {
    fn new(now: Instant) -> Self {
        Self { start: now, count: 0, noted: false }
    }

    fn admit(&mut self, now: Instant) -> Verdict {
        if now.saturating_duration_since(self.start) >= Duration::from_secs(1) {
            *self = Self::new(now);
        }
        if self.count < NOTICES_PER_SEC {
            self.count += 1;
            return Verdict::Pass;
        }
        if self.noted {
            Verdict::Drop
        } else {
            self.noted = true;
            Verdict::DropNoted
        }
    }
}

/// What the worker needs to connect.
pub struct Params {
    pub addr: PeerAddr,
    /// The relay route, when the office has one (tried after the direct address).
    pub relay: Option<csm_peer_proto::pairing::RelayRoute>,
    pub host_public: [u8; KEY_LEN],
    pub identity: Identity,
    pub cfg: ClientConfig,
}

/// Starts the worker. The returned sender is the only way to reach it.
pub fn spawn(params: Params, up: Sender<Up>, latest: Arc<Latest>) -> std::io::Result<Sender<Cmd>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("csm-remote-link".into()).spawn(move || Worker::new(up, latest).run(params, &rx))?;
    Ok(tx)
}

/// The emulator's replies (DA1 and the like) go nowhere: the host's emulator answers them.
struct Discard;

impl ReplySink for Discard {
    fn send(&self, _: &[u8]) {}
}

fn new_terminal(cols: u16, rows: u16) -> Terminal {
    Terminal::new(GridSize::new(cols, rows), TermConfig::default(), Arc::new(Discard))
}

/// Feeds office bytes to a mirror emulator and drops the events it queues (titles, bells, OSC 133
/// marks, OSC 52 copies). Nothing here reads them, and an undrained queue grows for the life of
/// the stream: a hostile office could fill the laptop's memory with 1 MiB clipboard stores.
fn feed(t: &mut Terminal, bytes: &[u8]) {
    t.advance(bytes);
    drop(t.take_events());
}

/// One terminal stream: the session, its follower and the emulator the pane draws.
struct Stream {
    session: String,
    follower: TermFollower,
    term: Arc<Mutex<Terminal>>,
    mode: Arc<AtomicU32>,
    state: Arc<AtomicU8>,
}

/// How a live connection ended.
enum End {
    /// The window asked to close.
    Stop,
    /// The link is gone: the host's reason if it sent one, and what this client saw.
    Lost { bye: Option<ByeReason>, why: String },
}

struct Worker {
    up: Sender<Up>,
    latest: Arc<Latest>,
    mirror: Option<WireSnapshot>,
    /// A delta did not apply: the office was asked for a fresh snapshot, and deltas wait for it.
    resyncing: bool,
    notices: Limiter,
    streams: HashMap<u32, Stream>,
    pending: HashMap<u64, mpsc::Receiver<Outcome>>,
    uploads: Uploads,
}

/// The live connection as the uploads' wire.
struct Live<'a>(&'a Connection);

impl Wire for Live<'_> {
    fn send(&mut self, msg: Msg) -> Result<(), ()> {
        self.0.send(msg).map_err(|_| ())
    }

    fn send_frame(&mut self, frame: &Frame) -> Result<(), ()> {
        self.0.send_frame(frame).map_err(|_| ())
    }
}

impl Worker {
    fn new(up: Sender<Up>, latest: Arc<Latest>) -> Self {
        Self {
            up,
            latest,
            mirror: None,
            resyncing: false,
            notices: Limiter::new(Instant::now()),
            streams: HashMap::new(),
            pending: HashMap::new(),
            uploads: Uploads::new(),
        }
    }

    /// The mirror as the window sees it: the newest copy is kept for the window, and it is woken
    /// only when none is waiting.
    fn publish(&self) {
        if let Some(m) = &self.mirror
            && self.latest.put(Box::new(m.clone()))
        {
            self.send(Up::Snapshot);
        }
    }

    /// Shows an office notification, rate-limited. Consent cards are passed on whatever the rate.
    fn notice(&mut self, e: WireEvent) {
        match self.notices.admit(Instant::now()) {
            Verdict::Pass => self.send(Up::Event(e)),
            Verdict::DropNoted => self.send(Up::Event(WireEvent::Toast { message: "More notifications were dropped.".into() })),
            Verdict::Drop => {}
        }
    }

    fn send(&self, u: Up) {
        // The window is gone when the receiver is: nothing left to tell.
        let _ = self.up.send(u);
    }

    fn run(mut self, p: Params, cmds: &Receiver<Cmd>) {
        let mut attempt = 0u32;
        let mut silent_closes = 0u32;
        let mut reason = String::new();
        loop {
            let state = if attempt == 0 { LinkState::Connecting } else { LinkState::Reconnecting { attempt, reason: reason.clone() } };
            if attempt > 0 {
                // Until the link is back, what the terminals show is not the office's current screen.
                self.mark_streams(RUNNING, STALE);
            }
            self.send(Up::State(state));
            match client::connect_saved(Some(&p.addr), p.relay.as_ref(), &p.host_public, &p.identity, &p.cfg) {
                Ok(conn) => {
                    attempt = 0;
                    silent_closes = 0;
                    self.send(Up::State(LinkState::Live { host_name: conn.welcome.host_name.clone(), level: conn.welcome.level }));
                    match self.live(&conn, cmds) {
                        End::Stop => return self.finish(LinkState::Stopped),
                        End::Lost { bye: Some(b), .. } if refusal_of_bye(b).is_some() => {
                            return self.refuse(refusal_of_bye(b).unwrap_or_default());
                        }
                        End::Lost { bye, why } => {
                            reason = match bye {
                                Some(b) => format!("the office ended the connection ({b:?})"),
                                None => why,
                            };
                        }
                    }
                }
                Err(e) => match refusal_of_error(&e) {
                    // A silent close during the handshake is also what a Wi-Fi flap or a
                    // restarting listener looks like: only a streak of them is a refusal.
                    Some(why) if !matches!(e, ClientError::Refused) || note_silent_close(&mut silent_closes) => return self.refuse(why),
                    Some(_) => reason = "the office closed the connection during the handshake".into(),
                    None => {
                        silent_closes = 0;
                        reason = e.to_string();
                    }
                },
            }
            attempt += 1;
            if self.wait(backoff(attempt), cmds) {
                return self.finish(LinkState::Stopped);
            }
        }
    }

    /// Moves every stream in state `from` to `to` (the terminals' stale mark follows the link).
    fn mark_streams(&self, from: u8, to: u8) {
        for s in self.streams.values() {
            let _ = s.state.compare_exchange(from, to, Ordering::AcqRel, Ordering::Acquire);
        }
    }

    /// Fails every call still waiting, then says where the worker ended.
    fn finish(&mut self, state: LinkState) {
        for (key, _) in std::mem::take(&mut self.pending) {
            self.send(Up::Reply { key, outcome: lost_outcome() });
        }
        self.send(Up::State(state));
    }

    fn refuse(&mut self, reason: String) {
        self.finish(LinkState::Refused { reason });
    }

    /// Sleeps for `dur` while taking commands. `true` when the window asked to stop.
    fn wait(&mut self, dur: Duration, cmds: &Receiver<Cmd>) -> bool {
        let until = Instant::now() + dur;
        loop {
            loop {
                match cmds.try_recv() {
                    Ok(cmd) => {
                        if self.offline(cmd) {
                            return true;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return true,
                }
            }
            let now = Instant::now();
            if now >= until {
                return false;
            }
            std::thread::sleep(Duration::from_millis(20).min(until - now));
        }
    }

    /// A command while there is no link: calls are refused now (never queued, so nothing runs
    /// twice later), and terminals are only recorded (they are subscribed again on connect).
    fn offline(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Call { key, .. } => self.send(Up::Reply { key, outcome: Outcome::Err { kind: ErrorKind::Failed, message: "not connected to the office right now".into() } }),
            Cmd::Subscribe { stream, session, term, mode, state } => {
                self.streams.insert(stream, Stream { session, follower: TermFollower::new(stream), term, mode, state });
            }
            Cmd::Unsubscribe { stream } => {
                self.streams.remove(&stream);
            }
            Cmd::Resize { .. } | Cmd::Input { .. } => {}
            Cmd::Upload(req) => {
                let drop = req.drop;
                self.send(Up::Upload(UploadEvent::Failed { drop, message: "not connected to the office right now; the files were not sent. Drop them again when it is back.".into() }));
            }
            // Nothing is in flight without a link: every upload ended when the link did.
            Cmd::UploadCancel { .. } => {}
            Cmd::Stop => return true,
        }
        false
    }

    /// Sends the next upload frame, if one may go. `Err` when the link is gone. Returns whether a
    /// frame went out, so the caller does not wait for the office before the next one.
    fn pump_uploads(&mut self, conn: &Connection) -> Result<bool, End> {
        let mut events = Vec::new();
        let pumped = self.uploads.pump(&mut Live(conn), Instant::now(), &mut events);
        for ev in events {
            self.send(Up::Upload(ev));
        }
        match pumped {
            Pumped::Sent => Ok(true),
            Pumped::Idle => Ok(false),
            Pumped::Lost => Err(End::Lost { bye: None, why: "the connection closed during an upload".into() }),
        }
    }

    /// Serves one connection until it ends.
    fn live(&mut self, conn: &Connection, cmds: &Receiver<Cmd>) -> End {
        self.mark_streams(STALE, RUNNING);
        for (stream, s) in &mut self.streams {
            s.follower.new_link();
            if conn.subscribe(*stream, &s.session, s.follower.resume_point()).is_err() {
                return End::Lost { bye: None, why: "the connection closed while resuming the terminals".into() };
            }
        }
        match conn.call("tool_list", json!({})) {
            Ok(rx) => {
                self.pending.insert(TOOLS_KEY, rx);
            }
            Err(_) => return End::Lost { bye: None, why: "the connection closed before it was ready".into() },
        }
        let end = 'serve: loop {
            loop {
                match cmds.try_recv() {
                    Ok(cmd) => {
                        if let Some(end) = self.online(conn, cmd) {
                            break 'serve end;
                        }
                    }
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'serve End::Stop,
                }
            }
            // One upload frame per turn: a keystroke queued meanwhile goes out before the next chunk.
            let sent = match self.pump_uploads(conn) {
                Ok(sent) => sent,
                Err(end) => break 'serve end,
            };
            let wait = if sent { Duration::ZERO } else { Duration::from_millis(10) };
            match conn.recv_timeout(wait) {
                Some(Incoming::Frame(frame)) => {
                    if let Some(end) = self.frame(conn, &frame) {
                        break 'serve end;
                    }
                }
                Some(Incoming::Closed(bye)) => break 'serve End::Lost { bye, why: "the connection closed".into() },
                None => {}
            }
            self.poll_pending();
            if conn.is_closed() {
                break 'serve End::Lost { bye: None, why: "the connection closed".into() };
            }
        };
        self.fail_pending();
        for ev in self.uploads.lost() {
            self.send(Up::Upload(ev));
        }
        end
    }

    /// A command while the link is up. `Some` ends the connection.
    fn online(&mut self, conn: &Connection, cmd: Cmd) -> Option<End> {
        match cmd {
            Cmd::Call { key, tool, params } => match conn.call(&tool, params) {
                Ok(rx) => {
                    self.pending.insert(key, rx);
                }
                Err(_) => self.send(Up::Reply { key, outcome: lost_outcome() }),
            },
            Cmd::Subscribe { stream, session, term, mode, state } => {
                if conn.subscribe(stream, &session, None).is_err() {
                    return Some(End::Lost { bye: None, why: "the connection closed".into() });
                }
                self.streams.insert(stream, Stream { session, follower: TermFollower::new(stream), term, mode, state });
            }
            Cmd::Unsubscribe { stream } => {
                self.streams.remove(&stream);
                let _ = conn.unsubscribe(stream);
            }
            Cmd::Resize { stream, cols, rows, driver } => {
                if self.streams.contains_key(&stream) {
                    let _ = conn.resize(stream, cols, rows, driver);
                }
            }
            Cmd::Input { stream, input } => {
                if self.streams.contains_key(&stream) {
                    let _ = conn.input(stream, input);
                }
            }
            Cmd::Upload(req) => {
                for ev in self.uploads.enqueue(req) {
                    self.send(Up::Upload(ev));
                }
            }
            Cmd::UploadCancel { drop } => {
                for ev in self.uploads.cancel(drop, &mut Live(conn)) {
                    self.send(Up::Upload(ev));
                }
            }
            Cmd::Stop => return Some(End::Stop),
        }
        None
    }

    /// One frame from the host. `Some` ends the connection (the mirror can no longer be followed).
    fn frame(&mut self, conn: &Connection, frame: &Frame) -> Option<End> {
        match frame {
            Frame::Control(Msg::Snapshot { snapshot }) => {
                self.mirror = Some((**snapshot).clone());
                self.resyncing = false;
                self.publish();
            }
            Frame::Control(Msg::Delta { delta }) => {
                if self.resyncing {
                    // A snapshot is on its way; the deltas after it follow from that version.
                    return None;
                }
                let applied = match self.mirror.as_mut() {
                    Some(m) => delta.apply(m).map_err(|e| format!("{e:?}")),
                    None => Err("a change arrived before the snapshot".to_owned()),
                };
                match applied {
                    Ok(()) => self.publish(),
                    Err(why) => {
                        // The mirror is out of step with the office. Ask for the whole snapshot on this
                        // link, instead of dropping the link and reconnecting for the same result.
                        log::warn!("remote window: a change did not apply ({why}); asking the office for a fresh snapshot");
                        self.resyncing = true;
                        if conn.send(Msg::SnapshotRequest).is_err() {
                            return Some(End::Lost { bye: None, why: "the connection closed while asking for a snapshot".into() });
                        }
                    }
                }
            }
            Frame::Control(Msg::Event { event: Some(e) }) => match e {
                WireEvent::ConsentRequested { .. } | WireEvent::ConsentResolved { .. } => self.send(Up::Event(e.clone())),
                _ => self.notice(e.clone()),
            },
            Frame::Control(Msg::ConsentCard { request }) => self.send(Up::Event(WireEvent::ConsentRequested { request: request.clone() })),
            Frame::TermAttach { .. } | Frame::TermData { .. } | Frame::Control(Msg::TermResized { .. } | Msg::TermClosed { .. }) => {
                self.term_frame(conn, frame);
            }
            Frame::Control(m @ (Msg::UploadAccepted { .. } | Msg::UploadAck { .. } | Msg::UploadDone { .. } | Msg::UploadRefused { .. })) => {
                let events = self.uploads.office(m, Instant::now(), &mut Live(conn));
                for ev in events {
                    self.send(Up::Upload(ev));
                }
            }
            _ => {}
        }
        None
    }

    /// A terminal frame: the follower checks the order, the emulator takes the bytes.
    fn term_frame(&mut self, conn: &Connection, frame: &Frame) {
        let stream = match frame {
            Frame::TermAttach { stream, .. } | Frame::TermData { stream, .. } => *stream,
            Frame::Control(Msg::TermResized { stream, .. } | Msg::TermClosed { stream, .. }) => *stream,
            _ => return,
        };
        if let Frame::TermAttach { cols, rows, .. } = frame {
            let others: u64 = self
                .streams
                .iter()
                .filter(|(k, _)| **k != stream)
                .map(|(_, o)| {
                    let z = lock(&o.term).size();
                    u64::from(z.cols) * u64::from(z.rows)
                })
                .sum();
            if others + u64::from(*cols) * u64::from(*rows) > MAX_MIRROR_CELLS {
                log::warn!("remote terminal {stream}: {cols}x{rows} would exceed the mirror size limit; stream ended");
                if let Some(s) = self.streams.get(&stream) {
                    s.state.store(ENDED, Ordering::Release);
                }
                return;
            }
        }
        let Some(s) = self.streams.get_mut(&stream) else { return };
        match s.follower.on_frame(frame) {
            Ok(TermUpdate::Attached(a)) => {
                let mut t = lock(&s.term);
                *t = new_terminal(a.cols, a.rows);
                feed(&mut t, &a.vt);
                s.mode.store(t.mode().bits(), Ordering::Release);
                s.state.store(RUNNING, Ordering::Release);
            }
            Ok(TermUpdate::Data(bytes)) => {
                let mut t = lock(&s.term);
                feed(&mut t, &bytes);
                s.mode.store(t.mode().bits(), Ordering::Release);
            }
            Ok(TermUpdate::Resized { cols, rows }) => {
                let mut t = lock(&s.term);
                let cur = t.size();
                t.resize(GridSize { cols, rows, cell_width: cur.cell_width, cell_height: cur.cell_height });
            }
            Ok(TermUpdate::Closed(_)) => s.state.store(ENDED, Ordering::Release),
            Ok(TermUpdate::Gap) => {
                let _ = conn.resync(stream);
            }
            Ok(TermUpdate::Ignored) => {}
            Err(e) => {
                log::warn!("remote terminal {stream}: bad frame ({e:?}); asking for a fresh screen");
                let _ = conn.resync(stream);
            }
        }
    }

    /// Hands over the outcomes that have arrived.
    fn poll_pending(&mut self) {
        let keys: Vec<u64> = self.pending.keys().copied().collect();
        for key in keys {
            let done = match self.pending.get(&key).map(mpsc::Receiver::try_recv) {
                Some(Ok(o)) => Some(o),
                Some(Err(TryRecvError::Disconnected)) => Some(lost_outcome()),
                _ => None,
            };
            if let Some(outcome) = done {
                self.pending.remove(&key);
                self.deliver(key, outcome);
            }
        }
    }

    /// The link ended: an outcome that arrived is delivered, the rest are lost.
    fn fail_pending(&mut self) {
        self.poll_pending();
        for (key, _) in std::mem::take(&mut self.pending) {
            self.deliver(key, lost_outcome());
        }
    }

    fn deliver(&self, key: u64, outcome: Outcome) {
        if key == TOOLS_KEY {
            let list = match &outcome {
                Outcome::Ok { data, .. } => map::tools(data),
                Outcome::Err { .. } => Vec::new(),
            };
            self.send(Up::Tools(list));
        } else {
            self.send(Up::Reply { key, outcome });
        }
    }
}

fn lost_outcome() -> Outcome {
    Outcome::Err { kind: ErrorKind::Failed, message: "the connection to the office ended before the answer came back; the call may have run, so check its result".into() }
}

/// Silent closes in a row before the office is taken at its word that it does not accept this
/// device. Each is retried after the usual backoff (1, 2, 5, 10 s), so a Wi-Fi flap or a
/// listener restart is outlasted before the window gives up for good.
const SILENT_CLOSES_BEFORE_REFUSAL: u32 = 5;

/// Counts one more silent close during the handshake; `true` when the streak is long enough to
/// be a refusal.
fn note_silent_close(streak: &mut u32) -> bool {
    *streak = streak.saturating_add(1);
    *streak >= SILENT_CLOSES_BEFORE_REFUSAL
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_secs(match attempt {
        0 | 1 => 1,
        2 => 2,
        3 => 5,
        4 => 10,
        _ => 30,
    })
}

/// A bye the retry cannot get past, with what to tell the person.
pub(crate) fn refusal_of_bye(b: ByeReason) -> Option<String> {
    Some(match b {
        ByeReason::Revoked => "This device was removed from the office's list. Pair it again to connect.".into(),
        ByeReason::ProtocolMismatch => "The two CodeShellManager versions do not speak the same protocol. Update both.".into(),
        ByeReason::TermMismatch => "The two CodeShellManager versions have different terminal emulators. Update both to the same version.".into(),
        ByeReason::Replaced => "Another window connected with this device, and took the connection over.".into(),
        _ => return None,
    })
}

pub(crate) fn refusal_of_error(e: &ClientError) -> Option<String> {
    match e {
        ClientError::Refused => Some("The office does not accept this device. It may have been removed, or the office forgot the pairing.".into()),
        ClientError::Bye(b) => refusal_of_bye(*b),
        _ => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn office_output_leaves_no_queued_events() {
        let mut t = new_terminal(80, 24);
        let mut junk = Vec::new();
        for i in 0..500 {
            junk.extend_from_slice(format!("]0;title {i}]133;A").as_bytes());
        }
        junk.extend_from_slice(b"]52;c;aGVsbG8=");
        feed(&mut t, &junk);
        assert!(t.take_events().is_empty(), "the mirror must not keep events nobody reads");
    }

    #[test]
    fn a_silent_close_is_a_refusal_only_after_a_streak() {
        let mut streak = 0;
        for _ in 1..SILENT_CLOSES_BEFORE_REFUSAL {
            assert!(!note_silent_close(&mut streak), "one flap must not end the retries");
        }
        assert!(note_silent_close(&mut streak));
    }

    #[test]
    fn backoff_follows_the_schedule_and_caps() {
        let secs: Vec<u64> = (1..=7).map(|a| backoff(a).as_secs()).collect();
        assert_eq!(secs, vec![1, 2, 5, 10, 30, 30, 30]);
    }

    #[test]
    fn only_refusals_end_the_retries() {
        assert!(refusal_of_bye(ByeReason::Revoked).is_some());
        assert!(refusal_of_bye(ByeReason::ProtocolMismatch).is_some());
        assert!(refusal_of_bye(ByeReason::TermMismatch).is_some());
        assert!(refusal_of_bye(ByeReason::Replaced).is_some());
        for transient in [ByeReason::ServerOff, ByeReason::ServerQuit, ByeReason::Timeout, ByeReason::BadMessage, ByeReason::ClientQuit] {
            assert!(refusal_of_bye(transient).is_none(), "{transient:?} is worth a retry");
        }
        assert!(refusal_of_error(&ClientError::Refused).is_some());
        assert!(refusal_of_error(&ClientError::Connect("refused".into())).is_none());
        assert!(refusal_of_error(&ClientError::Timeout).is_none());
    }

    #[test]
    fn the_window_gets_only_the_newest_snapshot_and_one_wake_at_a_time() {
        let latest = Latest::default();
        let mut wakes = 0;
        for version in 1..=1000u64 {
            let snap = WireSnapshot { version, ..WireSnapshot::default() };
            if latest.put(Box::new(snap)) {
                wakes += 1;
            }
        }
        assert_eq!(wakes, 1, "one wake is pending, however many snapshots came in");
        assert_eq!(latest.take().map(|s| s.version), Some(1000), "the window gets the newest");
        assert!(latest.take().is_none(), "nothing is left for a second take");
        assert!(latest.put(Box::new(WireSnapshot::default())), "after a take, the next snapshot wakes the window again");
    }

    #[test]
    fn a_snapshot_put_while_the_window_takes_is_not_lost() {
        let latest = Latest::default();
        assert!(latest.put(Box::new(WireSnapshot { version: 1, ..WireSnapshot::default() })));
        assert_eq!(latest.take().map(|s| s.version), Some(1));
        // The worker puts a newer one after the take: it must wake the window again.
        assert!(latest.put(Box::new(WireSnapshot { version: 2, ..WireSnapshot::default() })));
        assert_eq!(latest.take().map(|s| s.version), Some(2));
    }

    #[test]
    fn notices_are_admitted_at_a_fixed_rate_with_one_note_per_second() {
        let t0 = Instant::now();
        let mut lim = Limiter::new(t0);
        let verdicts: Vec<Verdict> = (0..25).map(|_| lim.admit(t0)).collect();
        assert_eq!(verdicts.iter().filter(|v| **v == Verdict::Pass).count(), NOTICES_PER_SEC as usize);
        assert_eq!(verdicts.iter().filter(|v| **v == Verdict::DropNoted).count(), 1, "one drop is noted");
        assert_eq!(verdicts.iter().filter(|v| **v == Verdict::Drop).count(), 25 - NOTICES_PER_SEC as usize - 1);
        // A new second admits again, and its first drop is noted again.
        let t1 = t0 + Duration::from_millis(1000);
        assert_eq!(lim.admit(t1), Verdict::Pass);
        for _ in 1..NOTICES_PER_SEC {
            lim.admit(t1);
        }
        assert_eq!(lim.admit(t1), Verdict::DropNoted);
    }

    #[test]
    fn a_lost_call_says_it_may_have_run() {
        let Outcome::Err { kind, message } = lost_outcome() else { panic!("an error") };
        assert_eq!(kind, ErrorKind::Failed);
        assert!(message.contains("may have run"));
    }
}
