//! Claude Code token and cost analytics (a port of ContextCost): parsing transcript lines,
//! pricing, finding transcripts and reading only their new bytes.
//!
//! [`parse`] and [`pricing`] are pure. [`discover`] and [`tail`] read the disk and must run on a
//! worker thread. The summaries over parsed requests are in `csm_core::claude_usage`.

pub mod discover;
pub mod parse;
pub mod pricing;
pub mod subagents;
pub mod tail;

pub use parse::{Line, ParsedCompaction, ParsedRequest, parse_line};
pub use pricing::{OVERRIDE_FILE, OverrideStatus, PriceTable, Priced};

/// The lines of one pass over one transcript, folded: requests de-duplicated by id keeping the
/// maximum per token field (one response is often several lines), compactions with their line
/// number, the first `cwd` seen, and the data-quality counters.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Batch {
    pub requests: Vec<ParsedRequest>,
    /// `(line number, compaction)`, line numbers counted from the file's start.
    pub compactions: Vec<(u64, ParsedCompaction)>,
    pub cwd: Option<String>,
    /// The last Remote Control line of the pass (`csm_core::remote_control`), if any.
    pub remote_control: Option<csm_core::remote_control::RemoteControlMark>,
    pub lines: u64,
    /// Not JSON, not an object, or an assistant line without a message.
    pub bad_lines: u64,
    /// JSON without a `type`.
    pub unknown_lines: u64,
    /// Position of each request id in `requests`.
    by_id: std::collections::HashMap<String, usize>,
}

impl Batch {
    /// A batch whose line numbers continue after `lines_before`.
    pub fn after(lines_before: u64) -> Batch {
        Batch { lines: lines_before, ..Batch::default() }
    }

    pub fn push(&mut self, line: &str) {
        self.lines += 1;
        match parse_line(line) {
            Line::Request(r) => {
                if self.cwd.is_none() {
                    self.cwd.clone_from(&r.cwd);
                }
                match self.by_id.get(&r.id).and_then(|i| self.requests.get_mut(*i)) {
                    Some(x) => {
                        x.tokens.max_with(&r.tokens);
                        x.fast |= r.fast;
                    }
                    None => {
                        self.by_id.insert(r.id.clone(), self.requests.len());
                        self.requests.push(r);
                    }
                }
            }
            Line::Compaction(c) => self.compactions.push((self.lines, c)),
            Line::RemoteControl { mark, cwd } => {
                if self.cwd.is_none() {
                    self.cwd = cwd;
                }
                self.remote_control = Some(mark);
            }
            Line::Other { cwd } => {
                if self.cwd.is_none() {
                    self.cwd = cwd;
                }
            }
            Line::Bad(_) => self.bad_lines += 1,
            Line::Unknown => self.unknown_lines += 1,
        }
    }
}

#[cfg(test)]
mod golden;
