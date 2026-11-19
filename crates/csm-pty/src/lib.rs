//! Pseudo-terminals for CodeShellManager.
//!
//! * Windows: our own ConPTY binding (`conpty` module): the bundled `conpty.dll` +
//!   `OpenConsole.exe` loaded **by full path** from the executable's folder, falling back to the
//!   inbox `CreatePseudoConsole` from `System32\kernel32.dll` (also by full path). Each pseudo
//!   console remembers the implementation that created it. Three consecutive bundled failures
//!   demote the rest of the run to inbox. See the WPF repo's CLAUDE.md "Bundled ConPTY".
//! * Unix: `portable-pty` (openpty), which is fine there (its Windows backend loads `conpty.dll`
//!   by bare name, which is why it is not used on Windows).
//!
//! Output read before the first subscriber is kept and flushed in order ([`early`]). The whole
//! process tree is killed on drop (Job Object on Windows, the session's process group on Unix).
//!
//! `unsafe` is denied crate-wide; only the Windows FFI module opts back in, and every block
//! there carries a `// SAFETY:` comment.

#![deny(unsafe_code)]

pub mod cmdline;
pub mod early;
pub mod policy;

#[cfg(windows)]
#[allow(unsafe_code)]
mod conpty;
#[cfg(windows)]
mod resolve;
#[cfg(unix)]
mod unix;

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};

pub use early::{EarlyOutputBuffer, OutputSink};

/// A PTY size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PtySize {
    pub cols: u16,
    pub rows: u16,
}

impl PtySize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols: cols.max(1), rows: rows.max(1) }
    }
}

/// What to start. `argv[0]` is resolved on `PATH` (never the current directory); nothing is
/// passed through a shell.
#[derive(Debug, Clone)]
pub struct SpawnOptions {
    pub argv: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Environment changes on top of the current process environment: `Some` sets, `None`
    /// removes.
    pub env: Vec<(OsString, Option<OsString>)>,
    /// The size the PTY is created with. Callers pass the real measured grid, never a
    /// placeholder (CLAUDE.md "A fit that changes nothing must still report the size").
    pub size: PtySize,
    /// Windows only: false keeps this PTY on the inbox ConPTY whatever the process-wide choice
    /// (headless run commands, which have no terminal to answer OpenConsole's DA1).
    pub allow_bundled: bool,
}

impl SpawnOptions {
    pub fn new(argv: Vec<OsString>, size: PtySize) -> Self {
        Self { argv, cwd: None, env: Vec::new(), size, allow_bundled: true }
    }
}

/// Which implementation a PTY runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Backend {
    /// Bundled `conpty.dll` + `OpenConsole.exe` beside the executable.
    BundledConPty,
    /// The Windows inbox ConPTY (kernel32 → system conhost).
    InboxConPty,
    /// openpty via portable-pty.
    Unix,
}

impl Backend {
    pub fn label(self) -> &'static str {
        match self {
            Backend::BundledConPty => "bundled ConPTY",
            Backend::InboxConPty => "inbox ConPTY",
            Backend::Unix => "openpty",
        }
    }
}

/// Called once with the exit code (`None` when it could not be read).
pub type ExitCallback = Box<dyn FnOnce(Option<i32>) + Send>;

/// A running pseudo-terminal. All methods are cheap and non-blocking; writes are queued to a
/// dedicated writer thread so their order is the order of the calls.
pub trait Pty: Send + Sync {
    /// Installs the output sink, called on the reader thread with raw bytes. The first sink
    /// receives everything read before it was installed, in order.
    fn subscribe(&self, sink: OutputSink);
    /// Runs `cb` once the child has exited — immediately if it already has.
    fn on_exit(&self, cb: ExitCallback);
    /// Queues bytes for the child's input.
    fn write(&self, bytes: &[u8]) -> io::Result<()>;
    fn resize(&self, size: PtySize) -> io::Result<()>;
    /// Latched once the child has actually exited. Use this, not "do we hold a handle".
    fn has_exited(&self) -> bool;
    fn exit_code(&self) -> Option<i32>;
    /// Kills the whole process tree. Output already produced can still be read.
    fn kill(&self);
    fn backend(&self) -> Backend;
    fn pid(&self) -> Option<u32>;
}

/// Starts `opts.argv` on a new PTY. Blocking (process creation, a DLL load on first use): call
/// it off the UI thread.
pub fn spawn(opts: SpawnOptions) -> io::Result<Arc<dyn Pty>> {
    if opts.argv.is_empty() {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "empty argv"));
    }
    #[cfg(windows)]
    {
        conpty::spawn(&opts).map(|p| p as Arc<dyn Pty>)
    }
    #[cfg(unix)]
    {
        unix::spawn(&opts).map(|p| p as Arc<dyn Pty>)
    }
}

/// Windows: whether new PTYs prefer the bundled ConPTY. Read when the first PTY is created;
/// later changes have no effect (as in the WPF app, a change needs a restart).
pub fn set_prefer_bundled(prefer: bool) {
    #[cfg(windows)]
    conpty::set_prefer_bundled(prefer);
    #[cfg(not(windows))]
    let _ = prefer;
}

/// Windows: the ConPTY implementation new PTYs use and why, resolving it if needed (this loads
/// a DLL — call it off the UI thread). `None` on other platforms.
pub fn conpty_selection() -> Option<(Backend, String)> {
    #[cfg(windows)]
    {
        conpty::selection()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

// ── Shared plumbing (safe) ───────────────────────────────────────────────────────────────

/// Exit latch: `has_exited` becomes true BEFORE callbacks run, so a caller that checks it from
/// inside a callback, or subscribes concurrently, never sees "not exited" for a gone process.
#[derive(Default)]
pub(crate) struct ExitState {
    exited: AtomicBool,
    inner: Mutex<ExitInner>,
}

#[derive(Default)]
struct ExitInner {
    code: Option<i32>,
    latched: bool,
    callbacks: Vec<ExitCallback>,
}

impl ExitState {
    fn lock(&self) -> std::sync::MutexGuard<'_, ExitInner> {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn latch(&self, code: Option<i32>) {
        let callbacks = {
            let mut inner = self.lock();
            if inner.latched {
                return;
            }
            inner.latched = true;
            inner.code = code;
            self.exited.store(true, Ordering::Release);
            std::mem::take(&mut inner.callbacks)
        };
        for cb in callbacks {
            cb(code);
        }
    }

    pub(crate) fn on_exit(&self, cb: ExitCallback) {
        let code = {
            let mut inner = self.lock();
            if !inner.latched {
                inner.callbacks.push(cb);
                return;
            }
            inner.code
        };
        cb(code);
    }

    pub(crate) fn has_exited(&self) -> bool {
        self.exited.load(Ordering::Acquire)
    }

    pub(crate) fn code(&self) -> Option<i32> {
        self.lock().code
    }
}

/// A dedicated reader thread (never a pool thread: the read blocks for the session's life).
pub(crate) fn spawn_reader(
    name: String,
    mut source: impl Read + Send + 'static,
    output: Arc<EarlyOutputBuffer>,
) -> io::Result<()> {
    std::thread::Builder::new().name(name).spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match source.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => output.deliver(buf.get(..n).unwrap_or_default()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    log::debug!("PTY reader ended: {e}");
                    break;
                }
            }
        }
    })?;
    Ok(())
}

/// A dedicated writer thread: one queue, so writes from the UI (keys, pastes) and from the
/// emulator (DA1, 7501 replies) reach the child in exactly the order they were queued, and a
/// child that stops reading can never block the caller.
pub(crate) fn spawn_writer(
    name: String,
    mut sink: impl Write + Send + 'static,
) -> io::Result<Sender<Vec<u8>>> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    std::thread::Builder::new().name(name).spawn(move || {
        for chunk in rx {
            if let Err(e) = sink.write_all(&chunk).and_then(|()| sink.flush()) {
                log::debug!("PTY writer ended: {e}");
                break;
            }
        }
    })?;
    Ok(tx)
}

pub(crate) fn send_to_writer(writer: &Mutex<Option<Sender<Vec<u8>>>>, bytes: &[u8]) -> io::Result<()> {
    let guard = writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_ref() {
        Some(tx) => tx
            .send(bytes.to_vec())
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "PTY writer has stopped")),
        None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "PTY is closed")),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn exit_latch_runs_late_subscribers_immediately() {
        let state = ExitState::default();
        let early = Arc::new(Mutex::new(None));
        let e = early.clone();
        state.on_exit(Box::new(move |c| *e.lock().unwrap() = Some(c)));
        assert!(!state.has_exited());
        state.latch(Some(7));
        assert!(state.has_exited());
        assert_eq!(*early.lock().unwrap(), Some(Some(7)));
        let late = Arc::new(Mutex::new(None));
        let l = late.clone();
        state.on_exit(Box::new(move |c| *l.lock().unwrap() = Some(c)));
        assert_eq!(*late.lock().unwrap(), Some(Some(7)));
        // A second latch is ignored.
        state.latch(Some(9));
        assert_eq!(state.code(), Some(7));
    }

    #[test]
    fn has_exited_is_true_inside_the_callback() {
        let state = Arc::new(ExitState::default());
        let seen = Arc::new(AtomicBool::new(false));
        let (s, v) = (state.clone(), seen.clone());
        state.on_exit(Box::new(move |_| v.store(s.has_exited(), Ordering::SeqCst)));
        state.latch(None);
        assert!(seen.load(Ordering::SeqCst));
    }
}
