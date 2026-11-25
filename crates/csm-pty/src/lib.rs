//! Pseudo-terminals for CodeShellManager.
//!
//! * Windows: our own ConPTY binding (`conpty` module): the bundled `conpty.dll` +
//!   `OpenConsole.exe` loaded **by full path** from the executable's folder, falling back to the
//!   inbox `CreatePseudoConsole` from `System32\kernel32.dll` (also by full path). Each pseudo
//!   console remembers the implementation that created it. Three consecutive bundled failures
//!   demote the rest of the run to inbox. See docs/wpf/lessons.md "Bundled ConPTY".
//! * Unix: `portable-pty` (openpty), which is fine there (its Windows backend loads `conpty.dll`
//!   by bare name, which is why it is not used on Windows).
//!
//! Output read before the first subscriber is kept and flushed in order ([`early`]). The whole
//! process tree is killed on drop (Job Object on Windows, the session's process group on Unix).
//!
//! `unsafe` is denied crate-wide; only the Windows FFI module opts back in, and every block
//! there carries a `// SAFETY:` comment.

#![deny(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    /// Windows only: when set, the command line is `argv[0]` followed by this text verbatim, and
    /// `argv[1..]` must be empty. For `cmd.exe /c "<command>"` run commands, whose parser is not
    /// MSVCRT's (see [`cmdline::build_command_line_verbatim`]). Ignored elsewhere (argv is used).
    pub verbatim_args: Option<OsString>,
}

impl SpawnOptions {
    pub fn new(argv: Vec<OsString>, size: PtySize) -> Self {
        Self { argv, cwd: None, env: Vec::new(), size, allow_bundled: true, verbatim_args: None }
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
    /// Queues an emulator-generated reply (DA1, cursor report, colour reply…). Same queue and
    /// order as [`Pty::write`], but dropped while more than [`MAX_PENDING_REPLY_BYTES`] are
    /// already waiting, so a child that floods queries and never reads cannot grow the queue
    /// without bound. Keystrokes and pastes use `write`, which is never dropped.
    fn write_reply(&self, bytes: &[u8]) -> io::Result<()> {
        self.write(bytes)
    }
    fn resize(&self, size: PtySize) -> io::Result<()>;
    /// Latched once the child has actually exited. Use this, not "do we hold a handle".
    fn has_exited(&self) -> bool;
    fn exit_code(&self) -> Option<i32>;
    /// Kills the whole process tree. Output already produced can still be read.
    fn kill(&self);
    /// Asks the program to end gracefully: the Windows pseudo console is closed (attached
    /// processes get `CTRL_CLOSE_EVENT`, as a closed console window would send), on Unix the
    /// process group gets `SIGHUP`. Never blocks; wait for [`Pty::has_exited`] and fall back to
    /// [`Pty::kill`]. The default is a kill, for implementations with no gentler way.
    fn close(&self) {
        self.kill();
    }
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
pub(crate) fn spawn_reader(name: String, mut source: impl Read + Send + 'static, output: Arc<EarlyOutputBuffer>) -> io::Result<()> {
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
pub(crate) fn spawn_writer(name: String, mut sink: impl Write + Send + 'static) -> io::Result<WriterTx> {
    let (tx, rx) = mpsc::channel::<Vec<u8>>();
    let pending = Arc::new(AtomicUsize::new(0));
    let counter = pending.clone();
    std::thread::Builder::new().name(name).spawn(move || {
        for chunk in rx {
            let len = chunk.len();
            let result = sink.write_all(&chunk).and_then(|()| sink.flush());
            counter.fetch_sub(len, Ordering::AcqRel);
            if let Err(e) = result {
                log::debug!("PTY writer ended: {e}");
                break;
            }
        }
    })?;
    Ok(WriterTx { tx, pending })
}

/// Most emulator-reply bytes allowed to wait in the writer queue (16 KiB: a few hundred DA1 /
/// cursor-report answers). The writer drains the queue as fast as the child reads; this only
/// bites when the child does not read.
pub const MAX_PENDING_REPLY_BYTES: usize = 16 * 1024;

/// The sending half of a writer thread's queue, plus how many bytes are waiting in it.
pub(crate) struct WriterTx {
    tx: Sender<Vec<u8>>,
    pending: Arc<AtomicUsize>,
}

impl WriterTx {
    fn send(&self, bytes: &[u8]) -> io::Result<()> {
        self.pending.fetch_add(bytes.len(), Ordering::AcqRel);
        self.tx.send(bytes.to_vec()).map_err(|_| {
            self.pending.fetch_sub(bytes.len(), Ordering::AcqRel);
            io::Error::new(io::ErrorKind::BrokenPipe, "PTY writer has stopped")
        })
    }
}

/// User input (keys, pastes, programmatic sends): always queued. These are bounded by their
/// sources (a human, a paste, a tool call), never by the child's output, and dropping any of it
/// would corrupt what the user typed.
pub(crate) fn send_to_writer(writer: &Mutex<Option<WriterTx>>, bytes: &[u8]) -> io::Result<()> {
    let guard = writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_ref() {
        Some(w) => w.send(bytes),
        None => Err(io::Error::new(io::ErrorKind::BrokenPipe, "PTY is closed")),
    }
}

/// Emulator replies: queued in order with user input, but dropped (not an error) while
/// [`MAX_PENDING_REPLY_BYTES`] are already waiting. A reply to a query the child is flooding and
/// not reading has no reader to matter to.
pub(crate) fn send_reply_to_writer(writer: &Mutex<Option<WriterTx>>, bytes: &[u8]) -> io::Result<()> {
    let guard = writer.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_ref() {
        Some(w) if w.pending.load(Ordering::Acquire) >= MAX_PENDING_REPLY_BYTES => {
            log::debug!("PTY reply dropped: {} bytes already queued", w.pending.load(Ordering::Relaxed));
            Ok(())
        }
        Some(w) => w.send(bytes),
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

    /// A sink that never finishes its first write, like a child that never reads stdin.
    struct Stuck(mpsc::Receiver<()>);

    impl Write for Stuck {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            let _ = self.0.recv();
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn reply_flood_to_a_child_that_never_reads_is_bounded() {
        let (release, gate) = mpsc::channel::<()>();
        let tx = spawn_writer("test-stuck".into(), Stuck(gate)).unwrap();
        let slot = Mutex::new(Some(tx));
        // 2.1M DA1 answers, as in the probe: 8 bytes each is ~17 MB if unbounded.
        for _ in 0..2_100_000 {
            send_reply_to_writer(&slot, b"\x1b[?62;c").unwrap();
        }
        let pending = slot.lock().unwrap().as_ref().map(|w| w.pending.load(Ordering::Acquire)).unwrap();
        assert!(pending <= MAX_PENDING_REPLY_BYTES + 16, "pending {pending}");
        // User input is never dropped, even over the reply cap.
        send_to_writer(&slot, &vec![b'k'; MAX_PENDING_REPLY_BYTES * 2]).unwrap();
        let pending = slot.lock().unwrap().as_ref().map(|w| w.pending.load(Ordering::Acquire)).unwrap();
        assert!(pending >= MAX_PENDING_REPLY_BYTES * 2);
        drop(release);
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
