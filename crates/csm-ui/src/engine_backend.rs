//! The real backend: the screens draw `engine.view()` mapped to a [`UiModel`], panes draw the
//! engine's [`TerminalHandle`]s, and every action is a tool call the engine applies itself.
//!
//! - [`EngineBackend::model`] maps the engine's [`Snapshot`] (rebuilt only when its `version`
//!   changes) plus the few things the snapshot doesn't carry: the theme and launch commands (read
//!   from the state at start-up, then followed from `settings_set` replies), the WPF import offer,
//!   and the shutdown board, which is built from `QuitProgress` events.
//! - [`EngineTerminal`] adapts a [`TerminalHandle`] to [`TerminalSource`]: the first measured grid
//!   starts the PTY at exactly that size (the engine's rule), later ones resize it only when they
//!   change, keystrokes go through `send_key` (which acknowledges unseen results), mouse reports
//!   through `send_mouse`, clipboard text through `paste`.
//! - A waker thread turns engine events, snapshot republishes and terminal output into repaints,
//!   so nothing polls on the UI thread and an idle window doesn't redraw.
//!
//! Status: the engine's nine states map to the UI's vocabulary one to one, except `exited`, which
//! is shown as Idle with "Exited · code N" as its word (`SessionView::exited`), everywhere the
//! status word appears (sidebar, pane header, palette, inspector, busy dialog).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_core::models::{ALL_LAYOUT_KEY, GroupLayout, InspectorTab as CoreTab, LayoutMode as CoreMode};
use csm_engine::{Effects, Engine, Event, GroupView as EngineGroupView, Reply, Snapshot, Status as EngineStatus, TerminalHandle};
use csm_term::alacritty_terminal::term::TermMode;
use csm_term::input::AppAuthored;
use csm_term::{GridSize, Terminal};

use crate::backend::{Backend, Call, Notice};
use crate::model::*;
use crate::terminal::{SourceState, TerminalSource, lock};
use crate::theme::ThemeKind;

/// What the app knows before the engine starts (read from the state and the disk at start-up,
/// never in a frame).
#[derive(Debug, Clone, Default)]
pub struct EngineOptions {
    pub theme: ThemeKind,
    /// The quick-open's commands (`AppSettings::launch_commands`).
    pub launch_commands: Vec<String>,
    /// Extra folders the quick-open offers (home, the current folder).
    pub folders: Vec<String>,
    /// The WPF app's `state.json`, when it exists: offered on the empty screen.
    pub wpf_import: Option<String>,
    /// `StateOptions::shutdown_budget`, for the board's budget bar.
    pub shutdown_budget_s: f32,
}

/// The settings value of a theme (`AppSettings::theme`: `dark` / `light`).
pub fn theme_setting(kind: ThemeKind) -> &'static str {
    match kind {
        ThemeKind::Mocha => "dark",
        ThemeKind::Latte => "light",
    }
}

/// `dark`/`mocha` → Mocha, `light`/`latte` → Latte; anything else is the default (dark).
pub fn theme_from_setting(v: &str) -> ThemeKind {
    match v.trim().to_ascii_lowercase().as_str() {
        "light" | "latte" => ThemeKind::Latte,
        _ => ThemeKind::Mocha,
    }
}

/// The shutdown board while `app_quit` runs.
struct Quit {
    started: Instant,
    rows: Vec<ShutdownRow>,
}

pub struct EngineBackend {
    engine: Engine,
    opts: EngineOptions,
    theme: ThemeKind,
    version: Option<u64>,
    snap: Arc<UiModel>,
    terms: HashMap<SessionId, Arc<EngineTerminal>>,
    /// Sessions that had a process in the previous snapshot (the board's rows when a quit starts).
    last_live: Vec<SessionId>,
    quit: Option<Quit>,
    quit_ready: bool,
    waker: Option<Arc<AtomicBool>>,
    /// The Git tab's details, from git tool replies.
    git: crate::git_model::GitDetails,
}

impl EngineBackend {
    pub fn new(engine: Engine, opts: EngineOptions) -> Self {
        let theme = opts.theme;
        let mut b = EngineBackend {
            engine,
            opts,
            theme,
            version: None,
            snap: Arc::new(UiModel::default()),
            terms: HashMap::new(),
            last_live: Vec::new(),
            quit: None,
            quit_ready: false,
            waker: None,
            git: crate::git_model::GitDetails::default(),
        };
        b.refresh(true);
        b
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// Rebuilds the model when the snapshot changed (or `force`).
    fn refresh(&mut self, force: bool) {
        let view = self.engine.view();
        if !force && self.version == Some(view.version) {
            return;
        }
        self.version = Some(view.version);
        // Terminals: keep an adapter while its handle is the same run; a restart is a new one.
        let mut terms = HashMap::new();
        for s in &view.sessions {
            if let Some(h) = view.terminal(&s.id) {
                let keep = self.terms.get(&s.id).filter(|t| &t.handle == h).cloned();
                terms.insert(s.id.clone(), keep.unwrap_or_else(|| Arc::new(EngineTerminal::new(h.clone()))));
            }
        }
        self.terms = terms;
        if view.quit.is_some() && self.quit.is_none() {
            let rows =
                self.last_live.iter().map(|id| ShutdownRow { session: id.clone(), state: ShutdownRowState::Pending, elapsed_s: 0.0 }).collect();
            self.quit = Some(Quit { started: Instant::now(), rows });
        }
        if view.quit.is_none() {
            self.last_live = view.sessions.iter().filter(|s| s.live).map(|s| s.id.clone()).collect();
        }
        self.publish(&view);
    }

    fn publish(&mut self, view: &Snapshot) {
        let mut m = map_snapshot(view, &self.opts, self.theme);
        m.git = self.git.map.clone();
        if let Some(q) = &self.quit {
            let used = q.started.elapsed().as_secs_f32().min(self.opts.shutdown_budget_s.max(1.0));
            m.shutdown = Some(ShutdownBoard { rows: q.rows.clone(), budget_used_s: used, budget_s: self.opts.shutdown_budget_s.max(1.0) });
        }
        self.snap = Arc::new(m);
    }

    fn start_waker(&mut self, ctx: &egui::Context) {
        if self.waker.is_some() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        self.waker = Some(stop.clone());
        let engine = self.engine.clone();
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("csm-ui-waker".into()).spawn(move || waker(engine, ctx, stop));
        if let Err(e) = spawned {
            log::error!("ui: no waker thread ({e}); the window repaints only on input");
        }
    }
}

impl Drop for EngineBackend {
    fn drop(&mut self) {
        if let Some(s) = &self.waker {
            s.store(true, Ordering::Release);
        }
    }
}

/// Repaints when anything the window shows may have changed: an engine event, a new snapshot,
/// or output in any live terminal (a lock-free generation counter). Checks at most every 16 ms.
fn waker(engine: Engine, ctx: egui::Context, stop: Arc<AtomicBool>) {
    let events = engine.subscribe();
    let mut last = 0u64;
    while !stop.load(Ordering::Acquire) {
        let mut wake = match events.recv_timeout(Duration::from_millis(16)) {
            Ok(_) => true,
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        while events.try_recv().is_ok() {
            wake = true;
        }
        let view = engine.view();
        let sum = view.sessions.iter().filter_map(|s| view.terminal(&s.id)).fold(view.version, |a, t| a.wrapping_add(t.generation()));
        let sum = view.run_outputs().fold(sum, |a, r| a.wrapping_add(r.generation()));
        if sum != last {
            last = sum;
            wake = true;
        }
        if wake {
            ctx.request_repaint();
        }
    }
}

impl Backend for EngineBackend {
    fn model(&self) -> Arc<UiModel> {
        self.snap.clone()
    }

    fn terminal(&self, id: &str) -> Option<Arc<dyn TerminalSource>> {
        self.terms.get(id).map(|t| t.clone() as Arc<dyn TerminalSource>)
    }

    /// The engine already applied the effect. Errors are shown; so are the result sentences of
    /// calls that start, stop or import something (UI-state calls stay quiet).
    fn on_reply(&mut self, call: &Call, reply: &Reply) -> Vec<Notice> {
        if let Some(notices) = self.git.on_reply(call, reply) {
            self.refresh(true);
            return notices;
        }
        match reply {
            Err(e) => vec![Notice::Toast(e.to_string())],
            Ok(out) => {
                if call.id == "settings_set"
                    && out.data["key"] == "Theme"
                    && let Some(v) = out.data["value"].as_str()
                {
                    self.theme = theme_from_setting(v);
                    self.refresh(true);
                }
                // The quick-open's commands follow Settings › General.
                if call.id == "settings_set"
                    && out.data["key"] == "LaunchCommands"
                    && let Some(list) = out.data["value"].as_array()
                {
                    self.opts.launch_commands = list.iter().filter_map(|v| v.as_str().map(String::from)).collect();
                    self.refresh(true);
                }
                let loud = Effects::LAUNCHES_PROCESS.union(Effects::MAY_DISCARD_DATA);
                let effects = self.engine.registry().get(&call.id).map(|s| s.effects);
                let say = call.id == "state_import_wpf" || (call.id != "app_quit" && effects.is_some_and(|e| e.intersects(loud)));
                if say && !out.message.is_empty() { vec![Notice::Toast(out.message.clone())] } else { vec![] }
            }
        }
    }

    fn on_event(&mut self, event: &Event) -> Vec<Notice> {
        match event {
            Event::Toast { message } => vec![Notice::Toast(message.clone())],
            Event::FocusRequested { session_id } => vec![Notice::Focus(session_id.clone())],
            Event::QuitProgress { session_id, phase, elapsed_ms } => {
                if self.quit.is_none() {
                    self.quit = Some(Quit { started: Instant::now(), rows: Vec::new() });
                }
                if let Some(q) = &mut self.quit {
                    let state = match phase.as_str() {
                        "closing" => ShutdownRowState::Closing,
                        "clean" => ShutdownRowState::Clean,
                        _ => ShutdownRowState::Forced,
                    };
                    let elapsed_s = *elapsed_ms as f32 / 1000.0;
                    match q.rows.iter_mut().find(|r| &r.session == session_id) {
                        Some(r) => {
                            r.state = state;
                            r.elapsed_s = elapsed_s;
                        }
                        None => q.rows.push(ShutdownRow { session: session_id.clone(), state, elapsed_s }),
                    }
                }
                self.refresh(true);
                vec![]
            }
            Event::QuitReady => {
                self.quit_ready = true;
                vec![Notice::QuitReady]
            }
            _ => vec![],
        }
    }

    fn run_output(&self, session: &str, run: &str) -> Option<csm_engine::RunOutputHandle> {
        self.engine.view().run_output(session, run).cloned()
    }

    fn tick(&mut self, ctx: &egui::Context) -> Vec<Notice> {
        self.start_waker(ctx);
        let quitting = self.quit.is_some() && !self.quit_ready;
        self.refresh(quitting);
        if quitting {
            // The budget bar moves with time, not with events.
            ctx.request_repaint_after(Duration::from_millis(100));
        }
        vec![]
    }
}

// ── Mapping ─────────────────────────────────────────────────────────────────────────────────

pub fn status_from_engine(s: EngineStatus) -> Status {
    match s {
        EngineStatus::Starting => Status::Starting,
        EngineStatus::Working => Status::Working,
        EngineStatus::NeedsYouApproval => Status::Approval,
        EngineStatus::NeedsYouQuestion => Status::Question,
        EngineStatus::DoneUnseen => Status::Done,
        EngineStatus::Error => Status::Error,
        EngineStatus::Idle | EngineStatus::Exited => Status::Idle,
        EngineStatus::Asleep => Status::Asleep,
    }
}

/// A `#rrggbb` accent: its index in the 12-colour palette (drawn in the theme's own shade), or
/// the exact colour (an OSC 9001 override). Anything unparseable is the default.
pub fn accent_from_hex(hex: &str) -> Accent {
    let palette = csm_core::color::palette();
    if let Some(i) = palette.iter().position(|c| c.eq_ignore_ascii_case(hex)) {
        return Accent::Palette(u8::try_from(i).unwrap_or(7));
    }
    let h = hex.trim().trim_start_matches('#');
    let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    match (h.len(), byte(0), byte(2), byte(4)) {
        (6 | 8, Some(r), Some(g), Some(b)) => Accent::Rgb([r, g, b]),
        _ => Accent::default(),
    }
}

pub fn layout_mode_from_core(m: CoreMode) -> LayoutMode {
    match m {
        CoreMode::Focus => LayoutMode::Focus,
        CoreMode::Side => LayoutMode::Side,
        CoreMode::Overview => LayoutMode::Overview,
        CoreMode::Grid => LayoutMode::Grid,
        CoreMode::Free => LayoutMode::Free,
    }
}

pub fn layout_mode_to_core(m: LayoutMode) -> CoreMode {
    match m {
        LayoutMode::Focus => CoreMode::Focus,
        LayoutMode::Side => CoreMode::Side,
        LayoutMode::Overview => CoreMode::Overview,
        LayoutMode::Grid => CoreMode::Grid,
        LayoutMode::Free => CoreMode::Free,
    }
}

/// The engine's stored layout as the screens draw it.
pub fn layout_from_core(l: &GroupLayout) -> Layout {
    let mut out = Layout {
        mode: layout_mode_from_core(l.mode),
        pinned: l.pinned.clone(),
        grid: GridPreset::new(l.grid_cols, l.grid_rows),
        ..Layout::default()
    };
    out.set_free(FreeLayout::from_value(l.free_tree.as_ref()));
    out
}

/// Writes a UI layout back into the engine's (mode, pins, grid and the Free blob).
pub fn layout_into_core(l: &Layout, core: &mut GroupLayout) {
    core.mode = layout_mode_to_core(l.mode);
    core.pinned = l.pinned.clone();
    core.grid_cols = l.grid.cols;
    core.grid_rows = l.grid.rows;
    let free = l.free();
    core.free_tree = (free != FreeLayout::default()).then(|| free.to_value());
}

/// The engine's layout key → the UI's scope key (`__ALL__` → `all`).
pub fn scope_key_from_engine(key: &str) -> String {
    if key == ALL_LAYOUT_KEY { Scope::All.key() } else { key.to_string() }
}

fn session_from_engine(s: &csm_engine::SessionView) -> SessionView {
    let kind = match s.kind {
        "ssh" => Kind::Ssh,
        "wsl" => Kind::Wsl,
        _ => Kind::Local,
    };
    let folder = match kind {
        Kind::Local if !s.folder.is_empty() => s.folder.clone(),
        _ => s.folder_short.clone(),
    };
    let exited = s.status == EngineStatus::Exited;
    let started = match (s.asleep, s.backend, exited) {
        (true, _, _) => "Asleep: the process is stopped; it starts again when woken".to_string(),
        (_, _, true) => "The process ended; restart or close the session".to_string(),
        (_, Some(b), _) if s.is_claude => format!("Running on the {b} · resumes its conversation on restart"),
        (_, Some(b), _) => format!("Running on the {b}"),
        (_, None, _) => "Starting".to_string(),
    };
    SessionView {
        id: s.id.clone(),
        name: s.name.clone(),
        group: (!s.group_id.is_empty()).then(|| s.group_id.clone()),
        kind,
        command: s.command.clone(),
        folder,
        branch: s.branch.clone(),
        dirty: s.git_dirty,
        ahead: s.git_ahead.and_then(|a| u32::try_from(a).ok()).unwrap_or(0),
        behind: s.git_behind.and_then(|b| u32::try_from(b).ok()).unwrap_or(0),
        upstream: s.git_upstream.clone(),
        changes: u32::try_from(s.git_changes).unwrap_or(u32::MAX),
        git_summary: s.git_summary.clone(),
        worktree_of: s.worktree_of.clone(),
        git_busy: s.git_busy,
        status: status_from_engine(s.status),
        progress: s.progress,
        detail: s.status_message.clone().unwrap_or_default(),
        status_since_ms: s.inbox_since_ms.and_then(|t| u64::try_from(t).ok()),
        accent: accent_from_hex(&s.accent),
        accent_overridden: s.accent_override,
        parent: None,
        runs: s.runs.iter().map(run_from_engine).collect(),
        open_with: s
            .open_with
            .iter()
            .map(|o| OpenWithItem {
                entry_id: o.entry_id.clone(),
                label: o.label.clone(),
                program: o.program.clone(),
                command: o.command.clone(),
                is_default: o.is_default,
            })
            .collect(),
        started,
        exited,
        exit_code: s.exit_code,
    }
}

/// A linked worktree's session nests under a session in its repo's main checkout (same repo
/// root, not itself a worktree), as the sidebar shows worktree clusters.
fn worktree_parents(sessions: &mut [SessionView], view: &Snapshot) {
    let root = |id: &str| view.session(id).and_then(|s| s.repo_root.as_deref()).map(str::to_lowercase);
    let mains: Vec<(String, String)> = view
        .sessions
        .iter()
        .filter(|s| s.worktree_of.is_none())
        .filter_map(|s| s.repo_root.as_deref().map(|r| (r.to_lowercase(), s.id.clone())))
        .collect();
    for s in sessions.iter_mut().filter(|s| s.worktree_of.is_some()) {
        if let Some(r) = root(&s.id) {
            s.parent = mains.iter().find(|(mr, id)| *mr == r && *id != s.id).map(|(_, id)| id.clone());
        }
    }
}

/// How long, in words ("12 s", "4 min", "2 h").
fn took(ms: i64) -> String {
    let s = (ms.max(0) + 500) / 1000;
    match s {
        0..60 => format!("{s} s"),
        60..3600 => format!("{} min", s / 60),
        _ => format!("{} h", s / 3600),
    }
}

/// A run command as the Run button, the menu and the Runs tab show it.
pub fn run_from_engine(r: &csm_engine::RunView) -> RunView {
    use csm_engine::RunPhase as P;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let dur = match (r.started_ms, r.ended_ms) {
        (Some(a), Some(b)) => Some(took(b - a)),
        (Some(a), None) => Some(took(now - a)),
        _ => None,
    };
    let with = |s: &str| dur.as_ref().map_or_else(|| s.to_string(), |d| format!("{s} · {d}"));
    let (state, detail) = match r.phase {
        P::Idle => (RunState::NotRun, "not run yet".to_string()),
        P::Starting => (RunState::Running, "starting".to_string()),
        P::Running => (RunState::Running, with("running")),
        P::Ok => (RunState::Passed, with("passed · exit 0")),
        P::Failed => match r.exit_code {
            Some(-1) => (RunState::Failed, "could not start".to_string()),
            Some(c) => (RunState::Failed, with(&format!("failed · exit {c}"))),
            None => (RunState::Failed, with("failed")),
        },
        P::Stopped => (RunState::NotRun, with("stopped")),
    };
    RunView {
        id: r.id.clone(),
        label: r.label.clone(),
        command_line: r.command_line.clone(),
        is_default: r.is_default,
        state,
        detail,
        output: String::new(),
        mode: r.mode.to_string(),
        post_run_url: r.post_run_url.clone(),
        exit_code: r.exit_code,
    }
}

fn leaf(path: &str) -> String {
    path.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().filter(|l| !l.is_empty()).unwrap_or(path).to_string()
}

/// The quick-open's folders: every local session's folder (with how many are open there), then
/// the extra folders from start-up.
fn folders(sessions: &[SessionView], extra: &[String]) -> Vec<FolderSuggestion> {
    let mut out: Vec<FolderSuggestion> = Vec::new();
    for s in sessions.iter().filter(|s| s.kind == Kind::Local && !s.folder.is_empty()) {
        let live = u32::from(s.status.is_live());
        match out.iter_mut().find(|f| f.path.eq_ignore_ascii_case(&s.folder)) {
            Some(f) => f.open += live,
            None => out.push(FolderSuggestion {
                name: leaf(&s.folder),
                path: s.folder.clone(),
                branch: s.branch.clone(),
                tag: "In the sidebar".into(),
                open: live,
                recent: true,
            }),
        }
    }
    for p in extra {
        if !p.is_empty() && !out.iter().any(|f| f.path.eq_ignore_ascii_case(p)) {
            out.push(FolderSuggestion { name: leaf(p), path: p.clone(), tag: "Folder".into(), ..FolderSuggestion::default() });
        }
    }
    out
}

/// The whole mapping, pure (tested without an engine thread).
pub fn map_snapshot(view: &Snapshot, opts: &EngineOptions, theme: ThemeKind) -> UiModel {
    let mut sessions: Vec<SessionView> = view.sessions.iter().map(session_from_engine).collect();
    worktree_parents(&mut sessions, view);
    let groups =
        view.groups.iter().map(|g| GroupView { id: g.id.clone(), name: g.name.clone(), torn_off: g.torn_off, collapsed: !g.expanded }).collect();
    let layouts = view.layouts.iter().map(|(k, l)| (scope_key_from_engine(k), layout_from_core(l))).collect();
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    let folders = folders(&sessions, &opts.folders);
    UiModel {
        group_mode: match view.view {
            EngineGroupView::Rail => GroupMode::Rail,
            EngineGroupView::Headers => GroupMode::Headers,
            EngineGroupView::Flat => GroupMode::Flat,
        },
        rail: view.rail_tab.clone().map_or(Scope::All, Scope::Group),
        layouts,
        active: view.focused.clone(),
        inspector: view.inspector.map(|t| match t {
            CoreTab::Notes => InspectorTab::Notes,
            CoreTab::Runs => InspectorTab::Runs,
            CoreTab::Git => InspectorTab::Git,
            CoreTab::Session => InspectorTab::Session,
        }),
        inbox_panel: InboxPanel {
            open: view.inbox_open,
            height: view.inbox_height.filter(|h| h.is_finite()).unwrap_or(crate::theme::size::INBOX_DEFAULT_H),
        },
        restore: view.restore.map(|p| RestoreProgress {
            done: u32::try_from(p.done).unwrap_or(u32::MAX),
            total: u32::try_from(p.total).unwrap_or(u32::MAX),
            current: None,
        }),
        shutdown: None,
        folders,
        launch_commands: opts.launch_commands.clone(),
        notes: Default::default(),
        theme,
        now_ms,
        wpf_import: if sessions.is_empty() { opts.wpf_import.clone() } else { None },
        windows: view.windows.clone(),
        git: Default::default(),
        open_with: view
            .open_with
            .iter()
            .map(|e| OpenWithEntryView {
                id: e.id.clone(),
                label: e.label.clone(),
                executable: e.executable.clone(),
                program: e.program.clone(),
                local_args: e.local_args.clone(),
                wsl_args: e.wsl_args.clone(),
                ssh_args: e.ssh_args.clone(),
                is_default: e.is_default,
            })
            .collect(),
        sessions,
        groups,
    }
}

// ── Terminals ───────────────────────────────────────────────────────────────────────────────

/// A pane's view of an engine session: [`TerminalHandle`] as a [`TerminalSource`].
pub struct EngineTerminal {
    handle: TerminalHandle,
    /// The last grid passed on, so an unchanged size never takes the terminal lock.
    grid: Mutex<Option<GridSize>>,
}

impl EngineTerminal {
    pub fn new(handle: TerminalHandle) -> Self {
        Self { handle, grid: Mutex::new(None) }
    }

    pub fn handle(&self) -> &TerminalHandle {
        &self.handle
    }

    /// The grid this pane reported last (the size the PTY was started or resized to).
    pub fn reported_grid(&self) -> Option<GridSize> {
        *lock(&self.grid)
    }
}

impl TerminalSource for EngineTerminal {
    fn terminal(&self) -> Option<Arc<Mutex<Terminal>>> {
        Some(self.handle.terminal().clone())
    }

    fn mode(&self) -> TermMode {
        self.handle.mode()
    }

    fn set_grid(&self, grid: GridSize) {
        let mut last = lock(&self.grid);
        if *last == Some(grid) {
            return;
        }
        *last = Some(grid);
        drop(last);
        self.handle.resize(grid);
    }

    fn write(&self, bytes: &[u8]) {
        // Raw bytes with no notion of who typed them: not an acknowledgement.
        self.handle.send_mouse(AppAuthored(bytes.to_vec()));
    }

    fn state(&self) -> SourceState {
        if self.handle.has_exited() {
            SourceState::Exited(None)
        } else if self.handle.is_started() {
            SourceState::Running
        } else {
            SourceState::Starting
        }
    }

    fn send_key(&self, bytes: &[u8]) {
        self.handle.send_key(AppAuthored(bytes.to_vec()));
    }

    fn send_mouse(&self, bytes: &[u8]) {
        self.handle.send_mouse(AppAuthored(bytes.to_vec()));
    }

    fn paste(&self, text: &str) {
        self.handle.paste(text);
    }

    fn focus_changed(&self, focused: bool) {
        self.handle.focus_changed(focused);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accents_map_to_the_palette_or_an_exact_colour() {
        let first = csm_core::color::palette()[0];
        assert_eq!(accent_from_hex(first), Accent::Palette(0));
        assert_eq!(accent_from_hex("#102030"), Accent::Rgb([0x10, 0x20, 0x30]));
        assert_eq!(accent_from_hex("nonsense"), Accent::default());
        assert_eq!(accent_from_hex("#zz"), Accent::default());
    }

    #[test]
    fn statuses_map_one_to_one_and_exited_is_idle() {
        assert_eq!(status_from_engine(EngineStatus::NeedsYouApproval), Status::Approval);
        assert_eq!(status_from_engine(EngineStatus::DoneUnseen), Status::Done);
        assert_eq!(status_from_engine(EngineStatus::Exited), Status::Idle);
        let s = SessionView { status: Status::Idle, exited: true, exit_code: Some(3), ..SessionView::default() };
        assert_eq!(s.status_text(), "Exited · code 3");
    }

    #[test]
    fn layouts_round_trip_through_the_engine_shape() {
        let mut l = Layout { mode: LayoutMode::Free, pinned: vec!["a".into()], grid: GridPreset::new(3, 2), ..Layout::default() };
        l.tree = SplitNode::row(&["a".into(), "b".into()]);
        l.floats = vec![FloatPane { session: "c".into(), rect: [1.0, 2.0, 3.0, 4.0] }];
        let mut core = GroupLayout::default();
        layout_into_core(&l, &mut core);
        assert_eq!(layout_from_core(&core), l);
        // An unreadable blob (an old or hand-edited file) is an empty Free layout, not an error.
        core.free_tree = Some(serde_json::json!({"tree": 5}));
        assert_eq!(layout_from_core(&core).tree, None);
    }

    #[test]
    fn themes_follow_the_setting() {
        assert_eq!(theme_from_setting("light"), ThemeKind::Latte);
        assert_eq!(theme_from_setting("Dark"), ThemeKind::Mocha);
        assert_eq!(theme_from_setting("latte"), ThemeKind::Latte);
        assert_eq!(theme_setting(ThemeKind::Latte), "light");
    }
}
