//! Grid serializer: turns a [`Terminal`]'s screen, scrollback, cursors and modes into a VT byte
//! stream that rebuilds an equal grid when replayed into a fresh emulator of the same size.
//!
//! This is the attach path for a remote client (design `docs/design/remote-csm.md`, 1.1): the
//! client starts from `serialize(server, history)` and then applies the live byte stream.
//!
//! Two steps: [`snapshot`] reads the emulator into owned data (cells, cursors, modes, title,
//! palette overrides), and [`render`] writes that data as bytes. The snapshot is what the
//! property tests compare: `snapshot(replayed, n) == snapshot(source, n)`.
//!
//! What is reproduced, and what is not, is written up under "P0 findings" in the design doc.
//! In short: cells (characters, zero-width marks, colours, attributes, underline colour, OSC 8
//! links, wide characters, wrapped lines), the primary and alternate screens, both cursors with
//! their pending-wrap state and saved cursors, the replayable DEC modes, the cursor style, the
//! window title and the palette overrides. Not reproduced: the scroll region, tab stops, the
//! active character set and the kitty keyboard stack (none are exposed by `alacritty_terminal`).
//! Wrap markers (`WRAPLINE`) are reproduced when their continuation follows in the stream; a few
//! cases are not (the P0 findings list them, and the random tests pin them).
//!
//! The serializer only reads emulator state. Everything it writes comes from cells the emulator
//! already holds, and every text field that could carry a control byte is filtered before it is
//! written, so a hostile title or link cannot inject sequences into the replay.

use alacritty_terminal::grid::{Cursor, Dimensions, Grid};
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, Rgb};

use crate::Terminal;

/// The highest palette index that OSC can set: the 256 ANSI/extended colours and the three
/// dynamic colours (foreground 256, background 257, cursor 258).
const LAST_PALETTE_INDEX: usize = 258;

/// Longest title written back, in characters.
const MAX_TITLE_CHARS: usize = 1024;

/// Longest hyperlink URI or id written back, in characters.
const MAX_LINK_CHARS: usize = 2048;

/// The SGR attributes a cell or the cursor template carries.
fn style_mask() -> Flags {
    Flags::INVERSE
        | Flags::BOLD
        | Flags::ITALIC
        | Flags::ALL_UNDERLINES
        | Flags::UNDERCURL
        | Flags::DOTTED_UNDERLINE
        | Flags::DASHED_UNDERLINE
        | Flags::DIM
        | Flags::HIDDEN
        | Flags::STRIKEOUT
}

/// Flags that mark the second column of a wide character (and the placeholder before a wide
/// character that did not fit at the end of a row). Their colours follow the wide character's
/// and are not written separately.
fn spacer_mask() -> Flags {
    Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER
}

/// The DEC and ANSI modes that the replay can set. Excludes the alternate screen (handled by the
/// switch itself), the vi-mode flag (UI only) and the kitty keyboard flags (not exposed).
fn replayable_mode() -> TermMode {
    TermMode::SHOW_CURSOR
        | TermMode::APP_CURSOR
        | TermMode::APP_KEYPAD
        | TermMode::MOUSE_REPORT_CLICK
        | TermMode::MOUSE_DRAG
        | TermMode::MOUSE_MOTION
        | TermMode::SGR_MOUSE
        | TermMode::UTF8_MOUSE
        | TermMode::ALTERNATE_SCROLL
        | TermMode::LINE_WRAP
        | TermMode::LINE_FEED_NEW_LINE
        | TermMode::ORIGIN
        | TermMode::INSERT
        | TermMode::FOCUS_IN_OUT
        | TermMode::ALT_SCREEN
        | TermMode::BRACKETED_PASTE
        | TermMode::URGENCY_HINTS
}

/// The attributes a cell was written with, as they matter for replay. Two cells with equal pens
/// print identically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pen {
    pub fg: Color,
    pub bg: Color,
    pub style: Flags,
    pub underline: Option<Color>,
    pub link: Option<(String, String)>,
}

impl Pen {
    fn from_cell(cell: &Cell) -> Self {
        Self {
            fg: cell.fg,
            bg: cell.bg,
            style: cell.flags & style_mask(),
            underline: cell.underline_color(),
            link: cell.hyperlink().map(|h| (clean_id(h.id()), clean(h.uri(), MAX_LINK_CHARS))),
        }
    }

    fn default_pen() -> Self {
        Self::from_cell(&Cell::default())
    }
}

/// Drops control characters, then caps the length. The result is what a replay can carry; the
/// snapshot stores it so both sides compare the same text.
fn clean(text: &str, max_chars: usize) -> String {
    text.chars().filter(|c| !c.is_control()).take(max_chars).collect()
}

/// A hyperlink id: as [`clean`], and without the characters that separate OSC link parameters.
fn clean_id(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() && *c != ';' && *c != ':').take(MAX_LINK_CHARS).collect()
}

/// One cell, as the replay has to reproduce it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CellSnap {
    pub c: char,
    pub zerowidth: Vec<char>,
    pub flags: Flags,
    pub pen: Pen,
}

impl CellSnap {
    fn from_cell(cell: &Cell) -> Self {
        Self { c: cell.c, zerowidth: cell.zerowidth().map(|z| z.iter().copied().take(crate::MAX_ZEROWIDTH).collect()).unwrap_or_default(), flags: cell.flags, pen: Pen::from_cell(cell) }
    }
}

/// A cursor: where it is, whether it has a wrap pending (the last column was just written), and
/// the attributes it writes with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorSnap {
    pub line: usize,
    pub col: usize,
    pub pending_wrap: bool,
    pub pen: Pen,
}

impl CursorSnap {
    fn from_cursor(cursor: &Cursor<Cell>) -> Self {
        Self {
            line: usize::try_from(cursor.point.line.0).unwrap_or(0),
            col: cursor.point.column.0,
            pending_wrap: cursor.input_needs_wrap,
            pen: Pen::from_cell(&cursor.template),
        }
    }
}

/// One screen buffer: its history (the last N lines, oldest first), its visible rows, its
/// cursor and its saved cursor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GridSnap {
    pub cols: usize,
    pub rows: usize,
    pub history: Vec<Vec<CellSnap>>,
    pub screen: Vec<Vec<CellSnap>>,
    pub cursor: CursorSnap,
    pub saved: CursorSnap,
}

/// Everything the replay reproduces about a terminal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    /// The primary screen. Always present: when the alternate screen is active it is the
    /// inactive buffer.
    pub primary: GridSnap,
    /// The alternate screen, present only while it is active (the replay re-enters it).
    pub alt: Option<GridSnap>,
    /// Replayable modes (see [`replayable_mode`]).
    pub mode: TermMode,
    pub cursor_style: CursorStyle,
    /// The window title as the emulator last set it, control characters removed.
    pub title: String,
    /// Palette overrides set by OSC 4/10/11/12, indexed as in `Colors` (0..=258).
    pub colors: Vec<(usize, Rgb)>,
}

fn row_snap(grid: &Grid<Cell>, line: i32, cols: usize) -> Vec<CellSnap> {
    let row = &grid[Line(line)];
    (0..cols).map(|c| CellSnap::from_cell(&row[Column(c)])).collect()
}

fn read_grid(grid: &Grid<Cell>, history: usize) -> GridSnap {
    let rows = grid.screen_lines();
    let cols = grid.columns();
    let take = history.min(grid.history_size());
    // Line(-take) is the oldest kept line; Line(-1) the newest history line.
    let history_rows = (1..=take).rev().map(|k| row_snap(grid, -i32::try_from(k).unwrap_or(0), cols)).collect();
    let screen = (0..rows).map(|l| row_snap(grid, i32::try_from(l).unwrap_or(0), cols)).collect();
    GridSnap {
        cols,
        rows,
        history: history_rows,
        screen,
        cursor: CursorSnap::from_cursor(&grid.cursor),
        saved: CursorSnap::from_cursor(&grid.saved_cursor),
    }
}

/// Reads everything the replay reproduces. `history` is how many scrollback lines to keep (the
/// most recent ones).
///
/// Takes `&mut` because the alternate screen's inactive buffer is only reachable by swapping
/// buffers. The swap is undone before returning, and the selection, which `swap_alt` clears, is
/// put back.
pub fn snapshot(t: &mut Terminal, history: usize) -> Snapshot {
    let title = t.title();
    let term = t.term_mut();
    let selection = term.selection.take();
    let alt_active = term.mode().contains(TermMode::ALT_SCREEN);
    let (primary, alt) = if alt_active {
        // `swap_alt` is not reversible: swapping back runs the "enter the alternate screen"
        // branch, which clears the alternate grid and copies the cursors. While the alternate
        // screen is active the primary's saved cursor always equals its cursor, so that copy is
        // harmless. The alternate grid is put back whole, cells and cursors included.
        let alt_grid = term.grid().clone();
        term.swap_alt();
        let primary = read_grid(term.grid(), history);
        term.swap_alt();
        *term.grid_mut() = alt_grid;
        (primary, Some(read_grid(term.grid(), 0)))
    } else {
        (read_grid(term.grid(), history), None)
    };
    term.selection = selection;
    let mode = *term.mode() & replayable_mode();
    let cursor_style = term.cursor_style();
    let colors = (0..=LAST_PALETTE_INDEX).filter_map(|i| term.colors()[i].map(|rgb| (i, rgb))).collect();
    Snapshot { primary, alt, mode, cursor_style, title: clean(&title, MAX_TITLE_CHARS), colors }
}

/// Serializes the terminal: its last `history` scrollback lines, its screens, cursors, modes,
/// title and palette overrides, as a byte stream for a fresh emulator of the same size.
pub fn serialize(t: &mut Terminal, history: usize) -> Vec<u8> {
    render(&snapshot(t, history))
}

/// Writes a snapshot as VT bytes. Feeding the result to a fresh emulator of the same size gives
/// a snapshot equal to the one it came from.
pub fn render(s: &Snapshot) -> Vec<u8> {
    let mut w = Writer::new(s.primary.cols);

    // Primary content first: scrollback then screen, one line after another. Printing exactly
    // the cells of a row and then CR LF reproduces the row; a row that wrapped is printed with
    // no line break so the emulator wraps it itself and sets WRAPLINE the same way.
    let primary_rows: Vec<&[CellSnap]> = s.primary.history.iter().chain(s.primary.screen.iter()).map(Vec::as_slice).collect();
    w.rows(&primary_rows);

    match &s.alt {
        None => {
            w.place(&s.primary.screen, &s.primary.saved);
            w.raw(b"\x1b7");
            w.settle(s.mode);
            w.place(&s.primary.screen, &s.primary.cursor);
        }
        Some(alt) => {
            // Entering the alternate screen copies the primary cursor (and its template) into
            // the alternate buffer and clears it; the primary saved cursor is set to the same
            // value. So the primary cursor must be in place before the switch.
            w.place(&s.primary.screen, &s.primary.cursor);
            w.raw(b"\x1b[?1049h");
            let alt_rows: Vec<&[CellSnap]> = alt.screen.iter().map(Vec::as_slice).collect();
            w.cup(0, 0);
            w.rows(&alt_rows);
            w.place(&alt.screen, &alt.saved);
            w.raw(b"\x1b7");
            w.settle(s.mode);
            w.place(&alt.screen, &alt.cursor);
        }
    }

    for (index, rgb) in &s.colors {
        let code = match *index {
            0..=255 => format!("4;{index}"),
            256 => "10".to_string(),
            257 => "11".to_string(),
            258 => "12".to_string(),
            _ => continue,
        };
        w.raw(format!("\x1b]{code};rgb:{:02x}/{:02x}/{:02x}\x1b\\", rgb.r, rgb.g, rgb.b).as_bytes());
    }
    if !s.title.is_empty() {
        w.raw(format!("\x1b]2;{}\x1b\\", s.title).as_bytes());
    }
    w.raw(format!("\x1b[{} q", decscusr(s.cursor_style)).as_bytes());
    w.out
}

/// DECSCUSR parameter for a cursor style. A hollow block has no DECSCUSR code and is written as
/// a block (a known gap); a hidden cursor is written as a steady block and its visibility is
/// carried by the cursor-visibility mode.
fn decscusr(style: CursorStyle) -> u8 {
    let blink = u8::from(style.blinking);
    match style.shape {
        CursorShape::Block | CursorShape::HollowBlock | CursorShape::Hidden => 2 - blink,
        CursorShape::Underline => 4 - blink,
        CursorShape::Beam => 6 - blink,
    }
}

struct Writer {
    out: Vec<u8>,
    /// The pen the replayed emulator is writing with (its cursor template), tracked so that
    /// SGR is only written when it changes.
    pen: Pen,
    cols: usize,
}

impl Writer {
    fn new(cols: usize) -> Self {
        Self { out: Vec::new(), pen: Pen::default_pen(), cols }
    }

    fn raw(&mut self, bytes: &[u8]) {
        self.out.extend_from_slice(bytes);
    }

    fn cup(&mut self, line: usize, col: usize) {
        self.raw(format!("\x1b[{};{}H", line + 1, col + 1).as_bytes());
    }

    fn set_pen(&mut self, target: &Pen) {
        if *target == self.pen {
            return;
        }
        self.raw(sgr(target).as_bytes());
        self.raw(link(target).as_bytes());
        self.pen = target.clone();
    }

    fn print(&mut self, cell: &CellSnap) {
        let mut buf = [0u8; 4];
        self.raw(cell.c.encode_utf8(&mut buf).as_bytes());
        for z in &cell.zerowidth {
            self.raw(z.encode_utf8(&mut buf).as_bytes());
        }
    }

    /// Prints one row's cells, starting at the cursor's column (the caller has put the cursor at
    /// the start of the line).
    fn row_cells(&mut self, row: &[CellSnap]) {
        let cols = self.cols;
        let mut col = 0;
        while let Some(cell) = row.get(col) {
            let next = row.get(col + 1);
            let next_is_spacer = next.is_some_and(|n| n.flags.contains(Flags::WIDE_CHAR_SPACER));
            if cell.flags.contains(Flags::WIDE_CHAR) {
                self.set_pen(&cell.pen);
                self.print(cell);
                if next_is_spacer {
                    // The wide character wrote its spacer; skip it. A tab can have been written
                    // over the spacer (its character is a blank), so send the tab there too.
                    if next.is_some_and(|n| n.c == '\t') {
                        self.cup_col(col + 1);
                        self.raw(b"\t");
                        if col + 2 < cols {
                            self.cup_col(col + 2);
                        }
                    }
                    col += 2;
                    continue;
                }
                if next.is_some() && col + 2 <= cols {
                    // Orphan wide character: its right half was erased, so the glyph's flag
                    // survives with an ordinary cell after it. Printing the glyph wrote a spacer
                    // at col + 1; erase it there (erasing does not touch the glyph's flag).
                    self.cup_col(col + 1);
                    self.raw(b"\x1b[1X");
                }
                col += 1;
                continue;
            }
            if cell.flags.intersects(spacer_mask()) {
                // Printed with its wide character (or a leading spacer, which the wrap writes).
                col += 1;
                continue;
            }
            if next_is_spacer {
                // Orphan spacer: its glyph was erased, leaving the spacer in place. Print a
                // wide placeholder here so the spacer is written with the right pen, erase the
                // placeholder, then print this cell (which does not touch the spacer).
                if let Some(spacer) = next {
                    self.set_pen(&spacer.pen);
                }
                self.raw("\u{4e2d}".as_bytes());
                self.cup_col(col);
                self.raw(b"\x1b[1X");
                self.set_pen(&cell.pen);
                if cell.c == '\t' {
                    self.tab_cell(col);
                } else {
                    self.print(cell);
                }
                if col + 2 < cols {
                    self.cup_col(col + 2);
                }
                col += 2;
                continue;
            }
            self.set_pen(&cell.pen);
            if cell.c == '\t' {
                self.tab_cell(col);
            } else {
                self.print(cell);
            }
            col += 1;
        }
    }

    /// Moves the cursor to a column on the current line (absolute, so it never depends on the
    /// line the cursor is on).
    fn cup_col(&mut self, col: usize) {
        self.raw(format!("\x1b[{}G", col + 1).as_bytes());
    }

    /// Prints each row's cells in order. A row ends with CR LF unless its last cell is WRAPLINE,
    /// in which case the next row's first character makes the emulator wrap.
    fn rows(&mut self, rows: &[&[CellSnap]]) {
        let last = rows.len().saturating_sub(1);
        for (i, row) in rows.iter().enumerate() {
            self.row_cells(row);
            if i < last {
                let wrapped = row.last().is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
                if !wrapped {
                    self.raw(b"\r\n");
                }
            }
        }
    }

    /// Writes a tab cell. The emulator stores a tab as a `\t` character in a blank cell it passes
    /// over (`put_tab`), so the cell is printed as a space and then a horizontal tab is sent while
    /// the cursor is on it. The tab moves the cursor on, so the cursor is put back on the next
    /// column with an absolute move. (A tab in the last column of a wrapped row cannot be
    /// reproduced: the wrap needs a pending print there. That is a known gap.)
    fn tab_cell(&mut self, col: usize) {
        let cols = self.cols;
        let mut buf = [0u8; 4];
        self.raw(' '.encode_utf8(&mut buf).as_bytes());
        self.raw(format!("\x1b[{}G", col + 1).as_bytes());
        self.raw(b"\t");
        if col + 1 < cols {
            self.raw(format!("\x1b[{}G", col + 2).as_bytes());
        }
    }

    /// Puts the cursor where `cur` is, with its pen, and with its pending-wrap state.
    ///
    /// A pending wrap means the last column was just written. The emulator reaches that state
    /// only by writing there, so the cell is written again with its own character and pen, which
    /// leaves its contents unchanged. When that cell is the second half of a wide character, the
    /// wide character is written again one column to the left instead.
    fn place(&mut self, screen: &[Vec<CellSnap>], cur: &CursorSnap) {
        let cols = self.cols;
        let row = screen.get(cur.line);
        let pending_row = match (cur.pending_wrap, row) {
            (true, Some(row)) if cols >= 2 => Some(row),
            _ => None,
        };
        match pending_row {
            Some(row) => {
                let last_is_spacer = row.get(cols - 1).is_some_and(|c| c.flags.contains(Flags::WIDE_CHAR_SPACER));
                let (col, cell) = if last_is_spacer { (cols - 2, row.get(cols - 2)) } else { (cols - 1, row.get(cols - 1)) };
                match cell {
                    Some(cell) => {
                        self.set_pen(&cell.pen);
                        self.cup(cur.line, col);
                        self.print(cell);
                    }
                    None => self.cup(cur.line, cur.col),
                }
            }
            None => self.cup(cur.line, cur.col),
        }
        self.set_pen(&cur.pen);
    }

    /// Writes the DEC and ANSI modes, each set or reset explicitly so the result does not depend
    /// on the emulator's defaults.
    fn settle(&mut self, mode: TermMode) {
        const PRIVATE: [(TermMode, u16); 13] = [
            (TermMode::ORIGIN, 6),
            (TermMode::APP_CURSOR, 1),
            (TermMode::SHOW_CURSOR, 25),
            (TermMode::LINE_WRAP, 7),
            (TermMode::BRACKETED_PASTE, 2004),
            (TermMode::FOCUS_IN_OUT, 1004),
            (TermMode::ALTERNATE_SCROLL, 1007),
            (TermMode::URGENCY_HINTS, 1042),
            (TermMode::SGR_MOUSE, 1006),
            (TermMode::UTF8_MOUSE, 1005),
            (TermMode::MOUSE_REPORT_CLICK, 1000),
            (TermMode::MOUSE_DRAG, 1002),
            (TermMode::MOUSE_MOTION, 1003),
        ];
        for (flag, code) in PRIVATE {
            let on = mode.contains(flag);
            self.raw(format!("\x1b[?{code}{}", if on { 'h' } else { 'l' }).as_bytes());
        }
        for (flag, code) in [(TermMode::INSERT, 4u8), (TermMode::LINE_FEED_NEW_LINE, 20u8)] {
            let on = mode.contains(flag);
            self.raw(format!("\x1b[{code}{}", if on { 'h' } else { 'l' }).as_bytes());
        }
        self.raw(if mode.contains(TermMode::APP_KEYPAD) { b"\x1b=" } else { b"\x1b>" });
    }
}

/// The SGR sequence that makes the pen `p`. It starts from a reset, which keeps the template's
/// hyperlink and underline colour (see `Attr::Reset`), so both are written explicitly.
fn sgr(p: &Pen) -> String {
    let mut s = String::from("\x1b[0");
    let f = p.style;
    if f.contains(Flags::BOLD) {
        s.push_str(";1");
    }
    if f.contains(Flags::DIM) {
        s.push_str(";2");
    }
    if f.contains(Flags::ITALIC) {
        s.push_str(";3");
    }
    if let Some(u) = underline_param(f) {
        s.push(';');
        s.push_str(u);
    }
    if f.contains(Flags::INVERSE) {
        s.push_str(";7");
    }
    if f.contains(Flags::HIDDEN) {
        s.push_str(";8");
    }
    if f.contains(Flags::STRIKEOUT) {
        s.push_str(";9");
    }
    if let Some(fg) = color_param(p.fg, false) {
        s.push(';');
        s.push_str(&fg);
    }
    if let Some(bg) = color_param(p.bg, true) {
        s.push(';');
        s.push_str(&bg);
    }
    match p.underline.and_then(color_body) {
        Some(body) => {
            s.push_str(";58;");
            s.push_str(&body);
        }
        None => s.push_str(";59"),
    }
    s.push('m');
    s
}

fn underline_param(f: Flags) -> Option<&'static str> {
    if f.contains(Flags::DOUBLE_UNDERLINE) {
        Some("4:2")
    } else if f.contains(Flags::UNDERCURL) {
        Some("4:3")
    } else if f.contains(Flags::DOTTED_UNDERLINE) {
        Some("4:4")
    } else if f.contains(Flags::DASHED_UNDERLINE) {
        Some("4:5")
    } else if f.contains(Flags::UNDERLINE) {
        Some("4")
    } else {
        None
    }
}

/// `38;…` or `48;…` for a colour. `None` for the default colours, which the reset already gives.
fn color_param(c: Color, background: bool) -> Option<String> {
    let (base, ext) = if background { (40u16, 48u16) } else { (30u16, 38u16) };
    match c {
        Color::Named(n) => {
            let i = n as u16;
            match i {
                0..=7 => Some((base + i).to_string()),
                8..=15 => Some((base + 60 + (i - 8)).to_string()),
                _ => None,
            }
        }
        Color::Indexed(i) => Some(format!("{ext};5;{i}")),
        Color::Spec(rgb) => Some(format!("{ext};2;{};{};{}", rgb.r, rgb.g, rgb.b)),
    }
}

/// The colour part of an SGR 58 (underline colour) sequence.
fn color_body(c: Color) -> Option<String> {
    match c {
        Color::Named(n) => {
            let i = n as usize;
            u8::try_from(i).ok().map(|i| format!("5;{i}"))
        }
        Color::Indexed(i) => Some(format!("5;{i}")),
        Color::Spec(rgb) => Some(format!("2;{};{};{}", rgb.r, rgb.g, rgb.b)),
    }
}

/// OSC 8 for a pen: opens its link, or closes any open one.
fn link(p: &Pen) -> String {
    match &p.link {
        Some((id, uri)) if id.is_empty() => format!("\x1b]8;;{uri}\x1b\\"),
        Some((id, uri)) => format!("\x1b]8;id={id};{uri}\x1b\\"),
        None => "\x1b]8;;\x1b\\".to_string(),
    }
}
