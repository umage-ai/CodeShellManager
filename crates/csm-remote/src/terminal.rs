//! A remote pane's terminal: a local emulator that the worker feeds with the host's bytes, and
//! the input and size requests that go back to the host.
//!
//! The emulator is never resized locally. Its size changes only when the host says the PTY changed
//! (an attach or a `term_resized` in stream order), so it stays in lockstep with the host's.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex, PoisonError};

use csm_peer_proto::codec::Input;
use csm_term::alacritty_terminal::term::TermMode;
use csm_term::{GridSize, ReplySink, TermConfig, Terminal, load_mode};
use csm_ui::terminal::{SourceState, TerminalSource};

use crate::link::Cmd;

/// No attach has arrived yet.
pub(crate) const STARTING: u8 = 0;
/// The emulator shows the host's screen.
pub(crate) const RUNNING: u8 = 1;
/// The host ended the stream (the session is gone, or it was unsubscribed).
pub(crate) const ENDED: u8 = 2;

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

struct Discard;

impl ReplySink for Discard {
    fn send(&self, _: &[u8]) {}
}

/// One session's terminal in a remote window. Created by [`crate::RemoteBackend::show_terminals`]
/// when the pane is shown, and dropped (with its stream) when it is hidden.
pub struct RemoteTerminal {
    session: String,
    stream: u32,
    term: Arc<Mutex<Terminal>>,
    mode: Arc<AtomicU32>,
    state: Arc<AtomicU8>,
    cmds: Sender<Cmd>,
    /// The last size the pane asked for (`set_grid`), so an unchanged size sends nothing.
    requested: Mutex<Option<(u16, u16)>>,
    driver: AtomicBool,
}

impl RemoteTerminal {
    pub(crate) fn new(session: &str, stream: u32, cmds: Sender<Cmd>) -> Self {
        let term = Terminal::new(GridSize::new(80, 24), TermConfig::default(), Arc::new(Discard));
        let mode = Arc::new(AtomicU32::new(term.mode().bits()));
        Self {
            session: session.to_owned(),
            stream,
            term: Arc::new(Mutex::new(term)),
            mode,
            state: Arc::new(AtomicU8::new(STARTING)),
            cmds,
            requested: Mutex::new(None),
            driver: AtomicBool::new(false),
        }
    }

    /// The session this pane shows.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The stream id the host knows this pane by.
    pub fn stream(&self) -> u32 {
        self.stream
    }

    /// Whether this pane is the one typing: the host then sizes the PTY to it. Sent again with the
    /// last requested size when it changes. Called every frame by the UI: an unchanged mark sends nothing.
    pub fn set_driver(&self, driver: bool) {
        if self.driver.swap(driver, Ordering::AcqRel) == driver {
            return;
        }
        if let Some((cols, rows)) = *lock(&self.requested) {
            self.send(Cmd::Resize { stream: self.stream, cols, rows, driver });
        }
    }

    pub(crate) fn subscribe_cmd(&self) -> Cmd {
        Cmd::Subscribe {
            stream: self.stream,
            session: self.session.clone(),
            term: self.term.clone(),
            mode: self.mode.clone(),
            state: self.state.clone(),
        }
    }

    fn send(&self, cmd: Cmd) {
        // A closed worker has nothing to hear the input: the window shows its state anyway.
        let _ = self.cmds.send(cmd);
    }

    fn input(&self, input: Input) {
        self.send(Cmd::Input { stream: self.stream, input });
    }
}

impl TerminalSource for RemoteTerminal {
    fn terminal(&self) -> Option<Arc<Mutex<Terminal>>> {
        Some(self.term.clone())
    }

    fn mode(&self) -> TermMode {
        load_mode(&self.mode)
    }

    fn set_grid(&self, grid: GridSize) {
        let want = (grid.cols, grid.rows);
        let mut requested = lock(&self.requested);
        if *requested == Some(want) {
            return;
        }
        *requested = Some(want);
        drop(requested);
        self.send(Cmd::Resize { stream: self.stream, cols: want.0, rows: want.1, driver: self.driver.load(Ordering::Acquire) });
    }

    fn write(&self, bytes: &[u8]) {
        self.input(Input::Key(bytes.to_vec()));
    }

    fn send_key(&self, bytes: &[u8]) {
        self.input(Input::Key(bytes.to_vec()));
    }

    fn send_mouse(&self, bytes: &[u8]) {
        self.input(Input::Mouse(bytes.to_vec()));
    }

    /// The host brackets the text (it is untrusted), so the client never encodes it.
    fn paste(&self, text: &str) {
        self.input(Input::Paste(text.to_owned()));
    }

    fn focus_changed(&self, focused: bool) {
        self.input(Input::Focus(focused));
    }

    fn state(&self) -> SourceState {
        match self.state.load(Ordering::Acquire) {
            RUNNING => SourceState::Running,
            ENDED => SourceState::Exited(None),
            _ => SourceState::Starting,
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::mpsc;

    use super::*;

    fn resizes(rx: &mpsc::Receiver<Cmd>) -> Vec<bool> {
        rx.try_iter()
            .filter_map(|c| match c {
                Cmd::Resize { driver, .. } => Some(driver),
                _ => None,
            })
            .collect()
    }

    /// The driver mark is set every frame by the UI: only a change reaches the host, and a change
    /// re-sends the size the pane last asked for, with the new mark.
    #[test]
    fn an_unchanged_driver_mark_sends_nothing() {
        let (tx, rx) = mpsc::channel();
        let t = RemoteTerminal::new("s1", 1, tx);
        t.set_grid(GridSize::new(90, 20));
        assert_eq!(resizes(&rx), vec![false], "the first size is sent, not driving");

        t.set_driver(false);
        assert!(resizes(&rx).is_empty(), "the same mark again sends nothing");

        t.set_driver(true);
        assert_eq!(resizes(&rx), vec![true], "taking the driver role re-sends the size");
        for _ in 0..10 {
            t.set_driver(true);
        }
        assert!(resizes(&rx).is_empty(), "a frame that keeps the mark sends nothing");

        t.set_grid(GridSize::new(90, 20));
        assert!(resizes(&rx).is_empty(), "an unchanged size sends nothing");
    }
}
