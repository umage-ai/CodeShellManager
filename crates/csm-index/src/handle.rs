//! The DB thread and the handle that talks to it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use crate::clean::LineCleaner;
use crate::db::{Db, IndexError};

/// Where the index lives.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexLocation {
    /// In memory: `--clean`, an isolated run, tests. Nothing touches the disk.
    Memory,
    File(PathBuf),
}

#[derive(Debug, Clone)]
pub struct IndexOptions {
    pub location: IndexLocation,
    /// Newest bytes of output kept per session (the retention cap).
    pub max_bytes_per_session: u64,
    /// Raw output queued for the DB thread; over this a chunk is dropped and counted.
    pub queue_bytes: usize,
    /// How often pending lines are written.
    pub flush_interval: Duration,
    /// A session that went quiet this long with an unfinished line (a prompt) has it indexed.
    pub idle_flush: Duration,
}

impl IndexOptions {
    pub fn new(location: IndexLocation) -> Self {
        Self {
            location,
            max_bytes_per_session: 20 * 1024 * 1024,
            queue_bytes: 8 * 1024 * 1024,
            flush_interval: Duration::from_millis(500),
            idle_flush: Duration::from_secs(2),
        }
    }

    pub fn memory() -> Self {
        Self::new(IndexLocation::Memory)
    }
}

type Job = Box<dyn FnOnce(Result<&mut Db, &IndexError>) + Send>;

enum Msg {
    Feed { session_id: String, ts: i64, bytes: Vec<u8> },
    Job(Job),
    Shutdown,
}

struct Shared {
    pending: AtomicUsize,
    dropped: AtomicU64,
    /// The file could not be used; the index runs in memory.
    fell_back: AtomicBool,
}

/// A cheap handle to the index thread. Every method returns at once.
#[derive(Clone)]
pub struct Index {
    tx: Sender<Msg>,
    shared: Arc<Shared>,
    queue_bytes: usize,
    joined: Arc<Mutex<Option<thread::JoinHandle<()>>>>,
}

impl std::fmt::Debug for Index {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Index")
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)).unwrap_or(0)
}

impl Index {
    /// Starts the DB thread. The database is opened on that thread, so this never waits on disk.
    pub fn start(options: IndexOptions) -> Result<Index, IndexError> {
        let (tx, rx) = mpsc::channel();
        let shared = Arc::new(Shared { pending: AtomicUsize::new(0), dropped: AtomicU64::new(0), fell_back: AtomicBool::new(false) });
        let queue_bytes = options.queue_bytes;
        let s2 = shared.clone();
        let handle = thread::Builder::new()
            .name("csm-index".into())
            .spawn(move || run(rx, options, s2))
            .map_err(|e| IndexError::Unavailable(format!("could not start the index thread: {e}")))?;
        Ok(Index { tx, shared, queue_bytes, joined: Arc::new(Mutex::new(Some(handle))) })
    }

    /// Queues raw PTY output for indexing. Called from the PTY reader thread: never blocks;
    /// when the queue is over its byte bound the chunk is dropped and counted.
    pub fn feed(&self, session_id: &str, bytes: &[u8]) {
        let n = bytes.len();
        if n == 0 {
            return;
        }
        if self.shared.pending.fetch_add(n, Ordering::AcqRel).saturating_add(n) > self.queue_bytes {
            self.shared.pending.fetch_sub(n, Ordering::AcqRel);
            self.shared.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let msg = Msg::Feed { session_id: session_id.to_owned(), ts: now_ms(), bytes: bytes.to_vec() };
        if self.tx.send(msg).is_err() {
            self.shared.pending.fetch_sub(n, Ordering::AcqRel);
        }
    }

    /// Chunks of output dropped because the DB thread could not keep up.
    pub fn dropped(&self) -> u64 {
        self.shared.dropped.load(Ordering::Relaxed)
    }

    /// The file could not be used and the index is in memory this run.
    pub fn is_fallback(&self) -> bool {
        self.shared.fell_back.load(Ordering::Relaxed)
    }

    /// Runs `f` on the DB thread (after any pending output is written) and hands the result to
    /// `done`, which also runs there: keep it short (send on a channel, post to the engine).
    pub fn call<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Db) -> Result<T, IndexError> + Send + 'static,
        done: impl FnOnce(Result<T, IndexError>) + Send + 'static,
    ) {
        let job: Job = Box::new(move |db| match db {
            Ok(db) => done(f(db)),
            Err(e) => done(Err(e.clone())),
        });
        if let Err(mpsc::SendError(Msg::Job(job))) = self.tx.send(Msg::Job(job)) {
            job(Err(&IndexError::Unavailable("the search index is not running".into())));
        }
    }

    /// [`Index::call`] and wait. For tests and one-shot tools, never for a UI or engine thread.
    pub fn call_blocking<T: Send + 'static>(&self, f: impl FnOnce(&mut Db) -> Result<T, IndexError> + Send + 'static) -> Result<T, IndexError> {
        let (tx, rx) = mpsc::channel();
        self.call(f, move |r| {
            let _ = tx.send(r);
        });
        rx.recv_timeout(Duration::from_secs(30)).unwrap_or_else(|_| Err(IndexError::Unavailable("the search index did not answer".into())))
    }

    /// Records which session a stream of output belongs to (its name, folder, group).
    pub fn note_session(&self, id: &str, name: &str, folder: &str, group_id: &str) {
        let (id, name, folder, group) = (id.to_owned(), name.to_owned(), folder.to_owned(), group_id.to_owned());
        self.call(move |db| db.set_session(&id, &name, &folder, &group, now_ms()), log_err);
    }

    /// Saves the config snapshot a closed session can be reopened from.
    pub fn record_history(&self, id: &str, name: &str, folder: &str, snapshot_json: &str) {
        let (id, name, folder, snap) = (id.to_owned(), name.to_owned(), folder.to_owned(), snapshot_json.to_owned());
        self.call(move |db| db.save_history(&id, &name, &folder, &snap, now_ms()), log_err);
    }

    /// Writes what is pending and stops the thread. Later calls fail with "not running".
    pub fn shutdown(&self) {
        let _ = self.tx.send(Msg::Shutdown);
        let handle = self.joined.lock().unwrap_or_else(PoisonError::into_inner).take();
        if let Some(h) = handle {
            let _ = h.join();
        }
    }
}

fn log_err(r: Result<(), IndexError>) {
    if let Err(e) = r {
        log::warn!("index: {e}");
    }
}

/// Opens the database; a file that will not open is moved aside and replaced; failing that,
/// memory.
fn open(location: &IndexLocation, shared: &Shared) -> Result<Db, IndexError> {
    let IndexLocation::File(path) = location else { return Db::open(None) };
    match Db::open(Some(path)) {
        Ok(db) => return Ok(db),
        Err(e) => log::warn!("index: {} cannot be used ({e}); starting a fresh one", path.display()),
    }
    let mut aside = path.as_os_str().to_owned();
    aside.push(format!(".bad-{}", now_ms()));
    if std::fs::rename(path, &aside).is_ok() {
        for ext in ["-wal", "-shm"] {
            let mut p = path.as_os_str().to_owned();
            p.push(ext);
            let _ = std::fs::remove_file(PathBuf::from(p));
        }
        if let Ok(db) = Db::open(Some(path)) {
            return Ok(db);
        }
    }
    log::warn!("index: falling back to an in-memory index");
    shared.fell_back.store(true, Ordering::Relaxed);
    Db::open(None)
}

#[derive(Default)]
struct Batch {
    lines: Vec<String>,
    ts: i64,
}

fn run(rx: Receiver<Msg>, opts: IndexOptions, shared: Arc<Shared>) {
    let mut db = open(&opts.location, &shared);
    if let Err(e) = &db {
        log::error!("index: {e}");
    }
    let mut cleaners: HashMap<String, (LineCleaner, Instant)> = HashMap::new();
    let mut batches: HashMap<String, Batch> = HashMap::new();
    let mut batched_bytes = 0usize;
    let mut last_flush = Instant::now();

    let flush = |db: &mut Result<Db, IndexError>, batches: &mut HashMap<String, Batch>, bytes: &mut usize| {
        if batches.is_empty() {
            return;
        }
        for (sid, b) in batches.drain() {
            if let Ok(db) = db.as_mut()
                && let Err(e) = db.insert_output(&sid, b.ts, &b.lines, opts.max_bytes_per_session)
            {
                log::warn!("index: could not write output: {e}");
            }
        }
        *bytes = 0;
    };

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Msg::Feed { session_id, ts, bytes }) => {
                shared.pending.fetch_sub(bytes.len(), Ordering::AcqRel);
                let entry = cleaners.entry(session_id.clone()).or_insert_with(|| (LineCleaner::default(), Instant::now()));
                entry.1 = Instant::now();
                let lines = entry.0.push(&bytes);
                if !lines.is_empty() {
                    batched_bytes += lines.iter().map(String::len).sum::<usize>();
                    let b = batches.entry(session_id).or_default();
                    b.ts = ts;
                    b.lines.extend(lines);
                }
            }
            Ok(Msg::Job(job)) => {
                flush(&mut db, &mut batches, &mut batched_bytes);
                last_flush = Instant::now();
                match db.as_mut() {
                    Ok(d) => job(Ok(d)),
                    Err(e) => job(Err(&e.clone())),
                }
            }
            Ok(Msg::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                let now = now_ms();
                for (sid, (c, _)) in &mut cleaners {
                    let lines = c.finish();
                    if !lines.is_empty() {
                        let b = batches.entry(sid.clone()).or_default();
                        b.ts = now;
                        b.lines.extend(lines);
                    }
                }
                flush(&mut db, &mut batches, &mut batched_bytes);
                return;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        // A prompt with no newline is searchable once the session has been quiet a while.
        let now = now_ms();
        for (sid, (c, last)) in &mut cleaners {
            if !c.is_empty() && last.elapsed() >= opts.idle_flush {
                let lines = c.finish();
                if !lines.is_empty() {
                    let b = batches.entry(sid.clone()).or_default();
                    b.ts = now;
                    b.lines.extend(lines);
                }
            }
        }
        if batched_bytes >= 256 * 1024 || (!batches.is_empty() && last_flush.elapsed() >= opts.flush_interval) {
            flush(&mut db, &mut batches, &mut batched_bytes);
            last_flush = Instant::now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::fts_phrase;

    fn search(ix: &Index, q: &str) -> Vec<crate::OutputHit> {
        let m = fts_phrase(q);
        ix.call_blocking(move |db| db.search_output(&m, 50, None)).unwrap()
    }

    #[test]
    fn output_fed_from_another_thread_is_found_after_a_query() {
        let ix = Index::start(IndexOptions::memory()).unwrap();
        ix.note_session("s1", "Odulodo", "c:\\x", "");
        let w = ix.clone();
        thread::spawn(move || {
            w.feed("s1", b"\x1b[1m compiling crate-a\x1b[0m\r\nwarning: unused import\r\n");
            w.feed("s1", b"error[E0599]: no method named");
            w.feed("s1", b" `frobnicate` found\r\n");
        })
        .join()
        .unwrap();
        let hits = search(&ix, "no method named frobnicate");
        assert_eq!(hits.len(), 1, "a line split across two chunks is one line");
        assert_eq!(hits[0].session_name, "Odulodo");
        assert_eq!(search(&ix, "unused import").len(), 1);
        ix.shutdown();
    }

    #[test]
    fn a_prompt_without_a_newline_is_indexed_when_the_session_goes_quiet() {
        let mut o = IndexOptions::memory();
        o.idle_flush = Duration::from_millis(50);
        o.flush_interval = Duration::from_millis(20);
        let ix = Index::start(o).unwrap();
        ix.feed("s", b"Do you want to proceed? (y/n)");
        thread::sleep(Duration::from_millis(400));
        assert_eq!(search(&ix, "want to proceed").len(), 1);
        ix.shutdown();
    }

    #[test]
    fn overflow_drops_and_counts_instead_of_blocking() {
        let mut o = IndexOptions::memory();
        o.queue_bytes = 1;
        let ix = Index::start(o).unwrap();
        ix.feed("s", b"hello world this is larger than the queue\n");
        assert_eq!(ix.dropped(), 1);
        assert!(search(&ix, "hello").is_empty());
        ix.shutdown();
    }

    #[test]
    fn shutdown_flushes_and_later_calls_fail_cleanly() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("sub").join("i.db");
        let ix = Index::start(IndexOptions::new(IndexLocation::File(p.clone()))).unwrap();
        ix.feed("s", b"persisted line\n");
        ix.shutdown();
        assert!(matches!(ix.call_blocking(|_| Ok(())), Err(IndexError::Unavailable(_))));
        let again = Index::start(IndexOptions::new(IndexLocation::File(p))).unwrap();
        assert_eq!(search(&again, "persisted line").len(), 1);
        again.shutdown();
    }

    #[test]
    fn a_corrupt_file_is_moved_aside_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("i.db");
        std::fs::write(&p, b"this is not sqlite, it is a text file that is long enough to be a header .....").unwrap();
        let ix = Index::start(IndexOptions::new(IndexLocation::File(p.clone()))).unwrap();
        ix.feed("s", b"works anyway\n");
        assert_eq!(search(&ix, "works anyway").len(), 1);
        assert!(!ix.is_fallback());
        ix.shutdown();
        let aside = std::fs::read_dir(dir.path()).unwrap().filter_map(Result::ok).any(|e| e.file_name().to_string_lossy().contains(".bad-"));
        assert!(aside);
    }

    #[test]
    fn memory_location_never_creates_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let before = std::fs::read_dir(dir.path()).unwrap().count();
        let ix = Index::start(IndexOptions::memory()).unwrap();
        ix.feed("s", b"in memory only\n");
        assert_eq!(search(&ix, "in memory only").len(), 1);
        ix.shutdown();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), before);
    }

    #[test]
    fn many_threads_feeding_and_querying_do_not_interfere() {
        let ix = Index::start(IndexOptions::memory()).unwrap();
        let mut hs = Vec::new();
        for t in 0..4 {
            let w = ix.clone();
            hs.push(thread::spawn(move || {
                for i in 0..50 {
                    w.feed(&format!("s{t}"), format!("thread{t} line{i}\n").as_bytes());
                    if i % 10 == 0 {
                        let _ = w.call_blocking(|db| db.stats());
                    }
                }
            }));
        }
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(search(&ix, "thread2 line49").len(), 1);
        ix.shutdown();
    }
}
