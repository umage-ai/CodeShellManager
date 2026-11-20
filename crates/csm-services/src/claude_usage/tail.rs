//! Reading only the new part of a transcript (ContextCost `importer.js` `readLines`): complete
//! `\n`-terminated lines from a byte offset; a partial last line waits for the next pass.
//! Blocking file I/O: worker threads only.

use std::fs;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// How many leading bytes identify a file: a different head means the file was rewritten.
pub const HEAD_BYTES: usize = 64;
/// A single line longer than this is skipped (counted as bad), never held in memory whole.
pub const MAX_LINE: usize = 32 * 1024 * 1024;

/// What one pass read.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TailPass {
    /// The offset after the last complete line read.
    pub end: u64,
    pub lines: u64,
    /// Lines over [`MAX_LINE`] that were skipped.
    pub oversized: u64,
    /// Stopped at the byte budget with more to read.
    pub more: bool,
}

/// The size, modification time (epoch ms) and head of a file.
pub fn stat(path: &Path) -> io::Result<(u64, i64, Vec<u8>)> {
    let meta = fs::metadata(path)?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
    let mut head = Vec::with_capacity(HEAD_BYTES);
    fs::File::open(path)?.take(HEAD_BYTES as u64).read_to_end(&mut head)?;
    Ok((meta.len(), mtime, head))
}

/// Was the file replaced since `known_head` was taken (`known_offset` bytes read)? A shorter file
/// or a different head means it was truncated or rewritten: start again from 0.
pub fn was_rewritten(size: u64, head: &[u8], known_offset: u64, known_head: &[u8]) -> bool {
    if size < known_offset {
        return true;
    }
    let n = known_head.len().min(head.len());
    known_head.get(..n) != head.get(..n)
}

/// Calls `f` with each complete line from `offset`, reading at most about `budget` bytes.
pub fn read_lines(path: &Path, offset: u64, budget: u64, mut f: impl FnMut(&str)) -> io::Result<TailPass> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut r = BufReader::with_capacity(256 * 1024, file);
    let mut pass = TailPass { end: offset, ..TailPass::default() };
    let mut buf: Vec<u8> = Vec::new();
    loop {
        if pass.end - offset >= budget {
            pass.more = true;
            return Ok(pass);
        }
        buf.clear();
        let mut skipped = 0u64;
        // Read one line, but never keep more than MAX_LINE bytes of it.
        let complete = loop {
            let avail = r.fill_buf()?;
            if avail.is_empty() {
                break false;
            }
            let (take, found) = match avail.iter().position(|b| *b == b'\n') {
                Some(i) => (i + 1, true),
                None => (avail.len(), false),
            };
            if buf.len() < MAX_LINE {
                buf.extend_from_slice(avail.get(..take).unwrap_or_default());
            } else {
                skipped += take as u64;
            }
            r.consume(take);
            if found {
                break true;
            }
        };
        if !complete {
            // A partial last line: not consumed, read again next pass.
            return Ok(pass);
        }
        pass.end += buf.len() as u64 + skipped;
        pass.lines += 1;
        if skipped > 0 || buf.len() > MAX_LINE {
            pass.oversized += 1;
            continue;
        }
        let text = String::from_utf8_lossy(&buf);
        f(text.trim_end_matches(['\n', '\r']));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn complete_lines_only_and_incremental_appends() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        fs::write(&p, "one\r\ntwo\nthr").unwrap();
        let mut got = Vec::new();
        let pass = read_lines(&p, 0, u64::MAX, |l| got.push(l.to_owned())).unwrap();
        assert_eq!(got, ["one", "two"]);
        assert_eq!(pass.end, 9);
        assert_eq!(pass.lines, 2);
        let mut f = fs::OpenOptions::new().append(true).open(&p).unwrap();
        f.write_all(b"ee\nfour\n").unwrap();
        got.clear();
        let pass = read_lines(&p, pass.end, u64::MAX, |l| got.push(l.to_owned())).unwrap();
        assert_eq!(got, ["three", "four"]);
        assert_eq!(pass.end, fs::metadata(&p).unwrap().len());
    }

    #[test]
    fn the_budget_stops_at_a_line_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        fs::write(&p, "aaaa\nbbbb\ncccc\n").unwrap();
        let mut got = Vec::new();
        let pass = read_lines(&p, 0, 6, |l| got.push(l.to_owned())).unwrap();
        assert_eq!(got, ["aaaa", "bbbb"]);
        assert!(pass.more);
        let pass = read_lines(&p, pass.end, 100, |l| got.push(l.to_owned())).unwrap();
        assert_eq!(got.last().map(String::as_str), Some("cccc"));
        assert!(!pass.more);
    }

    #[test]
    fn invalid_utf8_is_lossy_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        fs::write(&p, b"\xff\xfe{\n").unwrap();
        let mut n = 0;
        read_lines(&p, 0, u64::MAX, |_| n += 1).unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn rewrites_are_detected_by_size_or_head() {
        assert!(!was_rewritten(100, b"abc", 50, b"abc"));
        assert!(was_rewritten(10, b"abc", 50, b"abc"), "shorter than what was read");
        assert!(was_rewritten(100, b"xbc", 50, b"abc"), "a different head");
        assert!(!was_rewritten(100, b"abcdef", 3, b"abc"), "a head that grew");
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        fs::write(&p, "hello\n").unwrap();
        let (size, _, head) = stat(&p).unwrap();
        assert_eq!((size, head.as_slice()), (6, b"hello\n".as_slice()));
    }
}
