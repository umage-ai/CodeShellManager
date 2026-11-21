//! The highlight worker. A frame never highlights: it asks [`Highlighter::get`] for a document and
//! draws plain monospace text until the spans arrive from a worker thread (which asks for a
//! repaint). Results are cached by (document hash, language, path) in an LRU of at most
//! [`CACHE_ENTRIES`] documents or [`CACHE_BYTES`] of spans; a request that no frame asks about
//! any more is dropped before it runs, so switching quickly between big files does not queue
//! work for files nobody is looking at.
//!
//! The engine is injectable (`with_engine`): production uses `csm_services::highlight`, tests
//! use a gate to prove that frames keep drawing while the worker is busy.

use std::collections::HashMap;
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_core::addons::screen::Doc;
use csm_services::highlight::{Highlighted, Outcome};

use crate::terminal::lock;

pub const CACHE_ENTRIES: usize = 64;
pub const CACHE_BYTES: usize = 64 * 1024 * 1024;
/// A pending request that has not been asked for in this long is dropped.
const STALE: Duration = Duration::from_secs(3);

/// What the worker answers for one document.
#[derive(Clone)]
pub enum Hl {
    /// Not yet: draw plain text.
    Pending,
    /// Spans, line by line.
    Ready(Arc<Highlighted>),
    /// Not highlighted for good: no grammar, or too large. The text is drawn plain.
    Off(Reason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    NoSyntax,
    TooLarge,
}

/// `(text, language, path) -> outcome`; runs on the worker.
pub type Engine = Arc<dyn Fn(&str, &str, &str) -> Outcome + Send + Sync>;

type Key = (u64, String, String);

enum State {
    Pending,
    Done(Hl),
}

struct Entry {
    state: State,
    touched: Instant,
    /// Counts up on every access, for the LRU.
    tick: u64,
    bytes: usize,
}

#[derive(Default)]
struct Cache {
    map: HashMap<Key, Entry>,
    tick: u64,
    bytes: usize,
}

struct Job {
    key: Key,
    doc: Arc<Doc>,
}

struct Shared {
    cache: Mutex<Cache>,
    ctx: Mutex<Option<egui::Context>>,
    engine: Engine,
}

pub struct Highlighter {
    shared: Arc<Shared>,
    tx: Mutex<Option<Sender<Job>>>,
}

impl Default for Highlighter {
    fn default() -> Self {
        Highlighter::with_engine(Arc::new(|t: &str, l: &str, p: &str| csm_services::highlight::highlight(t, l, p)))
    }
}

impl Highlighter {
    pub fn with_engine(engine: Engine) -> Highlighter {
        Highlighter { shared: Arc::new(Shared { cache: Mutex::new(Cache::default()), ctx: Mutex::new(None), engine }), tx: Mutex::new(None) }
    }

    /// The highlight of `doc`, or `Pending` (and a request to the worker). Never blocks on the
    /// highlighter: the lock is held for map operations only.
    pub fn get(&self, ctx: &egui::Context, doc: &Arc<Doc>, language: &str, path: &str) -> Hl {
        let key: Key = (doc.hash() ^ (doc.text().len() as u64).rotate_left(32), language.to_owned(), path.to_owned());
        {
            let mut c = lock(&self.shared.cache);
            c.tick += 1;
            let tick = c.tick;
            if let Some(e) = c.map.get_mut(&key) {
                e.touched = Instant::now();
                e.tick = tick;
                return match &e.state {
                    State::Pending => Hl::Pending,
                    State::Done(h) => h.clone(),
                };
            }
            c.map.insert(key.clone(), Entry { state: State::Pending, touched: Instant::now(), tick, bytes: 0 });
        }
        *lock(&self.shared.ctx) = Some(ctx.clone());
        let mut tx = lock(&self.tx);
        if tx.is_none() {
            let (s, r) = channel::<Job>();
            let shared = self.shared.clone();
            let spawned = std::thread::Builder::new().name("csm-highlight".into()).spawn(move || {
                while let Ok(job) = r.recv() { // policy-ok: the worker thread, never a frame
                    work(&shared, job);
                }
            });
            if spawned.is_ok() {
                *tx = Some(s);
            }
        }
        let sent = tx.as_ref().is_some_and(|t| t.send(Job { key: key.clone(), doc: doc.clone() }).is_ok());
        if !sent {
            // No worker (it could not start, or it died): this document stays plain.
            *tx = None;
            drop(tx);
            self.finish(&key, Hl::Off(Reason::NoSyntax));
        }
        Hl::Pending
    }

    fn finish(&self, key: &Key, hl: Hl) {
        finish(&self.shared, key, hl);
    }

    /// Documents waiting for the worker or being worked on.
    pub fn pending(&self) -> usize {
        lock(&self.shared.cache).map.values().filter(|e| matches!(e.state, State::Pending)).count()
    }

    /// Documents in the cache (pending or done).
    pub fn cached(&self) -> usize {
        lock(&self.shared.cache).map.len()
    }
}

fn finish(shared: &Shared, key: &Key, hl: Hl) {
    let bytes = match &hl {
        Hl::Ready(h) => h.bytes(),
        _ => 0,
    };
    {
        let mut c = lock(&shared.cache);
        let Some(e) = c.map.get_mut(key) else { return };
        e.state = State::Done(hl);
        e.bytes = bytes;
        c.bytes += bytes;
        // Evict the least recently used documents, never the one just finished.
        while c.map.len() > CACHE_ENTRIES || c.bytes > CACHE_BYTES {
            let victim = c.map.iter().filter(|(k, e)| *k != key && matches!(e.state, State::Done(_))).min_by_key(|(_, e)| e.tick).map(|(k, _)| k.clone());
            let Some(v) = victim else { break };
            if let Some(e) = c.map.remove(&v) {
                c.bytes = c.bytes.saturating_sub(e.bytes);
            }
        }
    }
    if let Some(ctx) = lock(&shared.ctx).as_ref() {
        ctx.request_repaint();
    }
}

fn work(shared: &Shared, job: Job) {
    {
        let mut c = lock(&shared.cache);
        let stale = c.map.get(&job.key).is_none_or(|e| e.touched.elapsed() > STALE);
        if stale {
            c.map.remove(&job.key);
            return;
        }
    }
    let engine = shared.engine.clone();
    let (lang, path) = (job.key.1.clone(), job.key.2.clone());
    let doc = job.doc.clone();
    // A panic in the grammar engine on hostile text costs this document its colours, not the worker.
    let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || engine(doc.text(), &lang, &path)));
    let hl = match out {
        Ok(Outcome::Highlighted(h)) => Hl::Ready(Arc::new(h)),
        Ok(Outcome::NoSyntax) | Err(_) => Hl::Off(Reason::NoSyntax),
        Ok(Outcome::TooLarge) => Hl::Off(Reason::TooLarge),
    };
    finish(shared, &job.key, hl);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::sync_channel;

    fn doc(s: &str) -> Arc<Doc> {
        Arc::new(Doc::new(s.to_owned()))
    }

    fn wait(h: &Highlighter, ctx: &egui::Context, d: &Arc<Doc>) -> Hl {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            match h.get(ctx, d, "rust", "") {
                Hl::Pending if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
                other => return other,
            }
        }
    }

    #[test]
    fn the_work_happens_on_the_worker_and_get_never_waits_for_it() {
        let ctx = egui::Context::default();
        let (gate_tx, gate_rx) = sync_channel::<()>(0);
        let gate_rx = Mutex::new(gate_rx);
        let h = Highlighter::with_engine(Arc::new(move |t: &str, l: &str, p: &str| {
            let _ = gate_rx.lock().map(|g| g.recv_timeout(Duration::from_secs(20)));
            csm_services::highlight::highlight(t, l, p)
        }));
        let d = doc("fn main() { let x = \"hi\"; }\n");
        let started = Instant::now();
        assert!(matches!(h.get(&ctx, &d, "rust", ""), Hl::Pending));
        assert!(started.elapsed() < Duration::from_millis(500), "get returned while the engine was blocked");
        assert!(matches!(h.get(&ctx, &d, "rust", ""), Hl::Pending), "still pending, and not requested twice");
        assert_eq!(h.pending(), 1);
        gate_tx.send(()).unwrap();
        match wait(&h, &ctx, &d) {
            Hl::Ready(r) => {
                assert_ne!(r.thread, std::thread::current().id(), "highlighted off the calling thread");
                assert!(!r.line(0).is_empty());
            }
            _ => panic!("expected spans"),
        }
        assert_eq!(h.pending(), 0);
    }

    #[test]
    fn no_grammar_and_too_large_are_final_answers() {
        let ctx = egui::Context::default();
        let h = Highlighter::default();
        let d = doc("hello");
        let end = Instant::now() + Duration::from_secs(20);
        let r = loop {
            match h.get(&ctx, &d, "klingon", "") {
                Hl::Pending if Instant::now() < end => std::thread::sleep(Duration::from_millis(5)),
                other => break other,
            }
        };
        assert!(matches!(r, Hl::Off(Reason::NoSyntax)));
        let big = doc(&"a".repeat(csm_services::highlight::MAX_BYTES + 1));
        assert!(matches!(wait(&h, &ctx, &big), Hl::Off(Reason::TooLarge)));
    }

    #[test]
    fn the_cache_is_a_bounded_lru() {
        let ctx = egui::Context::default();
        let h = Highlighter::with_engine(Arc::new(|_: &str, _: &str, _: &str| Outcome::NoSyntax));
        let docs: Vec<Arc<Doc>> = (0..CACHE_ENTRIES + 20).map(|i| doc(&format!("let a{i} = {i};"))).collect();
        for d in &docs {
            wait(&h, &ctx, d);
        }
        assert!(h.cached() <= CACHE_ENTRIES, "{} cached", h.cached());
        // The newest is still there, the oldest was evicted (asking again starts new work).
        assert!(matches!(h.get(&ctx, docs.last().unwrap(), "rust", ""), Hl::Off(_)));
    }

    #[test]
    fn a_panicking_engine_costs_the_document_its_colours_and_the_worker_survives() {
        let ctx = egui::Context::default();
        let h = Highlighter::with_engine(Arc::new(|t: &str, l: &str, p: &str| {
            if t.contains("boom") {
                std::panic::resume_unwind(Box::new("boom"));
            }
            csm_services::highlight::highlight(t, l, p)
        }));
        let bad = doc("boom");
        assert!(matches!(wait(&h, &ctx, &bad), Hl::Off(_)));
        let good = doc("fn a() {}");
        assert!(matches!(wait(&h, &ctx, &good), Hl::Ready(_)), "the worker is still alive");
    }
}
