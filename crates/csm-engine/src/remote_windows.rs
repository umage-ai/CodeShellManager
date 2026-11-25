//! Which remote windows are open, so the next start can open them again (the setting
//! `ReopenRemoteWindows`).
//!
//! A remote window is a process of its own (`codeshell --remote <host-id>`), started by the main
//! app. The main app is the one that knows: [`RemoteWindows::open`] starts the window through a
//! [`WindowSpawner`] and records the office in a small file in the client folder
//! (`peer-client/open-remote-windows.json`) at once, and the spawner reports when the window's
//! process ends. An end while the app runs is the person closing the window: the office leaves the
//! file. An end after [`RemoteWindows::begin_quit`] (the app is quitting, and the window went with
//! it, or on its own) leaves the file as it was. The file is written at each change, not at quit,
//! so a crash of the main app still leaves what was open.
//!
//! At start [`RemoteWindows::reopen`] reads what was remembered (leniently: an entry that is not a
//! plain host id is skipped), drops every office that is not paired any more, and opens the rest
//! through the same `open`. It pairs nothing and sends nothing: a window dials only an office the
//! laptop is already paired with. The spawner is the only thing that starts a process, so tests
//! use a fake one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// The file in the client folder that keeps the offices whose window is open.
pub const FILE: &str = "open-remote-windows.json";
/// The longest host id accepted from the file (the same bound `csm-remote` puts on a view file's name).
const MAX_HOST_ID: usize = 64;

/// Starts the process of a remote window. The real one runs `codeshell --remote <host-id>` on a
/// thread of its own (never the caller's); tests record the call.
pub trait WindowSpawner: Send + Sync {
    /// Starts the window for `host_id` and returns at once. `ended` is called once, from any
    /// thread, when the window's process has ended (for whatever reason). `Err` is shown to the
    /// person and nothing is recorded.
    fn spawn(&self, host_id: &str, ended: Box<dyn FnOnce() + Send>) -> Result<(), String>;
}

/// A spawner that starts nothing (tests, `--clean`, the demo, the CLI).
struct NoSpawner;

impl WindowSpawner for NoSpawner {
    fn spawn(&self, _: &str, _: Box<dyn FnOnce() + Send>) -> Result<(), String> {
        Err("remote windows are not available in this build".into())
    }
}

#[derive(Default)]
struct Inner {
    /// Open windows by office (two windows onto one office count twice).
    open: BTreeMap<String, usize>,
    /// The app is quitting: a window ending now is not a close by the person.
    quitting: bool,
}

/// The open remote windows of this run and what the last run left open.
pub struct RemoteWindows {
    file: Option<PathBuf>,
    spawner: Arc<dyn WindowSpawner>,
    /// What the file said when this was made: the last run's open offices. Read once.
    remembered: Mutex<Vec<String>>,
    inner: Mutex<Inner>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A host id as the client folder names one: plain letters and digits, not empty, not long.
fn plain_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_HOST_ID && id.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The offices a file says were open. A missing, empty or damaged file says none; an entry that is
/// not a plain host id is skipped, the others are kept (once each, in order).
pub fn parse(text: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else { return Vec::new() };
    let Some(items) = value.get("open").and_then(serde_json::Value::as_array) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    for id in items.iter().filter_map(serde_json::Value::as_str).filter(|s| plain_id(s)) {
        if !out.iter().any(|o| o == id) {
            out.push(id.to_owned());
        }
    }
    out
}

impl RemoteWindows {
    /// Keeps nothing and starts nothing: the default of a run that has no client folder or no
    /// launcher (tests, `--clean`, the demo).
    pub fn disabled() -> Arc<Self> {
        Self::new(None, Arc::new(NoSpawner))
    }

    /// Records in `file` (when there is one) and starts windows through `spawner`. Reads the file
    /// now, once: that is what the last run left open.
    pub fn new(file: Option<PathBuf>, spawner: Arc<dyn WindowSpawner>) -> Arc<Self> {
        let remembered = file.as_deref().and_then(|p| std::fs::read_to_string(p).ok()).map(|t| parse(&t)).unwrap_or_default();
        Arc::new(Self { file, spawner, remembered: Mutex::new(remembered), inner: Mutex::new(Inner::default()) })
    }

    /// The offices with a window open now, sorted.
    pub fn open_ids(&self) -> Vec<String> {
        lock(&self.inner).open.keys().cloned().collect()
    }

    /// Starts a window onto `host_id` and records it as open. The caller has checked that the
    /// office is paired. The process starts on the spawner's own thread: this returns at once.
    pub fn open(self: &Arc<Self>, host_id: &str) -> Result<(), String> {
        if !plain_id(host_id) {
            // The id names a file (`view-<id>.json`) and goes into the window's argv.
            return Err("that is not a valid office id".into());
        }
        // Recorded before the process starts, so a window that ends at once still finds its entry.
        {
            let mut g = lock(&self.inner);
            *g.open.entry(host_id.to_owned()).or_insert(0) += 1;
            self.persist(&g);
        }
        let this = self.clone();
        let id = host_id.to_owned();
        let ended: Box<dyn FnOnce() + Send> = Box::new(move || this.ended(&id));
        if let Err(e) = self.spawner.spawn(host_id, ended) {
            self.ended_inner(host_id, true);
            return Err(e);
        }
        Ok(())
    }

    /// A window's process ended. While the app runs that is the person closing it; once the app is
    /// quitting it is not.
    fn ended(&self, host_id: &str) {
        self.ended_inner(host_id, false);
    }

    fn ended_inner(&self, host_id: &str, failed_to_start: bool) {
        let mut g = lock(&self.inner);
        if g.quitting && !failed_to_start {
            return;
        }
        if let Some(n) = g.open.get_mut(host_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                g.open.remove(host_id);
            }
        }
        self.persist(&g);
    }

    /// The app is quitting: from now on a window that ends stays remembered. Called when a quit
    /// starts, and again (harmlessly) when the engine ends.
    pub fn begin_quit(&self) {
        lock(&self.inner).quitting = true;
    }

    /// Writes the open offices. Best effort: a file that cannot be written only costs the reopen.
    fn persist(&self, g: &Inner) {
        let Some(path) = &self.file else { return };
        let ids: Vec<&String> = g.open.keys().collect();
        let text = serde_json::json!({ "open": ids }).to_string();
        if let Err(e) = crate::persist::write_atomic(path, &text) {
            log::warn!("remote windows: the list of open windows was not saved: {e}");
        }
    }

    /// At start: opens a window for each remembered office that is still in `paired`, and forgets
    /// the rest. `enabled` is the setting; off reopens nothing and forgets the list. `paired` is
    /// the laptop's host list, or why it could not be read: then nothing is opened and the
    /// remembered list is kept for the next start. Returns the offices opened. Starts processes
    /// (through the spawner), so it never runs on the UI thread.
    pub fn reopen(self: &Arc<Self>, enabled: bool, paired: Result<Vec<String>, String>) -> Vec<String> {
        let remembered = std::mem::take(&mut *lock(&self.remembered));
        let paired = match paired {
            Ok(p) => p,
            Err(e) => {
                log::warn!("remote windows: not reopened, the paired offices could not be read: {e}");
                *lock(&self.remembered) = remembered;
                return Vec::new();
            }
        };
        let mut opened = Vec::new();
        for id in remembered {
            if !enabled {
                break;
            }
            if !paired.contains(&id) {
                log::info!("remote windows: {id} is not paired any more; not reopened");
                continue;
            }
            match self.open(&id) {
                Ok(()) => opened.push(id),
                Err(e) => log::warn!("remote windows: could not reopen {id}: {e}"),
            }
        }
        // What the file says now is what is open: the reopened windows, none of the dropped ones.
        self.persist(&lock(&self.inner));
        opened
    }

    /// The file this keeps, if any.
    pub fn file(&self) -> Option<&Path> {
        self.file.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Records what it was asked to start; the test ends a window by calling its `ended`.
    #[derive(Default)]
    struct Fake {
        started: Mutex<Vec<String>>,
        ended: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
        fail: Mutex<bool>,
    }

    impl WindowSpawner for Fake {
        fn spawn(&self, id: &str, ended: Box<dyn FnOnce() + Send>) -> Result<(), String> {
            if *self.fail.lock().unwrap() {
                return Err("no".into());
            }
            self.started.lock().unwrap().push(id.to_owned());
            self.ended.lock().unwrap().push(ended);
            Ok(())
        }
    }

    impl Fake {
        fn started(&self) -> Vec<String> {
            self.started.lock().unwrap().clone()
        }
        /// The window opened `n`th (from 0) ends.
        fn end(&self, n: usize) {
            let f = self.ended.lock().unwrap().remove(n);
            f();
        }
    }

    fn rig(initial: Option<&str>) -> (tempfile::TempDir, PathBuf, Arc<Fake>, Arc<RemoteWindows>) {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(FILE);
        if let Some(t) = initial {
            std::fs::write(&file, t).unwrap();
        }
        let fake = Arc::new(Fake::default());
        let rw = RemoteWindows::new(Some(file.clone()), fake.clone());
        (dir, file, fake, rw)
    }

    fn on_disk(file: &Path) -> Vec<String> {
        parse(&std::fs::read_to_string(file).unwrap())
    }

    fn ids(v: &[&str]) -> Result<Vec<String>, String> {
        Ok(v.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn opening_records_the_office_at_once() {
        let (_d, file, fake, rw) = rig(None);
        rw.open("abc123").unwrap();
        assert_eq!(fake.started(), ["abc123"]);
        assert_eq!(rw.open_ids(), ["abc123"]);
        assert_eq!(on_disk(&file), ["abc123"], "written on open, so a crash keeps it");
    }

    #[test]
    fn a_window_the_person_closes_is_forgotten() {
        let (_d, file, fake, rw) = rig(None);
        rw.open("a1").unwrap();
        rw.open("b2").unwrap();
        fake.end(0);
        assert_eq!(rw.open_ids(), ["b2"]);
        assert_eq!(on_disk(&file), ["b2"]);
    }

    #[test]
    fn two_windows_on_one_office_need_both_closed() {
        let (_d, file, fake, rw) = rig(None);
        rw.open("a1").unwrap();
        rw.open("a1").unwrap();
        fake.end(0);
        assert_eq!(on_disk(&file), ["a1"]);
        fake.end(0);
        assert!(on_disk(&file).is_empty());
    }

    #[test]
    fn windows_that_end_because_the_app_quits_stay_remembered() {
        let (_d, file, fake, rw) = rig(None);
        rw.open("a1").unwrap();
        rw.open("b2").unwrap();
        fake.end(1); // the person closed b2 before quitting
        rw.begin_quit();
        fake.end(0); // a1 went with the app
        assert_eq!(on_disk(&file), ["a1"]);
        assert_eq!(rw.open_ids(), ["a1"]);
    }

    #[test]
    fn a_window_that_could_not_start_is_not_recorded() {
        let (_d, file, fake, rw) = rig(None);
        *fake.fail.lock().unwrap() = true;
        assert!(rw.open("a1").is_err());
        assert!(rw.open_ids().is_empty());
        assert!(on_disk(&file).is_empty());
        rw.begin_quit();
        assert!(rw.open("../x").is_err(), "an id that is not plain is refused before anything starts");
    }

    #[test]
    fn start_reopens_only_paired_offices_and_drops_the_rest() {
        let (_d, file, fake, rw) = rig(Some(r#"{"open":["a1","gone","b2"]}"#));
        let opened = rw.reopen(true, ids(&["b2", "a1", "other"]));
        assert_eq!(opened, ["a1", "b2"]);
        assert_eq!(fake.started(), ["a1", "b2"]);
        assert_eq!(on_disk(&file), ["a1", "b2"], "the unpaired office is gone from the file");
    }

    #[test]
    fn the_setting_off_reopens_nothing_and_forgets_the_list() {
        let (_d, file, fake, rw) = rig(Some(r#"{"open":["a1"]}"#));
        assert!(rw.reopen(false, ids(&["a1"])).is_empty());
        assert!(fake.started().is_empty());
        assert!(on_disk(&file).is_empty());
    }

    #[test]
    fn an_unreadable_host_list_opens_nothing_and_keeps_the_list() {
        let (_d, file, fake, rw) = rig(Some(r#"{"open":["a1"]}"#));
        assert!(rw.reopen(true, Err("damaged".into())).is_empty());
        assert!(fake.started().is_empty());
        assert_eq!(on_disk(&file), ["a1"], "untouched");
        assert_eq!(rw.reopen(true, ids(&["a1"])), ["a1"], "and the next try still has it");
    }

    #[test]
    fn a_bad_entry_is_skipped_and_never_a_crash() {
        for text in ["", "not json", "[]", "{}", r#"{"open":"a1"}"#, r#"{"open":null}"#, "\u{0}\u{1}"] {
            assert!(parse(text).is_empty(), "{text:?}");
        }
        let long = "x".repeat(65);
        let text = format!(r#"{{"open":[1,null,"","../etc","a b","ok1",{{"x":1}},"ok1","{long}","ok2"]}}"#);
        assert_eq!(parse(&text), ["ok1", "ok2"]);
        let (_d, _file, fake, rw) = rig(Some(&text));
        assert_eq!(rw.reopen(true, ids(&["ok1", "ok2", "../etc"])), ["ok1", "ok2"]);
        assert_eq!(fake.started(), ["ok1", "ok2"]);
    }

    #[test]
    fn a_missing_file_and_a_disabled_tracker_open_nothing() {
        let (d, _file, fake, rw) = rig(None);
        assert!(rw.reopen(true, ids(&["a1"])).is_empty());
        assert!(fake.started().is_empty());
        assert!(!d.path().join(FILE).exists() || on_disk(&d.path().join(FILE)).is_empty());
        let off = RemoteWindows::disabled();
        assert!(off.file().is_none());
        assert!(off.open("a1").is_err());
        assert!(off.open_ids().is_empty());
        assert!(off.reopen(true, ids(&["a1"])).is_empty());
    }
}
