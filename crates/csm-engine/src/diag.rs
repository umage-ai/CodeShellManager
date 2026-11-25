//! Diagnostics that survive a crash: a panic hook that writes `crash-<UTC>.log`, a small file
//! logger for the `log` crate (`logs/codeshell.log`, rotated), and a run marker that tells the
//! next start whether the last run ended in an orderly quit.
//!
//! All of it lives in `<config dir>/logs` (the channel's own folder, `--config-dir`,
//! `CSM_CONFIG_DIR`, portable: see [`crate::persist::config_dir`]). Nothing here may panic or
//! block the UI thread: the logger hands lines to a writer thread over a bounded channel and
//! drops them when it is full; the hook ignores every I/O error.
//!
//! Which panics end the app: one on the main (UI / event-loop) thread unwinds out of `main`, so
//! the process exits (code 101) without saving. A panic on any other thread (PTY readers, the
//! engine thread, tool workers, the IPC bridge, add-on threads) ends only that thread, but may
//! leave the feature it served stalled or a mutex poisoned. The hook logs both.
use std::backtrace::Backtrace;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, sync_channel};
use std::sync::{Mutex, OnceLock, PoisonError};
use std::time::{Duration, SystemTime};

/// Crash reports kept; older ones are deleted when a new one is written.
pub const KEEP_CRASH_LOGS: usize = 20;
/// The log file rotates at about this size.
pub const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Files kept: `codeshell.log`, `codeshell.log.1`, `codeshell.log.2`.
pub const LOG_KEEP_FILES: usize = 3;
const LOG_QUEUE: usize = 2048;
const MARKER: &str = "run-state";

/// `<config dir>/logs`, or `None` for an isolated run (nothing is written to disk).
pub fn logs_dir(cli_config_dir: Option<&Path>) -> Option<PathBuf> {
    match crate::persist::config_dir(cli_config_dir) {
        crate::persist::ConfigDir::Dir(d) => Some(d.join("logs")),
        crate::persist::ConfigDir::Isolated => None,
    }
}

fn utc_stamp() -> String {
    jiff::Timestamp::now().strftime("%Y%m%dT%H%M%SZ").to_string()
}

// ---------------------------------------------------------------------------------------------
// Crash reports

/// Everything a crash report says about the panic.
pub struct PanicFacts<'a> {
    pub app: &'a str,
    pub thread: &'a str,
    pub message: &'a str,
    pub location: &'a str,
    pub backtrace: &'a str,
}

/// The text of a crash report.
pub fn render_report(f: &PanicFacts<'_>) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "CodeShellManager crash report");
    let _ = writeln!(s, "time (UTC): {}", jiff::Timestamp::now());
    let _ = writeln!(s, "program: {}", f.app);
    let _ = writeln!(s, "version: {} ({} channel)", env!("CARGO_PKG_VERSION"), csm_core::channel::CURRENT.id());
    let _ = writeln!(s, "os: {} {}", std::env::consts::OS, std::env::consts::ARCH);
    let _ = writeln!(s, "pid: {}", std::process::id());
    let _ = writeln!(s, "thread: {}", f.thread);
    let _ = writeln!(s, "location: {}", f.location);
    let _ = writeln!(s, "message: {}", f.message);
    let _ = writeln!(s, "\nbacktrace:\n{}", f.backtrace);
    s
}

/// Writes `crash-<UTC>.log` into `dir` (created if missing) and prunes to [`KEEP_CRASH_LOGS`].
pub fn write_crash_log(dir: &Path, report: &str) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let stamp = utc_stamp();
    let mut n = 0u32;
    let (path, mut file) = loop {
        let name = if n == 0 { format!("crash-{stamp}.log") } else { format!("crash-{stamp}-{n:02}.log") };
        let path = dir.join(name);
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(f) => break (path, f),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 99 => n += 1,
            Err(e) => return Err(e),
        }
    };
    file.write_all(report.as_bytes())?;
    let _ = file.flush();
    prune_crash_logs(dir, KEEP_CRASH_LOGS);
    Ok(path)
}

/// Crash reports in `dir`, oldest first (the names sort by time).
pub fn crash_logs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> = rd
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("crash-") && n.ends_with(".log")))
        .collect();
    v.sort();
    v
}

/// Keeps the newest `keep` crash reports.
pub fn prune_crash_logs(dir: &Path, keep: usize) {
    let all = crash_logs(dir);
    let excess = all.len().saturating_sub(keep);
    for p in all.into_iter().take(excess) {
        let _ = std::fs::remove_file(p);
    }
}

fn payload_text(info: &std::panic::PanicHookInfo<'_>) -> String {
    let p = info.payload();
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "(non-string panic payload)".to_owned()
    }
}

/// Installs the panic hook: every panic, on any thread, is logged to the file log and written as
/// a crash report into `dir` (when there is one); the previous hook then runs as before. `app` is
/// the program's name for the report. Call first thing in `main`.
pub fn install_panic_hook(dir: Option<PathBuf>, app: &'static str) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let thread = std::thread::current();
            let location = info.location().map_or_else(|| "unknown".to_owned(), |l| format!("{}:{}:{}", l.file(), l.line(), l.column()));
            let message = payload_text(info);
            let backtrace = Backtrace::force_capture().to_string();
            let thread_name = thread.name().unwrap_or("<unnamed>").to_owned();
            log::error!("panic in thread '{thread_name}' at {location}: {message}");
            flush_log(Duration::from_millis(300));
            if let Some(dir) = &dir {
                let report = render_report(&PanicFacts { app, thread: &thread_name, message: &message, location: &location, backtrace: &backtrace });
                let _ = write_crash_log(dir, &report);
            }
        }));
        previous(info);
    }));
}

// ---------------------------------------------------------------------------------------------
// The file logger

/// A log file that rotates at `max_bytes`, keeping `keep` files (`name`, `name.1`, ...).
pub struct RotatingFile {
    path: PathBuf,
    max_bytes: u64,
    keep: usize,
    file: Option<std::fs::File>,
    size: u64,
}

impl RotatingFile {
    pub fn new(path: PathBuf, max_bytes: u64, keep: usize) -> RotatingFile {
        RotatingFile { path, max_bytes, keep: keep.max(1), file: None, size: 0 }
    }

    fn numbered(&self, i: usize) -> PathBuf {
        let mut os = self.path.clone().into_os_string();
        os.push(format!(".{i}"));
        PathBuf::from(os)
    }

    fn open(&mut self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let f = std::fs::OpenOptions::new().create(true).append(true).open(&self.path)?;
        self.size = f.metadata().map_or(0, |m| m.len());
        self.file = Some(f);
        Ok(())
    }

    fn rotate(&mut self) {
        self.file = None;
        if self.keep > 1 {
            let _ = std::fs::remove_file(self.numbered(self.keep - 1));
            for i in (1..self.keep - 1).rev() {
                let _ = std::fs::rename(self.numbered(i), self.numbered(i + 1));
            }
            let _ = std::fs::rename(&self.path, self.numbered(1));
        } else {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// Appends one line (a newline is added). Errors are returned; the logger ignores them.
    pub fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        if self.file.is_none() {
            self.open()?;
        }
        let len = line.len() as u64 + 1;
        if self.size > 0 && self.size + len > self.max_bytes {
            self.rotate();
            self.open()?;
        }
        if let Some(f) = self.file.as_mut() {
            f.write_all(line.as_bytes())?;
            f.write_all(b"\n")?;
            self.size += len;
        }
        Ok(())
    }

    pub fn flush(&mut self) {
        if let Some(f) = self.file.as_mut() {
            let _ = f.flush();
        }
    }
}

enum Msg {
    Line(String),
    Flush(SyncSender<()>),
}

struct FileLogger {
    tx: SyncSender<Msg>,
    level: log::LevelFilter,
}

static LOGGER: OnceLock<FileLogger> = OnceLock::new();
static WRITER: Mutex<Option<std::thread::JoinHandle<()>>> = Mutex::new(None);

impl log::Log for FileLogger {
    fn enabled(&self, m: &log::Metadata<'_>) -> bool {
        m.level() <= self.level
    }

    fn log(&self, r: &log::Record<'_>) {
        if !self.enabled(r.metadata()) {
            return;
        }
        let ts = jiff::Timestamp::now().strftime("%Y-%m-%dT%H:%M:%S%.3fZ");
        let line = format!("{ts} {:<5} {}: {}", r.level(), r.target(), r.args());
        // A full queue drops the line: logging never waits for the disk.
        let _ = self.tx.try_send(Msg::Line(line));
    }

    fn flush(&self) {
        flush_log(Duration::from_millis(500));
    }
}

fn writer_loop(rx: &Receiver<Msg>, mut file: RotatingFile) {
    while let Ok(first) = rx.recv() {
        let mut next = Some(first);
        while let Some(m) = next.take() {
            match m {
                Msg::Line(l) => {
                    let _ = file.write_line(&l);
                }
                Msg::Flush(ack) => {
                    file.flush();
                    let _ = ack.send(());
                }
            }
            match rx.try_recv() {
                Ok(m) => next = Some(m),
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
            }
        }
        file.flush();
    }
}

/// The level: `CSM_LOG` (`error`..`trace`, `off`) when set and valid, else Info.
pub fn level_from_env(v: Option<&str>) -> log::LevelFilter {
    v.and_then(|s| s.trim().parse().ok()).unwrap_or(log::LevelFilter::Info)
}

/// Starts logging to `<dir>/codeshell.log` (level from `CSM_LOG`, default Info). Returns false
/// when a logger is already set or the writer thread cannot start.
pub fn init_file_logger(dir: &Path) -> bool {
    let level = level_from_env(std::env::var("CSM_LOG").ok().as_deref());
    let (tx, rx) = sync_channel(LOG_QUEUE);
    let file = RotatingFile::new(dir.join("codeshell.log"), LOG_MAX_BYTES, LOG_KEEP_FILES);
    let Ok(handle) = std::thread::Builder::new().name("csm-log".into()).spawn(move || writer_loop(&rx, file)) else {
        return false;
    };
    let logger = LOGGER.get_or_init(|| FileLogger { tx, level });
    if log::set_logger(logger).is_err() {
        return false;
    }
    log::set_max_level(level);
    *WRITER.lock().unwrap_or_else(PoisonError::into_inner) = Some(handle);
    true
}

/// Waits (up to `timeout`) until everything logged so far is on disk. Best effort.
pub fn flush_log(timeout: Duration) {
    let Some(l) = LOGGER.get() else { return };
    let (ack_tx, ack_rx) = sync_channel(1);
    if l.tx.try_send(Msg::Flush(ack_tx)).is_ok() {
        let _ = ack_rx.recv_timeout(timeout);
    }
}

// ---------------------------------------------------------------------------------------------
// The run marker

/// What the last run left behind, read at start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LastRun {
    /// Ended in an orderly quit and wrote no crash report since it started (or the first run).
    Fine,
    /// The marker still said "running" (killed, aborted, power loss): no report, or the report here.
    Unclean(Option<PathBuf>),
    /// Quit normally, but a panic on another thread wrote this report during that run.
    PanickedThread(PathBuf),
}

impl LastRun {
    /// The one-time notice for the next start, if one is due.
    pub fn notice(&self) -> Option<String> {
        match self {
            LastRun::Fine => None,
            LastRun::Unclean(Some(p)) => Some(format!("CodeShellManager closed unexpectedly last time. A crash report was saved: {}", p.display())),
            LastRun::Unclean(None) => Some("CodeShellManager closed unexpectedly last time.".to_owned()),
            LastRun::PanickedThread(p) => {
                Some(format!("CodeShellManager hit an internal error last time. A crash report was saved: {}", p.display()))
            }
        }
    }
}

fn newest_crash_since(dir: &Path, since: SystemTime) -> Option<PathBuf> {
    crash_logs(dir).into_iter().rev().find(|p| std::fs::metadata(p).and_then(|m| m.modified()).is_ok_and(|t| t >= since))
}

/// Reads the last run's marker, then marks this run as running. Call once per start, after the
/// one-app-per-state check. Never fails: a folder that cannot be written just means no notice.
pub fn begin_run(dir: &Path) -> LastRun {
    let marker = dir.join(MARKER);
    let last = std::fs::metadata(&marker).ok().map(|m| (m.modified().ok(), std::fs::read_to_string(&marker).unwrap_or_default()));
    let result = match last {
        None => LastRun::Fine,
        Some((started, state)) => {
            let report = started.and_then(|t| newest_crash_since(dir, t));
            if state.trim() == "clean" {
                report.map_or(LastRun::Fine, LastRun::PanickedThread)
            } else {
                LastRun::Unclean(report)
            }
        }
    };
    let _ = std::fs::create_dir_all(dir);
    let _ = std::fs::write(&marker, "running");
    result
}

/// Marks an orderly quit and flushes the log. The marker file is kept (its modified time is the
/// start of the run, which is what "a crash report newer than that run" compares against).
pub fn end_run(dir: &Path) {
    flush_log(Duration::from_secs(1));
    let marker = dir.join(MARKER);
    let started = std::fs::metadata(&marker).and_then(|m| m.modified()).ok();
    let _ = std::fs::write(&marker, "clean");
    // Writing moved the modified time to now; put it back to the start of the run.
    if let (Some(t), Ok(f)) = (started, std::fs::OpenOptions::new().write(true).open(&marker)) {
        let _ = f.set_modified(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>() -> PanicFacts<'a> {
        PanicFacts { app: "codeshell", thread: "main", message: "boom", location: "x.rs:1:2", backtrace: "frames" }
    }

    #[test]
    fn a_crash_report_is_written_with_the_facts() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path().join("logs");
        let p = write_crash_log(&dir, &render_report(&facts())).unwrap();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("crash-") && name.ends_with("Z.log"), "{name}");
        let text = std::fs::read_to_string(p).unwrap();
        for want in ["version:", "channel", "os:", "thread: main", "message: boom", "location: x.rs:1:2", "frames"] {
            assert!(text.contains(want), "{want} missing in {text}");
        }
    }

    #[test]
    fn two_reports_in_one_second_do_not_collide_and_old_ones_are_pruned() {
        let t = tempfile::tempdir().unwrap();
        for _ in 0..(KEEP_CRASH_LOGS + 5) {
            write_crash_log(t.path(), "x").unwrap();
        }
        assert_eq!(crash_logs(t.path()).len(), KEEP_CRASH_LOGS);
    }

    #[test]
    fn the_hook_writes_a_report_for_a_panic_on_a_thread_and_chains() {
        // The only test that installs the hook (it is process-global); the thread's panic is
        // joined, so the test itself does not fail.
        let t = tempfile::tempdir().unwrap();
        install_panic_hook(Some(t.path().to_path_buf()), "test");
        let r = std::thread::Builder::new().name("doomed".into()).spawn(|| panic!("kaboom")).unwrap().join();
        assert!(r.is_err());
        let logs = crash_logs(t.path());
        assert_eq!(logs.len(), 1);
        let text = std::fs::read_to_string(&logs[0]).unwrap();
        assert!(text.contains("thread: doomed") && text.contains("message: kaboom") && text.contains("backtrace:"), "{text}");
    }

    #[test]
    fn the_log_file_rotates_and_keeps_three_files() {
        let t = tempfile::tempdir().unwrap();
        let path = t.path().join("codeshell.log");
        let mut f = RotatingFile::new(path.clone(), 100, 3);
        for i in 0..40 {
            f.write_line(&format!("line number {i:04} with some padding")).unwrap();
        }
        f.flush();
        assert!(path.is_file());
        assert!(t.path().join("codeshell.log.1").is_file());
        assert!(t.path().join("codeshell.log.2").is_file());
        assert!(!t.path().join("codeshell.log.3").exists());
        assert!(std::fs::metadata(&path).unwrap().len() <= 100);
        let newest = std::fs::read_to_string(&path).unwrap();
        assert!(newest.contains("0039"));
    }

    #[test]
    fn the_level_comes_from_csm_log() {
        assert_eq!(level_from_env(None), log::LevelFilter::Info);
        assert_eq!(level_from_env(Some("debug")), log::LevelFilter::Debug);
        assert_eq!(level_from_env(Some("nonsense")), log::LevelFilter::Info);
    }

    #[test]
    fn marker_logic() {
        let t = tempfile::tempdir().unwrap();
        let dir = t.path();
        // First run, then an orderly quit, then a start: nothing to say.
        assert_eq!(begin_run(dir), LastRun::Fine);
        end_run(dir);
        assert_eq!(begin_run(dir), LastRun::Fine);
        // Killed without a report.
        let again = begin_run(dir);
        assert_eq!(again, LastRun::Unclean(None));
        assert_eq!(again.notice().as_deref(), Some("CodeShellManager closed unexpectedly last time."));
        // Killed after a panic report written during that run.
        std::thread::sleep(Duration::from_millis(20));
        let report = write_crash_log(dir, "r").unwrap();
        let got = begin_run(dir);
        assert_eq!(got, LastRun::Unclean(Some(report.clone())));
        assert!(got.notice().unwrap().contains(&report.display().to_string()));
        // A panic on a worker thread, then an orderly quit.
        std::thread::sleep(Duration::from_millis(20));
        let report2 = write_crash_log(dir, "r2").unwrap();
        end_run(dir);
        assert_eq!(begin_run(dir), LastRun::PanickedThread(report2));
        // The notice is one-time: the run after it is fine.
        end_run(dir);
        assert_eq!(begin_run(dir), LastRun::Fine);
    }
}
