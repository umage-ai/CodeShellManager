//! Git state per live session: the poll, the `.git` watcher, the busy slot per repository, and
//! the fields the snapshot shows (port of the WPF `SessionViewModel` git half).
//!
//! Rules carried over (the WPF CLAUDE.md, "Never spawn a process on the UI thread"):
//! - Every git spawn runs on a worker thread and reports back with `engine.post`; nothing here
//!   blocks the engine thread.
//! - Don't poll what you can watch: [`RepoWatchers`] catches checkout/commit/stage at once (a
//!   shared watcher per git directory); the poll is for working-tree edits only. Focused
//!   session 10 s, others 120 s, WSL 30 s (a WSL "not a repo" answer is cached: every probe is a
//!   `wsl.exe` spawn). Focusing a session refreshes it at once.
//! - A session that pushed its branch with OSC 9001 is not polled: the pushed value wins.
//! - One git operation per repository at a time, keyed by the canonical repo root
//!   ([`worktrees::busy_key`]), so every worktree of one repo shares the slot.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use csm_core::models::{SessionKind, ShellSession};
use csm_services::git::watch::{RepoWatchers, WatchGuard};
use csm_services::git::{GitRepoStatus, git_info, menu_header, run, worktrees};

use crate::engine::{Ctx, Engine};
use crate::state::State;

pub(crate) const FOREGROUND_POLL: Duration = Duration::from_secs(10);
pub(crate) const BACKGROUND_POLL: Duration = Duration::from_secs(120);
pub(crate) const WSL_POLL: Duration = Duration::from_secs(30);
/// Probes started at once (a 50-session restore must not spawn 50 gits in one tick).
const MAX_PROBES: usize = 4;
/// "Never again" for a cached WSL "not a repo" (until the folder changes).
const NEVER: Duration = Duration::from_secs(60 * 60 * 24 * 365);

/// The engine's git runtime (in `Runtime::git`).
#[derive(Default)]
pub(crate) struct GitRt {
    pub(crate) sessions: HashMap<String, SessionGit>,
    /// Busy keys of repositories with an operation running.
    pub(crate) busy: HashSet<String>,
    last_focused: Option<String>,
    pub(crate) watchers: RepoWatchers,
}

/// What the poll knows about one live session's folder.
pub(crate) struct SessionGit {
    pub(crate) folder: String,
    pub(crate) repo_root: Option<String>,
    pub(crate) worktree_of: Option<String>,
    pub(crate) status: Option<GitRepoStatus>,
    git_dir: Option<String>,
    /// Repo root, "worktree of" and the watcher were resolved (once per repo).
    located: bool,
    next_due: Instant,
    inflight: bool,
    /// A refresh was asked for while a probe ran: probe again when it lands.
    again: bool,
    _watch: Option<WatchGuard>,
}

impl SessionGit {
    fn new(folder: &str, now: Instant) -> Self {
        SessionGit {
            folder: folder.to_owned(),
            repo_root: None,
            worktree_of: None,
            status: None,
            git_dir: None,
            located: false,
            next_due: now,
            inflight: false,
            again: false,
            _watch: None,
        }
    }

    pub(crate) fn busy_key(&self) -> String {
        worktrees::busy_key(self.repo_root.as_deref(), Some(&self.folder))
    }
}

fn interval(wsl: bool, focused: bool) -> Duration {
    if wsl {
        WSL_POLL
    } else if focused {
        FOREGROUND_POLL
    } else {
        BACKGROUND_POLL
    }
}

/// A session whose folder git can be run in (local or WSL, with a folder).
pub(crate) fn has_git_folder(s: &ShellSession) -> bool {
    s.kind != SessionKind::Ssh && !s.working_folder.trim().is_empty()
}

/// Called on every engine wake-up: track live sessions, drop gone ones, start due probes.
pub(crate) fn tick(ctx: &mut Ctx<'_>) {
    let now = Instant::now();
    let state = &mut *ctx.state;
    let wanted: Vec<(String, String)> = state
        .app
        .sessions
        .iter()
        .filter(|s| !s.is_dormant && has_git_folder(s))
        .filter(|s| state.rt.sessions.get(&s.id).is_some_and(|rt| !rt.git_osc))
        .map(|s| (s.id.clone(), s.working_folder.clone()))
        .collect();
    let focused = state.rt.focused.clone();
    let git = &mut state.rt.git;
    let before = git.sessions.len();
    git.sessions.retain(|id, _| wanted.iter().any(|(w, _)| w == id));
    let mut changed = git.sessions.len() != before;
    for (id, folder) in &wanted {
        let fresh = git.sessions.get(id).is_none_or(|e| &e.folder != folder);
        if fresh {
            git.sessions.insert(id.clone(), SessionGit::new(folder, now));
            changed = true;
        }
    }
    if git.last_focused != focused {
        git.last_focused = focused.clone();
        if let Some(e) = focused.as_ref().and_then(|f| git.sessions.get_mut(f)) {
            if e.inflight {
                e.again = true;
            } else {
                e.next_due = now;
            }
        }
    }
    let running = git.sessions.values().filter(|e| e.inflight).count();
    let mut due: Vec<String> = git.sessions.iter().filter(|(_, e)| !e.inflight && now >= e.next_due).map(|(id, _)| id.clone()).collect();
    // The focused session first, then a stable order.
    due.sort_by_key(|id| (Some(id) != focused.as_ref(), id.clone()));
    for id in due.into_iter().take(MAX_PROBES.saturating_sub(running)) {
        start_probe(ctx, &id);
    }
    if changed {
        ctx.state.touch();
    }
}

/// What a probe found besides the status (resolved once per repo).
struct Located {
    root: Option<String>,
    worktree_of: Option<String>,
    git_dir: Option<String>,
    watch: Option<WatchGuard>,
}

fn start_probe(ctx: &mut Ctx<'_>, id: &str) {
    let watchers = ctx.state.rt.git.watchers.clone();
    let Some(e) = ctx.state.rt.git.sessions.get_mut(id) else { return };
    e.inflight = true;
    e.again = false;
    let folder = e.folder.clone();
    let locate = !e.located;
    let engine = ctx.engine.clone();
    let sid = id.to_owned();
    let spawned = std::thread::Builder::new().name("csm-git-poll".into()).spawn({
        let folder = folder.clone();
        move || {
            let status = run::status(&folder);
            let loc = (locate && status.is_some()).then(|| locate_repo(&folder, &engine, &watchers));
            engine.post(Box::new(move |ctx| apply_probe(ctx, &sid, &folder, status, loc)));
        }
    });
    if spawned.is_err()
        && let Some(e) = ctx.state.rt.git.sessions.get_mut(id)
    {
        e.inflight = false;
        e.next_due = Instant::now() + BACKGROUND_POLL;
    }
}

/// Repo root, "worktree of" and the watcher. On the probe's worker thread.
fn locate_repo(folder: &str, engine: &Engine, watchers: &RepoWatchers) -> Located {
    let root = run::repo_root(folder);
    let worktree_of = run::linked_worktree_of(folder, root.as_deref());
    let gd = run::git_dir(folder);
    let git_dir = gd.as_ref().map(|p| p.display().to_string());
    // `\\wsl$` change notifications are unreliable; WSL folders are polled instead.
    let watch = match (&gd, run::is_wsl_folder(folder)) {
        (Some(gd), false) => {
            let key = gd.display().to_string();
            let eng = engine.clone();
            watchers.acquire(
                gd,
                Arc::new(move || {
                    let k = key.clone();
                    eng.post(Box::new(move |ctx| on_git_dir_changed(ctx, &k)));
                }),
            )
        }
        _ => None,
    };
    Located { root, worktree_of, git_dir, watch }
}

fn apply_probe(ctx: &mut Ctx<'_>, id: &str, folder: &str, status: Option<GitRepoStatus>, loc: Option<Located>) {
    let now = Instant::now();
    let focused = ctx.state.rt.focused.as_deref() == Some(id);
    // Gone (closed, slept, folder edited): drop what the probe found.
    let Some(e) = ctx.state.rt.git.sessions.get_mut(id) else { return };
    if e.folder != folder {
        e.inflight = false;
        return;
    }
    e.inflight = false;
    let wsl = run::is_wsl_folder(folder);
    let mut changed = false;
    if let Some(l) = loc {
        e.located = true;
        changed |= e.repo_root != l.root || e.worktree_of != l.worktree_of;
        e.repo_root = l.root;
        e.worktree_of = l.worktree_of;
        e.git_dir = l.git_dir;
        e._watch = l.watch;
    }
    if status.is_none() && e.located {
        // No longer a repo (deleted, moved): locate again when it is one.
        e.located = false;
        e.repo_root = None;
        e.worktree_of = None;
        e.git_dir = None;
        e._watch = None;
        changed = true;
    }
    e.next_due = if e.again {
        now
    } else if status.is_none() && wsl {
        now + NEVER
    } else {
        now + interval(wsl, focused)
    };
    e.again = false;
    if e.status != status {
        e.status = status;
        changed = true;
    }
    if changed {
        ctx.state.touch();
    }
}

/// `HEAD` or `index` changed in a watched git directory: refresh the sessions using it.
fn on_git_dir_changed(ctx: &mut Ctx<'_>, git_dir: &str) {
    let now = Instant::now();
    for e in ctx.state.rt.git.sessions.values_mut().filter(|e| e.git_dir.as_deref() == Some(git_dir)) {
        if e.inflight {
            e.again = true;
        } else {
            e.next_due = now;
        }
    }
}

/// Re-reads git state for every live session on the repository with this busy key (after any
/// operation: a fetch moves only remote refs, which the watcher never sees).
pub(crate) fn refresh_repo(state: &mut State, key: &str) {
    let now = Instant::now();
    for e in state.rt.git.sessions.values_mut() {
        if e.busy_key() == key || worktrees::busy_key(None, Some(&e.folder)) == key {
            if e.inflight {
                e.again = true;
            } else {
                e.next_due = now;
            }
        }
    }
}

/// Claims the repository's busy slot. False when an operation is already running there.
pub(crate) fn claim(state: &mut State, key: &str) -> bool {
    let ok = state.rt.git.busy.insert(key.to_owned());
    if ok {
        state.touch();
    }
    ok
}

/// Releases the slot and refreshes the repository's sessions.
pub(crate) fn release(state: &mut State, key: &str) {
    state.rt.git.busy.remove(key);
    refresh_repo(state, key);
    state.touch();
}

/// The known repo root of a session (when the poll resolved it).
pub(crate) fn known_root(state: &State, id: &str) -> Option<String> {
    state.rt.git.sessions.get(id).and_then(|e| e.repo_root.clone())
}

/// Stores a status read by a tool (so the snapshot follows at once).
pub(crate) fn store_status(state: &mut State, id: &str, folder: &str, root: Option<&str>, status: Option<GitRepoStatus>) {
    if let Some(e) = state.rt.git.sessions.get_mut(id)
        && e.folder == folder
    {
        if e.repo_root.is_none() && root.is_some() {
            e.repo_root = root.map(str::to_owned);
        }
        if e.status != status {
            e.status = status;
            state.touch();
        }
    }
}

/// The git fields of a session's view.
#[derive(Debug, Default)]
pub(crate) struct GitFields {
    pub branch: Option<String>,
    pub dirty: bool,
    pub upstream: Option<String>,
    pub ahead: Option<i32>,
    pub behind: Option<i32>,
    pub changes: usize,
    pub worktree_of: Option<String>,
    pub repo_root: Option<String>,
    pub summary: Option<String>,
    pub busy: bool,
}

/// What the snapshot shows: the OSC 9001 push when there was one (it wins: the poll would show
/// the local folder's state over a remote one), else the poll's status.
pub(crate) fn view_fields(state: &State, s: &ShellSession) -> GitFields {
    let rt = state.rt.sessions.get(&s.id);
    if let Some(rt) = rt.filter(|r| r.git_osc) {
        return GitFields { branch: rt.git_branch.clone(), dirty: rt.git_dirty, ..GitFields::default() };
    }
    let Some(e) = state.rt.git.sessions.get(&s.id) else {
        return GitFields::default();
    };
    let busy = state.rt.git.busy.contains(&e.busy_key());
    let Some(st) = &e.status else {
        return GitFields { busy, repo_root: e.repo_root.clone(), ..GitFields::default() };
    };
    let (branch, dirty) = git_info(Some(st));
    let summary = menu_header(st.branch.as_deref(), st.upstream.as_deref(), st.ahead, st.behind, st.changes.len(), e.worktree_of.as_deref());
    GitFields {
        branch,
        dirty,
        upstream: st.upstream.clone(),
        ahead: st.ahead,
        behind: st.behind,
        changes: st.changes.len(),
        worktree_of: e.worktree_of.clone(),
        repo_root: e.repo_root.clone(),
        summary: Some(summary),
        busy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_operation_per_repository_shared_by_its_worktrees() {
        let mut state = State::default();
        let now = Instant::now();
        let mut main = SessionGit::new(r"C:\src\repo", now);
        main.repo_root = Some("C:/src/repo".into());
        let mut wt = SessionGit::new(r"C:\src\repo-feat", now);
        wt.repo_root = Some("c:/SRC/repo".into());
        let other = SessionGit::new(r"C:\src\other", now);
        assert_eq!(main.busy_key(), wt.busy_key(), "every worktree of a repo shares the slot");
        assert_ne!(main.busy_key(), other.busy_key());
        let key = main.busy_key();
        state.rt.git.sessions.insert("m".into(), main);
        state.rt.git.sessions.insert("w".into(), wt);
        assert!(claim(&mut state, &key));
        assert!(!claim(&mut state, &key), "a second operation on the same repo is refused");
        release(&mut state, &key);
        assert!(claim(&mut state, &key));
    }
}
