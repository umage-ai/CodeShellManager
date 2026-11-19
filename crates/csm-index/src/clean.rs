//! Turning raw PTY bytes into searchable lines, and folder keys.

use csm_core::alert_detector::strip_ansi;

/// Longest partial line held back waiting for its newline before it is emitted anyway.
const MAX_CARRY: usize = 16 * 1024;
/// Longest line stored; the rest is cut (a minified bundle or a base64 blob is not searchable text).
const MAX_LINE_CHARS: usize = 4000;

/// Per-session state: bytes of an unfinished line, carried to the next chunk so an escape
/// sequence or a multi-byte character split across two reads is stripped, not indexed.
#[derive(Debug, Default)]
pub struct LineCleaner {
    carry: Vec<u8>,
}

impl LineCleaner {
    /// Feed raw bytes; returns the finished, cleaned, non-blank lines.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.carry.extend_from_slice(bytes);
        let Some(last_nl) = self.carry.iter().rposition(|b| *b == b'\n') else {
            if self.carry.len() > MAX_CARRY {
                return self.finish();
            }
            return Vec::new();
        };
        let rest = self.carry.split_off(last_nl + 1);
        let done = std::mem::replace(&mut self.carry, rest);
        clean_lines(&done)
    }

    /// Emit the unfinished line now (the session went quiet with a prompt on screen).
    pub fn finish(&mut self) -> Vec<String> {
        let done = std::mem::take(&mut self.carry);
        clean_lines(&done)
    }

    pub fn is_empty(&self) -> bool {
        self.carry.is_empty()
    }
}

fn clean_lines(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let stripped = strip_ansi(&text);
    stripped.split('\n').filter_map(clean_line).collect()
}

/// One line: a carriage return redraws the line, so the last non-empty piece after one is what
/// the screen showed; remaining control characters are dropped; blank lines are skipped.
fn clean_line(line: &str) -> Option<String> {
    let piece = line.rsplit('\r').find(|p| !p.trim().is_empty()).unwrap_or("");
    let mut out = String::with_capacity(piece.len());
    for c in piece.chars() {
        if c == '\t' {
            out.push(' ');
        } else if c == '\u{1b}' || c.is_control() || c == '\u{fffd}' {
            continue;
        } else {
            out.push(c);
        }
        if out.len() >= MAX_LINE_CHARS {
            break;
        }
    }
    let t = out.trim();
    if t.is_empty() { None } else { Some(t.to_owned()) }
}

/// The key notes are stored under: trimmed, one separator style, no trailing separator, and
/// case-folded when the path is a Windows drive path (those compare case-insensitively).
/// Returns `None` for a blank folder.
pub fn normalize_folder(folder: &str) -> Option<String> {
    let t: String = folder.chars().filter(|c| !c.is_control()).collect();
    let t = t.trim();
    if t.is_empty() {
        return None;
    }
    let windows_like = t.contains('\\') || t.as_bytes().get(1) == Some(&b':');
    let mut s = if windows_like { t.replace('/', "\\") } else { t.to_owned() };
    let root_len = if s.as_bytes().get(1) == Some(&b':') { 3 } else { 1 };
    while s.len() > root_len && (s.ends_with('\\') ||s.ends_with('/')) {
        s.pop();
    }
    let drive = s.as_bytes().get(1) == Some(&b':') && s.as_bytes().first().is_some_and(u8::is_ascii_alphabetic);
    if drive {
        s = s.to_lowercase();
    }
    Some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_and_the_bundled_conpty_opening_bytes() {
        let mut c = LineCleaner::default();
        let v = c.push(b"\x1b[1t\x1b[c\x1b[?1004h\x1b[?9001h\x1b[32mhello\x1b[0m world\r\n");
        assert_eq!(v, vec!["hello world"]);
    }

    #[test]
    fn escape_split_across_chunks_is_not_indexed() {
        let mut c = LineCleaner::default();
        assert!(c.push(b"abc \x1b[3").is_empty());
        assert_eq!(c.push(b"2mgreen\n"), vec!["abc green"]);
    }

    #[test]
    fn multibyte_split_across_chunks_survives() {
        let mut c = LineCleaner::default();
        let bytes = "héllo\n".as_bytes();
        assert!(c.push(&bytes[..2]).is_empty());
        assert_eq!(c.push(&bytes[2..]), vec!["héllo"]);
    }

    #[test]
    fn carriage_return_keeps_the_last_redraw_and_blank_lines_go() {
        let mut c = LineCleaner::default();
        assert_eq!(c.push(b"10%\r50%\r100% done\r\n\r\n   \n"), vec!["100% done"]);
    }

    #[test]
    fn unfinished_line_is_held_then_finished() {
        let mut c = LineCleaner::default();
        assert!(c.push(b"Proceed? (y/n)").is_empty());
        assert!(!c.is_empty());
        assert_eq!(c.finish(), vec!["Proceed? (y/n)"]);
        assert!(c.is_empty());
    }

    #[test]
    fn hostile_bytes_do_not_panic() {
        let mut c = LineCleaner::default();
        let junk: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
        let _ = c.push(&junk);
        let _ = c.finish();
        let big = vec![b'x'; 100_000];
        let v = c.push(&big);
        assert!(v.iter().all(|l| l.len() <= MAX_LINE_CHARS + 4));
    }

    #[test]
    fn folders_normalise() {
        assert_eq!(normalize_folder("C:/Repos/App/"), Some("c:\\repos\\app".into()));
        assert_eq!(normalize_folder("c:\\repos\\app"), Some("c:\\repos\\app".into()));
        assert_eq!(normalize_folder("  /home/u/Proj/ "), Some("/home/u/Proj".into()));
        assert_eq!(normalize_folder("/"), Some("/".into()));
        assert_eq!(normalize_folder("C:\\"), Some("c:\\".into()));
        assert_eq!(normalize_folder("   "), None);
        assert_eq!(normalize_folder("\u{1}\u{2}"), None);
    }
}
