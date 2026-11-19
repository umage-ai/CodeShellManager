//! A shared, reference-counted watcher on a repository's `HEAD` and `index`, so git state
//! refreshes on checkout, commit and stage instead of by polling (port of `GitRepoWatcher`).
//!
//! * One OS watcher per git directory, however many sessions share it (several sessions in one
//!   repo is the normal case): [`RepoWatchers::acquire`] returns a [`WatchGuard`]; the watcher
//!   goes when the last guard is dropped.
//! * The git directory is the one [`super::run::git_dir`] resolved, so a linked worktree watches
//!   its own `HEAD` and `index` (`.git/worktrees/<name>`), not the main checkout's.
//! * Events are debounced (400 ms in the app): git rewrites `index` through `index.lock` and a
//!   rename, and a checkout touches both files.
//! * Working-tree edits don't touch `.git`, so the caller still polls; this only makes the
//!   things that do change `.git` immediate.
//!
//! Setting a watch is file-system I/O: acquire on a worker thread. Callbacks run on the
//! debounce thread and must not block (the engine posts a job).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread;
use std::time::Duration;

use notify::{RecursiveMode, Watcher};

/// Called (on the debounce thread) after `HEAD` or `index` changed and stayed quiet for the
/// debounce interval.
pub type Callback = Arc<dyn Fn() + Send + Sync>;

struct Entry {
    /// Kept for its `Drop`: the OS watch ends with it.
    _watcher: notify::RecommendedWatcher,
    subs: Vec<(u64, Callback)>,
}

type Map = Mutex<HashMap<PathBuf, Entry>>;

/// The set of watched git directories. Cheap to clone; clones share the set.
#[derive(Clone)]
pub struct RepoWatchers {
    inner: Arc<Map>,
    debounce: Duration,
    next: Arc<AtomicU64>,
}

impl Default for RepoWatchers {
    fn default() -> Self {
        Self::new(Duration::from_millis(400))
    }
}

/// Keeps a watch alive; dropping it releases this subscriber (and the watcher with the last).
pub struct WatchGuard {
    owner: Weak<Map>,
    key: PathBuf,
    id: u64,
}

impl std::fmt::Debug for WatchGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WatchGuard")
            .field("git_dir", &self.key)
            .finish()
    }
}

impl Drop for WatchGuard {
    fn drop(&mut self) {
        let Some(map) = self.owner.upgrade() else {
            return;
        };
        let gone = {
            let mut m = map.lock().unwrap_or_else(PoisonError::into_inner);
            let empty = match m.get_mut(&self.key) {
                Some(e) => {
                    e.subs.retain(|(id, _)| *id != self.id);
                    e.subs.is_empty()
                }
                None => false,
            };
            if empty { m.remove(&self.key) } else { None }
        };
        // Dropped outside the lock: stopping an OS watcher may wait for its thread.
        drop(gone);
    }
}

fn is_watched_name(p: &Path) -> bool {
    p.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n == "HEAD" || n == "index")
}

impl RepoWatchers {
    #[must_use]
    pub fn new(debounce: Duration) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            debounce,
            next: Arc::new(AtomicU64::new(1)),
        }
    }

    /// How many git directories are watched (tests, diagnostics).
    #[must_use]
    pub fn watched(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Subscribes `cb` to `git_dir`'s `HEAD` and `index`. `None` when the directory can't be
    /// watched (gone, a network share that refuses): the caller then just polls.
    pub fn acquire(&self, git_dir: &Path, cb: Callback) -> Option<WatchGuard> {
        let key = std::fs::canonicalize(git_dir).unwrap_or_else(|_| git_dir.to_path_buf());
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let mut m = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(e) = m.get_mut(&key) {
            e.subs.push((id, cb));
            return Some(WatchGuard {
                owner: Arc::downgrade(&self.inner),
                key,
                id,
            });
        }
        let (tx, rx) = mpsc::channel::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res
                && ev.paths.iter().any(|p| is_watched_name(p))
            {
                let _ = tx.send(());
            }
        })
        .ok()?;
        watcher.watch(&key, RecursiveMode::NonRecursive).ok()?;
        let weak = Arc::downgrade(&self.inner);
        let debounce = self.debounce;
        let dkey = key.clone();
        let spawned = thread::Builder::new()
            .name("csm-git-watch".into())
            .spawn(move || debounce_loop(&rx, debounce, &weak, &dkey));
        if spawned.is_err() {
            return None;
        }
        m.insert(
            key.clone(),
            Entry {
                _watcher: watcher,
                subs: vec![(id, cb)],
            },
        );
        Some(WatchGuard {
            owner: Arc::downgrade(&self.inner),
            key,
            id,
        })
    }
}

/// Waits for a change, then for `debounce` of quiet, then calls every subscriber. Ends when the
/// watcher (and with it the sender) is dropped.
fn debounce_loop(rx: &mpsc::Receiver<()>, debounce: Duration, map: &Weak<Map>, key: &Path) {
    while rx.recv().is_ok() {
        loop {
            match rx.recv_timeout(debounce) {
                Ok(()) => continue,
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        let Some(m) = map.upgrade() else { return };
        let subs: Vec<Callback> = {
            let m = m.lock().unwrap_or_else(PoisonError::into_inner);
            match m.get(key) {
                Some(e) => e.subs.iter().map(|(_, cb)| cb.clone()).collect(),
                None => return,
            }
        };
        for cb in subs {
            cb();
        }
    }
}
