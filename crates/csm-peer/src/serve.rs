//! What the server shows one connected device: the snapshot and its deltas, and the terminal
//! streams (design sections 3 and 5).
//!
//! **Snapshot.** Right after `Welcome` the device gets the whole read model; a publisher thread
//! then looks at the engine's version every [`Knobs::snapshot_every`] and sends the delta since
//! the last version it sent, so a burst of changes coalesces into one delta and a slow link just
//! skips versions. A delta that is too big for one control message falls back to a full snapshot.
//!
//! **Terminal streams.** `TermSubscribe` starts one thread per stream. It reads the session's
//! output ring (`csm_engine::output_tap`) at its own pace, so nothing queues in this crate:
//!
//! - *Attach*: the grid as VT bytes (`csm_term::replay`) with the output offset it describes, then
//!   the live bytes from that offset. Used when the device has no usable resume point, when its
//!   offset has left the ring or is too far behind, and whenever the stream falls behind.
//! - *Resume*: when the device names the run (`instance`) and the offset it has, and the ring still
//!   holds everything after it (and every resize since), the stream just continues there.
//! - *Backpressure*: a stream that is more than [`Knobs::backlog`] bytes behind, or whose offset
//!   the ring evicted, sends a fresh attach instead of the bytes in between (state sync, as mosh
//!   does). Input, calls and replies are on the same link but are never queued behind terminal
//!   data: each is one message, written under the link's short writer lock.
//! - *Order*: only bytes the server's emulator has consumed are sent, and a resize is announced
//!   (`TermResized`) exactly at the offset it took effect, so the client's emulator is in lockstep.
//!
//! **Size.** One PTY has one size. The device that last typed, focused its pane or said it is the
//! `driver` decides it; a device that is not the driver only records what size it would like and
//! gets it when it becomes the driver. The office window does not fight back.
//!
//! Input from a device goes to the session as typed input (`TerminalHandle::send_key`,
//! `send_mouse`, `paste`, `focus_changed`), so "the person answered" and bracketed paste keep
//! working. It needs the Operator level; a Viewer only watches. Input is audited as byte counts
//! per session per minute, never content.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use csm_engine::input::AppAuthored;
use csm_engine::output_tap::Read;
use csm_engine::{AttachSnapshot, Engine, TerminalHandle};
use csm_peer_proto::Snapshot;
use csm_peer_proto::codec::{CodecError, Frame, Input};
use csm_peer_proto::delta::diff;
use csm_peer_proto::msg::{Level, Msg, TermClosedReason};
use serde_json::json;

use crate::audit::{Audit, clean};
use crate::link::{Link, LinkError};

/// Which connection drives each session's size.
pub(crate) type Drivers = Arc<Mutex<HashMap<String, u64>>>;

/// The numbers a server is tuned with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Knobs {
    /// A stream this many bytes behind resyncs instead of catching up.
    pub backlog: usize,
    /// Scrollback lines an attach carries.
    pub history: usize,
    /// Streams one connection may have open.
    pub max_streams: usize,
    pub snapshot_every: Duration,
}

/// Whose connection this is.
pub(crate) struct Who {
    pub conn_id: u64,
    pub device_id: String,
    pub level: Level,
}

struct Stream {
    stop: Arc<AtomicBool>,
    done: Arc<AtomicBool>,
    session: String,
    handle: TerminalHandle,
    /// The size this device's pane would like (applied once it drives).
    want: Option<(u16, u16)>,
}

struct Meter {
    bytes: HashMap<String, u64>,
    since: Instant,
}

/// What a device sees of the host, for the life of one connection.
pub(crate) struct Serving {
    engine: Engine,
    link: Arc<Link>,
    conn_id: u64,
    device_id: String,
    level: Level,
    knobs: Knobs,
    drivers: Drivers,
    audit: Arc<Audit>,
    streams: Mutex<HashMap<u32, Stream>>,
    stop: Arc<AtomicBool>,
    meter: Mutex<Meter>,
}

/// How a stream begins.
enum Start {
    Attach,
    /// Continue at this offset, after the resize marks up to `seq`.
    Resume { offset: u64, seq: u64 },
}

impl Serving {
    pub(crate) fn new(engine: Engine, link: Arc<Link>, who: Who, knobs: Knobs, drivers: Drivers, audit: Arc<Audit>) -> Arc<Self> {
        Arc::new(Self {
            engine,
            link,
            conn_id: who.conn_id,
            device_id: who.device_id,
            level: who.level,
            knobs,
            drivers,
            audit,
            streams: Mutex::new(HashMap::new()),
            stop: Arc::new(AtomicBool::new(false)),
            meter: Mutex::new(Meter { bytes: HashMap::new(), since: Instant::now() }),
        })
    }

    /// Sends the whole snapshot and starts the publisher.
    pub(crate) fn start(&self) -> Result<(), LinkError> {
        let view = self.engine.view();
        let first = Snapshot::from(&*view);
        self.link.send_msg(Msg::Snapshot { snapshot: Box::new(first.clone()) })?;
        let (engine, link, stop, every) = (self.engine.clone(), self.link.clone(), self.stop.clone(), self.knobs.snapshot_every);
        std::thread::Builder::new()
            .name("csm-peer-snapshot".into())
            .spawn(move || publish_loop(&engine, &link, &stop, every, first))
            .map(|_| ())
            .map_err(|e| LinkError::Io(e.to_string()))
    }

    /// The connection ended: stop every thread of it, give up the size, write the input counts.
    pub(crate) fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        let streams: Vec<Stream> = self.lock_streams().drain().map(|(_, s)| s).collect();
        for s in &streams {
            s.stop.store(true, Ordering::Release);
        }
        self.drivers.lock().unwrap_or_else(PoisonError::into_inner).retain(|_, c| *c != self.conn_id);
        self.flush_meter();
    }

    fn lock_streams(&self) -> std::sync::MutexGuard<'_, HashMap<u32, Stream>> {
        self.streams.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Handles the terminal messages of a device. Returns false for any other frame.
    pub(crate) fn on_frame(&self, frame: &Frame) -> bool {
        match frame {
            Frame::Control(Msg::TermSubscribe { stream, session, instance, from_offset }) => self.subscribe(*stream, session, instance.zip(*from_offset)),
            Frame::Control(Msg::TermUnsubscribe { stream }) => self.unsubscribe(*stream),
            Frame::Control(Msg::TermResync { stream }) => self.resync(*stream),
            Frame::Control(Msg::TermResize { stream, cols, rows, driver }) => self.resize(*stream, *cols, *rows, *driver),
            Frame::TermInput { stream, input } => self.input(*stream, input),
            _ => return false,
        }
        true
    }

    fn closed(&self, stream: u32, reason: TermClosedReason) {
        let _ = self.link.send_msg(Msg::TermClosed { stream, reason });
    }

    fn subscribe(&self, stream: u32, session: &str, resume: Option<(u64, u64)>) {
        let Some(handle) = self.engine.terminal(session) else {
            let known = self.engine.view().session(session).is_some();
            self.closed(stream, if known { TermClosedReason::NotRunning } else { TermClosedReason::UnknownSession });
            return;
        };
        {
            let mut streams = self.lock_streams();
            streams.retain(|_, s| !s.done.load(Ordering::Acquire));
            if !streams.contains_key(&stream) && streams.len() >= self.knobs.max_streams {
                drop(streams);
                self.closed(stream, TermClosedReason::Forbidden);
                return;
            }
        }
        let start = match resume {
            Some((instance, offset)) if handle.can_resume(instance, offset) && handle.processed_offset().saturating_sub(offset) <= self.knobs.backlog as u64 => {
                Start::Resume { offset, seq: handle.resizes_for_resume(offset) }
            }
            _ => Start::Attach,
        };
        self.begin(stream, session, handle, start);
    }

    /// Starts (or replaces) the thread of `stream`.
    fn begin(&self, stream: u32, session: &str, handle: TerminalHandle, start: Start) {
        let (stop, done) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicBool::new(false)));
        let old = {
            let mut streams = self.lock_streams();
            let want = streams.get(&stream).filter(|s| s.session == session).and_then(|s| s.want);
            streams.insert(stream, Stream { stop: stop.clone(), done: done.clone(), session: session.to_owned(), handle: handle.clone(), want })
        };
        if let Some(old) = old {
            old.stop.store(true, Ordering::Release);
        }
        let run = Run {
            engine: self.engine.clone(),
            link: self.link.clone(),
            stream,
            session: session.to_owned(),
            handle,
            knobs: self.knobs,
            stop,
            conn_stop: self.stop.clone(),
        };
        let spawned = std::thread::Builder::new().name("csm-peer-stream".into()).spawn(move || {
            run.go(start);
            done.store(true, Ordering::Release);
        });
        if let Err(e) = spawned {
            log::warn!("remote access: could not start a terminal stream: {e}");
            self.lock_streams().remove(&stream);
            self.closed(stream, TermClosedReason::Forbidden);
        }
    }

    fn unsubscribe(&self, stream: u32) {
        let gone = self.lock_streams().remove(&stream);
        if let Some(s) = gone {
            s.stop.store(true, Ordering::Release);
            // Not driving a session nobody of this device shows any more.
            let still_shown = self.lock_streams().values().any(|o| o.session == s.session);
            if !still_shown {
                let mut d = self.drivers.lock().unwrap_or_else(PoisonError::into_inner);
                if d.get(&s.session) == Some(&self.conn_id) {
                    d.remove(&s.session);
                }
            }
        }
        self.closed(stream, TermClosedReason::Unsubscribed);
    }

    fn resync(&self, stream: u32) {
        let found = self.lock_streams().get(&stream).map(|s| (s.session.clone(), s.handle.clone()));
        if let Some((session, handle)) = found {
            self.begin(stream, &session, handle, Start::Attach);
        }
    }

    fn operator(&self) -> bool {
        self.level.allows(Level::Operator)
    }

    /// Makes this device the one whose size the PTY follows, and applies the size its pane wants.
    fn drive(&self, stream: u32) {
        let found = self.lock_streams().get(&stream).map(|s| (s.session.clone(), s.handle.clone(), s.want));
        let Some((session, handle, want)) = found else { return };
        let changed = self.drivers.lock().unwrap_or_else(PoisonError::into_inner).insert(session, self.conn_id) != Some(self.conn_id);
        if changed && let Some((cols, rows)) = want {
            handle.resize_cells(cols, rows);
        }
    }

    fn resize(&self, stream: u32, cols: u16, rows: u16, driver: bool) {
        if !self.operator() {
            return;
        }
        let found = {
            let mut streams = self.lock_streams();
            streams.get_mut(&stream).map(|s| {
                s.want = Some((cols, rows));
                (s.session.clone(), s.handle.clone())
            })
        };
        let Some((session, handle)) = found else { return };
        if driver {
            self.drivers.lock().unwrap_or_else(PoisonError::into_inner).insert(session.clone(), self.conn_id);
        }
        let drives = self.drivers.lock().unwrap_or_else(PoisonError::into_inner).get(&session) == Some(&self.conn_id);
        if drives {
            handle.resize_cells(cols, rows);
        }
    }

    fn input(&self, stream: u32, input: &Input) {
        if !self.operator() {
            return;
        }
        let found = self.lock_streams().get(&stream).map(|s| (s.session.clone(), s.handle.clone()));
        let Some((session, handle)) = found else { return };
        let n = match input {
            Input::Key(b) => {
                self.drive(stream);
                handle.send_key(AppAuthored(b.clone()));
                b.len()
            }
            Input::Mouse(b) => {
                handle.send_mouse(AppAuthored(b.clone()));
                b.len()
            }
            Input::Focus(f) => {
                if *f {
                    self.drive(stream);
                }
                handle.focus_changed(*f);
                0
            }
            Input::Paste(t) => {
                self.drive(stream);
                // Untrusted text: the terminal handle brackets it when the program asked for that.
                handle.paste(t);
                t.len()
            }
        };
        if n > 0 {
            self.note_input(&session, n);
        }
    }

    fn note_input(&self, session: &str, n: usize) {
        let due = {
            let mut m = self.meter.lock().unwrap_or_else(PoisonError::into_inner);
            let slot = m.bytes.entry(session.to_owned()).or_insert(0);
            *slot = slot.saturating_add(n as u64);
            m.since.elapsed() >= Duration::from_secs(60)
        };
        if due {
            self.flush_meter();
        }
    }

    fn flush_meter(&self) {
        let taken = {
            let mut m = self.meter.lock().unwrap_or_else(PoisonError::into_inner);
            m.since = Instant::now();
            std::mem::take(&mut m.bytes)
        };
        for (session, bytes) in taken {
            self.audit.record("input", json!({"device_id": self.device_id, "session": clean(&session), "bytes": bytes}));
        }
    }
}

// ---- the snapshot publisher ------------------------------------------------------------------

fn publish_loop(engine: &Engine, link: &Link, stop: &AtomicBool, every: Duration, mut last: Snapshot) {
    loop {
        std::thread::sleep(every);
        if stop.load(Ordering::Acquire) || link.is_closed() {
            return;
        }
        let view = engine.view();
        if view.version == last.version {
            continue;
        }
        let next = Snapshot::from(&*view);
        let delta = diff(&last, &next);
        if delta.ops.is_empty() {
            // Nothing the wire carries changed: the device stays at the old version, and the next
            // delta starts there.
            continue;
        }
        let sent = match link.send_msg(Msg::Delta { delta }) {
            Err(LinkError::Codec(CodecError::TooLarge)) => link.send_msg(Msg::Snapshot { snapshot: Box::new(next.clone()) }),
            other => other,
        };
        match sent {
            Ok(()) => last = next,
            Err(LinkError::Codec(_)) => {} // too large even whole: skip this version
            Err(_) => return,
        }
    }
}

// ---- one terminal stream ---------------------------------------------------------------------

struct Run {
    engine: Engine,
    link: Arc<Link>,
    stream: u32,
    session: String,
    handle: TerminalHandle,
    knobs: Knobs,
    stop: Arc<AtomicBool>,
    conn_stop: Arc<AtomicBool>,
}

/// How long an idle stream waits for a nudge before it looks at its clocks.
const IDLE_WAIT: Duration = Duration::from_millis(25);
/// How often a stream checks that its session still runs.
const ALIVE_EVERY: Duration = Duration::from_millis(100);

impl Run {
    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire) || self.conn_stop.load(Ordering::Acquire) || self.link.is_closed()
    }

    fn go(&self, start: Start) {
        let instance = self.handle.instance();
        let (mut sub, mut seen_seq) = match start {
            Start::Resume { offset, seq } => (self.handle.subscribe_output(Some(offset)), seq),
            Start::Attach => match self.attach(instance) {
                Some(s) => s,
                None => return,
            },
        };
        let mut checked = Instant::now();
        loop {
            if self.stopped() {
                return;
            }
            if checked.elapsed() >= ALIVE_EVERY {
                checked = Instant::now();
                if let Some(reason) = self.gone(instance) {
                    let _ = self.link.send_msg(Msg::TermClosed { stream: self.stream, reason });
                    return;
                }
            }
            // Only what the emulator has consumed: read it first, then the marks, so a resize that
            // lands in between has an offset at or after the end of what is sent.
            let processed = self.handle.processed_offset();
            let pos = sub.offset();
            // Too far behind: state sync beats a backlog. (Measured against what can be sent: bytes
            // the emulator has not consumed yet are not a backlog, and right after an attach `pos`
            // is the consumed offset, so a fresh attach never triggers another.)
            if processed.saturating_sub(pos) > self.knobs.backlog as u64 {
                match self.attach(instance) {
                    Some(s) => {
                        (sub, seen_seq) = s;
                        continue;
                    }
                    None => return,
                }
            }
            let mut limit = processed.saturating_sub(pos);
            for m in self.handle.resizes_after(seen_seq) {
                if m.offset <= pos {
                    let msg = Msg::TermResized { stream: self.stream, offset: pos, cols: m.cols, rows: m.rows };
                    if self.link.send_msg(msg).is_err() {
                        return;
                    }
                    seen_seq = m.seq;
                } else {
                    limit = limit.min(m.offset - pos);
                    break;
                }
            }
            let max = usize::try_from(limit).unwrap_or(usize::MAX);
            if max == 0 {
                if sub.behind() > 0 {
                    // Fed but not yet consumed by the emulator: a moment.
                    std::thread::sleep(Duration::from_millis(1));
                } else {
                    sub.wait(IDLE_WAIT);
                }
                continue;
            }
            match sub.read(max) {
                Read::Bytes { from, data } => {
                    if data.is_empty() {
                        sub.wait(IDLE_WAIT);
                        continue;
                    }
                    for f in Frame::term_data(self.stream, instance, from, &data) {
                        if self.link.send_frame(&f).is_err() {
                            return;
                        }
                    }
                }
                Read::Resync { .. } => match self.attach(instance) {
                    Some(s) => (sub, seen_seq) = s,
                    None => return,
                },
            }
        }
    }

    /// Sends a fresh attach and returns the subscription that continues it, with the resize
    /// sequence the attach already includes. `None` ends the stream.
    fn attach(&self, instance: u64) -> Option<(csm_engine::output_tap::Subscription, u64)> {
        if let Some(reason) = self.gone(instance) {
            let _ = self.link.send_msg(Msg::TermClosed { stream: self.stream, reason });
            return None;
        }
        let a: AttachSnapshot = self.handle.attach_snapshot(self.knobs.history);
        let sub = self.handle.subscribe_output(Some(a.offset));
        for f in Frame::attach(self.stream, a.instance, a.cols, a.rows, a.offset, &a.vt) {
            if self.link.send_frame(&f).is_err() {
                if !self.link.is_closed() {
                    let _ = self.link.send_msg(Msg::TermClosed { stream: self.stream, reason: TermClosedReason::Forbidden });
                }
                return None;
            }
        }
        Some((sub, a.resize_seq))
    }

    /// Why this stream must end, if it must: the session was removed, or restarted (a new run
    /// has another instance and the device attaches again).
    fn gone(&self, instance: u64) -> Option<TermClosedReason> {
        if self.handle.is_closed() {
            return Some(TermClosedReason::SessionGone);
        }
        match self.engine.terminal(&self.session) {
            Some(h) if h.instance() == instance => None,
            Some(_) => Some(TermClosedReason::NotRunning),
            None if self.engine.view().session(&self.session).is_some() => Some(TermClosedReason::NotRunning),
            None => Some(TermClosedReason::SessionGone),
        }
    }
}
