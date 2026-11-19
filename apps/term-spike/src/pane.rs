//! A pane: one terminal + one PTY. The PTY is spawned on a background thread with the grid
//! size measured on the pane's first frame. PTY output is parsed on the PTY's reader thread;
//! the UI thread only ever `try_lock`s the terminal, so it never waits on a busy reader.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;

use csm_pty::{Backend, Pty, PtySize, SpawnOptions};
use csm_term::{GridSize, ProbeReply, ReplySink, TapEvent, TermConfig, TermEvent, Terminal};
use egui::{Context, Mesh, Rect, ViewportId};

/// Lock a std mutex, recovering from poisoning (a panicking thread must not wedge the UI).
pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    Grid,
    Floating,
    TornOff,
}

/// What the reader thread learned, for the header strip and the gate evidence.
#[derive(Debug, Default, Clone)]
pub struct PaneStatus {
    pub bytes: u64,
    pub title: String,
    pub program_status: Option<String>,
    pub probe_answered: Option<bool>,
    pub last_shell_integration: Option<String>,
    pub last_prompt_mark: Option<String>,
    pub cwd: Option<String>,
    pub exit: Option<Option<i32>>,
}

/// UI → terminal changes, applied when the UI next gets the terminal lock.
#[derive(Debug, Clone)]
pub enum Op {
    Resize(GridSize),
    Scroll(i32),
    ScrollBottom,
    SelStart { col: usize, row: usize, right_half: bool, clicks: u8 },
    SelUpdate { col: usize, row: usize, right_half: bool },
    SelClear,
    Copy,
}

pub struct RenderCache {
    pub mesh: Arc<Mesh>,
    pub key: (u64, u64, u32, bool, u64, u32, u32),
    pub cursor_rect: Rect,
}

struct PtyReplies(Arc<OnceLock<Arc<dyn Pty>>>);

impl ReplySink for PtyReplies {
    fn send(&self, bytes: &[u8]) {
        if let Some(pty) = self.0.get()
            && let Err(e) = pty.write(bytes)
        {
            log::debug!("reply write failed: {e}");
        }
    }
}

pub enum Started {
    Ok(Backend),
    Failed(String),
}

pub struct Pane {
    pub id: usize,
    pub name: String,
    pub placement: Placement,
    argv: Vec<OsString>,
    cwd: Option<PathBuf>,
    pub term: Option<Arc<Mutex<Terminal>>>,
    pub mode: Option<Arc<AtomicU32>>,
    pub pty: Arc<OnceLock<Arc<dyn Pty>>>,
    spawn_rx: Option<Receiver<Started>>,
    pub backend: Option<Backend>,
    pub failed: Option<String>,
    pub grid: Option<GridSize>,
    pub first_grid: Option<GridSize>,
    pty_size: Option<PtySize>,
    pub ops: Vec<Op>,
    pub cache: Option<RenderCache>,
    pub was_focused: bool,
    pub has_selection: bool,
    pub mouse_down: Option<csm_term::input::MouseButton>,
    pub drag_origin: Option<(usize, usize)>,
    pub last_click: Option<(Instant, usize, usize, u8)>,
    pub wheel_accum: f32,
    pub preedit: String,
    pub status: Arc<Mutex<PaneStatus>>,
    pub bytes: Arc<AtomicU64>,
    pub started_at: Option<Instant>,
}

impl Pane {
    pub fn new(id: usize, name: String, argv: Vec<OsString>, cwd: Option<PathBuf>, placement: Placement) -> Self {
        Self {
            id,
            name,
            placement,
            argv,
            cwd,
            term: None,
            mode: None,
            pty: Arc::new(OnceLock::new()),
            spawn_rx: None,
            backend: None,
            failed: None,
            grid: None,
            first_grid: None,
            pty_size: None,
            ops: Vec::new(),
            cache: None,
            was_focused: false,
            has_selection: false,
            mouse_down: None,
            drag_origin: None,
            last_click: None,
            wheel_accum: 0.0,
            preedit: String::new(),
            status: Arc::new(Mutex::new(PaneStatus::default())),
            bytes: Arc::new(AtomicU64::new(0)),
            started_at: None,
        }
    }

    pub fn pty(&self) -> Option<&Arc<dyn Pty>> {
        self.pty.get()
    }

    /// Writes app-authored bytes (keystrokes) to the PTY. Never blocks: the PTY queues.
    pub fn send(&self, bytes: &[u8]) {
        if let Some(pty) = self.pty.get()
            && let Err(e) = pty.write(bytes)
        {
            log::debug!("pane {} write failed: {e}", self.id);
        }
    }

    /// First frame with a measured grid: create the terminal at exactly that size and spawn the
    /// PTY with it on a background thread. There is no placeholder size anywhere.
    pub fn start(&mut self, grid: GridSize, ctx: &Context) {
        if self.term.is_some() {
            return;
        }
        log::info!("pane {} '{}': first measured grid {}x{} → creating PTY at that size", self.id, self.name, grid.cols, grid.rows);
        self.first_grid = Some(grid);
        self.grid = Some(grid);
        let replies: Arc<dyn ReplySink> = Arc::new(PtyReplies(self.pty.clone()));
        let terminal = Terminal::new(grid, TermConfig::default(), replies);
        self.mode = Some(terminal.shared_mode());
        let term = Arc::new(Mutex::new(terminal));
        self.term = Some(term.clone());
        let size = PtySize::new(grid.cols, grid.rows);
        self.pty_size = Some(size);
        self.started_at = Some(Instant::now());

        let mut opts = SpawnOptions::new(self.argv.clone(), size);
        opts.cwd = self.cwd.clone();
        opts.env.push(("TERM".into(), Some("xterm-256color".into())));
        opts.env.push(("COLORTERM".into(), Some("truecolor".into())));
        let (tx, rx) = channel();
        self.spawn_rx = Some(rx);
        let cell = self.pty.clone();
        let status = self.status.clone();
        let bytes_counter = self.bytes.clone();
        let ctx = ctx.clone();
        let id = self.id;
        let spawned = std::thread::Builder::new().name(format!("pane-{id}-spawn")).spawn(move || {
            let result = match csm_pty::spawn(opts) {
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
                    let sink_term = term.clone();
                    let sink_status = status.clone();
                    let sink_ctx = ctx.clone();
                    let spawned_at = Instant::now();
                    pty.subscribe(Box::new(move |bytes| {
                        if bytes_counter.load(Ordering::Relaxed) == 0 {
                            log::info!("pane {id}: first output ({} bytes) {} ms after spawn", bytes.len(), spawned_at.elapsed().as_millis());
                        }
                        // Parse in slices, releasing the lock between them so the UI's
                        // try_lock gets a turn even while output streams.
                        for piece in bytes.chunks(16 * 1024) {
                            let events = {
                                let mut t = lock(&sink_term);
                                t.advance(piece);
                                t.take_events()
                            };
                            if !events.is_empty() {
                                apply_events(id, &sink_status, events);
                            }
                        }
                        bytes_counter.fetch_add(bytes.len() as u64, Ordering::Relaxed);
                        sink_ctx.request_repaint_of(ViewportId::ROOT);
                    }));
                    let exit_status = status.clone();
                    let exit_ctx = ctx.clone();
                    pty.on_exit(Box::new(move |code| {
                        log::info!("pane {id}: process exited with {code:?}");
                        lock(&exit_status).exit = Some(code);
                        exit_ctx.request_repaint_of(ViewportId::ROOT);
                    }));
                    Started::Ok(backend)
                }
                Err(e) => {
                    log::warn!("pane {id}: spawn failed: {e}");
                    Started::Failed(e.to_string())
                }
            };
            let _ = tx.send(result);
            ctx.request_repaint_of(ViewportId::ROOT);
        });
        if let Err(e) = spawned {
            self.failed = Some(format!("could not start spawn thread: {e}"));
        }
    }

    /// Per frame: pick up the spawn result; keep the PTY size equal to the grid.
    pub fn poll(&mut self) {
        if let Some(rx) = &self.spawn_rx {
            match rx.try_recv() {
                Ok(Started::Ok(backend)) => {
                    self.backend = Some(backend);
                    self.spawn_rx = None;
                    if let Some(t) = self.started_at {
                        log::info!("pane {} running on {} ({} ms to spawn)", self.id, backend.label(), t.elapsed().as_millis());
                    }
                }
                Ok(Started::Failed(e)) => {
                    self.failed = Some(e);
                    self.spawn_rx = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => self.spawn_rx = None,
            }
        }
        if let (Some(grid), Some(pty)) = (self.grid, self.pty.get()) {
            let want = PtySize::new(grid.cols, grid.rows);
            if self.pty_size != Some(want) {
                if let Err(e) = pty.resize(want) {
                    log::warn!("pane {} resize failed: {e}", self.id);
                }
                log::debug!("pane {} PTY resized to {}x{}", self.id, want.cols, want.rows);
                self.pty_size = Some(want);
            }
        }
    }

    /// The grid changed (layout, window resize, DPI): the terminal and the PTY both follow.
    pub fn set_grid(&mut self, grid: GridSize) {
        if self.grid != Some(grid) {
            self.grid = Some(grid);
            self.ops.push(Op::Resize(grid));
        }
    }
}

fn apply_events(id: usize, status: &Mutex<PaneStatus>, events: Vec<TermEvent>) {
    let mut s = lock(status);
    for e in events {
        match e {
            TermEvent::Tap(TapEvent::ProgramStatus(p)) => {
                log::info!("pane {id}: OSC 7501 report {p}");
                s.program_status = Some(p);
            }
            TermEvent::Tap(TapEvent::ProgramStatusProbe) => {}
            TermEvent::ProbeAnswered { answered } => s.probe_answered = Some(answered),
            TermEvent::Tap(TapEvent::ShellIntegration(fields)) => {
                let text = fields.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join(" ");
                log::info!("pane {id}: OSC 9001 {text}");
                s.last_shell_integration = Some(text);
            }
            TermEvent::Tap(TapEvent::Prompt(mark)) => s.last_prompt_mark = Some(format!("{mark:?}")),
            TermEvent::Tap(TapEvent::Cwd(uri)) => s.cwd = Some(uri),
            TermEvent::Title(t) => s.title = t,
            TermEvent::Bell | TermEvent::ClipboardStore(_) => {}
        }
    }
}
