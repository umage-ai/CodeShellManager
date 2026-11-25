//! Copying a selection without the text the program concealed (SGR 8): a hidden cell is data the
//! person cannot see, so a copy must not carry it to the clipboard.

use csm_term::Terminal;
use csm_term::alacritty_terminal::grid::Dimensions;
use csm_term::alacritty_terminal::index::{Column, Point};
use csm_term::alacritty_terminal::term::cell::Flags;

/// The selected text, with every concealed cell read as a blank. `None` when nothing is selected.
///
/// The emulator's own `selection_to_string` has no notion of concealment, so the concealed cells
/// are blanked for the duration of the call (the terminal lock is held by the caller) and put
/// back exactly as they were.
pub fn visible_text(t: &mut Terminal) -> Option<String> {
    let range = t.term().selection.as_ref().and_then(|s| s.to_range(t.term()))?;
    let cols = t.term().columns();
    let (top, bottom) = (t.term().topmost_line(), t.term().bottommost_line());
    let first = range.start.line.max(top);
    let last = range.end.line.min(bottom);
    let mut saved: Vec<(Point, char, Flags)> = Vec::new();
    let mut line = first;
    while line <= last {
        for c in 0..cols {
            let p = Point::new(line, Column(c));
            let cell = &mut t.term_mut().grid_mut()[p];
            if cell.flags.contains(Flags::HIDDEN) {
                saved.push((p, cell.c, cell.flags));
                cell.c = ' ';
                cell.flags.remove(Flags::HIDDEN);
            }
        }
        line += 1;
    }
    let text = t.term().selection_to_string();
    for (p, c, flags) in saved {
        let cell = &mut t.term_mut().grid_mut()[p];
        cell.c = c;
        cell.flags = flags;
    }
    text
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use csm_term::alacritty_terminal::index::{Line, Side};
    use csm_term::alacritty_terminal::selection::{Selection, SelectionType};
    use csm_term::{GridSize, ReplySink, TermConfig};
    use std::sync::Arc;

    struct Discard;
    impl ReplySink for Discard {
        fn send(&self, _: &[u8]) {}
    }

    fn select_all(t: &mut Terminal, cols: usize) {
        let mut sel = Selection::new(SelectionType::Simple, Point::new(Line(0), Column(0)), Side::Left);
        sel.update(Point::new(Line(0), Column(cols - 1)), Side::Right);
        t.term_mut().selection = Some(sel);
    }

    #[test]
    fn concealed_text_is_not_copied_and_the_screen_is_unchanged() {
        let mut t = Terminal::new(GridSize { cols: 30, rows: 3, cell_width: 8, cell_height: 16 }, TermConfig::default(), Arc::new(Discard));
        t.advance(b"visible \x1b[8mHUNTER2\x1b[0m end");
        select_all(&mut t, 30);
        let text = visible_text(&mut t).unwrap();
        assert!(!text.contains("HUNTER2"), "copied: {text:?}");
        assert!(text.contains("visible") && text.contains("end"), "copied: {text:?}");
        // Concealment is restored: a second look at the raw grid still sees it hidden.
        let hidden = (0..30).filter(|&c| t.term().grid()[Point::new(Line(0), Column(c))].flags.contains(Flags::HIDDEN)).count();
        assert_eq!(hidden, 7);
        assert!(t.term().selection_to_string().unwrap().contains("HUNTER2"), "the cells themselves still hold their text");
    }

    #[test]
    fn text_without_concealment_copies_as_before() {
        let mut t = Terminal::new(GridSize { cols: 30, rows: 3, cell_width: 8, cell_height: 16 }, TermConfig::default(), Arc::new(Discard));
        t.advance(b"plain text");
        select_all(&mut t, 30);
        assert_eq!(visible_text(&mut t), t.term().selection_to_string());
    }
}
