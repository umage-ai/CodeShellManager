//! A live session's runtime: the terminal the UI renders, its PTY, and what its output told us.
//!
//! Threads:
//! - the **PTY reader** feeds bytes into the [`Terminal`] (under its mutex), stamps the last
//!   output time, feeds the alert heuristic, and posts the OSC events it saw to the engine;
//! - the **UI** reads the terminal through a [`TerminalHandle`] and writes keystrokes and
//!   resizes straight to the PTY (widget-level input is not a tool);
//! - the **engine** owns [`SessionRt`]: status, program-status records, phase.
//!
//! The first PTY size is the UI's first measured grid (no 80x24 placeholder, the spike's rule):
//! the PTY is spawned when the handle first reports a size, or at the fallback size after
//! `first_size_timeout` when no pane is showing it.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError, Weak};
use std::time::Instant;

use csm_core::alert_detector::AlertDetector;
use csm_core::models::AlertType;
use csm_core::program_status_records::ProgramStatusRecords;
use csm_pty::{Backend, Pty, PtySize};
use csm_term::alacritty_terminal::term::TermMode;
use csm_term::input::{self, AppAuthored, Untrusted};
use csm_term::{GridSize, ReplySink, TermConfig, TermEvent, Terminal};

use crate::engine::{Ctx, Engine};
use crate::output_tap::OutputTap;

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)).unwrap_or(0)
}

/// Where a session's process is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Phase {
    /// No process yet (waiting for a size, the stagger, or the spawn worker).
    Starting,
    Live,
    Exited {
        code: Option<i32>,
    },
}

/// Runs once the PTY started (`Ok`) or failed to (`Err`). Restart chains continue from here.
pub(crate) type StartedCb = Box<dyn FnOnce(&mut Ctx<'_>, Result<(), String>) + Send>;

/// The engine's view of one live session.
pub(crate) struct SessionRt {
    pub io: Arc<SessionIo>,
    pub phase: Phase,
    pub backend: Option<Backend>,
    pub records: ProgramStatusRecords,
    /// The program has sent a valid OSC 7501 report (the heuristic stands down).
    pub has_program_status: bool,
    /// The screen heuristic's current alert (only when no program status).
    pub heuristic: Option<AlertType>,
    pub heuristic_message: Option<String>,
    /// The status a profile's status rules read from the screen (`addons.rs`), when they match.
    pub addon_status: Option<csm_core::addons::ProfileStatus>,
    /// When the screen was last read for those rules (ms since the epoch).
    pub addon_checked_ms: i64,
    /// A screen read for those rules is still on its worker: no second one starts meanwhile.
    pub addon_busy: Arc<AtomicBool>,
    /// Pushed by OSC 9001 (`git-branch`, `git-dirty`); display only.
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    /// The program pushed git state with OSC 9001: the git poll stands down for this run (the
    /// pushed value describes a remote or overlay repo the local folder can't see).
    pub git_osc: bool,
    /// The terminal title (OSC 0/2).
    pub title: Option<String>,
    /// OSC 7 working directory, raw.
    pub cwd: Option<String>,
    /// Launched by restore, wake or restart (a Claude session resumes its conversation).
    pub restoring: bool,
    /// When the PTY starts at the fallback size if no pane reported one.
    pub start_deadline: Instant,
    pub on_started: Vec<StartedCb>,
    /// Usage stats: the command this run counts under and when its process started; taken
    /// (and its running time recorded) once, when the process ends or the runtime goes.
    pub usage: Option<(String, Instant)>,
    /// When its process started (epoch ms; 0 until then): transcript lines older than this are
    /// from an earlier run of the conversation.
    pub started_ms: i64,
    /// Claude Code Remote Control is on (`csm_core::remote_control`), from its transcript.
    pub remote_control: Option<csm_core::remote_control::RemoteControl>,
}

impl SessionRt {
    pub(crate) fn new(io: Arc<SessionIo>, restoring: bool, start_deadline: Instant) -> Self {
        Self {
            io,
            phase: Phase::Starting,
            backend: None,
            records: ProgramStatusRecords::new(),
            has_program_status: false,
            heuristic: None,
            heuristic_message: None,
            addon_status: None,
            addon_checked_ms: 0,
            addon_busy: Arc::new(AtomicBool::new(false)),
            git_branch: None,
            git_dirty: false,
            git_osc: false,
            title: None,
            cwd: None,
            restoring,
            start_deadline,
            on_started: Vec::new(),
            usage: None,
            started_ms: 0,
            remote_control: None,
        }
    }

    /// Records this run's running time in the usage stats, once.
    pub(crate) fn end_usage(&mut self, index: Option<&csm_index::Index>) {
        if let (Some((command, since)), Some(ix)) = (self.usage.take(), index) {
            ix.record_usage_seconds(&command, i64::try_from(since.elapsed().as_secs()).unwrap_or(i64::MAX));
        }
    }

    /// The person saw the session (a real keystroke, or focus): unseen results retire and the
    /// heuristic alert clears. Live states (working, blocked) stay: the program reports those.
    pub(crate) fn acknowledge(&mut self) -> bool {
        let mut changed = self.records.acknowledge();
        if self.heuristic.take().is_some() {
            changed = true;
        }
        self.heuristic_message = None;
        self.io.detector().notify_user_interacted();
        changed
    }

    /// The program no longer holds any OSC 7501 record: the heuristic is the source again.
    pub(crate) fn rearm(&mut self) {
        self.has_program_status = false;
        self.io.detector().rearm();
        self.io.detector_active.store(true, Ordering::Release);
    }

    pub(crate) fn stand_down(&mut self) {
        self.has_program_status = true;
        self.heuristic = None;
        self.heuristic_message = None;
        self.io.detector().stand_down();
        self.io.detector_active.store(false, Ordering::Release);
    }
}

/// The terminal's replies (DA1, the 7501 probe reply…) go to the PTY once it exists.
struct Replies(Arc<OnceLock<Arc<dyn Pty>>>);

impl ReplySink for Replies {
    fn send(&self, bytes: &[u8]) {
        if let Some(p) = self.0.get() {
            let _ = p.write(bytes);
        }
    }
}

/// State shared by the engine, the PTY reader and the UI's [`TerminalHandle`].
pub(crate) struct SessionIo {
    pub id: String,
    /// Distinguishes this run from a later restart of the same session id.
    pub instance: u64,
    pub terminal: Arc<Mutex<Terminal>>,
    mode: Arc<AtomicU32>,
    generation: AtomicU64,
    pub pty: Arc<OnceLock<Arc<dyn Pty>>>,
    /// The first size the UI measured (or the fallback); the PTY is created with it.
    pub first_size: Mutex<Option<GridSize>>,
    pub spawn_requested: AtomicBool,
    /// Set when the session is torn down: a spawn in flight kills what it started.
    pub closed: AtomicBool,
    /// Epoch ms of the last output; 0 = none yet (busy rules).
    pub last_output_ms: AtomicI64,
    detector: Mutex<AlertDetector>,
    pub detector_active: AtomicBool,
    /// A keystroke arrived and the engine has not been told yet.
    interacted: AtomicBool,
    engine: Engine,
    /// Where this session's output is indexed for search, when the run has an index.
    index: Option<csm_index::Index>,
    /// The session's output ring for subscribers and the byte-rate counters.
    pub output: Arc<OutputTap>,
    /// The output offset the emulator has consumed: written under the terminal lock right after
    /// `advance`, so a grid read under that lock and this offset describe the same moment.
    term_offset: AtomicU64,
    /// Every size change of this session's terminal, with the output offset it takes effect at.
    resizes: Mutex<ResizeLog>,
}

impl SessionIo {
    pub(crate) fn new(id: &str, name: &str, instance: u64, size: GridSize, engine: Engine, index: Option<csm_index::Index>) -> Arc<Self> {
        let cell: Arc<OnceLock<Arc<dyn Pty>>> = Arc::new(OnceLock::new());
        let term = Terminal::new(size, TermConfig::default(), Arc::new(Replies(cell.clone())));
        let mode = term.shared_mode();
        Arc::new(Self {
            id: id.to_owned(),
            instance,
            terminal: Arc::new(Mutex::new(term)),
            mode,
            generation: AtomicU64::new(1),
            pty: cell,
            first_size: Mutex::new(None),
            spawn_requested: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            last_output_ms: AtomicI64::new(0),
            detector: Mutex::new(AlertDetector::new(id, name)),
            detector_active: AtomicBool::new(true),
            interacted: AtomicBool::new(false),
            engine,
            index,
            output: Arc::new(OutputTap::new()),
            term_offset: AtomicU64::new(0),
            resizes: Mutex::new(ResizeLog::default()),
        })
    }

    pub(crate) fn detector(&self) -> std::sync::MutexGuard<'_, AlertDetector> {
        self.detector.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn lock_terminal(&self) -> std::sync::MutexGuard<'_, Terminal> {
        self.terminal.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn mode(&self) -> TermMode {
        csm_term::load_mode(&self.mode)
    }

    pub(crate) fn bump(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// Asks the engine to start the PTY (once).
    pub(crate) fn request_spawn(&self) {
        if !self.spawn_requested.swap(true, Ordering::AcqRel) {
            let (id, instance) = (self.id.clone(), self.instance);
            self.engine.post(Box::new(move |ctx| crate::lifecycle::start_spawn(ctx, &id, instance)));
        }
    }

    pub(crate) fn last_output(&self) -> Option<i64> {
        Some(self.last_output_ms.load(Ordering::Acquire)).filter(|v| *v > 0)
    }

    /// Writes app-authored or already-encoded bytes to the PTY (no-op before it started).
    pub(crate) fn write(&self, bytes: &[u8]) -> bool {
        match self.pty.get() {
            Some(p) => p.write(bytes).is_ok(),
            None => false,
        }
    }

    fn note_interaction(&self) {
        if !self.interacted.swap(true, Ordering::AcqRel) {
            let (id, instance) = (self.id.clone(), self.instance);
            self.engine.post(Box::new(move |ctx| crate::lifecycle::user_interacted(ctx, &id, instance)));
        }
    }

    pub(crate) fn clear_interaction(&self) {
        self.interacted.store(false, Ordering::Release);
    }

    /// The PTY reader's sink: feed the terminal, then tell the engine what the OSC tap saw.
    pub(crate) fn sink(self: &Arc<Self>) -> csm_pty::OutputSink {
        let weak: Weak<SessionIo> = Arc::downgrade(self);
        Box::new(move |bytes: &[u8]| {
            let Some(io) = weak.upgrade() else { return };
            let now = now_ms();
            io.last_output_ms.store(now, Ordering::Release);
            if let Some(ix) = &io.index {
                // Non-blocking: queues the raw bytes for the index thread (drops and counts on overflow).
                ix.feed(&io.id, bytes);
            }
            // Non-blocking too: the ring drops its oldest bytes, and no subscriber is waited for.
            let end = io.output.feed(bytes);
            let events = {
                let mut t = io.lock_terminal();
                t.advance(bytes);
                io.term_offset.store(end, Ordering::Release);
                t.take_events()
            };
            io.bump();
            if io.detector_active.load(Ordering::Acquire) {
                io.detector().feed(&String::from_utf8_lossy(bytes), now);
            }
            // OSC 52 copies (#156) come already size-capped by `csm_term`.
            let events: Vec<TermEvent> =
                events.into_iter().filter(|e| matches!(e, TermEvent::Tap(_) | TermEvent::Title(_) | TermEvent::ClipboardStore(_))).collect();
            if !events.is_empty() {
                let (id, instance) = (io.id.clone(), io.instance);
                io.engine.post(Box::new(move |ctx| crate::lifecycle::apply_term_events(ctx, &id, instance, events)));
            }
        })
    }
}

/// How many size changes a session remembers for stream resume.
const RESIZE_LOG_CAP: usize = 64;

/// One size change of a terminal: from output offset `offset` on, the grid is `cols` x `rows`.
/// `seq` counts the changes of this run, from 1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResizeMark {
    pub seq: u64,
    pub offset: u64,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Default)]
struct ResizeLog {
    marks: std::collections::VecDeque<ResizeMark>,
    last_seq: u64,
    /// The largest offset of a mark that was dropped to stay within the cap: a stream cannot
    /// resume at or before it (the change it needs is gone).
    evicted_through: Option<u64>,
}

impl ResizeLog {
    fn push(&mut self, offset: u64, cols: u16, rows: u16) {
        self.last_seq = self.last_seq.saturating_add(1);
        self.marks.push_back(ResizeMark { seq: self.last_seq, offset, cols, rows });
        while self.marks.len() > RESIZE_LOG_CAP {
            if let Some(old) = self.marks.pop_front() {
                self.evicted_through = Some(self.evicted_through.map_or(old.offset, |e| e.max(old.offset)));
            }
        }
    }
}

/// A terminal's screen as a byte stream for a fresh emulator, with where the live stream goes on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachSnapshot {
    /// The run this belongs to (a restart of the session has another).
    pub instance: u64,
    pub cols: u16,
    pub rows: u16,
    /// The output offset the grid describes: live bytes continue here.
    pub offset: u64,
    /// Resize marks with a `seq` after this one come after the grid's offset.
    pub resize_seq: u64,
    pub vt: Vec<u8>,
}

/// What the UI holds for one live session: the terminal to render, and the widget-level input
/// path (keys, mouse, paste, focus reports, resize). Cheap to clone; never blocks on the engine.
/// Programmatic input is the `session_send_text` tool, not this.
#[derive(Clone)]
pub struct TerminalHandle(pub(crate) Arc<SessionIo>);

impl TerminalHandle {
    pub fn session_id(&self) -> &str {
        &self.0.id
    }

    /// The emulator. Lock it to render or select; never hold it across frames.
    pub fn terminal(&self) -> &Arc<Mutex<Terminal>> {
        &self.0.terminal
    }

    /// Changes whenever the visible content may have changed (output, resize). Lock-free.
    pub fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Acquire)
    }

    /// The terminal modes (bracketed paste, mouse reporting…), lock-free.
    pub fn mode(&self) -> TermMode {
        self.0.mode()
    }

    /// The PTY exists (the first size was reported or the fallback elapsed).
    pub fn is_started(&self) -> bool {
        self.0.pty.get().is_some()
    }

    pub fn has_exited(&self) -> bool {
        self.0.pty.get().is_some_and(|p| p.has_exited())
    }

    pub fn backend(&self) -> Option<Backend> {
        self.0.pty.get().map(|p| p.backend())
    }

    // ---- the peer server's view of a terminal (design 3 and 5) --------------------------------

    /// Which run of the session this is; a restart has another.
    pub fn instance(&self) -> u64 {
        self.0.instance
    }

    /// The session was torn down (closed or removed).
    pub fn is_closed(&self) -> bool {
        self.0.closed.load(Ordering::Acquire)
    }

    /// A reader of the session's output ring, from `from` (or the current end).
    pub fn subscribe_output(&self, from: Option<u64>) -> crate::output_tap::Subscription {
        self.0.output.subscribe(from)
    }

    /// The offset just past the last byte the PTY produced.
    pub fn output_end(&self) -> u64 {
        self.0.output.fed()
    }

    /// The offset just past the last byte the emulator has consumed (at most [`output_end`]).
    /// A stream sends only bytes below it, so a client never runs ahead of the server's emulator
    /// and a resize lands at the same byte on both.
    ///
    /// [`output_end`]: Self::output_end
    pub fn processed_offset(&self) -> u64 {
        self.0.term_offset.load(Ordering::Acquire)
    }

    /// The emulator's size now.
    pub fn grid_size(&self) -> GridSize {
        self.0.lock_terminal().size()
    }

    /// Whether a stream that has everything up to `offset` of `instance` can continue from there:
    /// same run, the ring still holds the bytes after it, and every resize it needs is remembered.
    pub fn can_resume(&self, instance: u64, offset: u64) -> bool {
        if instance != self.0.instance || !self.0.output.holds(offset) {
            return false;
        }
        let log = self.0.resizes.lock().unwrap_or_else(PoisonError::into_inner);
        log.evicted_through.is_none_or(|e| e < offset)
    }

    /// The resize marks a stream resuming at `offset` has still to be told (those at or after it,
    /// which are replayed even if it already had one at exactly `offset`: applying a size twice is
    /// harmless), and the `seq` to continue after.
    pub fn resizes_for_resume(&self, offset: u64) -> u64 {
        let log = self.0.resizes.lock().unwrap_or_else(PoisonError::into_inner);
        log.marks.iter().filter(|m| m.offset < offset).map(|m| m.seq).max().unwrap_or(0)
    }

    /// The resize marks after `seq`, oldest first.
    pub fn resizes_after(&self, seq: u64) -> Vec<ResizeMark> {
        let log = self.0.resizes.lock().unwrap_or_else(PoisonError::into_inner);
        log.marks.iter().filter(|m| m.seq > seq).copied().collect()
    }

    /// The screen (and `history` lines of scrollback) as VT bytes, with the offset it describes.
    /// Taken under the terminal lock, so the grid and the offset are one moment. A synchronized
    /// update that is still open is flushed first (the grid would otherwise lack bytes the offset
    /// already counts): the office pane may show that frame a moment early.
    pub fn attach_snapshot(&self, history: usize) -> AttachSnapshot {
        let mut t = self.0.lock_terminal();
        t.flush_sync_if_due(Instant::now() + std::time::Duration::from_secs(3600));
        let size = t.size();
        let vt = csm_term::replay::serialize(&mut t, history);
        let offset = self.0.term_offset.load(Ordering::Acquire);
        let resize_seq = self.0.resizes.lock().unwrap_or_else(PoisonError::into_inner).last_seq;
        drop(t);
        AttachSnapshot { instance: self.0.instance, cols: size.cols, rows: size.rows, offset, resize_seq, vt }
    }

    /// Resizes the terminal and the PTY to `cols` x `rows` cells (a remote viewer that drives the
    /// size). Keeps the pixel size of a cell. Does nothing when the size is already that.
    pub fn resize_cells(&self, cols: u16, rows: u16) {
        let cur = self.grid_size();
        let want = GridSize { cols: cols.max(2), rows: rows.max(1), cell_width: cur.cell_width, cell_height: cur.cell_height };
        if want != cur {
            self.resize(want);
        }
    }

    /// The pane's measured grid. The first call starts the PTY at exactly this size; later calls
    /// resize the terminal and the PTY.
    pub fn resize(&self, size: GridSize) {
        {
            let mut t = self.0.lock_terminal();
            let changed = t.size() != size;
            t.resize(size);
            if changed {
                // Under the terminal lock: the offset is the one the grid has reached.
                let at = self.0.term_offset.load(Ordering::Acquire);
                self.0.resizes.lock().unwrap_or_else(PoisonError::into_inner).push(at, size.cols, size.rows);
            }
        }
        self.0.bump();
        match self.0.pty.get() {
            Some(p) => {
                let _ = p.resize(PtySize::new(size.cols, size.rows));
            }
            None => {
                let mut first = self.0.first_size.lock().unwrap_or_else(PoisonError::into_inner);
                if first.is_none() {
                    *first = Some(size);
                }
                drop(first);
                self.0.request_spawn();
            }
        }
    }

    /// A keystroke (or IME commit) from the person. Acknowledges unseen results.
    pub fn send_key(&self, bytes: AppAuthored) {
        self.0.note_interaction();
        self.scroll_to_bottom();
        self.0.write(&bytes.0);
    }

    /// Mouse reports and other encoded input that is not the person "answering" the session.
    pub fn send_mouse(&self, bytes: AppAuthored) {
        self.0.write(&bytes.0);
    }

    /// A paste from the clipboard or a drop: untrusted text, bracketed when the program asked.
    pub fn paste(&self, text: &str) {
        self.0.note_interaction();
        self.scroll_to_bottom();
        let bytes = input::paste(Untrusted(text), self.mode());
        self.0.write(&bytes);
    }

    /// Focus in/out reports, when the program asked for them.
    pub fn focus_changed(&self, focused: bool) {
        if let Some(b) = input::focus(focused, self.mode()) {
            self.0.write(&b.0);
        }
    }

    fn scroll_to_bottom(&self) {
        // try_lock: never wait for the reader thread on the UI thread for a cosmetic scroll.
        if let Ok(mut t) = self.0.terminal.try_lock() {
            t.scroll_to_bottom();
        }
    }
}

impl std::fmt::Debug for TerminalHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TerminalHandle").field("session_id", &self.0.id).finish()
    }
}

impl PartialEq for TerminalHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
