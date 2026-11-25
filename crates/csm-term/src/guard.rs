//! A streaming guard that bounds the size of OSC and DCS strings before any parser sees them.
//!
//! `vte` keeps an OSC payload in an uncapped `Vec`, and every byte goes to two parsers, so an
//! unterminated `ESC ] 0 ;` followed by hundreds of megabytes would grow memory (and the title)
//! without limit. The guard tracks string state across chunks. Once a string passes its cap it
//! feeds one CAN (which aborts the sequence in the parser) and drops everything up to the
//! terminator (BEL, ST, CAN, SUB, or an ESC starting a new sequence), then resumes normally.

use std::borrow::Cow;

/// Longest OSC or DCS string passed on, bytes.
pub const MAX_STRING_BYTES: usize = 64 * 1024;
/// OSC 52 (clipboard) may carry up to `MAX_CLIPBOARD_STORE_BYTES` decoded, i.e. 4/3 of that in
/// base64; allow a little over that.
pub const MAX_OSC52_BYTES: usize = crate::MAX_CLIPBOARD_STORE_BYTES / 3 * 4 + 1024;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    /// Saw ESC.
    Esc,
    /// Inside an OSC (`osc`) or DCS string.
    Str { osc: bool, len: usize, head: [u8; 3], dropping: bool },
}

#[derive(Debug, Default)]
pub(crate) struct StringGuard {
    state: State,
}

impl StringGuard {
    /// Returns `bytes` unchanged when nothing needs dropping (the common case), else a copy
    /// with over-long strings cut.
    pub(crate) fn filter<'a>(&mut self, bytes: &'a [u8]) -> Cow<'a, [u8]> {
        if self.state == State::Ground && !bytes.contains(&ESC) {
            return Cow::Borrowed(bytes);
        }
        let mut out: Vec<u8> = Vec::with_capacity(bytes.len().min(MAX_STRING_BYTES));
        for &b in bytes {
            self.step(b, &mut out);
        }
        Cow::Owned(out)
    }

    fn step(&mut self, b: u8, out: &mut Vec<u8>) {
        match self.state {
            State::Ground => {
                if b == ESC {
                    self.state = State::Esc;
                }
                out.push(b);
            }
            State::Esc => {
                self.state = match b {
                    b']' => State::Str { osc: true, len: 0, head: [0; 3], dropping: false },
                    b'P' => State::Str { osc: false, len: 0, head: [0; 3], dropping: false },
                    ESC => State::Esc,
                    _ => State::Ground,
                };
                out.push(b);
            }
            State::Str { osc, len, head, dropping } => {
                if dropping {
                    match b {
                        ESC => {
                            self.state = State::Esc;
                            out.push(b);
                        }
                        BEL | CAN | SUB => self.state = State::Ground,
                        _ => {}
                    }
                    return;
                }
                let cap = if osc && len >= head.len() && head == *b"52;" { MAX_OSC52_BYTES } else { MAX_STRING_BYTES };
                if len >= cap {
                    // Abort the sequence in the parsers, then swallow the rest of it.
                    out.push(CAN);
                    self.state = State::Str { osc, len, head, dropping: true };
                    self.step(b, out);
                    return;
                }
                let mut head = head;
                if let Some(slot) = head.get_mut(len) {
                    *slot = b;
                }
                out.push(b);
                self.state = match b {
                    ESC => State::Esc,
                    BEL if osc => State::Ground,
                    CAN | SUB => State::Ground,
                    _ => State::Str { osc, len: len + 1, head, dropping: false },
                };
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn run(g: &mut StringGuard, chunks: &[&[u8]]) -> Vec<u8> {
        chunks.iter().flat_map(|c| g.filter(c).into_owned()).collect()
    }

    #[test]
    fn short_strings_pass_untouched() {
        let mut g = StringGuard::default();
        assert_eq!(run(&mut g, &[b"a\x1b]0;hi\x07b\x1b]2;x\x1b\\c"]), b"a\x1b]0;hi\x07b\x1b]2;x\x1b\\c");
    }

    #[test]
    fn long_string_is_cut_across_chunks_and_text_after_survives() {
        let mut g = StringGuard::default();
        let mut out = g.filter(b"\x1b]0;").into_owned();
        let chunk = vec![b'x'; 1000];
        for _ in 0..500 {
            out.extend_from_slice(&g.filter(&chunk));
        }
        out.extend_from_slice(&g.filter(b"\x07after"));
        assert!(out.len() < MAX_STRING_BYTES + 100, "len {}", out.len());
        assert!(out.ends_with(b"after"));
        assert!(out.contains(&CAN));
    }

    #[test]
    fn esc_ends_the_drop_and_starts_a_new_sequence() {
        let mut g = StringGuard::default();
        let mut input = b"\x1b]0;".to_vec();
        input.extend(std::iter::repeat_n(b'x', MAX_STRING_BYTES + 10));
        input.extend_from_slice(b"\x1b[1mZ");
        let out = run(&mut g, &[&input]);
        assert!(out.ends_with(b"\x1b[1mZ"));
    }

    #[test]
    fn osc52_gets_the_larger_cap() {
        let mut g = StringGuard::default();
        let mut input = b"\x1b]52;c;".to_vec();
        input.extend(std::iter::repeat_n(b'A', 200_000));
        input.push(BEL);
        assert_eq!(g.filter(&input).len(), input.len());
    }
}
