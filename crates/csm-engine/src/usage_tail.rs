//! Follows the Claude transcripts of live Claude sessions on a background thread (`csm-usage`).
//!
//! What it does depends on the built-in Claude usage add-on ([`Mode`], from the settings):
//!
//! - **Off** (the default): a light tail of each live local Claude session's main transcript,
//!   kept in memory only: its context fill (the last main-thread request), its Remote Control
//!   line and its subagents (without their cost). Nothing is stored in the index, no subagent
//!   transcript is read, and the history is not touched.
//! - **On**: as well, every request of the followed sessions (main thread and subagents) is
//!   stored in the index (`claude_*` tables) and priced.
//! - **On, with "Read past Claude history"**: as well, every transcript under the roots is
//!   imported, a little per pass. Never under `--clean` or an isolated run ([`history_allowed`]).
//!
//! Turning the add-on off drops what the reader holds in memory; what the index holds stays
//! (`usage_forget_history` forgets the imported history). Turning it on carries on from the
//! stored read positions.
//!
//! - The engine tells the thread which sessions to follow ([`Link`]: CSM session, its Claude
//!   conversation id, its folder) from its tick; nothing here touches the engine's state.
//! - Each pass stats every followed transcript (main thread and subagents) and reads only the
//!   bytes added since the stored offset; a shorter file or a different head is read again from
//!   the start. The parsed numbers go to the index thread as one job per file; the file I/O never
//!   happens on the engine, index or UI thread.
//! - Prices: the embedded table plus `usage-prices.json` beside `state.json`. When the table
//!   changes, every stored request is priced again.
//! - A session without a known conversation id (one started before CSM recorded them) follows
//!   the newest transcript whose `cwd` is its folder: a guess when two conversations share a folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use csm_core::claude_usage::{Compaction, UsageRequest, project_name};
use csm_core::subagents::{AgentInput, RunInput, SubagentsReport};
use csm_index::{ClaudeFile, ClaudeIngest, Index};
use csm_services::claude_usage::{self as cu, Batch, OverrideStatus, PriceTable, discover, subagents as sub, tail};

use crate::state::State;

/// Where the tailer looks for Claude's transcripts.
#[derive(Debug, Clone, Default)]
pub enum ClaudeRoots {
    /// Nowhere: usage is not followed (tests, the demo).
    #[default]
    None,
    /// `$CLAUDE_CONFIG_DIR`, `~/.claude` and the other `~/.claude-*` profiles (production).
    Auto,
    /// These roots (tests).
    Dirs(Vec<PathBuf>),
}

/// One session to follow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Link {
    pub csm_session: String,
    pub claude_sid: Option<String>,
    pub folder: String,
    /// No other CSM Claude session works in this folder, so a newer transcript of the folder
    /// written while this one runs is its own (a `/clear` starts a new conversation id that CSM
    /// is not told about). The same rule a resume uses (`launch::choose_claude_id`).
    pub sole: bool,
    /// When its process started (ms since the epoch): an adopted transcript is newer.
    pub started_ms: i64,
}

/// Whether `session` is the only CSM Claude session working in its folder.
pub(crate) fn sole_claude_in_folder(sessions: &[csm_core::models::ShellSession], session: &csm_core::models::ShellSession) -> bool {
    !sessions.iter().any(|o| {
        o.id != session.id
            && o.kind == csm_core::models::SessionKind::Local
            && crate::launch::is_claude_command(&o.command)
            && discover::same_folder(&o.working_folder, &session.working_folder)
    })
}

enum Msg {
    Links(Vec<Link>),
    Mode(Mode),
    /// The imported history was deleted from the index: forget the read positions held.
    Forget,
    Shutdown,
}

/// What the reader does ([module docs](self)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct Mode {
    /// The Claude usage add-on is on: requests are stored and priced.
    pub usage: bool,
    /// And every past transcript is imported.
    pub history: bool,
}

/// The reader's mode for these settings in this run.
pub(crate) fn mode_of(settings: &csm_core::models::AppSettings, options: &crate::state::StateOptions) -> Mode {
    let usage = crate::addons::claude_usage_on(settings);
    Mode { usage, history: usage && settings.claude_usage_history && history_allowed(options) }
}

/// Whether this run may read past Claude history at all: never under `--clean` or an isolated
/// run (an index of its own with no state file), which start fresh and count only the sessions
/// they start. The add-on's stored on/off state is left as it is.
pub(crate) fn history_allowed(options: &crate::state::StateOptions) -> bool {
    !options.clean && !(matches!(options.index, crate::index::IndexSetting::Auto) && options.save_path.is_none())
}

/// A followed session's context, from the light tail (the add-on off).
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LiveContext {
    /// Its newest main-thread request (unpriced).
    pub last_main: Option<UsageRequest>,
    /// The largest main-thread prompt seen (a 1M window tell).
    pub largest_main_prompt: u64,
}

/// What the price table is, for the tools to report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PriceSource {
    /// The override file looked for (`None` when the run keeps no files).
    pub override_path: Option<String>,
    /// `embedded`, `override (N models)`, or why the file was ignored.
    pub status: String,
}

/// What the thread publishes for the engine: each followed session's subagents and Remote
/// Control lines.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// Bumped when `map` changes.
    pub generation: u64,
    pub map: HashMap<String, SubagentsReport>,
    /// The last Remote Control line of each followed session's main transcript.
    pub remote: HashMap<String, csm_core::remote_control::RemoteControlMark>,
    /// Where reading the history (every transcript, not only followed sessions) is.
    pub history: HistoryStatus,
    /// Each followed session's context while the add-on is off (empty while it is on: the index
    /// answers then).
    pub context: HashMap<String, LiveContext>,
    /// Each followed session's main transcript, as last found (`claude_reply_get` reads it).
    pub transcripts: HashMap<String, PathBuf>,
}

/// The history reader's progress, for `usage_report`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct HistoryStatus {
    /// The transcripts were listed at least once.
    pub scanned: bool,
    /// Main transcripts found by the last listing.
    pub files: usize,
    /// Of those, still to look at in this round.
    pub pending: usize,
}

/// The engine's handle to the thread.
pub(crate) struct UsageTail {
    pub subagents: Arc<Mutex<Shared>>,
    seen_gen: u64,
    tx: Sender<Msg>,
    last: Vec<Link>,
    last_check: Option<Instant>,
    pub prices: Arc<OnceLock<PriceSource>>,
    /// The price table the reader uses (set once it has loaded it).
    pub table: Arc<OnceLock<Arc<PriceTable>>>,
    handle: Option<thread::JoinHandle<()>>,
    /// The mode last sent to the thread.
    pub mode: Mode,
}

/// Bytes read from one file per pass; the rest is read on the next one (straight away).
const BUDGET: u64 = 16 * 1024 * 1024;
/// How often the engine re-checks which sessions to follow.
const LINK_CHECK: Duration = Duration::from_millis(1000);
/// How long a folder-based guess is reused before looking again.
const GUESS_TTL: Duration = Duration::from_secs(20);
/// How often a session followed by id checks its folder for a newer conversation (a `/clear`).
const NEWER_CHECK: Duration = Duration::from_secs(3);
/// How soon a transcript that was not found is looked for again (in its own folder's).
const MISSING_RETRY: Duration = Duration::from_secs(2);
/// Every project folder is searched for a known conversation not in its own folder's at most
/// once per this many polls (10 s in production).
const FULL_SCAN_POLLS: u32 = 5;
/// Every transcript under the roots is listed again once per this many polls (60 s in
/// production), to read the history and conversations CSM didn't start.
const HISTORY_POLLS: u32 = 30;
/// The most transcripts the history listing takes (newest first).
const HISTORY_MAX_FILES: usize = 20_000;
/// History files with new bytes read per pass, between the followed sessions' passes.
const HISTORY_READS_PER_PASS: usize = 8;
/// History files merely checked (stat, unchanged) per pass.
const HISTORY_CHECKS_PER_PASS: usize = 400;

impl UsageTail {
    /// Starts the thread when the run has an index and transcript roots.
    pub(crate) fn start(index: Option<&Index>, roots: &ClaudeRoots, save_path: Option<&Path>, poll: Duration, mode: Mode) -> Option<UsageTail> {
        let index = index?.clone();
        if matches!(roots, ClaudeRoots::None) {
            return None;
        }
        let roots = roots.clone();
        let override_path = save_path.and_then(Path::parent).map(|d| d.join(cu::OVERRIDE_FILE));
        let prices = Arc::new(OnceLock::new());
        let table = Arc::new(OnceLock::new());
        let cells = Cells { source: prices.clone(), table: table.clone() };
        let shared = Arc::new(Mutex::new(Shared::default()));
        let shared2 = shared.clone();
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("csm-usage".into())
            .spawn(move || run(rx, index, roots, override_path, poll, cells, shared2, mode))
            .map_err(|e| log::warn!("usage: could not start the transcript reader: {e}"))
            .ok()?;
        Some(UsageTail { subagents: shared, seen_gen: 0, tx, last: Vec::new(), last_check: None, prices, table, handle: Some(handle), mode })
    }

    /// The imported history was deleted from the index: the thread drops its read positions.
    pub(crate) fn forget(&self) {
        let _ = self.tx.send(Msg::Forget);
    }

    pub(crate) fn shutdown(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// The sessions to follow: live local Claude sessions. Pure.
pub(crate) fn links(state: &State) -> Vec<Link> {
    state
        .app
        .sessions
        .iter()
        .filter(|s| s.kind == csm_core::models::SessionKind::Local && crate::launch::is_claude_command(&s.command))
        .filter_map(|s| state.rt.sessions.get(&s.id).map(|rt| (s, rt.started_ms)))
        .map(|(s, started_ms)| Link {
            csm_session: s.id.clone(),
            claude_sid: s.claude_session_id.clone().filter(|c| discover::is_safe_session_id(c)),
            folder: s.working_folder.trim().to_owned(),
            sole: sole_claude_in_folder(&state.app.sessions, s),
            started_ms,
        })
        .collect()
}

/// From the engine's tick: tells the thread when the followed sessions changed. Cheap.
pub(crate) fn tick(state: &mut State) {
    take_subagents(state);
    configure(state);
    let due = match state.rt.usage.as_ref() {
        Some(u) => u.last_check.is_none_or(|t| t.elapsed() >= LINK_CHECK),
        None => false,
    };
    if !due {
        return;
    }
    let now = links(state);
    if let Some(u) = state.rt.usage.as_mut() {
        u.last_check = Some(Instant::now());
        if now != u.last {
            u.last = now.clone();
            let _ = u.tx.send(Msg::Links(now));
        }
    }
}

/// Tells the thread when the add-on or its history setting changed (from the tick, and at once
/// from `addon_enable` / `addon_disable`). Cheap: a map lookup and two booleans.
pub(crate) fn configure(state: &mut State) {
    let mode = mode_of(&state.app.settings, &state.options);
    if let Some(u) = state.rt.usage.as_mut()
        && u.mode != mode
    {
        u.mode = mode;
        let _ = u.tx.send(Msg::Mode(mode));
        state.rt.dirty = true;
    }
}

/// `(price key, cost)` of a request with `table`; `(None, None)` when unpriced.
pub(crate) fn price_of(table: &PriceTable, r: &UsageRequest) -> (Option<String>, Option<f64>) {
    match table.price(&r.model, &r.tokens, r.fast) {
        Some(p) => (Some(p.key), Some(p.cost_usd)),
        None => (None, None),
    }
}

fn load_prices(override_path: Option<&Path>) -> (PriceTable, PriceSource) {
    let text = override_path.and_then(|p| std::fs::read_to_string(p).ok());
    let (table, status) = PriceTable::embedded().with_override(text.as_deref());
    let status = match status {
        OverrideStatus::None => "embedded".to_owned(),
        OverrideStatus::Applied(n) => format!("embedded + override ({n} models)"),
        OverrideStatus::Ignored(why) => {
            log::warn!("usage: {} ignored: {why}", override_path.map(|p| p.display().to_string()).unwrap_or_default());
            format!("embedded (override ignored: {why})")
        }
    };
    (table, PriceSource { override_path: override_path.map(|p| p.display().to_string()), status })
}

fn resolve_roots(roots: &ClaudeRoots) -> Vec<PathBuf> {
    match roots {
        ClaudeRoots::None => Vec::new(),
        ClaudeRoots::Auto => discover::claude_roots(),
        ClaudeRoots::Dirs(d) => d.clone(),
    }
}

struct Tailer {
    index: Index,
    table: Arc<PriceTable>,
    roots: Vec<PathBuf>,
    /// Read positions, by path (loaded from the index the first time a file is seen).
    files: HashMap<String, ClaudeFile>,
    /// The main transcript of each followed session, as last looked up.
    mains: HashMap<String, MainLook>,
    /// How often every project folder may be searched for a transcript ([`FULL_SCAN_POLLS`]).
    full_every: Duration,
    /// Parsed subagent meta files and workflow journals.
    cache: sub::Cache,
    shared: Arc<Mutex<Shared>>,
    /// The history still to look at in this round (newest first), and when it was listed.
    history: std::collections::VecDeque<PathBuf>,
    history_at: Option<Instant>,
    history_every: Duration,
    history_files: usize,
    /// The light tail's positions (the add-on off), by path.
    light: HashMap<String, LightFile>,
}

/// The reader's two cells the engine reads: what the price table is, and the table itself.
struct Cells {
    source: Arc<OnceLock<PriceSource>>,
    table: Arc<OnceLock<Arc<PriceTable>>>,
}

/// Where a followed session's main transcript was found (or not), and when.
struct MainLook {
    path: Option<PathBuf>,
    at: Instant,
    /// The conversation id it was looked up for.
    sid: Option<String>,
    /// When every project folder was last searched for it.
    full_at: Option<Instant>,
    /// When the folder was last checked for a newer conversation (a `/clear`).
    newer_at: Option<Instant>,
}

#[allow(clippy::too_many_arguments)]
fn run(rx: Receiver<Msg>, index: Index, roots: ClaudeRoots, override_path: Option<PathBuf>, poll: Duration, cells: Cells, shared: Arc<Mutex<Shared>>, mode: Mode) {
    let (table, source) = load_prices(override_path.as_deref());
    let _ = cells.source.set(source);
    let table = Arc::new(table);
    let _ = cells.table.set(table.clone());
    // A new price table (an app update or an edited override): price everything again.
    let (sig, t2) = (table.signature(), table.clone());
    index.call(
        move |db| {
            if db.claude_meta("price_sig")?.as_deref() != Some(sig.as_str()) {
                let n = db.claude_reprice(&|r| price_of(&t2, r))?;
                db.claude_set_meta("price_sig", &sig)?;
                if n > 0 {
                    log::info!("usage: priced {n} stored requests again with the current price table");
                }
            }
            Ok(())
        },
        |r: Result<(), csm_index::IndexError>| {
            if let Err(e) = r {
                log::warn!("usage: {e}");
            }
        },
    );
    let full_every = poll.saturating_mul(FULL_SCAN_POLLS);
    let mut t = Tailer {
        index,
        table,
        roots: resolve_roots(&roots),
        files: HashMap::new(),
        mains: HashMap::new(),
        full_every,
        cache: sub::Cache::default(),
        shared,
        history: std::collections::VecDeque::new(),
        history_at: None,
        history_every: poll.saturating_mul(HISTORY_POLLS),
        history_files: 0,
        light: HashMap::new(),
    };
    let mut roots_at = Instant::now();
    let mut links: Vec<Link> = Vec::new();
    let mut more = false;
    let mut mode = mode;
    loop {
        let wait = if more { Duration::ZERO } else { poll };
        let mut msgs = Vec::new();
        match rx.recv_timeout(wait) {
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(m) => msgs.push(m),
            Err(RecvTimeoutError::Timeout) => {}
        }
        loop {
            match rx.try_recv() {
                Ok(Msg::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => return,
                Ok(m) => msgs.push(m),
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        for m in msgs {
            match m {
                Msg::Links(l) => links = l,
                Msg::Mode(m) => {
                    t.switch(mode, m);
                    mode = m;
                }
                Msg::Forget => t.forget(),
                Msg::Shutdown => return,
            }
        }
        if roots_at.elapsed() >= Duration::from_secs(60) {
            t.roots = resolve_roots(&roots);
            roots_at = Instant::now();
        }
        t.mains.retain(|k, _| links.iter().any(|l| &l.csm_session == k));
        t.light.retain(|_, f| links.iter().any(|l| l.csm_session == f.session));
        more = false;
        for link in &links {
            more |= if mode.usage { t.pass(link) } else { t.light_pass(link) };
        }
        t.prune_reports(&links);
        if mode.history {
            more |= t.history_pass();
        }
    }
}

/// Where the light tail (the add-on off) is in one main transcript. Memory only.
#[derive(Debug, Clone, Default)]
struct LightFile {
    seen: bool,
    session: String,
    size: u64,
    mtime_ms: i64,
    offset: u64,
    head: Vec<u8>,
    lines: u64,
    context: LiveContext,
}

impl Tailer {
    /// The add-on or its history setting changed: drop what the old mode held.
    fn switch(&mut self, from: Mode, to: Mode) {
        if from.usage != to.usage {
            // Off: the stored positions and the history go (the index keeps its rows); on: the
            // light tail goes and the index answers again, carrying on from its positions.
            self.files.clear();
            self.files.shrink_to_fit();
            self.light.clear();
            self.light.shrink_to_fit();
            let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
            sh.context.clear();
            sh.generation += 1;
        }
        if from.history && !to.history || from.usage != to.usage {
            self.history.clear();
            self.history.shrink_to_fit();
            self.history_at = None;
            self.history_files = 0;
            let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
            sh.history = HistoryStatus::default();
        }
    }

    /// The imported history was deleted from the index: every position held may be stale.
    fn forget(&mut self) {
        self.files.clear();
        self.history.clear();
        self.history_at = None;
        self.history_files = 0;
    }

    /// The light tail of a link (the add-on off): its main transcript only, in memory.
    fn light_pass(&mut self, link: &Link) -> bool {
        let Some((_, main)) = self.main_of(link) else {
            self.publish_transcript(link, None);
            return false;
        };
        self.publish_transcript(link, Some(&main));
        let more = self.light_file(link, &main);
        let found = sub::find(&main);
        self.report(link, &found, false);
        more
    }

    /// Reads what is new in a followed session's main transcript for its context fill and its
    /// Remote Control line; stores nothing.
    fn light_file(&mut self, link: &Link, path: &Path) -> bool {
        let key = path.to_string_lossy().into_owned();
        let Ok((size, mtime, head)) = tail::stat(path) else { return false };
        let cur = self.light.get(&key).cloned().unwrap_or_default();
        let known = cur.seen && cur.session == link.csm_session;
        if known && cur.size == size && cur.mtime_ms == mtime {
            return false;
        }
        let reset = !known || (cur.offset > 0 && tail::was_rewritten(size, &head, cur.offset, &cur.head));
        let (offset, lines, mut context) = if reset { (0, 0, LiveContext::default()) } else { (cur.offset, cur.lines, cur.context) };
        let mut batch = Batch::after(lines);
        let pass = match tail::read_lines(path, offset, BUDGET, |l| batch.push(l)) {
            Ok(p) => p,
            Err(e) => {
                log::debug!("usage: could not read {key}: {e}");
                return false;
            }
        };
        for r in batch.requests.iter().filter(|r| r.agent_id.is_none()) {
            context.largest_main_prompt = context.largest_main_prompt.max(r.tokens.prompt());
            context.last_main = Some(UsageRequest {
                id: r.id.clone(),
                claude_sid: String::new(),
                agent_id: None,
                ts_ms: r.ts_ms,
                model: r.model.clone(),
                price_key: None,
                fast: r.fast,
                tokens: r.tokens,
                cost_usd: None,
            });
        }
        if let Some(mark) = batch.remote_control.take() {
            self.publish_remote(link, mark);
        }
        {
            let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
            if sh.context.get(&link.csm_session) != Some(&context) {
                sh.context.insert(link.csm_session.clone(), context.clone());
                sh.generation += 1;
            }
        }
        let size_seen = if pass.more { 0 } else { size };
        self.light.insert(key, LightFile { seen: true, session: link.csm_session.clone(), size: size_seen, mtime_ms: mtime, offset: pass.end, head, lines: batch.lines + pass.oversized, context });
        pass.more
    }

    /// Where a followed session's main transcript is (or that none was found), for the engine.
    fn publish_transcript(&self, link: &Link, path: Option<&PathBuf>) {
        let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if sh.transcripts.get(&link.csm_session) == path {
            return;
        }
        match path {
            Some(p) => sh.transcripts.insert(link.csm_session.clone(), p.clone()),
            None => sh.transcripts.remove(&link.csm_session),
        };
        sh.generation += 1;
    }

    /// A followed session's last Remote Control line, for the engine.
    fn publish_remote(&self, link: &Link, mark: csm_core::remote_control::RemoteControlMark) {
        let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if sh.remote.get(&link.csm_session) != Some(&mark) {
            sh.remote.insert(link.csm_session.clone(), mark);
            sh.generation += 1;
        }
    }

    /// The main transcript of a link: by its id, else by the folder guess (cached a while).
    fn main_of(&mut self, link: &Link) -> Option<(String, PathBuf)> {
        let prev = self.mains.get(&link.csm_session).filter(|m| m.sid == link.claude_sid);
        if let Some(m) = prev
            && m.path.as_ref().is_some_and(|p| p.is_file())
            && (link.claude_sid.is_some() || m.at.elapsed() < GUESS_TTL)
        {
            let mut p = m.path.clone()?;
            // Followed by id: a `/clear` moved the session to a new conversation that nobody told
            // CSM about. Adopt the folder's newer transcript (the engine then stores its id).
            if link.claude_sid.is_some() && link.sole && m.newer_at.is_none_or(|t| t.elapsed() >= NEWER_CHECK) {
                if let Some(newer) = self.newer_in_folder(link, &p) {
                    p = newer;
                }
                if let Some(look) = self.mains.get_mut(&link.csm_session) {
                    look.newer_at = Some(Instant::now());
                    look.path = Some(p.clone());
                }
            }
            let id = p.file_stem()?.to_string_lossy().into_owned();
            return Some((id, p));
        }
        if prev.is_some_and(|m| m.at.elapsed() < MISSING_RETRY) {
            return None; // looked a moment ago and found nothing
        }
        let mut full_at = prev.and_then(|m| m.full_at);
        let found = match &link.claude_sid {
            // Its own folder's project directories every couple of seconds (a conversation nothing
            // was said in yet has no transcript); every project folder only now and then.
            Some(sid) => discover::find_in_folder(&self.roots, sid, &link.folder)
                .or_else(|| {
                    if full_at.is_some_and(|t| t.elapsed() < self.full_every) {
                        return None;
                    }
                    full_at = Some(Instant::now());
                    discover::find_transcript(&self.roots, sid)
                })
                .map(|p| (sid.clone(), p)),
            None => discover::newest_transcript_for_folder(&self.roots, &link.folder),
        };
        let look = MainLook { path: found.as_ref().map(|(_, p)| p.clone()), at: Instant::now(), sid: link.claude_sid.clone(), full_at, newer_at: None };
        self.mains.insert(link.csm_session.clone(), look);
        found
    }

    /// The folder's newest transcript when it is not `current`, was written after `current` and
    /// after the session's process started: the conversation a `/clear` started.
    fn newer_in_folder(&self, link: &Link, current: &Path) -> Option<PathBuf> {
        let (_, newest) = discover::newest_transcript_for_folder(&self.roots, &link.folder)?;
        if newest == current {
            return None;
        }
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let (new_m, cur_m) = (mtime(&newest)?, mtime(current)?);
        let new_ms = i64::try_from(new_m.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis()).ok()?;
        (new_m > cur_m && new_ms >= link.started_ms).then_some(newest)
    }

    /// One pass over a link's transcripts. True when a file has more to read straight away.
    fn pass(&mut self, link: &Link) -> bool {
        let Some((sid, main)) = self.main_of(link) else {
            self.publish_transcript(link, None);
            return false;
        };
        self.publish_transcript(link, Some(&main));
        let mut more = self.file(Some(link), &sid, None, &main, None).more;
        let project = self.files.get(&main.to_string_lossy().into_owned()).and_then(|f| f.project.clone());
        let found = sub::find(&main);
        for f in &found {
            more |= self.file(Some(link), &sid, Some(f.id.clone()), &f.transcript, project.clone()).more;
        }
        self.report(link, &found, true);
        more
    }

    /// Reads a little of the history: every transcript under the roots, not only the followed
    /// sessions' (the usage report's past days and conversations CSM didn't start). A few files
    /// with new bytes per pass, so the followed sessions never wait long; listed again now and
    /// then. True while there is more to read.
    fn history_pass(&mut self) -> bool {
        if self.history.is_empty() && self.history_at.is_none_or(|t| t.elapsed() >= self.history_every) {
            if self.history_at.is_none() {
                // The first listing: every stored read position at once, not one query per file.
                let known = self.index.call_blocking(|db| db.claude_files_all()).unwrap_or_default();
                for f in known {
                    self.files.entry(f.path.clone()).or_insert(f);
                }
            }
            let list = discover::all_transcripts(&self.roots, HISTORY_MAX_FILES);
            self.history_files = list.len();
            self.history = list.into_iter().map(|(p, _)| p).collect();
            self.history_at = Some(Instant::now());
        }
        let followed: Vec<PathBuf> = self.mains.values().filter_map(|m| m.path.clone()).collect();
        let (mut reads, mut checks) = (0, 0);
        while reads < HISTORY_READS_PER_PASS && checks < HISTORY_CHECKS_PER_PASS {
            let Some(main) = self.history.pop_front() else { break };
            checks += 1;
            // A followed session's files are read by its own pass.
            if followed.iter().any(|f| f == &main) {
                continue;
            }
            let Some(sid) = main.file_stem().map(|s| s.to_string_lossy().into_owned()) else { continue };
            let read = self.file(None, &sid, None, &main, None);
            let mut more = read.more;
            reads += usize::from(read.read);
            let project = self.files.get(&main.to_string_lossy().into_owned()).and_then(|f| f.project.clone());
            for (agent, path) in discover::subagent_files(&main) {
                let r = self.file(None, &sid, Some(agent), &path, project.clone());
                more |= r.more;
                reads += usize::from(r.read);
            }
            if more {
                // Mid-way through a big file: on with it next pass.
                self.history.push_front(main);
            }
        }
        let status = HistoryStatus { scanned: true, files: self.history_files, pending: self.history.len() };
        let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        sh.history = status;
        !self.history.is_empty()
    }

    /// The link's subagents: what the disk and the index say, published for the engine when it
    /// changed. Reads only small files, each again only when it changed.
    fn report(&mut self, link: &Link, found: &[sub::Found], usage: bool) {
        self.cache.retain(found);
        let report = if found.is_empty() {
            None
        } else {
            let sid = link.csm_session.clone();
            // Their tokens and cost only while the add-on is on (the index has them then).
            let mut usage: HashMap<String, csm_core::subagents::AgentUsage> = if usage {
                self.index.call_blocking(move |db| db.claude_agents_for(&sid)).unwrap_or_default().into_iter().collect()
            } else {
                HashMap::new()
            };
            let mut runs: Vec<RunInput> = Vec::new();
            let mut agents: Vec<AgentInput> = Vec::new();
            for f in found {
                if let Some((rid, dir)) = &f.run
                    && !runs.iter().any(|r| &r.id == rid)
                {
                    runs.push(RunInput { id: rid.clone(), journal: self.cache.journal(dir) });
                }
                agents.push(AgentInput {
                    id: f.id.clone(),
                    run: f.run.as_ref().map(|(r, _)| r.clone()),
                    meta: self.cache.meta(&f.transcript),
                    mtime_ms: f.mtime_ms,
                    usage: usage.remove(&f.id).unwrap_or_default(),
                });
            }
            Some(csm_core::subagents::derive(agents, runs, crate::session::now_ms()))
        };
        let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        match report {
            Some(r) => {
                if sh.map.get(&link.csm_session) != Some(&r) {
                    sh.map.insert(link.csm_session.clone(), r);
                    sh.generation += 1;
                }
            }
            None => {
                if sh.map.remove(&link.csm_session).is_some() {
                    sh.generation += 1;
                }
            }
        }
    }

    /// Drops the reports of sessions no longer followed.
    fn prune_reports(&mut self, links: &[Link]) {
        let mut sh = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        let before = sh.map.len() + sh.remote.len() + sh.context.len() + sh.transcripts.len();
        sh.map.retain(|k, _| links.iter().any(|l| &l.csm_session == k));
        sh.remote.retain(|k, _| links.iter().any(|l| &l.csm_session == k));
        sh.context.retain(|k, _| links.iter().any(|l| &l.csm_session == k));
        sh.transcripts.retain(|k, _| links.iter().any(|l| &l.csm_session == k));
        if sh.map.len() + sh.remote.len() + sh.context.len() + sh.transcripts.len() != before {
            sh.generation += 1;
        }
    }

    /// Reads what is new in one transcript and stores it. `link`: the followed session it
    /// belongs to; `None` for the history, which keeps whatever session the file was stored
    /// with. `project`: a subagent's (its main transcript's).
    fn file(&mut self, link: Option<&Link>, sid: &str, agent: Option<String>, path: &Path, project: Option<String>) -> FileRead {
        let key = path.to_string_lossy().into_owned();
        let Ok((size, mtime, head)) = tail::stat(path) else { return FileRead::default() };
        let cur = match (self.files.get(&key), link) {
            (Some(c), _) => c.clone(),
            // The history's first listing loaded every stored position.
            (None, None) => ClaudeFile::default(),
            (None, Some(_)) => {
                let k = key.clone();
                self.index.call_blocking(move |db| db.claude_file(&k)).ok().flatten().unwrap_or_default()
            }
        };
        let session = match link {
            Some(l) => Some(l.csm_session.clone()),
            None => cur.csm_session.clone(),
        };
        if cur.size == size && cur.mtime_ms == mtime && cur.csm_session == session && !cur.path.is_empty() {
            self.files.insert(key, cur);
            return FileRead::default();
        }
        // Read from history before a CSM session followed it: its rows take the session now.
        let claim = link.is_some() && !cur.path.is_empty() && cur.csm_session.is_none();
        let reset = cur.offset > 0 && tail::was_rewritten(size, &head, cur.offset, &cur.head);
        let (offset, lines, bad, unknown) = if reset { (0, 0, 0, 0) } else { (cur.offset, cur.lines, cur.bad_lines, cur.unknown_lines) };
        let mut batch = Batch::after(lines);
        let pass = match tail::read_lines(path, offset, BUDGET, |l| batch.push(l)) {
            Ok(p) => p,
            Err(e) => {
                log::debug!("usage: could not read {key}: {e}");
                return FileRead::default();
            }
        };
        let folder = link.map_or("", |l| l.folder.as_str());
        // A transcript without a `cwd`: the project directory's name stands in.
        let dir_name = || path.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let project = project
            .or_else(|| if reset { None } else { cur.project.clone() })
            .or_else(|| batch.cwd.as_deref().map(|c| project_name(c, folder)))
            .or_else(|| (!folder.is_empty()).then(|| project_name(folder, "")))
            .or_else(|| link.is_none().then(dir_name).filter(|d| !d.is_empty()));
        let requests: Vec<UsageRequest> = batch
            .requests
            .iter()
            .map(|r| UsageRequest {
                id: r.id.clone(),
                claude_sid: sid.to_owned(),
                agent_id: agent.clone().or_else(|| r.agent_id.clone()),
                ts_ms: r.ts_ms,
                model: r.model.clone(),
                price_key: None,
                fast: r.fast,
                tokens: r.tokens,
                cost_usd: None,
            })
            .collect();
        let compactions: Vec<(u64, Compaction)> = batch
            .compactions
            .iter()
            .map(|(n, c)| {
                (*n, Compaction {
                    ts_ms: c.ts_ms,
                    agent_id: agent.clone().or_else(|| c.agent_id.clone()),
                    trigger: c.trigger.clone(),
                    pre_tokens: c.pre_tokens,
                    post_tokens: c.post_tokens,
                })
            })
            .collect();
        // Remote Control is the conversation's: only its main transcript says.
        if agent.is_none()
            && let Some(link) = link
            && let Some(mark) = batch.remote_control.take()
        {
            self.publish_remote(link, mark);
        }
        let file = ClaudeFile {
            path: key.clone(),
            claude_sid: sid.to_owned(),
            agent_id: agent,
            csm_session: session,
            project,
            // Mid-way through a big file the size seen is not done yet: the next pass reads on.
            size: if pass.more { 0 } else { size },
            mtime_ms: mtime,
            offset: pass.end,
            head,
            lines: batch.lines + pass.oversized,
            bad_lines: bad + batch.bad_lines + pass.oversized,
            unknown_lines: unknown + batch.unknown_lines,
        };
        self.files.insert(key, file.clone());
        let ingest = ClaudeIngest { file, reset, claim, requests, compactions };
        let table = self.table.clone();
        self.index.call(
            move |db| db.claude_ingest(&ingest, &|r| price_of(&table, r)),
            |r| {
                if let Err(e) = r {
                    log::warn!("usage: could not store usage: {e}");
                }
            },
        );
        FileRead { more: pass.more, read: true }
    }
}

/// What [`Tailer::file`] did.
#[derive(Debug, Clone, Copy, Default)]
struct FileRead {
    /// The file has more to read straight away (the byte budget ran out).
    more: bool,
    /// It was read (it changed, or changed hands).
    read: bool,
}

impl Drop for UsageTail {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}

/// Copies the reader's subagent reports and Remote Control lines into the state when they
/// changed (marks it dirty so a snapshot follows). Never waits: a busy lock is tried again on
/// the next tick.
fn take_subagents(state: &mut State) {
    let Some(u) = state.rt.usage.as_mut() else { return };
    let Ok(sh) = u.subagents.try_lock() else { return };
    if sh.generation == u.seen_gen {
        return;
    }
    u.seen_gen = sh.generation;
    let map: HashMap<String, Arc<SubagentsReport>> = sh.map.iter().map(|(k, v)| (k.clone(), Arc::new(v.clone()))).collect();
    let remote = sh.remote.clone();
    let context = sh.context.clone();
    let transcripts = sh.transcripts.clone();
    drop(sh);
    state.rt.subagents = map;
    state.rt.live_context = context;
    adopt_conversations(state, &transcripts);
    state.rt.claude_transcripts = transcripts;
    apply_remote_control(state, &remote);
    state.rt.dirty = true;
}

/// A followed session whose transcript is now another conversation (the tail adopted the one a
/// `/clear` started) stores that id, so its fill, cost, replies and a later resume follow it.
pub(crate) fn adopt_conversations(state: &mut State, transcripts: &HashMap<String, PathBuf>) {
    let mut changed = false;
    for (id, path) in transcripts {
        let Some(cid) = path.file_stem().and_then(|s| s.to_str()).filter(|c| discover::is_safe_session_id(c)) else { continue };
        let Some(s) = state.app.sessions.iter_mut().find(|s| &s.id == id) else { continue };
        if s.claude_session_id.is_some() && s.claude_session_id.as_deref() != Some(cid) {
            log::info!("usage: session {id} moved to Claude conversation {cid} (a /clear)");
            s.claude_session_id = Some(cid.to_owned());
            changed = true;
        }
    }
    if changed {
        state.persist();
    }
}

/// Folds each session's last Remote Control line into its runtime: only into a running process,
/// and only a line written since that process started (`csm_core::remote_control::apply`).
pub(crate) fn apply_remote_control(state: &mut State, marks: &HashMap<String, csm_core::remote_control::RemoteControlMark>) {
    for (id, mark) in marks {
        let Some(rt) = state.rt.sessions.get_mut(id) else { continue };
        if rt.phase != crate::session::Phase::Live {
            continue;
        }
        let next = csm_core::remote_control::apply(rt.remote_control.clone(), mark, rt.started_ms);
        if next != rt.remote_control {
            rt.remote_control = next;
            state.rt.dirty = true;
        }
    }
}
