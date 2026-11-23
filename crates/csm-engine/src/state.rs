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
    /// How long a quit waits for EACH Claude session to exit cleanly before it is stopped. They
    /// close one after another, so the whole quit may take this times the number of Claude
    /// sessions (plus the launch stagger between them); the board shows that total.
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
    /// Where `profile_list` and `session_look_set` find Windows Terminal profiles. Production
    /// reads the install locations; the default (tests) has none.
    pub terminal_profiles: TerminalProfiles,
    /// Where Claude's transcripts are read from for the usage tools. Default (tests): nowhere;
    /// production: Claude's config folders ([`crate::ClaudeRoots::Auto`]). Needs an index.
    pub claude_roots: crate::usage_tail::ClaudeRoots,
    /// How often followed transcripts are checked for new lines.
    pub usage_poll: Duration,
    /// The bundled sample add-ons (`<install dir>/samples/addons`), which `addon_install_samples`
    /// copies into the add-ons folder. `None` (tests, unless they say otherwise): no samples.
    pub samples_dir: Option<PathBuf>,
    /// Where process add-ons keep their secrets. Default (tests): memory. Production: the OS
    /// keychain.
    pub secrets: Arc<dyn csm_services::secrets::SecretStore>,
    /// Starting the app at sign-in (`app_autostart_get` / `app_autostart_set`). Default (tests, the
    /// demo): an entry in memory, so nothing real changes. Production: the current user's own.
    pub autostart: Autostart,
    /// The update check (`update_check`). Default (tests, the demo): no network, no schedule.
    /// Production: GitHub, once a day ([`crate::UpdateOptions::production`]).
    pub update: crate::tools::update::UpdateOptions,
}

/// Where the start-at-sign-in entry lives and the program it should start.
#[derive(Clone)]
pub struct Autostart {
    /// `None`: this platform has no entry the app can manage.
    pub backend: Option<Arc<dyn csm_services::autostart::Backend>>,
    /// The program the entry starts (this exe; an AppImage's own file). `None`: unknown.
    pub program: Option<PathBuf>,
}

impl Default for Autostart {
    fn default() -> Self {
        Autostart { backend: Some(Arc::new(csm_services::autostart::MemoryBackend::default())), program: std::env::current_exe().ok() }
    }
}

impl Autostart {
    /// The real entry: the current user's Run key, LaunchAgent or XDG autostart file.
    pub fn system() -> Self {
        Autostart { backend: csm_services::autostart::system().map(Arc::from), program: csm_services::autostart::current_program() }
    }

    /// The start-up fix-up (`csm_services::autostart::refresh`): an entry naming another exe is
    /// pointed at this one. What happened, for the log; `None` when nothing changed.
    pub fn refresh(&self, only_if_missing: bool) -> Option<String> {
        let (b, exe) = (self.backend.as_ref()?, self.program.as_ref()?);
        match csm_services::autostart::refresh(b.as_ref(), exe, only_if_missing) {
            Ok(Some(old)) => Some(format!("the sign-in entry now starts {} (it started {})", exe.display(), old.display())),
            Ok(None) => None,
            Err(e) => Some(format!("could not update the sign-in entry at {}: {e}", b.location())),
        }
    }

    /// The uninstaller's clean-up (`csm_services::autostart::remove_if_ours`), for the log.
    pub fn remove_if_ours(&self) -> Option<String> {
        let (b, exe) = (self.backend.as_ref()?, self.program.as_ref()?);
        match csm_services::autostart::remove_if_ours(b.as_ref(), exe) {
            Ok(true) => Some(format!("removed the sign-in entry at {}", b.location())),
            Ok(false) => None,
            Err(e) => Some(format!("could not remove the sign-in entry at {}: {e}", b.location())),
        }
    }
}

/// The Windows Terminal settings the profile tools read.
#[derive(Debug, Clone, Default)]
pub enum TerminalProfiles {
    /// No profiles (tests, unless they say otherwise).
    #[default]
    None,
    /// WT's install locations (`csm_services::wt_profiles::known_paths`): production.
    Installed,
    /// These `settings.json` files, as `(install name, path)`.
    Files(Vec<(String, PathBuf)>),
    /// These `settings.json` texts, as `(install name, text)`: the demo's profiles, no file I/O.
    Texts(Vec<(String, String)>),
}

impl Default for StateOptions {
    fn default() -> Self {
        Self {
            save_path: None,
            clean: false,
            pty: real_pty_factory(),
            first_size_timeout: Duration::from_millis(1500),
            fallback_size: (220, 50),
            shutdown_budget: Duration::from_millis(15_000),
            exit_wait: Duration::from_millis(10_000),
            save_debounce: Duration::from_millis(500),
            restore_on_start: true,
            seed_editors: false,
            index: crate::index::IndexSetting::Off,
            terminal_profiles: TerminalProfiles::None,
            claude_roots: crate::usage_tail::ClaudeRoots::None,
            usage_poll: Duration::from_secs(2),
            samples_dir: None,
            secrets: Arc::new(csm_services::secrets::MemoryStore::default()),
            autostart: Autostart::default(),
            url_opener: Arc::new(|url| log::info!("open_url: {url} (this engine opens no browser)")),
            update: crate::tools::update::UpdateOptions::default(),
        }
    }
}

/// `samples/addons` beside the running exe (where the installer, the portable zip and the Linux
/// packages put it). A macOS app bundle keeps it in `Contents/Resources` instead, because
/// codesign treats every file in `Contents/MacOS` as code and refuses data files there.
/// Called once at startup (two `stat`s), never from frame code.
fn bundled_samples_dir() -> Option<PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    let beside = exe_dir.join("samples").join("addons");
    let bundle = exe_dir.join("..").join("Resources").join("samples").join("addons");
    if !beside.is_dir() && bundle.is_dir() { Some(bundle) } else { Some(beside) }
}

impl StateOptions {
    /// The production options: the state file at [`persist::default_state_path`].
    pub fn production() -> Self {
        Self {
            save_path: persist::default_state_path(),
            seed_editors: true,
            index: crate::index::IndexSetting::Auto,
            terminal_profiles: TerminalProfiles::Installed,
            claude_roots: crate::usage_tail::ClaudeRoots::Auto,
            url_opener: real_url_opener(),
            samples_dir: bundled_samples_dir(),
            secrets: Arc::new(csm_services::secrets::KeyringStore),
            autostart: Autostart::system(),
            update: crate::tools::update::UpdateOptions::production(),
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
    /// Unreadable state files kept aside as `<file>.bad-<ms>` (issue #166); empty normally.
    pub set_aside: Vec<PathBuf>,
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
    /// Add-ons found on disk and their state (`addons.rs`).
    pub(crate) addons: crate::addons::Catalogue,
    pub sessions: HashMap<String, SessionRt>,
    pub focused: Option<String>,
    /// Sessions being restarted: their old PTY is going, the new one not started yet.
    pub restarting: HashSet<String>,
    /// Restore loop: sessions still to start.
    pub restore_pending: HashSet<String>,
    /// Sessions from an add-on profile, waiting for the add-on scan before they can restore.
    pub(crate) addon_restore_waiting: Vec<String>,
    pub restore: Option<Progress>,
    /// A restart (single or bulk) holds this for its whole run.
    pub restart_active: bool,
    pub restart: Option<Progress>,
    pub restart_cancel: bool,
    pub shutting_down: bool,
    pub quit: Option<Progress>,
    /// The whole quit's wait budget while it runs: the per-session wait times the Claude
    /// sessions to close, plus the stagger between them.
    pub quit_budget_ms: Option<u64>,
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
    /// The Claude transcript reader (usage tools), when the run has an index and roots.
    pub usage: Option<crate::usage_tail::UsageTail>,
    /// Each followed Claude session's subagents, as the reader last published them (shared, so
    /// the snapshot clones only a pointer).
    pub subagents: std::collections::HashMap<String, std::sync::Arc<csm_core::subagents::SubagentsReport>>,
    /// Each followed Claude session's context fill from the light tail, while the Claude usage
    /// add-on is off (`usage_context` answers from it then).
    pub(crate) live_context: HashMap<String, crate::usage_tail::LiveContext>,
    /// Each followed local Claude session's main transcript, as the reader last found it
    /// (`claude_reply_get` and `claude_code_block_get` read Claude's replies from it).
    pub(crate) claude_transcripts: HashMap<String, std::path::PathBuf>,
    /// The Board's tasks (`tasks.json`; loaded and saved only when the state is).
    pub tasks: csm_core::tasks::TaskFile,
    pub tasks_saver: Option<persist::Saver>,
    pub tasks_save_due: Option<Instant>,
    /// Bumped on every change to `tasks`; the snapshot shares `tasks_view` until it moves.
    pub tasks_gen: u64,
    pub tasks_view: Option<(u64, Arc<Vec<csm_core::tasks::Task>>)>,
    /// Each session's status and when it entered it (epoch ms): `status_since_ms`, the status log.
    pub status_since: HashMap<String, (crate::status::Status, i64)>,
    /// A process add-on's status override and annotation on sessions it opened, by session id.
    /// Runtime only: cleared when its add-on stops or is disabled.
    pub(crate) plugin_marks: HashMap<String, crate::plugins::PluginMark>,
    /// The update check: the last result, the schedule, a dismissed notice.
    pub(crate) update: crate::tools::update::UpdateRt,
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
        // The file is untrusted: remember a focus only for a group and session that exist.
        let (groups, sessions) = (&app.groups, &app.sessions);
        app.ui.group_focus.retain(|g, s| groups.iter().any(|x| &x.id == g) && sessions.iter().any(|x| &x.id == s && &x.group_id == g));
        let focused = app.ui.focused_session.clone().filter(|f| app.sessions.iter().any(|s| &s.id == f));
        let mut rt = Runtime { focused, dirty: true, index: crate::index::open(&options), ..Runtime::default() };
        let mode = crate::usage_tail::mode_of(&app.settings, &options);
        rt.usage = crate::usage_tail::UsageTail::start(rt.index.as_ref(), &options.claude_roots, options.save_path.as_deref(), options.usage_poll, mode);
        if options.saves()
            && let Some(path) = &options.save_path
        {
            rt.saver = persist::Saver::start(path.clone());
            let tasks = persist::tasks_path(path);
            rt.tasks = persist::load_tasks(&tasks, crate::session::now_ms());
            rt.tasks_saver = persist::Saver::start_plain(tasks, "csm-tasks-saver");
            // Tasks of sessions that are gone (closed while the app was not running) go back.
            let gone: HashSet<String> =
                rt.tasks.tasks.iter().filter_map(|t| t.assignee.clone()).filter(|a| !app.sessions.iter().any(|s| &s.id == a)).collect();
            for g in gone {
                rt.tasks.release_session(&g, crate::session::now_ms());
            }
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
        // Issue #166: an unreadable file is kept where no save can overwrite it (only when this
        // run saves at all: `--clean` and isolated runs never touch the disk).
        let set_aside = match (&path, source) {
            (Some(p), Some(src)) if options.saves() => persist::keep_unreadable(p, src, crate::session::now_ms()),
            _ => Vec::new(),
        };
        for f in &set_aside {
            log::warn!("state: the unreadable file was kept as {}", f.display());
        }
        (State::new(app, options), LoadReport { path, source, set_aside })
    }

    pub fn options(&self) -> &StateOptions {
        &self.options
    }

    /// Mark the snapshot stale (it is rebuilt and republished after the current step).
    pub fn touch(&mut self) {
        self.rt.dirty = true;
    }

    /// Mark the snapshot stale and schedule a debounced save of `tasks.json`.
    pub(crate) fn persist_tasks(&mut self) {
        self.rt.dirty = true;
        self.rt.tasks_gen = self.rt.tasks_gen.wrapping_add(1);
        if self.rt.tasks_save_due.is_none() {
            self.rt.tasks_save_due = Some(Instant::now() + self.options.save_debounce);
        }
    }

    /// Mark the snapshot stale and schedule a debounced save.
    pub fn persist(&mut self) {
        self.rt.dirty = true;
        if self.rt.save_due.is_none() {
            self.rt.save_due = Some(Instant::now() + self.options.save_debounce);
        }
    }
}
