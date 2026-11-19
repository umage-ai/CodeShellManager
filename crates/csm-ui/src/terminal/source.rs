//! What a pane draws: a [`TerminalSource`]. The demo backend implements it with
//! [`LocalTerminal`](super::LocalTerminal) (a live PTY) and [`StaticTerminal`] (bytes fed once);
//! the engine's `TerminalHandle` implements it when the real engine is wired in.

use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex, PoisonError};

use csm_term::alacritty_terminal::term::TermMode;
use csm_term::input::{self, Untrusted};
use csm_term::{GridSize, ReplySink, TermConfig, Terminal, load_mode};

/// Lock a std mutex, recovering from poisoning (a panicking thread must not wedge the UI).
/// Only for short critical sections that no other thread holds for long.
pub fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the view shows while there is nothing to draw, and in the pane header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceState {
    /// Waiting for the first measured size, or for the process to start.
    Starting,
    Running,
    Failed(String),
    Exited(Option<i32>),
}

/// A terminal a pane can show. Every method is cheap and never blocks the UI thread: the
/// emulator is behind a mutex the view only ever `try_lock`s.
pub trait TerminalSource: Send + Sync {
    /// The emulator. `None` until the first [`TerminalSource::set_grid`].
    fn terminal(&self) -> Option<Arc<Mutex<Terminal>>>;
    /// Terminal modes (mouse reporting, bracketed paste, …) without taking the lock.
    fn mode(&self) -> TermMode;
    /// The grid the interactive view measured, every frame. The first call creates the
    /// emulator and, for a live source, starts the process at exactly that size: there is no
    /// placeholder size anywhere. Later calls resize the emulator and the PTY when it changed.
    fn set_grid(&self, grid: GridSize);
    /// Raw bytes for the PTY. The view uses the typed methods below; a source with no notion of
    /// "who typed" implements only this and gets them for free.
    fn write(&self, bytes: &[u8]);
    fn state(&self) -> SourceState;

    /// A keystroke, typed text or an IME commit from the person (encoded by the view). The
    /// engine treats it as the person having seen the session.
    fn send_key(&self, bytes: &[u8]) {
        self.write(bytes);
    }

    /// Mouse reports: input, but not the person answering the session.
    fn send_mouse(&self, bytes: &[u8]) {
        self.write(bytes);
    }

    /// Clipboard text or a drop: untrusted, bracketed when the program asked for it.
    fn paste(&self, text: &str) {
        self.write(&input::paste(Untrusted(text), self.mode()));
    }

    /// Focus in/out reports, when the program asked for them.
    fn focus_changed(&self, focused: bool) {
        if let Some(b) = input::focus(focused, self.mode()) {
            self.write(&b.0);
        }
    }
}

/// Replies the emulator writes (DA1 answers and the like). A static terminal has no process, so
/// they go nowhere.
struct Discard;

impl ReplySink for Discard {
    fn send(&self, _: &[u8]) {}
}

/// A terminal with no process: bytes fed once (a transcript, a test fixture). Input is dropped.
pub struct StaticTerminal {
    term: Arc<Mutex<Terminal>>,
    mode: Arc<AtomicU32>,
}

impl StaticTerminal {
    /// Feeds `bytes` into a terminal of `grid`. Resizing later reflows like a real one.
    pub fn new(grid: GridSize, bytes: &[u8]) -> Self {
        let mut t = Terminal::new(grid, TermConfig::default(), Arc::new(Discard));
        t.advance(bytes);
        let mode = t.shared_mode();
        Self { term: Arc::new(Mutex::new(t)), mode }
    }
}

impl TerminalSource for StaticTerminal {
    fn terminal(&self) -> Option<Arc<Mutex<Terminal>>> {
        Some(self.term.clone())
    }

    fn mode(&self) -> TermMode {
        load_mode(&self.mode)
    }

    fn set_grid(&self, grid: GridSize) {
        if let Ok(mut t) = self.term.try_lock()
            && t.size() != grid
        {
            t.resize(grid);
        }
    }

    fn write(&self, _: &[u8]) {}

    fn state(&self) -> SourceState {
        SourceState::Running
    }
}
