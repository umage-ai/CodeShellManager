//! A live terminal: one `csm-term` emulator fed by one `csm-pty` process. The process is spawned
//! on a background thread with the grid the view measured on its first frame. PTY output is
//! parsed on the PTY's reader thread; the UI only ever `try_lock`s the emulator.
//!
//! Moved from the Phase 0 spike (`apps/term-spike/src/pane.rs`); the widget state that lived in
//! the same struct there is now [`TerminalView`](super::TerminalView).

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use csm_pty::{Backend, Pty, PtySize, SpawnOptions};
use csm_term::alacritty_terminal::term::TermMode;
use csm_term::{GridSize, Palette, ProbeReply, ReplySink, TapEvent, TermConfig, TermEvent, Terminal, load_mode};

use super::resize::Coalescer;
use super::source::{SourceState, TerminalSource, lock};

/// Wakes the UI (an `egui::Context::request_repaint` in the app). Called from PTY threads.
pub type Repaint = Arc<dyn Fn() + Send + Sync>;

/// What the reader thread learned from the program (title, OSC 7501, OSC 9001, exit).
#[derive(Debug, Default, Clone)]
pub struct TermStatus {
    pub bytes: u64,
    pub title: String,
    pub program_status: Option<String>,
    pub probe_answered: Option<bool>,
    pub last_shell_integration: Option<String>,
    pub last_prompt_mark: Option<String>,
    pub cwd: Option<String>,
    pub exit: Option<Option<i32>>,
    /// The program's last OSC 52 copy (#156), waiting for the host to take it.
    pub clipboard: Option<String>,
}

struct PtyReplies(Arc<OnceLock<Arc<dyn Pty>>>);

impl ReplySink for PtyReplies {
    fn send(&self, bytes: &[u8]) {
        if let Some(pty) = self.0.get()
            && let Err(e) = pty.write_reply(bytes)
        {
            log::debug!("reply write failed: {e}");
        }
    }
}

#[derive(Default)]
struct Inner {
    term: Option<Arc<Mutex<Terminal>>>,
    first_grid: Option<GridSize>,
    pty_size: Option<PtySize>,
    backend: Option<Backend>,
    failed: Option<String>,
}

pub struct LocalTerminal {
    name: String,
    argv: Vec<OsString>,
    cwd: Option<PathBuf>,
    palette: Palette,
    repaint: Repaint,
    inner: Mutex<Inner>,
    mode: Arc<AtomicU32>,
    pty: Arc<OnceLock<Arc<dyn Pty>>>,
    status: Arc<Mutex<TermStatus>>,
    bytes: Arc<AtomicU64>,
    spawn_error: Arc<OnceLock<String>>,
    /// Applies PTY resizes off the UI thread (made with the first one).
    resizer: OnceLock<Coalescer<PtySize>>,
}

impl LocalTerminal {
    /// Nothing starts until the view reports its first measured grid.
    pub fn new(name: impl Into<String>, argv: Vec<OsString>, cwd: Option<PathBuf>, palette: Palette, repaint: Repaint) -> Self {
        Self {
            name: name.into(),
            argv,
            cwd,
            palette,
            repaint,
            inner: Mutex::new(Inner::default()),
            mode: Arc::new(AtomicU32::new(TermMode::default().bits())),
            pty: Arc::new(OnceLock::new()),
            status: Arc::new(Mutex::new(TermStatus::default())),
            bytes: Arc::new(AtomicU64::new(0)),
            spawn_error: Arc::new(OnceLock::new()),
            resizer: OnceLock::new(),
        }
    }

    pub fn status(&self) -> TermStatus {
        let mut s = lock(&self.status).clone();
        s.bytes = self.bytes.load(Ordering::Relaxed);
        s
    }

    /// The grid the PTY was created with (the first measured grid).
    pub fn first_grid(&self) -> Option<GridSize> {
        lock(&self.inner).first_grid
    }

    pub fn backend(&self) -> Option<Backend> {
        if let Some(pty) = self.pty.get() {
            return Some(pty.backend());
        }
        lock(&self.inner).backend
    }

    pub fn pty(&self) -> Option<&Arc<dyn Pty>> {
        self.pty.get()
    }

    /// Bytes read from the PTY so far.
    pub fn bytes_read(&self) -> u64 {
        self.bytes.load(Ordering::Relaxed)
    }

    /// Ends the process tree (the pane is closing or the session sleeps).
    pub fn kill(&self) {
        if let Some(p) = self.pty.get() {
            p.kill();
        }
    }

    fn start(&self, inner: &mut Inner, grid: GridSize) {
        log::info!("terminal '{}': first measured grid {}x{} → creating the PTY at that size", self.name, grid.cols, grid.rows);
        inner.first_grid = Some(grid);
        let replies: Arc<dyn ReplySink> = Arc::new(PtyReplies(self.pty.clone()));
        let config = TermConfig { palette: self.palette.clone(), ..TermConfig::default() };
        let terminal = Terminal::new(grid, config, replies);
        let shared_mode = terminal.shared_mode();
        let term = Arc::new(Mutex::new(terminal));
        inner.term = Some(term.clone());
        let size = PtySize::new(grid.cols, grid.rows);
        inner.pty_size = Some(size);
        // Mirror the emulator's live mode bits into ours (the view reads `mode()` lock-free).
        let mode = self.mode.clone();
        mode.store(shared_mode.load(Ordering::Relaxed), Ordering::Relaxed);

        let mut opts = SpawnOptions::new(self.argv.clone(), size);
        opts.cwd = self.cwd.clone();
        opts.env.push(("TERM".into(), Some("xterm-256color".into())));
        opts.env.push(("COLORTERM".into(), Some("truecolor".into())));
        let cell = self.pty.clone();
        let status = self.status.clone();
        let bytes_counter = self.bytes.clone();
        let repaint = self.repaint.clone();
        let spawn_error = self.spawn_error.clone();
        let name = self.name.clone();
        let spawned = std::thread::Builder::new().name(format!("term-{name}-spawn")).spawn(move || {
            let started = Instant::now();
            match csm_pty::spawn(opts) {
                Ok(pty) => {
                    let backend = pty.backend();
                    // Answer the 7501 probe where the reply can arrive in order: on the bundled
                    // ConPTY (OSC input passes through, DA1 is forwarded) and on Unix PTYs.
                    let probe = match backend {
                        Backend::BundledConPty | Backend::Unix => ProbeReply::Raw,
                        Backend::InboxConPty => ProbeReply::Off,
                    };
                    lock(&term).set_probe_reply(probe);
                    let _ = cell.set(pty.clone());
                    log::info!("terminal '{name}' running on {} ({} ms to spawn)", backend.label(), started.elapsed().as_millis());
                    let sink_term = term.clone();
                    let sink_status = status.clone();
                    let sink_repaint = repaint.clone();
                    let sink_mode = shared_mode.clone();
                    pty.subscribe(Box::new(move |bytes| {
                        // Parse in slices, releasing the lock between them so the UI's try_lock
                        // gets a turn even while output streams.
                        for piece in bytes.chunks(16 * 1024) {
                            let events = {
                                let mut t = lock(&sink_term);
                                t.advance(piece);
                                t.take_events()
                            };
                            if !events.is_empty() {
                                apply_events(&sink_status, events);
                            }
                        }
                        mode.store(sink_mode.load(Ordering::Relaxed), Ordering::Relaxed);
                        bytes_counter.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        sink_repaint();
                    }));
                    let exit_status = status.clone();
                    let exit_repaint = repaint.clone();
                    pty.on_exit(Box::new(move |code| {
                        lock(&exit_status).exit = Some(code);
                        exit_repaint();
                    }));
                }
                Err(e) => {
                    log::warn!("terminal '{name}': spawn failed: {e}");
                    let _ = spawn_error.set(e.to_string());
                }
            }
            repaint();
        });
        if let Err(e) = spawned {
            inner.failed = Some(format!("could not start the spawn thread: {e}"));
        }
    }
}

impl TerminalSource for LocalTerminal {
    fn terminal(&self) -> Option<Arc<Mutex<Terminal>>> {
        lock(&self.inner).term.clone()
    }

    fn mode(&self) -> TermMode {
        load_mode(&self.mode)
    }

    fn set_grid(&self, grid: GridSize) {
        let mut inner = lock(&self.inner);
        let Some(term) = inner.term.clone() else {
            self.start(&mut inner, grid);
            return;
        };
        // The emulator follows when its lock is free; the view calls this every frame, so a
        // busy reader only delays the resize by a frame.
        if let Ok(mut t) = term.try_lock()
            && t.size() != grid
        {
            t.resize(grid);
        }
        if let Some(pty) = self.pty.get() {
            let want = PtySize::new(grid.cols, grid.rows);
            if inner.pty_size != Some(want) {
                // The ConPTY call can block: a worker applies the latest size (never the UI thread).
                let resizer = self.resizer.get_or_init(|| {
                    let (pty, name) = (pty.clone(), self.name.clone());
                    Coalescer::spawn("csm-pty-resize", move |size: PtySize| {
                        if let Err(e) = pty.resize(size) {
                            log::warn!("terminal '{name}': resize failed: {e}");
                        }
                    })
                });
                resizer.submit(want);
                inner.pty_size = Some(want);
            }
        }
    }

    fn write(&self, bytes: &[u8]) {
        if let Some(pty) = self.pty.get()
            && let Err(e) = pty.write_reply(bytes)
        {
            log::debug!("terminal '{}': write failed: {e}", self.name);
        }
    }

    fn state(&self) -> SourceState {
        let inner = lock(&self.inner);
        if let Some(f) = &inner.failed {
            return SourceState::Failed(f.clone());
        }
        if let Some(e) = self.spawn_error.get() {
            return SourceState::Failed(e.clone());
        }
        let status = lock(&self.status);
        if let Some(code) = status.exit {
            return SourceState::Exited(code);
        }
        if self.pty.get().is_some() { SourceState::Running } else { SourceState::Starting }
    }

    fn take_clipboard(&self) -> Option<String> {
        lock(&self.status).clipboard.take()
    }
}

impl Drop for LocalTerminal {
    fn drop(&mut self) {
        self.kill();
    }
}

fn apply_events(status: &Mutex<TermStatus>, events: Vec<TermEvent>) {
    let mut s = lock(status);
    for e in events {
        match e {
            TermEvent::Tap(TapEvent::ProgramStatus(p)) => s.program_status = Some(p),
            TermEvent::Tap(TapEvent::ProgramStatusProbe) => {}
            TermEvent::ProbeAnswered { answered } => s.probe_answered = Some(answered),
            TermEvent::Tap(TapEvent::ShellIntegration(fields)) => {
                let text = fields.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
                s.last_shell_integration = Some(text);
            }
            TermEvent::Tap(TapEvent::Prompt(mark)) => s.last_prompt_mark = Some(format!("{mark:?}")),
            TermEvent::Tap(TapEvent::Cwd(uri)) => s.cwd = Some(uri),
            TermEvent::Title(t) => s.title = t,
            TermEvent::ClipboardStore(text) => s.clipboard = Some(text),
            TermEvent::Bell => {}
        }
    }
}
