//! A run's output: ANSI-stripped text, capped at [`MAX_OUTPUT_BYTES`] (the head is dropped).
//!
//! Written by the PTY reader thread, read by the UI (through [`RunOutputHandle`], once a frame
//! at most) and by the `run_output` tool. Chunks can split an escape sequence or a UTF-8
//! character; both are carried to the next chunk rather than stripped half-way.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

/// The output kept per run (`RunInstance.MaxBufferChars` in the WPF app: about 1 MB).
pub const MAX_OUTPUT_BYTES: usize = 1_000_000;

/// The longest incomplete escape sequence carried over to the next chunk; anything longer is
/// not a sequence that will complete, and is flushed as text.
const MAX_CARRY: usize = 256;

#[derive(Default)]
struct Buf {
    text: String,
    /// Bytes of an incomplete UTF-8 character at the end of the last chunk.
    utf8: Vec<u8>,
    /// An escape sequence the last chunk ended inside.
    esc: String,
}

/// The shared buffer. Cheap to clone through an `Arc`.
#[derive(Default)]
pub struct RunOutput {
    buf: Mutex<Buf>,
    generation: AtomicU64,
}

impl RunOutput {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Buf> {
        self.buf.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Raw PTY bytes.
    pub fn append_bytes(&self, bytes: &[u8]) {
        let mut b = self.lock();
        let mut raw = std::mem::take(&mut b.utf8);
        raw.extend_from_slice(bytes);
        let (text, rest) = split_utf8(&raw);
        b.utf8 = rest.to_vec();
        let mut s = std::mem::take(&mut b.esc);
        s.push_str(&text);
        // Hold back an escape sequence the chunk ended inside.
        if let Some(i) = s.rfind('\u{1b}') {
            let tail = s.get(i..).unwrap_or("");
            let complete = csm_core::alert_detector::strip_ansi(tail) != tail || tail.contains('\u{7}');
            if !complete && tail.len() <= MAX_CARRY {
                b.esc = tail.to_owned();
                s.truncate(i);
            }
        }
        let clean = normalise(&csm_core::alert_detector::strip_ansi(&s));
        push_capped(&mut b.text, &clean);
        drop(b);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    /// App-authored text ("Cannot start: …").
    pub fn append_text(&self, text: &str) {
        let mut b = self.lock();
        push_capped(&mut b.text, text);
        drop(b);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub fn text(&self) -> String {
        self.lock().text.clone()
    }

    /// The last `max_bytes` of the text (cut at a line start when one is near).
    pub fn tail(&self, max_bytes: usize) -> String {
        let b = self.lock();
        tail_of(&b.text, max_bytes).to_owned()
    }

    pub fn len(&self) -> usize {
        self.lock().text.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Changes whenever the text may have changed. Lock-free.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}

/// The valid UTF-8 prefix (lossy for invalid bytes) and an incomplete trailing character.
fn split_utf8(raw: &[u8]) -> (String, &[u8]) {
    // An incomplete character is at most 3 bytes at the very end.
    let cut = (1..=3.min(raw.len()))
        .find(|&n| {
            let start = raw.len() - n;
            let lead = raw.get(start).copied().unwrap_or(0);
            let need = if lead >= 0xF0 { 4 } else if lead >= 0xE0 { 3 } else if lead >= 0xC0 { 2 } else { 0 };
            need > n && raw.get(start + 1..).is_some_and(|c| c.iter().all(|b| (0x80..0xC0).contains(b)))
        })
        .map_or(raw.len(), |n| raw.len() - n);
    let (head, rest) = raw.split_at(cut);
    (String::from_utf8_lossy(head).into_owned(), rest)
}

/// `\r\n` → `\n`; a lone `\r` (a progress line redrawing itself) is dropped; other C0 controls
/// except tab and newline are removed.
fn normalise(s: &str) -> String {
    s.replace("\r\n", "\n").chars().filter(|c| *c == '\n' || *c == '\t' || !c.is_control()).collect()
}

fn push_capped(text: &mut String, add: &str) {
    text.push_str(add);
    if text.len() > MAX_OUTPUT_BYTES {
        let mut cut = text.len() - MAX_OUTPUT_BYTES;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        text.drain(..cut);
    }
}

fn tail_of(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut cut = text.len() - max_bytes;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    let t = text.get(cut..).unwrap_or("");
    if text.as_bytes().get(cut.wrapping_sub(1)) == Some(&b'\n') {
        return t;
    }
    // Start at a line when one begins soon after the cut (and something follows it).
    match t.find('\n') {
        Some(i) if i < 200 && i + 1 < t.len() => t.get(i + 1..).unwrap_or(t),
        _ => t,
    }
}

/// A run's output as the UI holds it: read once a frame at most, never blocks on the engine.
#[derive(Clone)]
pub struct RunOutputHandle(pub(crate) Arc<RunOutput>);

impl RunOutputHandle {
    pub fn text(&self) -> String {
        self.0.text()
    }

    pub fn tail(&self, max_bytes: usize) -> String {
        self.0.tail(max_bytes)
    }

    pub fn generation(&self) -> u64 {
        self.0.generation()
    }
}

impl std::fmt::Debug for RunOutputHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunOutputHandle").field("len", &self.0.len()).finish()
    }
}

impl PartialEq for RunOutputHandle {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_and_carries_split_sequences_and_characters() {
        let o = RunOutput::new();
        o.append_bytes(b"\x1b[32mok\x1b[");
        o.append_bytes(b"0m done\r\n");
        // "é" split across two chunks.
        o.append_bytes(&[b'c', b'a', b'f', 0xC3]);
        o.append_bytes(&[0xA9, b'\r', b'\n']);
        assert_eq!(o.text(), "ok done\ncafé\n");
        assert!(o.generation() >= 4);
    }

    #[test]
    fn osc_titles_and_the_conpty_opening_bytes_are_removed() {
        let o = RunOutput::new();
        o.append_bytes(b"\x1b[?25l\x1b]0;C:\\WINDOWS\\system32\\cmd.exe\x07hello\x1b[?25h\r\n");
        assert_eq!(o.text(), "hello\n");
    }

    #[test]
    fn the_buffer_is_capped_at_the_head() {
        let o = RunOutput::new();
        let line = "x".repeat(999) + "\n";
        for _ in 0..1100 {
            o.append_bytes(line.as_bytes());
        }
        o.append_text("END");
        let t = o.text();
        assert!(t.len() <= MAX_OUTPUT_BYTES, "{}", t.len());
        assert!(t.ends_with("END"));
        assert_eq!(o.tail(10), "END");
        assert!(o.tail(1004).starts_with('x'));
    }

    #[test]
    fn a_lone_escape_that_never_completes_is_not_held_forever() {
        let o = RunOutput::new();
        o.append_bytes(b"a\x1b");
        o.append_bytes(&vec![b'b'; 300]);
        assert!(o.text().ends_with("bbb"), "{:?}", o.text());
    }
}
