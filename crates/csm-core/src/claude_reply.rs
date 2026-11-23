//! Claude's replies, read back from a conversation's transcript (`claude_reply_get`,
//! `claude_code_block_get`).
//!
//! A *reply* is what Claude wrote in one turn of the main thread: the text blocks of every
//! assistant message between one prompt of the person and the next, joined with a blank line,
//! exactly as Claude wrote them (Markdown). Tool calls, tool results, thinking and subagent
//! (sidechain) lines are not part of it.
//!
//! Transcript lines are untrusted: anything that is not the shape expected is skipped, never an
//! error. [`ReplyCollector`] keeps only the newest replies, so a long transcript can be streamed
//! through it in bounded memory.

use std::collections::VecDeque;

use serde_json::Value;

/// The longest reply kept (bytes); the rest is cut at a character boundary.
pub const MAX_REPLY_BYTES: usize = 4 << 20;
/// Characters in a preview.
pub const PREVIEW_CHARS: usize = 80;

/// One reply of Claude's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeReply {
    /// The id of its first assistant message (`msg_…`), or of its first line when that has no
    /// message id. Stable while the transcript grows, unlike an index counted from the newest.
    pub id: String,
    /// When its first message was written (the line's `timestamp`), as given.
    pub timestamp: Option<String>,
    /// The Markdown, as Claude wrote it.
    pub text: String,
}

impl ClaudeReply {
    /// Its first non-empty line, trimmed and shortened ([`preview`]).
    pub fn preview(&self) -> String {
        preview(&self.text)
    }

    /// Its lines.
    pub fn line_count(&self) -> usize {
        self.text.lines().count()
    }

    /// Its fenced and indented code blocks, in order.
    pub fn code_blocks(&self) -> Vec<CodeBlock> {
        code_blocks(&self.text)
    }
}

/// One code block of a reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CodeBlock {
    /// The fence's language tag (`powershell`), if any.
    pub lang: Option<String>,
    /// The code, without the fences and without the final newline.
    pub code: String,
}

impl CodeBlock {
    pub fn preview(&self) -> String {
        preview(&self.code)
    }
}

/// The first non-empty line of `text`, trimmed, control characters removed, cut at
/// [`PREVIEW_CHARS`] with `…`.
pub fn preview(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or_default();
    let clean: String = line.chars().filter(|c| !c.is_control()).collect();
    if clean.chars().count() > PREVIEW_CHARS {
        let cut: String = clean.chars().take(PREVIEW_CHARS - 1).collect();
        format!("{}…", cut.trim_end())
    } else {
        clean
    }
}

/// The code blocks of a Markdown text, in order (fenced and indented).
pub fn code_blocks(md: &str) -> Vec<CodeBlock> {
    use pulldown_cmark::{CodeBlockKind, Event, Parser, Tag, TagEnd};
    let mut out = Vec::new();
    let mut cur: Option<CodeBlock> = None;
    for ev in Parser::new(md) {
        match ev {
            Event::Start(Tag::CodeBlock(kind)) => {
                let lang = match kind {
                    CodeBlockKind::Fenced(info) => info.split_whitespace().next().filter(|l| !l.is_empty()).map(|l| l.chars().filter(|c| !c.is_control()).collect()),
                    CodeBlockKind::Indented => None,
                };
                cur = Some(CodeBlock { lang, code: String::new() });
            }
            Event::Text(t) => {
                if let Some(c) = cur.as_mut() {
                    c.code.push_str(&t);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some(mut c) = cur.take() {
                    if c.code.ends_with('\n') {
                        c.code.pop();
                        if c.code.ends_with('\r') {
                            c.code.pop();
                        }
                    }
                    out.push(c);
                }
            }
            _ => {}
        }
    }
    out
}

/// The reply being gathered.
#[derive(Debug, Default)]
struct Building {
    id: Option<String>,
    timestamp: Option<String>,
    parts: Vec<String>,
    /// `(message id, text)` of the parts, to drop a block written twice.
    seen: Vec<(Option<String>, String)>,
    bytes: usize,
}

/// Folds transcript lines, oldest first, into the newest `keep` replies.
#[derive(Debug)]
pub struct ReplyCollector {
    keep: usize,
    /// Assistant lines count only after a prompt was seen (a window that starts mid-file would
    /// otherwise begin with the tail of a turn).
    started: bool,
    cur: Option<Building>,
    done: VecDeque<ClaudeReply>,
    /// Replies finished so far (including those dropped for `keep`).
    total: usize,
}

impl ReplyCollector {
    /// A collector over a whole transcript, from its first line.
    pub fn from_start(keep: usize) -> Self {
        ReplyCollector { keep: keep.max(1), started: true, cur: None, done: VecDeque::new(), total: 0 }
    }

    /// A collector over the end of a transcript, from some line in the middle: whatever comes
    /// before the first prompt seen is the tail of a turn, and is dropped.
    pub fn mid_file(keep: usize) -> Self {
        ReplyCollector { started: false, ..ReplyCollector::from_start(keep) }
    }

    /// One transcript line (JSONL). Anything unexpected is ignored.
    pub fn push_line(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(line) else { return };
        // Subagents' lines are not the main thread.
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true) {
            return;
        }
        match v.get("type").and_then(Value::as_str) {
            Some("user") => {
                if is_prompt(&v) {
                    self.finish();
                    self.started = true;
                }
            }
            Some("assistant") if self.started => self.assistant(&v),
            _ => {}
        }
    }

    fn assistant(&mut self, v: &Value) {
        let Some(msg) = v.get("message") else { return };
        if msg.get("model").and_then(Value::as_str) == Some("<synthetic>") {
            return;
        }
        let Some(Value::Array(content)) = msg.get("content") else { return };
        let mid = msg.get("id").and_then(Value::as_str).map(clean_id);
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("text") {
                continue;
            }
            let Some(text) = block.get("text").and_then(Value::as_str) else { continue };
            if text.trim().is_empty() {
                continue;
            }
            let b = self.cur.get_or_insert_with(Building::default);
            if b.id.is_none() {
                b.id = mid.clone().or_else(|| v.get("uuid").and_then(Value::as_str).map(clean_id));
                b.timestamp = v.get("timestamp").and_then(Value::as_str).map(|t| t.chars().filter(|c| !c.is_control()).take(64).collect());
            }
            let key = (mid.clone(), text.to_owned());
            if b.seen.contains(&key) {
                continue;
            }
            if b.bytes >= MAX_REPLY_BYTES {
                continue;
            }
            let room = MAX_REPLY_BYTES - b.bytes;
            let part = cut(text, room);
            b.bytes += part.len();
            b.parts.push(part.to_owned());
            b.seen.push(key);
        }
    }

    /// Ends the reply being gathered (a new prompt came).
    fn finish(&mut self) {
        let Some(b) = self.cur.take() else { return };
        if b.parts.is_empty() {
            return;
        }
        let text = b.parts.iter().map(|p| p.trim_matches('\n')).collect::<Vec<_>>().join("\n\n");
        self.total += 1;
        self.done.push_back(ClaudeReply { id: b.id.unwrap_or_else(|| format!("reply-{}", self.total)), timestamp: b.timestamp, text });
        while self.done.len() > self.keep {
            self.done.pop_front();
        }
    }

    /// The replies gathered, newest first (the one still being written included).
    pub fn into_newest_first(mut self) -> Vec<ClaudeReply> {
        self.finish();
        self.done.into_iter().rev().collect()
    }
}

/// A prompt of the person's (a new turn): a `user` line that is not a tool result, nor a meta
/// line Claude Code adds itself.
fn is_prompt(v: &Value) -> bool {
    if v.get("isMeta").and_then(Value::as_bool) == Some(true) {
        return false;
    }
    match v.get("message").and_then(|m| m.get("content")) {
        Some(Value::String(s)) => !s.trim().is_empty(),
        Some(Value::Array(blocks)) => {
            !blocks.is_empty() && !blocks.iter().any(|b| b.get("type").and_then(Value::as_str) == Some("tool_result"))
        }
        _ => false,
    }
}

fn clean_id(s: &str) -> String {
    s.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-')).take(128).collect()
}

/// At most `max` bytes of `s`, cut at a character boundary.
fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.get(..end).unwrap_or_default()
}

/// The newest-first replies of a whole transcript text (tests, small files).
pub fn replies_of(transcript: &str, keep: usize) -> Vec<ClaudeReply> {
    let mut c = ReplyCollector::from_start(keep);
    for line in transcript.lines() {
        c.push_line(line);
    }
    c.into_newest_first()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = include_str!("claude_reply_fixture.jsonl");

    #[test]
    fn turns_are_split_at_prompts_and_tool_traffic_is_left_out() {
        let r = replies_of(FIXTURE, 50);
        assert_eq!(r.len(), 3, "{r:#?}");
        // Newest first.
        assert!(r[0].text.starts_with("Run this in PowerShell"), "{}", r[0].text);
        assert!(r[1].text.starts_with("I'll look at the folder first."));
        assert!(r[1].text.contains("\n\nThe folder has two projects:"), "text blocks of one turn are joined: {}", r[1].text);
        assert!(!r[1].text.contains("tool_use") && !r[1].text.contains("Get-ChildItem C:\\work"), "no tool JSON: {}", r[1].text);
        assert_eq!(r[2].text, "Hello! What can I do for you?");
        assert_eq!(r[0].id, "msg_03");
        assert_eq!(r[2].timestamp.as_deref(), Some("2026-11-20T10:00:01.000Z"));
    }

    #[test]
    fn sidechain_thinking_synthetic_and_duplicate_blocks_are_skipped() {
        let r = replies_of(FIXTURE, 50);
        let all: String = r.iter().map(|x| x.text.clone()).collect();
        assert!(!all.contains("subagent says"), "{all}");
        assert!(!all.contains("private thoughts"), "{all}");
        assert!(!all.contains("No response requested"), "{all}");
        assert_eq!(r[0].text.matches("Run this in PowerShell").count(), 1, "a block written twice counts once");
    }

    #[test]
    fn code_blocks_keep_their_language_and_exact_text() {
        let r = replies_of(FIXTURE, 50);
        let blocks = r[0].code_blocks();
        assert_eq!(blocks.len(), 2, "{blocks:#?}");
        assert_eq!(blocks[0].lang.as_deref(), Some("powershell"));
        assert_eq!(
            blocks[0].code,
            "Get-ChildItem -Path C:\\work -Recurse -File | Where-Object { $_.Length -gt 1MB } | Sort-Object Length -Descending | Select-Object -First 20 FullName, Length"
        );
        assert_eq!(blocks[1].lang.as_deref(), Some("json"));
        assert_eq!(blocks[1].code, "{\n  \"a\": 1\n}");
        assert!(r[1].code_blocks().is_empty());
    }

    #[test]
    fn keep_bounds_memory_to_the_newest() {
        let r = replies_of(FIXTURE, 1);
        assert_eq!(r.len(), 1);
        assert!(r[0].text.starts_with("Run this in PowerShell"));
    }

    #[test]
    fn a_window_from_mid_file_drops_the_partial_first_turn() {
        // Start in the middle of the second turn: its tail is not a whole reply.
        let lines: Vec<&str> = FIXTURE.lines().collect();
        let from = lines.iter().position(|l| l.contains("The folder has two projects")).unwrap();
        let mut c = ReplyCollector::mid_file(50);
        // A partial first line (cut mid-JSON) is ignored.
        c.push_line("\"text\":\"half a line\"}]}}");
        for l in &lines[from..] {
            c.push_line(l);
        }
        let r = c.into_newest_first();
        assert_eq!(r.len(), 1, "{r:#?}");
        assert!(r[0].text.starts_with("Run this in PowerShell"));
    }

    #[test]
    fn hostile_lines_never_panic() {
        let mut c = ReplyCollector::from_start(5);
        for l in [
            "",
            "not json",
            "{",
            "[]",
            "{\"type\":5}",
            "{\"type\":\"assistant\"}",
            "{\"type\":\"assistant\",\"message\":{\"content\":\"str\"}}",
            "{\"type\":\"assistant\",\"message\":{\"content\":[{\"type\":\"text\",\"text\":7}]}}",
            "{\"type\":\"user\",\"message\":{\"content\":[]}}",
            "{\"type\":\"assistant\",\"message\":{\"id\":\"\\u0000\\u0007\",\"content\":[{\"type\":\"text\",\"text\":\"ok\"}]}}",
        ] {
            c.push_line(l);
        }
        let r = c.into_newest_first();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].text, "ok");
    }

    #[test]
    fn previews_are_one_trimmed_line() {
        assert_eq!(preview("\n\n  Hello there  \nsecond"), "Hello there");
        let long = "x".repeat(200);
        let p = preview(&long);
        assert_eq!(p.chars().count(), PREVIEW_CHARS);
        assert!(p.ends_with('…'));
        assert_eq!(preview("a\u{1b}[31mb"), "a[31mb");
    }

    #[test]
    fn cut_respects_char_boundaries() {
        assert_eq!(cut("æøå", 3), "æ");
        assert_eq!(cut("abc", 10), "abc");
    }
}
