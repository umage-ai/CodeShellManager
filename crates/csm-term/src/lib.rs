//! Terminal emulation for CodeShellManager: `alacritty_terminal` (VT state, scrollback,
//! selection, DEC 2026 synchronized output) behind an OSC tap that turns OSC 9001 / 7501 / 133 / 7
//! into typed events, and an in-order reply path.
//!
//! **Ordering.** Everything the terminal writes back (DA1, cursor reports, colour replies, the
//! 7501 probe reply) goes to one [`ReplySink`] in stream order: the emulator's own replies are
//! written synchronously while it parses, and the tap stops right after any sequence it acts on
//! so its reply lands between the replies to earlier and later bytes. Claude Code sends
//! `OSC 7501 ; ?` then `CSI c` and treats the DA1 answer as "no more replies"; the 7501 reply
//! therefore has to be written first, and is. The emulator runs in-process, so there is no web
//! round trip to race (the WPF app's "the reply is written synchronously on the UI thread").
//!
//! No UI toolkit here; the renderer reads [`Terminal::term`].

#![forbid(unsafe_code)]

pub mod colors;
pub mod input;
pub mod links;
pub mod probe;
pub mod tap;

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub use alacritty_terminal;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::{self, ansi};

pub use colors::Palette;
pub use probe::ProbeReply;
pub use tap::{PromptMark, TapEvent};

/// The XTVERSION answer: `DCS > | name(version) ST`.
pub const XTVERSION_REPLY: &[u8] = concat!("P>|CodeShellManager(", env!("CARGO_PKG_VERSION"), ")\\").as_bytes();

/// Where the terminal's own replies go (the PTY input). Called on whatever thread drives
/// [`Terminal::advance`]; implementations queue, they never block.
pub trait ReplySink: Send + Sync {
    fn send(&self, bytes: &[u8]);
}

/// Grid size plus the cell size in pixels (for `CSI 14 t` style queries).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub cols: u16,
    pub rows: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}

impl GridSize {
    pub fn new(cols: u16, rows: u16) -> Self {
        Self { cols: cols.max(2), rows: rows.max(1), cell_width: 1, cell_height: 1 }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }
    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }
    fn columns(&self) -> usize {
        usize::from(self.cols)
    }
}

#[derive(Debug, Clone)]
pub struct TermConfig {
    pub scrollback: usize,
    pub palette: Palette,
    pub probe_reply: ProbeReply,
}

impl Default for TermConfig {
    fn default() -> Self {
        Self { scrollback: 10_000, palette: Palette::default(), probe_reply: ProbeReply::Off }
    }
}

/// The largest OSC 52 copy passed on as [`TermEvent::ClipboardStore`] (decoded text, bytes).
/// A larger one is dropped here, so no host ever carries it.
pub const MAX_CLIPBOARD_STORE_BYTES: usize = 1024 * 1024;

/// Things the host should know about, in stream order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermEvent {
    Tap(TapEvent),
    /// The 7501 probe was seen; `answered` says whether a reply was written.
    ProbeAnswered {
        answered: bool,
    },
    Title(String),
    Bell,
    /// OSC 52 copy request. The host decides whether to honour it.
    ClipboardStore(String),
}

struct Shared {
    replies: Arc<dyn ReplySink>,
    events: Mutex<Vec<TermEvent>>,
    /// Colour slots with overrides applied, for answering OSC colour queries without the term.
    colors: Mutex<Vec<vte::ansi::Rgb>>,
    window: Mutex<WindowSize>,
}

impl Shared {
    fn push(&self, e: TermEvent) {
        self.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(e);
    }
}

/// The alacritty event listener: replies are written immediately (in order), everything else
/// becomes a [`TermEvent`].
#[derive(Clone)]
pub struct Listener(Arc<Shared>);

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        let shared = &self.0;
        match event {
            Event::PtyWrite(text) => shared.replies.send(text.as_bytes()),
            Event::ColorRequest(index, format) => {
                let rgb = shared.colors.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(index).copied();
                if let Some(rgb) = rgb {
                    shared.replies.send(format(rgb).as_bytes());
                }
            }
            Event::TextAreaSizeRequest(format) => {
                let size = *shared.window.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                shared.replies.send(format(size).as_bytes());
            }
            Event::Title(t) => shared.push(TermEvent::Title(t)),
            Event::ResetTitle => shared.push(TermEvent::Title(String::new())),
            Event::Bell => shared.push(TermEvent::Bell),
            Event::ClipboardStore(_, text) if text.len() <= MAX_CLIPBOARD_STORE_BYTES => shared.push(TermEvent::ClipboardStore(text)),
            Event::ClipboardStore(_, text) => log::warn!("an OSC 52 copy of {} bytes was dropped (over the cap)", text.len()),
            // Never answer an OSC 52 read: it would hand the clipboard to whatever is printing.
            Event::ClipboardLoad(..) => {}
            Event::MouseCursorDirty | Event::CursorBlinkingChange | Event::Wakeup | Event::Exit | Event::ChildExit(_) => {}
        }
    }
}

/// One terminal: emulator + parser + OSC tap.
pub struct Terminal {
    term: Term<Listener>,
    processor: ansi::Processor,
    tap_parser: vte::Parser,
    tap: tap::OscTap,
    shared: Arc<Shared>,
    config: TermConfig,
    probe: probe::ProbeGate,
    generation: u64,
    mode: Arc<AtomicU32>,
    size: GridSize,
}

impl Terminal {
    pub fn new(size: GridSize, config: TermConfig, replies: Arc<dyn ReplySink>) -> Self {
        let shared =
            Arc::new(Shared { replies, events: Mutex::new(Vec::new()), colors: Mutex::new(Vec::new()), window: Mutex::new(window_size(size)) });
        let term_config = Config { scrolling_history: config.scrollback, ..Config::default() };
        let term = Term::new(term_config, &size, Listener(shared.clone()));
        let mut t = Self {
            term,
            processor: ansi::Processor::new(),
            tap_parser: vte::Parser::new(),
            tap: tap::OscTap::default(),
            shared,
            config,
            probe: probe::ProbeGate::default(),
            generation: 0,
            mode: Arc::new(AtomicU32::new(TermMode::default().bits())),
            size,
        };
        t.refresh_colors();
        t.publish_mode();
        t
    }

    /// Feeds PTY output. See the crate docs for the ordering guarantee.
    pub fn advance(&mut self, bytes: &[u8]) {
        let mut rest = bytes;
        while !rest.is_empty() {
            self.tap.reset_stop();
            let mut n = self.tap_parser.advance_until_terminated(&mut self.tap, rest);
            if n == 0 {
                n = rest.len(); // cannot happen with a fresh stop flag; never loop forever
            }
            let (segment, tail) = rest.split_at(n.min(rest.len()));
            self.processor.advance(&mut self.term, segment);
            if std::mem::take(&mut self.tap.colors_changed) {
                self.refresh_colors();
            }
            if std::mem::take(&mut self.tap.xtversion_query) {
                self.shared.replies.send(XTVERSION_REPLY);
            }
            let events = std::mem::take(&mut self.tap.events);
            for event in events {
                self.handle_tap(event);
            }
            rest = tail;
        }
        self.generation = self.generation.wrapping_add(1);
        self.publish_mode();
    }

    fn handle_tap(&mut self, event: TapEvent) {
        if event == TapEvent::ProgramStatusProbe {
            // Bytes before the probe that the emulator is still holding in a synchronized
            // update would be answered AFTER our reply; flush them first.
            if self.processor.sync_bytes_count() > 0 {
                self.processor.stop_sync(&mut self.term);
            }
            let reply = self.probe.reply(Instant::now(), self.config.probe_reply, self.tap.win32_input_mode);
            let answered = reply.is_some();
            if let Some(bytes) = reply {
                self.shared.replies.send(&bytes);
            }
            log::info!("OSC 7501 probe seen; answered={answered} ({:?})", self.config.probe_reply);
            self.shared.push(TermEvent::ProbeAnswered { answered });
        }
        self.shared.push(TermEvent::Tap(event));
    }

    fn refresh_colors(&mut self) {
        let snap = self.config.palette.snapshot(self.term.colors());
        *self.shared.colors.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = snap;
    }

    fn publish_mode(&self) {
        self.mode.store(self.term.mode().bits(), Ordering::Release);
    }

    /// When a synchronized update (DEC 2026) is open, the time it must be flushed by.
    pub fn sync_deadline(&self) -> Option<Instant> {
        self.processor.sync_timeout().sync_timeout()
    }

    /// Flushes a synchronized update whose timeout passed. Returns true if it did.
    pub fn flush_sync_if_due(&mut self, now: Instant) -> bool {
        match self.sync_deadline() {
            Some(deadline) if now >= deadline => {
                self.processor.stop_sync(&mut self.term);
                self.generation = self.generation.wrapping_add(1);
                self.publish_mode();
                true
            }
            _ => false,
        }
    }

    pub fn resize(&mut self, size: GridSize) {
        if size == self.size {
            return;
        }
        self.size = size;
        *self.shared.window.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = window_size(size);
        self.term.resize(size);
        self.generation = self.generation.wrapping_add(1);
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    pub fn set_probe_reply(&mut self, mode: ProbeReply) {
        self.config.probe_reply = mode;
    }

    pub fn palette(&self) -> &Palette {
        &self.config.palette
    }

    /// The default colours: what OSC 4/10/11/12 queries answer (a session's own look, or the
    /// app's). Colours a program set with OSC still win.
    pub fn set_palette(&mut self, palette: Palette) {
        if self.config.palette == palette {
            return;
        }
        self.config.palette = palette;
        self.refresh_colors();
        self.touch();
    }

    /// The cursor a program gets until it asks for its own (DECSCUSR), and goes back to when it
    /// resets it (`CSI 0 SP q`).
    pub fn set_default_cursor(&mut self, shape: ansi::CursorShape, blinking: bool) {
        let style = ansi::CursorStyle { shape, blinking };
        let config = Config { scrolling_history: self.config.scrollback, default_cursor_style: style, ..Config::default() };
        self.term.set_options(config);
        self.touch();
    }

    pub fn term(&self) -> &Term<Listener> {
        &self.term
    }

    /// Mutable access for selection and scrolling. Call [`Terminal::touch`] after a change that
    /// affects rendering.
    pub fn term_mut(&mut self) -> &mut Term<Listener> {
        &mut self.term
    }

    pub fn touch(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Changes whenever anything visible may have changed. Renderers cache on it.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn mode(&self) -> TermMode {
        *self.term.mode()
    }

    /// The lowest line of the visible screen with a letter or digit on it, trimmed (the Board's
    /// "doing now" line). Borders, rules and an empty prompt don't count. `None` when there is
    /// no such line. At most `max_chars` characters.
    pub fn last_text_line(&self, max_chars: usize) -> Option<String> {
        use alacritty_terminal::index::{Column, Line};
        use alacritty_terminal::term::cell::Flags;
        let grid = self.term.grid();
        let rows = i32::try_from(grid.screen_lines()).unwrap_or(0);
        let cols = grid.columns();
        for line in (0..rows).rev() {
            let row = &grid[Line(line)];
            let mut s = String::with_capacity(cols);
            for c in 0..cols {
                let cell = &row[Column(c)];
                if !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    s.push(cell.c);
                }
            }
            let text = s.trim();
            if text.chars().any(char::is_alphanumeric) {
                return Some(text.chars().filter(|c| !c.is_control()).take(max_chars).collect());
            }
        }
        None
    }

    /// The current mode, readable without locking the terminal (input encoding on the UI
    /// thread must never wait for the reader thread).
    pub fn shared_mode(&self) -> Arc<AtomicU32> {
        self.mode.clone()
    }

    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
        self.touch();
    }

    pub fn scroll_to_bottom(&mut self) {
        if self.term.grid().display_offset() != 0 {
            self.term.scroll_display(Scroll::Bottom);
            self.touch();
        }
    }

    pub fn take_events(&mut self) -> Vec<TermEvent> {
        std::mem::take(&mut *self.shared.events.lock().unwrap_or_else(std::sync::PoisonError::into_inner))
    }

    /// Whether the ConPTY asked for win32-input-mode (seen `CSI ? 9001 h`).
    pub fn win32_input_mode(&self) -> bool {
        self.tap.win32_input_mode
    }
}

/// Reads a mode published by [`Terminal::shared_mode`].
pub fn load_mode(mode: &AtomicU32) -> TermMode {
    TermMode::from_bits_truncate(mode.load(Ordering::Acquire))
}

fn window_size(size: GridSize) -> WindowSize {
    WindowSize { num_lines: size.rows, num_cols: size.cols, cell_width: size.cell_width, cell_height: size.cell_height }
}
