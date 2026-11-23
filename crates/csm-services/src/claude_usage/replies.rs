//! Reading Claude's last replies from a transcript (`csm_core::claude_reply`). Only the end of
//! the file is read, a larger part each time until enough whole replies are found (or the
//! whole file is). Blocking file I/O: worker threads only.

use std::io;
use std::path::Path;

use csm_core::claude_reply::{ClaudeReply, ReplyCollector};

use super::tail;

/// The ends of the file tried, in bytes; then the whole file.
const WINDOWS: [u64; 3] = [2 << 20, 16 << 20, 128 << 20];

/// The newest `want` replies (fewer when the transcript has fewer), newest first.
pub fn read_newest(path: &Path, want: usize) -> io::Result<Vec<ClaudeReply>> {
    let want = want.max(1);
    let size = std::fs::metadata(path)?.len();
    for w in WINDOWS {
        if w >= size {
            break;
        }
        let from = size - w;
        let mut c = ReplyCollector::mid_file(want);
        // The first line read is most likely cut: it is not JSON and is skipped.
        tail::read_lines(path, from, u64::MAX, |l| c.push_line(l))?;
        let got = c.into_newest_first();
        if got.len() >= want {
            return Ok(got);
        }
    }
    let mut c = ReplyCollector::from_start(want);
    tail::read_lines(path, 0, u64::MAX, |l| c.push_line(l))?;
    Ok(c.into_newest_first())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn(i: usize, pad: usize) -> String {
        let prompt = serde_json::json!({"type": "user", "message": {"role": "user", "content": format!("q{i}")}});
        let answer = serde_json::json!({"type": "assistant", "message": {"id": format!("msg_{i}"), "content": [{"type": "text", "text": format!("answer {i} {}", "x".repeat(pad))}]}});
        format!("{prompt}\n{answer}\n")
    }

    #[test]
    fn reads_the_newest_replies_from_the_end_and_the_whole_file_when_needed() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        // About 3 MiB: the first window (2 MiB) holds only part of it.
        let text: String = (0..30).map(|i| turn(i, 100_000)).collect();
        std::fs::write(&p, text).unwrap();
        let r = read_newest(&p, 3).unwrap();
        assert_eq!(r.len(), 3);
        assert!(r[0].text.starts_with("answer 29 "));
        assert!(r[2].text.starts_with("answer 27 "));
        // More than the window holds: the whole file is read.
        let r = read_newest(&p, 50).unwrap();
        assert_eq!(r.len(), 30);
        assert!(r[29].text.starts_with("answer 0 "));
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read_newest(&dir.path().join("never.jsonl"), 1).is_err());
    }
}
