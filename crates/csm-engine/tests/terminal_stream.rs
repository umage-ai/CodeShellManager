//! What the peer server reads from a terminal handle: the consumed offset, the attach (grid and
//! offset from one moment), the resize marks and the resume check.

mod common;

use common::H;
use csm_term::{GridSize, TermConfig, Terminal};
use std::sync::Arc;

struct Discard;
impl csm_term::ReplySink for Discard {
    fn send(&self, _bytes: &[u8]) {}
}

#[test]
fn the_attach_and_its_offset_describe_the_same_moment() {
    let h = H::new();
    let id = h.session("fake");
    let pty = h.pty_of(&id);
    let handle = h.engine.terminal(&id).unwrap();
    pty.emit(b"hello\r\nworld\r\n");
    let end = handle.output_end();
    assert_eq!(end, 14);
    let a = loop {
        let a = handle.attach_snapshot(100);
        if a.offset == end {
            break a;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    };
    assert_eq!(a.instance, handle.instance());
    let mut t = Terminal::new(GridSize::new(a.cols, a.rows), TermConfig::default(), Arc::new(Discard));
    t.advance(&a.vt);
    let text: String = csm_term::replay::snapshot(&mut t, 10).primary.screen.iter().flat_map(|r| r.iter().map(|c| c.c)).collect();
    assert!(text.contains("hello") && text.contains("world"));
    // Bytes the emulator has not consumed are never claimed by the offset.
    assert!(handle.processed_offset() <= handle.output_end());
}

#[test]
fn resizes_are_logged_at_the_offset_they_take_effect_and_gate_resuming() {
    let h = H::new();
    let id = h.session("fake");
    let pty = h.pty_of(&id);
    let handle = h.engine.terminal(&id).unwrap();
    let start = handle.resizes_after(0).len();
    pty.emit(b"abc");
    while handle.processed_offset() < 3 {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let cur = handle.grid_size();
    handle.resize_cells(cur.cols + 10, cur.rows + 5);
    handle.resize_cells(cur.cols + 10, cur.rows + 5); // the same size again: no new mark
    let marks = handle.resizes_after(0);
    assert_eq!(marks.len(), start + 1);
    let m = marks.last().unwrap();
    assert_eq!((m.offset, m.cols, m.rows), (3, cur.cols + 10, cur.rows + 5));
    assert!(pty.size.lock().unwrap().cols == cur.cols + 10, "the PTY followed");
    // A stream that has everything before byte 3 still needs this mark; one past it does not.
    assert_eq!(handle.resizes_for_resume(3), m.seq - 1);
    assert_eq!(handle.resizes_for_resume(4), m.seq);
    // Resuming: same run, inside the ring; another run or an offset past the end is not.
    assert!(handle.can_resume(handle.instance(), 3));
    assert!(!handle.can_resume(handle.instance() + 1, 3));
    assert!(!handle.can_resume(handle.instance(), 1_000_000));
    // The attach now reports the new size and says the mark is already inside it.
    let a = handle.attach_snapshot(10);
    assert_eq!((a.cols, a.rows, a.resize_seq), (cur.cols + 10, cur.rows + 5, m.seq));
}
