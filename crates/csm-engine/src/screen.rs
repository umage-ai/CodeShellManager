//! Terminal contents as plain text (for `session_read_screen` / `session_read_output`). Runs on a
//! worker thread: it locks the terminal, which the PTY reader also holds while parsing.

use std::time::Instant;

use csm_term::Terminal;
use csm_term::alacritty_terminal::grid::Dimensions;
use csm_term::alacritty_terminal::index::{Column, Line};
use csm_term::alacritty_terminal::term::cell::Flags;

/// Hard cap on lines returned by `session_read_output`.
pub const MAX_OUTPUT_LINES: usize = 2000;

fn line_text(t: &Terminal, line: i32) -> (String, bool) {
    let grid = t.term().grid();
    let cols = grid.columns();
    let row = &grid[Line(line)];
    let mut s = String::with_capacity(cols);
    let mut wrapped = false;
    for c in 0..cols {
        let cell = &row[Column(c)];
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        s.push(cell.c);
        if c + 1 == cols {
            wrapped = cell.flags.contains(Flags::WRAPLINE);
        }
    }
    if !wrapped {
        let trimmed = s.trim_end().len();
        s.truncate(trimmed);
    }
    (s, wrapped)
}

/// The visible screen (what a person sees with the view scrolled to the bottom).
pub fn screen(t: &mut Terminal) -> String {
    t.flush_sync_if_due(Instant::now());
    let rows = i32::try_from(t.term().grid().screen_lines()).unwrap_or(0);
    let mut lines: Vec<String> = (0..rows).map(|l| line_text(t, l).0).collect();
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines.join("\n")
}

/// The last `max` logical lines of scrollback + screen, wrapped rows joined.
pub fn tail(t: &mut Terminal, max: usize) -> String {
    t.flush_sync_if_due(Instant::now());
    let grid = t.term().grid();
    let history = i32::try_from(grid.history_size()).unwrap_or(0);
    let rows = i32::try_from(grid.screen_lines()).unwrap_or(0);
    let mut logical: Vec<String> = Vec::new();
    let mut current = String::new();
    for l in -history..rows {
        let (text, wrapped) = line_text(t, l);
        current.push_str(&text);
        if !wrapped {
            logical.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        logical.push(current);
    }
    while logical.last().is_some_and(String::is_empty) {
        logical.pop();
    }
    let max = max.min(MAX_OUTPUT_LINES);
    let start = logical.len().saturating_sub(max);
    logical.get(start..).unwrap_or_default().join("\n")
}
