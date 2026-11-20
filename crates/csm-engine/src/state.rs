//! Everything the engine owns. Only the engine thread touches it, so it needs no locks of its
//! own; anything shared with other threads (a terminal the UI renders) is behind its own handle.
//!
//! Plain data plus handles; behaviour lives in tools and in [`crate::lifecycle`].

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use csm_core::models::AppState;
use csm_core::session_manager::SessionManager;
use csm_core::state::LoadSource;
use csm_pty::{Pty, SpawnOptions};

use crate::engine::ParkedReply;
use crate::persist;
use crate::session::SessionRt;
use crate::snapshot::Snapshot;

/// Creates PTYs. Production uses [`csm_pty::spawn`]; tests inject a fake. Called on a worker
/// thread, never on the engine or UI thread.
pub type PtyFactory = Arc<dyn Fn(SpawnOptions) -> io::Result<Arc<dyn Pty>> + Send + Sync>;

/// The real PTY factory.
pub fn real_pty_factory() -> PtyFactory {
    Arc::new(csm_pty::spawn)
}

/// Opens an already-checked absolute `http(s)` URL (`open_url`). Called on the engine thread;
/// it must hand the work off and return at once.
pub type UrlOpener = Arc<dyn Fn(String) + Send + Sync>;

/// The real URL opener: the default browser, on a worker.
pub fn real_url_opener() -> UrlOpener {
    Arc::new(crate::tools::open::open_url_detached)
}

/// How the engine runs. `Default` is safe for tests and tools: nothing is saved, real PTYs.
#[derive(Clone)]
pub struct StateOptions {
    /// Where `state.json` lives. `None`: never written (and nothing to load).
    pub save_path: Option<PathBuf>,
    /// The `--clean` mode: start without sessions or groups and never write anything.
    pub clean: bool,
    pub pty: PtyFactory,
    /// How long a new session waits for the UI to report its first measured grid before it
    /// starts at [`StateOptions::fallback_size`] (headless and MCP-created sessions).
    pub first_size_timeout: Duration,
    /// Size used when no pane reported one. Matches the WPF app's `PseudoTerminal` default.
    pub fallback_size: (u16, u16),
    /// Total time a quit waits for Claude sessions to exit cleanly (`ClaudeShutdownBudgetMs`).
    pub shutdown_budget: Duration,
    /// How long a restart (and sleep/close) waits for a Claude process to exit before killing it.
    pub exit_wait: Duration,
    /// Delay before saving after a change (saves coalesce).
    pub save_debounce: Duration,
    /// Run the restore loop when the engine starts.
    pub restore_on_start: bool,
    /// Detect installed editors once to seed the Open with list (production only: detection
    /// spawns vswhere and reads the registry, which tests must not depend on).
    pub seed_editors: bool,
    /// The search index and notes store (`docs/tools.md`, notes and search). Off by default;
    /// production uses [`IndexSetting::Auto`].
    pub index: crate::index::IndexSetting,
    /// What `open_url` calls. Default: nothing opens (logged), so tests and headless engines
    /// never start a browser; production uses [`real_url_opener`].
    pub url_opener: UrlOpener,
}

impl Default for StateOptions {
    fn default() -> Self {
        Self {
            save_path: None,
            clean: false,
            pty: real_pty_factory(),
            first_size_timeout: Duration::from_millis(1500),
            fallback_size: (220, 50),
            shutdown_budget: Duration::from_millis(30_000),
            exit_wait: Duration::from_millis(10_000),
            save_debounce: Duration::from_millis(500),
            restore_on_start: true,
            seed_editors: false,
            index: crate::index::IndexSetting::Off,
            url_opener: Arc::new(|url| log::info!("open_url: {url} (this engine opens no browser)")),
        }
    }
}

impl StateOptions {
    /// The production options: the state file at [`persist::default_state_path`].
    pub fn production() -> Self {
        Self {
            save_path: persist::default_state_path(),
            seed_editors: true,
            index: crate::index::IndexSetting::Auto,
            url_opener: real_url_opener(),
            ..Self::default()
        }
    }

    pub(crate) fn saves(&self) -> bool {
        !self.clean && self.save_path.is_some()
    }
}

/// What [`State::load`] found on disk.
#[derive(Debug, Clone)]
pub struct LoadReport {
    pub path: Option<PathBuf>,
    /// `None` when nothing was read (no path).
    pub source: Option<LoadSource>,
}

/// Progress of a long loop (restore, bulk restart, quit), for the rail and the shutdown board.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
}

/// A `session_wait` call waiting for a status.
pub(crate) struct Wait {
    pub session_id: String,
    pub until: crate::status::WaitUntil,
    pub deadline: Instant,
    pub reply: ParkedReply,
    /// For `any_change`: the status when the wait started.
    pub start: crate::status::Status,
}

/// Runtime-only state: never persisted.
#[derive(Default)]
pub(crate) struct Runtime {
    pub sessions: HashMap<String, SessionRt>,
    pub focused: Option<String>,
    /// Sessions being restarted: their old PTY is going, the new one not started yet.
    pub restarting: HashSet<String>,
    /// Restore loop: sessions still to start.
    pub restore_pending: HashSet<String>,
    pub restore: Option<Progress>,
    /// A restart (single or bulk) holds this for its whole run.
    pub restart_active: bool,
    pub restart: Option<Progress>,
    pub restart_cancel: bool,
    pub shutting_down: bool,
    pub quit: Option<Progress>,
    /// `app_quit { force: true }` while a quit runs: skip the remaining Claude exit waits.
    pub quit_force: Arc<std::sync::atomic::AtomicBool>,
    pub waits: Vec<Wait>,
    /// When the session entered the inbox (epoch ms), for "oldest first".
    pub inbox_since: HashMap<String, i64>,
    pub last_toast: HashMap<String, Instant>,
    pub save_due: Option<Instant>,
    /// The snapshot is stale and must be rebuilt after this step.
    pub dirty: bool,
    pub prev: Arc<Snapshot>,
    pub instance_seq: u64,
    /// Next Claude launch slot (flat stagger), shared with spawn workers.
    pub claude_gate: Arc<Mutex<Option<Instant>>>,
    pub saver: Option<persist::Saver>,
    /// The ConPTY in use and why (resolved once off the engine thread).
    pub conpty: Arc<OnceLock<String>>,
    /// Run commands' invocations, by session id.
    pub runs: crate::runs::Runs,
    /// The search index and notes store, when the run has one.
    pub index: Option<csm_index::Index>,
    /// Git status per live session, the `.git` watchers and the per-repo busy slots.
    pub git: crate::tools::git::GitRt,
}

/// The engine's state. Create with [`State::default`] (tests), [`State::new`] or [`State::load`].
#[derive(Default)]
pub struct State {
    /// The persisted part (sessions, groups, settings, UI memory). `app.sessions` order is the
    /// one source of truth for order (sidebar, grid and `state.json`).
    pub app: AppState,
    pub(crate) rt: Runtime,
    pub(crate) options: StateOptions,
}

impl State {
    /// A state from an already-loaded `AppState`. Applies the load migrations (legacy "Default"
    /// group, `LastActivityAt` backfill) and drops sessions without an id or with a duplicate
    /// one (the file is untrusted). `--clean` drops sessions, groups and recently closed.
    pub fn new(mut app: AppState, options: StateOptions) -> State {
        let mut m = SessionManager::new();
        m.load_from_state(&mut app);
        m.populate_state(&mut app);
        if options.clean {
            app.sessions.clear();
            app.groups.clear();
            app.recently_closed.clear();
        }
        let mut seen = HashSet::new();
        app.sessions.retain(|s| !s.id.trim().is_empty() && seen.insert(s.id.clone()));
        let mut seen = HashSet::new();
        app.groups.retain(|g| !g.id.trim().is_empty() && seen.insert(g.id.clone()));
        app.recently_closed.truncate(crate::lifecycle::MAX_RECENTLY_CLOSED);
        let focused = app.ui.focused_session.clone().filter(|f| app.sessions.iter().any(|s| &s.id == f));
        let mut rt = Runtime { focused, dirty: true, index: crate::index::open(&options), ..Runtime::default() };
        if options.saves()
            && let Some(path) = &options.save_path
        {
            rt.saver = persist::Saver::start(path.clone());
        }
        State { app, rt, options }
    }

    /// Reads `options.save_path` (primary, then `.bak`; never fails) and builds the state.
    /// File I/O: call it at start-up, not from a UI frame.
    pub fn load(options: StateOptions) -> (State, LoadReport) {
        let path = options.save_path.clone();
        let (app, source) = match &path {
            Some(p) => {
                let loaded = persist::load(p);
                (loaded.state, Some(loaded.source))
            }
            None => (AppState::default(), None),
        };
        match source {
            Some(LoadSource::Backup) => log::warn!("state: primary file unreadable, recovered from the backup"),
            Some(LoadSource::Unreadable) => {
                log::error!("state: neither the file nor its backup could be read; starting empty")
            }
            _ => {}
        }
        (State::new(app, options), LoadReport { path, source })
    }

    pub fn options(&self) -> &StateOptions {
        &self.options
    }

    /// Mark the snapshot stale (it is rebuilt and republished after the current step).
    pub fn touch(&mut self) {
        self.rt.dirty = true;
    }

    /// Mark the snapshot stale and schedule a debounced save.
    pub fn persist(&mut self) {
        self.rt.dirty = true;
        if self.rt.save_due.is_none() {
            self.rt.save_due = Some(Instant::now() + self.options.save_debounce);
        }
    }
}
