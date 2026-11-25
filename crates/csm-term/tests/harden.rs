//! Hostile-input tests: oversize OSC strings and combining-mark floods.
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use csm_term::alacritty_terminal::index::{Column, Line, Point};
use csm_term::{GridSize, MAX_TITLE_BYTES, MAX_ZEROWIDTH, ReplySink, TermConfig, Terminal};

#[derive(Default)]
struct Replies(Mutex<Vec<u8>>);

impl ReplySink for Replies {
    fn send(&self, bytes: &[u8]) {
        self.0.lock().unwrap().extend_from_slice(bytes);
    }
}

fn term() -> Terminal {
    Terminal::new(GridSize::new(80, 24), TermConfig::default(), Arc::new(Replies::default()))
}

fn row_text(t: &Terminal, line: i32) -> String {
    (0..80).map(|c| t.term().grid()[Point::new(Line(line), Column(c))].c).collect::<String>().trim_end().to_string()
}

#[test]
fn unterminated_osc_over_many_chunks_is_bounded_and_the_terminal_recovers() {
    let mut t = term();
    t.advance(b"\x1b]0;");
    let chunk = vec![b'T'; 8 * 1024];
    for _ in 0..400 {
        t.advance(&chunk); // ~3 MiB, split over many chunks
    }
    assert!(t.title().len() <= MAX_TITLE_BYTES);
    t.advance(b"\x07hello");
    assert_eq!(row_text(&t, 0), "hello");
    // And a later well-formed title still works.
    t.advance(b"\x1b]2;fine\x07");
    assert_eq!(t.title(), "fine");
}

#[test]
fn unterminated_osc_ended_by_st_or_a_new_escape_recovers() {
    let mut t = term();
    t.advance(b"\x1b]0;");
    t.advance(&vec![b'x'; 200_000]);
    t.advance(b"\x1b\\ok");
    assert_eq!(row_text(&t, 0), "ok");
    t.advance(b"\x1b]8;;");
    t.advance(&vec![b'u'; 200_000]);
    t.advance(b"\x1b[2;1Hnext");
    assert_eq!(row_text(&t, 1), "next");
}

#[test]
fn long_titles_are_cut_on_a_char_boundary() {
    let mut t = term();
    let mut input = b"\x1b]2;".to_vec();
    input.extend("é".repeat(5000).bytes());
    input.push(0x07);
    t.advance(&input);
    let title = t.title();
    assert!(!title.is_empty() && title.len() <= MAX_TITLE_BYTES);
    assert!(title.chars().all(|c| c == 'é'));
}

#[test]
fn combining_mark_flood_is_capped_per_cell() {
    let mut t = term();
    let mut input = String::from("a");
    input.push_str(&"\u{301}".repeat(60_000));
    input.push('b');
    t.advance(input.as_bytes());
    let cell = &t.term().grid()[Point::new(Line(0), Column(0))];
    assert_eq!(cell.c, 'a');
    assert!(cell.zerowidth().unwrap().len() <= MAX_ZEROWIDTH);
    assert_eq!(row_text(&t, 0).chars().nth(1), Some('b'));
    // Same flood split over many small chunks.
    let mut t = term();
    t.advance(b"a");
    for _ in 0..600 {
        t.advance("\u{301}".repeat(100).as_bytes());
    }
    let cell = &t.term().grid()[Point::new(Line(0), Column(0))];
    assert!(cell.zerowidth().unwrap().len() <= MAX_ZEROWIDTH);
}

#[test]
fn snapshot_of_a_flooded_cell_is_capped() {
    let mut t = term();
    t.advance(format!("a{}", "\u{301}".repeat(60_000)).as_bytes());
    let snap = csm_term::replay::snapshot(&mut t, 0);
    assert!(snap.primary.screen[0][0].zerowidth.len() <= MAX_ZEROWIDTH);
}
