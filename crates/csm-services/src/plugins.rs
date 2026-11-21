//! Pipe plumbing for process add-ons (docs/addons.md, "Process add-ons"): reading a process's stdout
//! one capped line at a time, writing a line, and keeping the tail of its stderr. Starting the
//! process is `csm_engine::proc::spawn_piped`: the policy allows process spawns only in the modules
//! that own them.
//!
//! Nothing here interprets a message; `csm_core::addons::protocol` does that. Every read is bounded.

use std::collections::VecDeque;
use std::io::{self, BufRead, Read, Write};
use std::sync::{Mutex, PoisonError};

/// One read from a capped line reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineRead {
    /// A line, without its newline (and without a trailing `\r`). Bytes that are not UTF-8 are
    /// replaced, so the protocol check sees them as a bad message.
    Line(String),
    /// The line is longer than the cap. The rest of it is not read: the caller must stop the
    /// process, since the stream can no longer be trusted to be in step.
    TooLong,
    /// The stream ended.
    Eof,
}

/// Reads one `\n`-terminated line of at most `max` bytes. Never allocates more than `max + 1`
/// bytes for a line, whatever the process writes.
pub fn read_line_capped<R: BufRead>(r: &mut R, max: usize) -> io::Result<LineRead> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let (done, used) = {
            let avail = r.fill_buf()?;
            if avail.is_empty() {
                return Ok(if buf.is_empty() { LineRead::Eof } else { finish(buf) });
            }
            match avail.iter().position(|b| *b == b'\n') {
                Some(i) => {
                    if buf.len() + i > max {
                        return Ok(LineRead::TooLong);
                    }
                    buf.extend_from_slice(avail.get(..i).unwrap_or(&[]));
                    (true, i + 1)
                }
                None => {
                    if buf.len() + avail.len() > max {
                        return Ok(LineRead::TooLong);
                    }
                    buf.extend_from_slice(avail);
                    (false, avail.len())
                }
            }
        };
        r.consume(used);
        if done {
            return Ok(finish(buf));
        }
    }
}

fn finish(mut buf: Vec<u8>) -> LineRead {
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    LineRead::Line(String::from_utf8_lossy(&buf).into_owned())
}

/// Writes one message and its newline, then flushes. The line must not contain a newline (a
/// message built by `csm_core::addons::protocol` never does; a stray one is refused).
pub fn write_line<W: Write>(w: &mut W, line: &str) -> io::Result<()> {
    if line.contains('\n') {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "a message may not contain a newline"));
    }
    w.write_all(line.as_bytes())?;
    w.write_all(b"\n")?;
    w.flush()
}

/// The last bytes a process wrote to stderr, capped. Written by the stderr reader thread, read
/// when the status is shown. Lock poisoning is ignored: the bytes are only a log.
#[derive(Debug)]
pub struct StderrTail {
    cap: usize,
    bytes: Mutex<VecDeque<u8>>,
}

impl StderrTail {
    pub fn new(cap: usize) -> StderrTail {
        StderrTail { cap, bytes: Mutex::new(VecDeque::new()) }
    }

    pub fn push(&self, chunk: &[u8]) {
        let mut b = self.bytes.lock().unwrap_or_else(PoisonError::into_inner);
        b.extend(chunk.iter().copied());
        while b.len() > self.cap {
            b.pop_front();
        }
    }

    /// The tail as text (lossy), at most `cap` bytes, with control characters other than
    /// newline and tab removed.
    pub fn text(&self) -> String {
        let b = self.bytes.lock().unwrap_or_else(PoisonError::into_inner);
        let raw: Vec<u8> = b.iter().copied().collect();
        String::from_utf8_lossy(&raw).chars().filter(|c| *c == '\n' || *c == '\t' || !c.is_control()).collect()
    }
}

/// Copies a process's stderr into `tail` until it ends. Runs on its own thread.
pub fn pump_stderr<R: Read>(mut r: R, tail: &StderrTail) {
    let mut chunk = [0u8; 4096];
    loop {
        match r.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => tail.push(chunk.get(..n).unwrap_or(&[])),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn reads_lines_and_strips_carriage_returns() {
        let mut r = Cursor::new(b"one\r\ntwo\nthree".to_vec());
        assert_eq!(read_line_capped(&mut r, 100).unwrap(), LineRead::Line("one".into()));
        assert_eq!(read_line_capped(&mut r, 100).unwrap(), LineRead::Line("two".into()));
        assert_eq!(read_line_capped(&mut r, 100).unwrap(), LineRead::Line("three".into()), "a last line without a newline");
        assert_eq!(read_line_capped(&mut r, 100).unwrap(), LineRead::Eof);
    }

    #[test]
    fn a_line_over_the_cap_is_too_long_even_without_a_newline() {
        let mut r = Cursor::new(vec![b'a'; 50]);
        assert_eq!(read_line_capped(&mut r, 10).unwrap(), LineRead::TooLong);
        let mut r = Cursor::new(format!("{}\nok\n", "b".repeat(20)).into_bytes());
        assert_eq!(read_line_capped(&mut r, 10).unwrap(), LineRead::TooLong);
    }

    #[test]
    fn a_line_exactly_at_the_cap_is_fine() {
        let mut r = Cursor::new(format!("{}\n", "c".repeat(10)).into_bytes());
        assert_eq!(read_line_capped(&mut r, 10).unwrap(), LineRead::Line("c".repeat(10)));
    }

    #[test]
    fn invalid_utf8_is_replaced_not_fatal() {
        let mut r = Cursor::new(vec![0xff, b'x', b'\n']);
        match read_line_capped(&mut r, 10).unwrap() {
            LineRead::Line(s) => assert!(s.contains('x')),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn write_line_refuses_embedded_newlines() {
        let mut out = Vec::new();
        write_line(&mut out, "{\"a\":1}").unwrap();
        assert_eq!(out, b"{\"a\":1}\n");
        assert!(write_line(&mut Vec::new(), "a\nb").is_err());
    }

    #[test]
    fn the_stderr_tail_keeps_only_the_end() {
        let t = StderrTail::new(8);
        t.push(b"0123456789");
        assert_eq!(t.text(), "23456789");
        t.push(b"\x07ab\n");
        assert!(t.text().ends_with("ab\n"), "{:?}", t.text());
        assert!(!t.text().contains('\u{7}'));
    }
}
