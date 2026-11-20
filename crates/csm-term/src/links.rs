//! Links in the terminal: OSC 8 hyperlinks (`ESC ] 8 ; params ; uri ST`, kept by
//! alacritty_terminal on each cell) and `http(s)://` URLs found in the text.
//!
//! Only what is under the pointer is looked at ([`crate::Terminal::link_at`]): one logical line
//! (a row and the rows it wraps into), never the whole screen. Nothing here opens anything; the
//! engine's `open_url` checks the URL again and refuses anything but absolute http/https.

use std::ops::Range;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags;

/// A row may wrap into at most this many rows in either direction before the search stops
/// (a 2000-character URL on an 80-column terminal is 25 rows).
const MAX_WRAP_ROWS: i32 = 32;

/// Characters a URL found in plain text never contains (they end it): ASCII delimiters that are
/// not valid unescaped in a URL, and the box-drawing characters TUIs draw borders with.
fn ends_url(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(c, '"' | '\'' | '`' | '<' | '>' | '{' | '}' | '|' | '\\' | '^')
        || ('\u{2500}'..='\u{259F}').contains(&c)
}

/// The `http://` / `https://` URLs in `text`, as char index ranges. Trailing sentence
/// punctuation and unbalanced closing brackets are not part of the URL
/// (`(see https://a.b/c).` → `https://a.b/c`; `https://en.wikipedia.org/wiki/X_(y)` keeps its
/// own parentheses).
pub fn find_urls(text: &[char]) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        let Some(scheme) = scheme_at(text, i) else {
            i += 1;
            continue;
        };
        // `xhttp://` is not a link; `(http://` and `=http://` are.
        if i > 0 && text.get(i - 1).is_some_and(|c| c.is_alphanumeric()) {
            i += 1;
            continue;
        }
        let mut end = i + scheme;
        while end < text.len() && text.get(end).is_some_and(|&c| !ends_url(c)) {
            end += 1;
        }
        let end = trim_end(text, i + scheme, end);
        // A host must follow the scheme.
        if end > i + scheme && text.get(i + scheme).is_some_and(|c| c.is_alphanumeric() || *c == '[') {
            out.push(i..end);
            i = end;
        } else {
            i += scheme;
        }
    }
    out
}

/// Length of the `http://` or `https://` at `i` (any case), if there is one.
fn scheme_at(text: &[char], i: usize) -> Option<usize> {
    let starts = |p: &str| {
        let n = p.chars().count();
        text.get(i..i + n).is_some_and(|s| s.iter().zip(p.chars()).all(|(a, b)| a.to_ascii_lowercase() == b))
    };
    if starts("https://") {
        Some(8)
    } else if starts("http://") {
        Some(7)
    } else {
        None
    }
}

fn trim_end(text: &[char], min: usize, mut end: usize) -> usize {
    // How often `c` occurs in the URL so far (after the scheme).
    let count = |c: char, end: usize| text.get(min..end).map_or(0, |s| s.iter().filter(|&&x| x == c).count());
    while end > min {
        let Some(&last) = text.get(end - 1) else { break };
        let drop = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '*' => true,
            ')' => count('(', end) < count(')', end),
            ']' => count('[', end) < count(']', end),
            _ => false,
        };
        if !drop {
            break;
        }
        end -= 1;
    }
    end
}

/// A link under the pointer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    /// From OSC 8 (the program named it), not found in the text.
    pub explicit: bool,
    /// The cells it covers on screen: `(visible row, first column, last column)`, one per row.
    /// Rows scrolled out of view are left out.
    pub spans: Vec<(usize, usize, usize)>,
}

impl crate::Terminal {
    /// The link at a visible cell (`row` 0 is the top of the view, scrollback included): an OSC 8
    /// hyperlink, else an http(s) URL in the text of the logical line through that cell.
    pub fn link_at(&self, col: usize, row: usize) -> Option<Link> {
        let grid = self.term().grid();
        let cols = grid.columns();
        if col >= cols || row >= grid.screen_lines() {
            return None;
        }
        let offset = i32::try_from(grid.display_offset()).ok()?;
        let line = Line(i32::try_from(row).ok()? - offset);
        let (top, bottom) = (grid.topmost_line(), grid.bottommost_line());
        if line < top || line > bottom {
            return None;
        }
        let cell_at = |l: Line, c: usize| {
            let r = &grid[l];
            (c < r.len()).then(|| &r[Column(c)])
        };
        let wraps = |l: Line| cell_at(l, cols - 1).is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
        let mut start = line;
        while start > top && line.0 - start.0 < MAX_WRAP_ROWS && wraps(start - 1) {
            start = start - 1;
        }
        let mut end = line;
        while end < bottom && end.0 - line.0 < MAX_WRAP_ROWS && wraps(end) {
            end = end + 1;
        }
        // The logical line's text, and where each char sits.
        let mut text = Vec::new();
        let mut at: Vec<(Line, usize)> = Vec::new();
        let mut l = start;
        while l <= end {
            for c in 0..cols {
                let Some(cell) = cell_at(l, c) else { break };
                if cell.flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER) {
                    continue;
                }
                text.push(cell.c);
                at.push((l, c));
            }
            l = l + 1;
        }
        // The char under the pointer (a wide char's spacer belongs to the char before it).
        let here = at.iter().rposition(|&(al, ac)| al == line && ac <= col)?;
        let to_spans = |range: Range<usize>| -> Vec<(usize, usize, usize)> {
            let mut spans: Vec<(usize, usize, usize)> = Vec::new();
            for &(l, c) in at.get(range).unwrap_or_default() {
                let Some(vis) = usize::try_from(l.0 + offset).ok().filter(|v| *v < grid.screen_lines()) else { continue };
                // A wide char covers its spacer too.
                let last = if cell_at(l, c).is_some_and(|x| x.flags.contains(Flags::WIDE_CHAR)) { c + 1 } else { c };
                match spans.last_mut() {
                    Some(s) if s.0 == vis => s.2 = last,
                    _ => spans.push((vis, c, last)),
                }
            }
            spans
        };

        // OSC 8: every cell of the logical line around the pointer carrying the same link.
        let (hl, hc) = *at.get(here)?;
        if let Some(h) = cell_at(hl, hc).and_then(|c| c.hyperlink()) {
            let same = |i: usize| at.get(i).and_then(|&(l, c)| cell_at(l, c)).and_then(|c| c.hyperlink()).as_ref() == Some(&h);
            let mut a = here;
            while a > 0 && same(a - 1) {
                a -= 1;
            }
            let mut b = here + 1;
            while b < at.len() && same(b) {
                b += 1;
            }
            let uri: String = h.uri().chars().filter(|c| !c.is_control()).collect();
            return (!uri.is_empty()).then(|| Link { uri, explicit: true, spans: to_spans(a..b) });
        }

        let range = find_urls(&text).into_iter().find(|r| r.contains(&here))?;
        let uri: String = text.get(range.clone())?.iter().collect();
        Some(Link { uri, explicit: false, spans: to_spans(range) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(s: &str) -> Vec<String> {
        let chars: Vec<char> = s.chars().collect();
        find_urls(&chars).into_iter().map(|r| chars[r].iter().collect()).collect()
    }

    #[test]
    fn plain_urls_are_found() {
        assert_eq!(urls("see https://example.com/a/b?c=1&d=2#frag now"), ["https://example.com/a/b?c=1&d=2#frag"]);
        assert_eq!(urls("http://localhost:5173/"), ["http://localhost:5173/"]);
        assert_eq!(urls("HTTPS://EXAMPLE.COM"), ["HTTPS://EXAMPLE.COM"]);
        assert_eq!(urls("two: https://a.io and http://b.io/x"), ["https://a.io", "http://b.io/x"]);
        assert_eq!(urls("http://[::1]:8080/x"), ["http://[::1]:8080/x"]);
    }

    #[test]
    fn trailing_punctuation_is_not_part_of_the_url() {
        assert_eq!(urls("Go to https://example.com."), ["https://example.com"]);
        assert_eq!(urls("https://example.com/x, then"), ["https://example.com/x"]);
        assert_eq!(urls("really? https://example.com/q?!"), ["https://example.com/q"]);
        assert_eq!(urls("**https://example.com/bold**"), ["https://example.com/bold"]);
        assert_eq!(urls("https://example.com/a.html."), ["https://example.com/a.html"]);
    }

    #[test]
    fn parentheses_and_brackets_balance() {
        assert_eq!(urls("(see https://example.com/x)"), ["https://example.com/x"]);
        assert_eq!(urls("(see https://example.com/x)."), ["https://example.com/x"]);
        assert_eq!(urls("https://en.wikipedia.org/wiki/Rust_(programming_language)"), ["https://en.wikipedia.org/wiki/Rust_(programming_language)"]);
        assert_eq!(urls("[docs](https://docs.rs/egui)"), ["https://docs.rs/egui"]);
        assert_eq!(urls("[https://a.io/x]"), ["https://a.io/x"]);
    }

    #[test]
    fn delimiters_end_the_url() {
        assert_eq!(urls("\"https://example.com/q\""), ["https://example.com/q"]);
        assert_eq!(urls("<https://example.com>"), ["https://example.com"]);
        assert_eq!(urls("'https://example.com'"), ["https://example.com"]);
        assert_eq!(urls("│ https://example.com/x │"), ["https://example.com/x"]);
        assert_eq!(urls("`https://example.com`"), ["https://example.com"]);
    }

    struct NoReplies;
    impl crate::ReplySink for NoReplies {
        fn send(&self, _: &[u8]) {}
    }

    fn term(cols: u16, rows: u16, bytes: &[u8]) -> crate::Terminal {
        let mut t = crate::Terminal::new(crate::GridSize::new(cols, rows), crate::TermConfig::default(), std::sync::Arc::new(NoReplies));
        t.advance(bytes);
        t
    }

    #[test]
    fn a_url_on_screen_is_found_under_the_pointer_only() {
        let t = term(40, 4, b"run: https://example.com/x.\r\nnext line");
        let l = t.link_at(10, 0).unwrap();
        assert_eq!((l.uri.as_str(), l.explicit), ("https://example.com/x", false));
        assert_eq!(l.spans, [(0, 5, 25)]);
        assert!(t.link_at(2, 0).is_none(), "the word before it");
        assert!(t.link_at(26, 0).is_none(), "the full stop after it");
        assert!(t.link_at(3, 1).is_none());
        assert!(t.link_at(99, 0).is_none() && t.link_at(0, 99).is_none(), "outside the grid");
    }

    #[test]
    fn a_url_wrapped_over_rows_is_one_link() {
        // 20 columns: the URL wraps onto the next row (the terminal wrapped it, WRAPLINE).
        let t = term(20, 4, b"> https://example.com/a/very/long/path ok");
        let want = "https://example.com/a/very/long/path";
        let top = t.link_at(5, 0).unwrap();
        let bottom = t.link_at(3, 1).unwrap();
        assert_eq!(top.uri, want);
        assert_eq!(bottom, top);
        assert_eq!(top.spans, [(0, 2, 19), (1, 0, 17)]);
        // A hard line break is not a wrap: the next line's text is not glued on.
        let t = term(40, 4, b"https://example.com/a\r\nb/c");
        assert_eq!(t.link_at(0, 0).unwrap().uri, "https://example.com/a");
    }

    #[test]
    fn osc_8_hyperlinks_win_over_the_text() {
        let t = term(40, 4, b"see \x1b]8;;https://docs.rs/egui\x1b\\the docs\x1b]8;;\x1b\\ here");
        let l = t.link_at(6, 0).unwrap();
        assert_eq!((l.uri.as_str(), l.explicit), ("https://docs.rs/egui", true));
        assert_eq!(l.spans, [(0, 4, 11)]);
        assert!(t.link_at(13, 0).is_none());
        // A hyperlink with any scheme is reported as is; the engine decides what it opens.
        let t = term(40, 4, b"\x1b]8;;file:///etc/passwd\x1b\\x\x1b]8;;\x1b\\");
        assert_eq!(t.link_at(0, 0).unwrap().uri, "file:///etc/passwd");
    }

    #[test]
    fn wide_chars_keep_columns_right() {
        let t = term(40, 4, "日本 https://例え.jp/x".as_bytes());
        let l = t.link_at(8, 0).unwrap();
        assert_eq!(l.uri, "https://例え.jp/x");
        assert_eq!(l.spans, [(0, 5, 21)]);
    }

    #[test]
    fn not_links() {
        assert!(urls("xhttp://example.com").is_empty());
        assert!(urls("https:// nothing").is_empty());
        assert!(urls("http://").is_empty());
        assert!(urls("ftp://example.com file:///c:/x www.example.com").is_empty());
        assert!(urls("").is_empty());
        assert!(urls("https://.").is_empty());
    }
}
