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
use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
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
use csm_ui::backend::ToolInfo;
use serde_json::{Value, json};

use crate::map;
use crate::terminal::{ENDED, RUNNING, lock};

/// The call key the worker uses for its own `tool_list` at connect. The UI's keys start at 1.
const TOOLS_KEY: u64 = 0;

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
    /// Close the window's connection and end the worker.
    Stop,
}

/// What the worker tells the UI thread.
pub enum Up {
    State(LinkState),
    /// The host's `tool_list`, once per connection.
    Tools(Vec<ToolInfo>),
    /// The whole mirrored snapshot, after a full copy or a delta.
    Snapshot(Box<WireSnapshot>),
    Event(WireEvent),
    Reply { key: u64, outcome: Outcome },
}

/// What the worker needs to connect.
pub struct Params {
    pub addr: PeerAddr,
    pub host_public: [u8; KEY_LEN],
    pub identity: Identity,
    pub cfg: ClientConfig,
}

/// Starts the worker. The returned sender is the only way to reach it.
pub fn spawn(params: Params, up: Sender<Up>) -> std::io::Result<Sender<Cmd>> {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new().name("csm-remote-link".into()).spawn(move || Worker::new(up).run(params, &rx))?;
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
    mirror: Option<WireSnapshot>,
    streams: HashMap<u32, Stream>,
    pending: HashMap<u64, mpsc::Receiver<Outcome>>,
}

impl Worker {
    fn new(up: Sender<Up>) -> Self {
        Self { up, mirror: None, streams: HashMap::new(), pending: HashMap::new() }
    }

    fn send(&self, u: Up) {
        // The window is gone when the receiver is: nothing left to tell.
        let _ = self.up.send(u);
    }

    fn run(mut self, p: Params, cmds: &Receiver<Cmd>) {
        let mut attempt = 0u32;
        let mut reason = String::new();
        loop {
            let state = if attempt == 0 { LinkState::Connecting } else { LinkState::Reconnecting { attempt, reason: reason.clone() } };
            self.send(Up::State(state));
            match client::connect(&p.addr, &p.host_public, &p.identity, &p.cfg) {
                Ok(conn) => {
                    attempt = 0;
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
                    Some(why) => return self.refuse(why),
                    None => reason = e.to_string(),
                },
            }
            attempt += 1;
            if self.wait(backoff(attempt), cmds) {
                return self.finish(LinkState::Stopped);
            }
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
            Cmd::Stop => return true,
        }
        false
    }

    /// Serves one connection until it ends.
    fn live(&mut self, conn: &Connection, cmds: &Receiver<Cmd>) -> End {
        for (stream, s) in &self.streams {
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
            match conn.recv_timeout(Duration::from_millis(10)) {
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
            Cmd::Stop => return Some(End::Stop),
        }
        None
    }

    /// One frame from the host. `Some` ends the connection (the mirror can no longer be followed).
    fn frame(&mut self, conn: &Connection, frame: &Frame) -> Option<End> {
        match frame {
            Frame::Control(Msg::Snapshot { snapshot }) => {
                self.mirror = Some((**snapshot).clone());
                self.send(Up::Snapshot(snapshot.clone()));
            }
            Frame::Control(Msg::Delta { delta }) => {
                let applied = match self.mirror.as_mut() {
                    Some(m) => delta.apply(m).map(|()| m.clone()),
                    None => return Some(End::Lost { bye: None, why: "a change arrived before the snapshot".into() }),
                };
                match applied {
                    Ok(m) => self.send(Up::Snapshot(Box::new(m))),
                    Err(e) => return Some(End::Lost { bye: None, why: format!("could not follow the office's changes ({e:?})") }),
                }
            }
            Frame::Control(Msg::Event { event: Some(e) }) => self.send(Up::Event(e.clone())),
            Frame::Control(Msg::ConsentCard { request }) => self.send(Up::Event(WireEvent::ConsentRequested { request: request.clone() })),
            Frame::TermAttach { .. } | Frame::TermData { .. } | Frame::Control(Msg::TermResized { .. } | Msg::TermClosed { .. }) => {
                self.term_frame(conn, frame);
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
        let Some(s) = self.streams.get_mut(&stream) else { return };
        match s.follower.on_frame(frame) {
            Ok(TermUpdate::Attached(a)) => {
                let mut t = lock(&s.term);
                *t = new_terminal(a.cols, a.rows);
                t.advance(&a.vt);
                s.mode.store(t.mode().bits(), Ordering::Release);
                s.state.store(RUNNING, Ordering::Release);
            }
            Ok(TermUpdate::Data(bytes)) => {
                let mut t = lock(&s.term);
                t.advance(&bytes);
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
    fn a_lost_call_says_it_may_have_run() {
        let Outcome::Err { kind, message } = lost_outcome() else { panic!("an error") };
        assert_eq!(kind, ErrorKind::Failed);
        assert!(message.contains("may have run"));
    }
}
