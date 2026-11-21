//! Follows the Claude transcripts of live Claude sessions on a background thread (`csm-usage`)
//! and stores their per-request numbers in the index (`claude_*` tables).
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
}

enum Msg {
    Links(Vec<Link>),
    Shutdown,
}

/// What the price table is, for the tools to report.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PriceSource {
    /// The override file looked for (`None` when the run keeps no files).
    pub override_path: Option<String>,
    /// `embedded`, `override (N models)`, or why the file was ignored.
    pub status: String,
}

/// What the thread publishes for the engine: each followed session's subagents.
#[derive(Debug, Default)]
pub(crate) struct Shared {
    /// Bumped when `map` changes.
    pub generation: u64,
    pub map: HashMap<String, SubagentsReport>,
}

/// The engine's handle to the thread.
pub(crate) struct UsageTail {
    pub subagents: Arc<Mutex<Shared>>,
    seen_gen: u64,
    tx: Sender<Msg>,
    last: Vec<Link>,
    last_check: Option<Instant>,
    pub prices: Arc<OnceLock<PriceSource>>,
    handle: Option<thread::JoinHandle<()>>,
}

/// Bytes read from one file per pass; the rest is read on the next one (straight away).
const BUDGET: u64 = 16 * 1024 * 1024;
/// How often the engine re-checks which sessions to follow.
const LINK_CHECK: Duration = Duration::from_millis(1000);
/// How long a folder-based guess is reused before looking again.
const GUESS_TTL: Duration = Duration::from_secs(20);
/// How soon a transcript that was not found is looked for again (in its own folder's).
const MISSING_RETRY: Duration = Duration::from_secs(2);
/// Every project folder is searched for a known conversation not in its own folder's at most
/// once per this many polls (10 s in production).
const FULL_SCAN_POLLS: u32 = 5;

impl UsageTail {
    /// Starts the thread when the run has an index and transcript roots.
    pub(crate) fn start(index: Option<&Index>, roots: &ClaudeRoots, save_path: Option<&Path>, poll: Duration) -> Option<UsageTail> {
        let index = index?.clone();
        if matches!(roots, ClaudeRoots::None) {
            return None;
        }
        let roots = roots.clone();
        let override_path = save_path.and_then(Path::parent).map(|d| d.join(cu::OVERRIDE_FILE));
        let prices = Arc::new(OnceLock::new());
        let cell = prices.clone();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let shared2 = shared.clone();
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("csm-usage".into())
            .spawn(move || run(rx, index, roots, override_path, poll, cell, shared2))
            .map_err(|e| log::warn!("usage: could not start the transcript reader: {e}"))
            .ok()?;
        Some(UsageTail { subagents: shared, seen_gen: 0, tx, last: Vec::new(), last_check: None, prices, handle: Some(handle) })
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
        .filter(|s| state.rt.sessions.contains_key(&s.id))
        .map(|s| Link {
            csm_session: s.id.clone(),
            claude_sid: s.claude_session_id.clone().filter(|c| discover::is_safe_session_id(c)),
            folder: s.working_folder.trim().to_owned(),
        })
        .collect()
}

/// From the engine's tick: tells the thread when the followed sessions changed. Cheap.
pub(crate) fn tick(state: &mut State) {
    take_subagents(state);
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
}

/// Where a followed session's main transcript was found (or not), and when.
struct MainLook {
    path: Option<PathBuf>,
    at: Instant,
    /// The conversation id it was looked up for.
    sid: Option<String>,
    /// When every project folder was last searched for it.
    full_at: Option<Instant>,
}

fn run(rx: Receiver<Msg>, index: Index, roots: ClaudeRoots, override_path: Option<PathBuf>, poll: Duration, cell: Arc<OnceLock<PriceSource>>, shared: Arc<Mutex<Shared>>) {
    let (table, source) = load_prices(override_path.as_deref());
    let _ = cell.set(source);
    let table = Arc::new(table);
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
    let mut t = Tailer { index, table, roots: resolve_roots(&roots), files: HashMap::new(), mains: HashMap::new(), full_every, cache: sub::Cache::default(), shared };
    let mut roots_at = Instant::now();
    let mut links: Vec<Link> = Vec::new();
    let mut more = false;
    loop {
        let wait = if more { Duration::ZERO } else { poll };
        match rx.recv_timeout(wait) {
            Ok(Msg::Links(l)) => links = l,
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => return,
            Err(RecvTimeoutError::Timeout) => {}
        }
        loop {
            match rx.try_recv() {
                Ok(Msg::Links(l)) => links = l,
                Ok(Msg::Shutdown) | Err(mpsc::TryRecvError::Disconnected) => return,
                Err(mpsc::TryRecvError::Empty) => break,
            }
        }
        if roots_at.elapsed() >= Duration::from_secs(60) {
            t.roots = resolve_roots(&roots);
            roots_at = Instant::now();
        }
        t.mains.retain(|k, _| links.iter().any(|l| &l.csm_session == k));
        more = false;
        for link in &links {
            more |= t.pass(link);
        }
        t.prune_reports(&links);
    }
}

impl Tailer {
    /// The main transcript of a link: by its id, else by the folder guess (cached a while).
    fn main_of(&mut self, link: &Link) -> Option<(String, PathBuf)> {
        let prev = self.mains.get(&link.csm_session).filter(|m| m.sid == link.claude_sid);
        if let Some(m) = prev
            && m.path.as_ref().is_some_and(|p| p.is_file())
            && (link.claude_sid.is_some() || m.at.elapsed() < GUESS_TTL)
        {
            let p = m.path.clone()?;
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
        let look = MainLook { path: found.as_ref().map(|(_, p)| p.clone()), at: Instant::now(), sid: link.claude_sid.clone(), full_at };
        self.mains.insert(link.csm_session.clone(), look);
        found
    }

    /// One pass over a link's transcripts. True when a file has more to read straight away.
    fn pass(&mut self, link: &Link) -> bool {
        let Some((sid, main)) = self.main_of(link) else { return false };
        let mut more = self.file(link, &sid, None, &main);
        let project = self.files.get(&main.to_string_lossy().into_owned()).and_then(|f| f.project.clone());
        let found = sub::find(&main);
        for f in &found {
            more |= self.file_with_project(link, &sid, Some(f.id.clone()), &f.transcript, project.clone());
        }
        self.report(link, &found);
        more
    }

    /// The link's subagents: what the disk and the index say, published for the engine when it
    /// changed. Reads only small files, each again only when it changed.
    fn report(&mut self, link: &Link, found: &[sub::Found]) {
        self.cache.retain(found);
        let report = if found.is_empty() {
            None
        } else {
            let sid = link.csm_session.clone();
            let mut usage: HashMap<String, csm_core::subagents::AgentUsage> =
                self.index.call_blocking(move |db| db.claude_agents_for(&sid)).unwrap_or_default().into_iter().collect();
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
        let before = sh.map.len();
        sh.map.retain(|k, _| links.iter().any(|l| &l.csm_session == k));
        if sh.map.len() != before {
            sh.generation += 1;
        }
    }

    fn file(&mut self, link: &Link, sid: &str, agent: Option<String>, path: &Path) -> bool {
        self.file_with_project(link, sid, agent, path, None)
    }

    fn file_with_project(&mut self, link: &Link, sid: &str, agent: Option<String>, path: &Path, project: Option<String>) -> bool {
        let key = path.to_string_lossy().into_owned();
        let Ok((size, mtime, head)) = tail::stat(path) else { return false };
        let cur = match self.files.get(&key) {
            Some(c) => c.clone(),
            None => {
                let k = key.clone();
                self.index.call_blocking(move |db| db.claude_file(&k)).ok().flatten().unwrap_or_default()
            }
        };
        let same_link = cur.csm_session.as_deref() == Some(link.csm_session.as_str());
        if cur.size == size && cur.mtime_ms == mtime && same_link && !cur.path.is_empty() {
            self.files.insert(key, cur);
            return false;
        }
        let reset = cur.offset > 0 && tail::was_rewritten(size, &head, cur.offset, &cur.head);
        let (offset, lines, bad, unknown) = if reset { (0, 0, 0, 0) } else { (cur.offset, cur.lines, cur.bad_lines, cur.unknown_lines) };
        let mut batch = Batch::after(lines);
        let pass = match tail::read_lines(path, offset, BUDGET, |l| batch.push(l)) {
            Ok(p) => p,
            Err(e) => {
                log::debug!("usage: could not read {key}: {e}");
                return false;
            }
        };
        let project = project
            .or_else(|| if reset { None } else { cur.project.clone() })
            .or_else(|| batch.cwd.as_deref().map(|c| project_name(c, &link.folder)))
            .or_else(|| (!link.folder.is_empty()).then(|| project_name(&link.folder, "")));
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
        let file = ClaudeFile {
            path: key.clone(),
            claude_sid: sid.to_owned(),
            agent_id: agent,
            csm_session: Some(link.csm_session.clone()),
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
        let ingest = ClaudeIngest { file, reset, requests, compactions };
        let table = self.table.clone();
        self.index.call(
            move |db| db.claude_ingest(&ingest, &|r| price_of(&table, r)),
            |r| {
                if let Err(e) = r {
                    log::warn!("usage: could not store usage: {e}");
                }
            },
        );
        pass.more
    }
}

impl Drop for UsageTail {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}

/// Copies the reader's subagent reports into the state when they changed (marks it dirty so a
/// snapshot follows). Never waits: a busy lock is tried again on the next tick.
fn take_subagents(state: &mut State) {
    let Some(u) = state.rt.usage.as_mut() else { return };
    let Ok(sh) = u.subagents.try_lock() else { return };
    if sh.generation == u.seen_gen {
        return;
    }
    u.seen_gen = sh.generation;
    let map: HashMap<String, Arc<SubagentsReport>> = sh.map.iter().map(|(k, v)| (k.clone(), Arc::new(v.clone()))).collect();
    drop(sh);
    state.rt.subagents = map;
    state.rt.dirty = true;
}
