//! The `code` and `diff` widgets of a view tree: what a plugin sends (text or a unified patch,
//! the language, gutter marks, inline comment threads, range actions), validated. The text is
//! untrusted: it is kept as is (control and bidi characters are *shown* by the renderer, never
//! interpreted), and every list and size is capped.
//!
//! Text over [`MAX_CODE_BYTES`] and `blob` references (the next stream's work) are not shown:
//! the node keeps a [`Withheld`] reason and the renderer draws it as a notice, so the rest of the
//! screen still works.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde_json::{Map, Value};

use super::{Res, ScreenError, Tone, TreeParser, clean_label, err, label, valid_key};
use crate::diff::{self, Patch, Side};

/// Inline `text` or `patch` of a code or diff node.
pub const MAX_CODE_BYTES: usize = 512 * 1024;
/// Threads on one code or diff node.
pub const MAX_THREADS: usize = 500;
/// Gutter marks on one node.
pub const MAX_MARKS: usize = 5_000;
/// Range actions on one node.
pub const MAX_RANGE_ACTIONS: usize = 4;
/// Highlighted lines (`highlight_lines`) on one code node.
pub const MAX_HIGHLIGHT_LINES: usize = 5_000;
/// Line numbers a plugin may name (the first line and the anchors); larger values are clamped.
pub const MAX_LINE_NO: u32 = 100_000_000;

/// Text split into lines once, so the renderer indexes by line without scanning.
#[derive(Debug, PartialEq, Eq)]
pub struct Doc {
    text: String,
    /// Byte offset of each line's start. An empty text has no lines; a final line break does not
    /// start another one.
    starts: Vec<u32>,
    hash: u64,
}

impl Doc {
    pub fn new(text: String) -> Doc {
        let mut starts: Vec<u32> = Vec::new();
        if !text.is_empty() {
            starts.push(0);
        }
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' && i + 1 < text.len() {
                starts.push((i + 1) as u32);
            }
        }
        let mut h = DefaultHasher::new();
        text.hash(&mut h);
        Doc { hash: h.finish(), text, starts }
    }

    /// Lines in the text.
    pub fn len(&self) -> usize {
        self.starts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.starts.is_empty()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// A hash of the text (a cache key, not a security measure).
    pub fn hash(&self) -> u64 {
        self.hash
    }

    /// Line `i` (0-based) without its line break (`\n` or `\r\n`).
    pub fn line(&self, i: usize) -> &str {
        let Some(&s) = self.starts.get(i) else { return "" };
        let e = self.starts.get(i + 1).map_or(self.text.len(), |n| *n as usize);
        let l = self.text.get(s as usize..e).unwrap_or("");
        let l = l.strip_suffix('\n').unwrap_or(l);
        l.strip_suffix('\r').unwrap_or(l)
    }
}

/// Why a node's content is not shown. The renderer draws it as a notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Withheld(pub String);

#[derive(Debug, Clone, PartialEq)]
pub enum CodeContent {
    Text(Arc<Doc>),
    Withheld(Withheld),
}

#[derive(Debug, Clone, PartialEq)]
pub enum DiffContent {
    Patch(Arc<Patch>),
    Withheld(Withheld),
}

/// A dot or a count in the gutter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    pub line: u32,
    pub side: Side,
    pub tone: Tone,
    pub count: Option<u32>,
}

/// The lines a thread is about: `start..=end` on one side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub side: Side,
    pub start: u32,
    pub end: u32,
}

/// An inline comment thread: a block under line `end` while `open`, a gutter badge otherwise.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub key: String,
    pub anchor: Anchor,
    pub open: bool,
    /// The number the closed badge shows (the comments in the thread).
    pub count: u32,
    pub child: Box<super::Node>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RangeAction {
    pub id: String,
    pub label: String,
    pub icon: String,
}

/// What `code` and `diff` share: marks, threads, range actions, a line to scroll to.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Overlay {
    pub marks: Vec<Mark>,
    pub threads: Vec<Thread>,
    pub range_actions: Vec<RangeAction>,
    pub scroll_to: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodeView {
    pub content: CodeContent,
    pub language: String,
    pub path: String,
    /// The number of the first line (1 unless the plugin shows a slice of a file).
    pub first_line: u32,
    /// Lines drawn with a highlight band (numbers, as shown).
    pub highlight_lines: Vec<u32>,
    pub overlay: Overlay,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DiffView {
    pub content: DiffContent,
    /// Side by side instead of unified.
    pub split: bool,
    pub language: String,
    pub path: String,
    pub overlay: Overlay,
}

impl Overlay {
    pub(super) fn bytes(&self) -> usize {
        self.marks.len() * 16 + self.threads.iter().map(|t| t.key.len() + 24).sum::<usize>() + self.range_actions.iter().map(|a| a.id.len() + a.label.len() + 8).sum::<usize>()
    }
}

impl CodeView {
    pub(super) fn bytes(&self) -> usize {
        let t = match &self.content {
            CodeContent::Text(d) => d.text().len() + d.len() * 4,
            CodeContent::Withheld(w) => w.0.len(),
        };
        t + self.language.len() + self.path.len() + self.highlight_lines.len() * 4 + self.overlay.bytes()
    }
}

impl DiffView {
    pub(super) fn bytes(&self) -> usize {
        let t = match &self.content {
            DiffContent::Patch(p) => p.files.iter().map(|f| f.path().len() + 32 + f.hunks.iter().map(|h| h.heading.len() + 32 + h.lines.iter().map(|l| l.text.len() + 24).sum::<usize>()).sum::<usize>()).sum(),
            DiffContent::Withheld(w) => w.0.len(),
        };
        t + self.language.len() + self.path.len() + self.overlay.bytes()
    }
}

fn line_no(v: Option<&Value>, default: u32) -> u32 {
    v.and_then(Value::as_u64).map_or(default, |n| n.min(u64::from(MAX_LINE_NO)) as u32)
}

fn short(o: &Map<String, Value>, k: &str, cap: usize) -> String {
    o.get(k).and_then(Value::as_str).map(|s| clean_label(s, cap)).unwrap_or_default()
}

fn size_text(n: usize) -> String {
    if n >= 1024 * 1024 { format!("{:.1} MiB", n as f64 / (1024.0 * 1024.0)) } else { format!("{} KiB", n.div_ceil(1024)) }
}

impl TreeParser<'_> {
    /// `marks`, `threads`, `range_actions` and `scroll_to`, shared by `code` and `diff`.
    /// Returns the overlay and whether the node sends events (so it needs a key).
    pub(super) fn overlay(&mut self, o: &Map<String, Value>, depth: usize) -> Res<(Overlay, bool)> {
        let mut ov = Overlay::default();
        match o.get("marks") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => {
                if a.len() > MAX_MARKS {
                    return err(format!("more than {MAX_MARKS} marks on one node"));
                }
                for m in a {
                    let Some(m) = m.as_object() else { continue };
                    let Some(line) = m.get("line").and_then(Value::as_u64) else { continue };
                    ov.marks.push(Mark {
                        line: line.clamp(1, u64::from(MAX_LINE_NO)) as u32,
                        side: Side::parse(m.get("side").and_then(Value::as_str)),
                        tone: Tone::parse(m.get("tone")),
                        count: m.get("count").and_then(Value::as_u64).map(|c| c.min(9999) as u32),
                    });
                }
            }
            Some(_) => return err("`marks` must be an array"),
        }
        match o.get("threads") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => {
                if a.len() > MAX_THREADS {
                    return err(format!("more than {MAX_THREADS} threads on one node"));
                }
                for t in a {
                    let m = t.as_object().ok_or_else(|| ScreenError::new("a thread is not an object"))?;
                    let key = m.get("key").and_then(Value::as_str).ok_or_else(|| ScreenError::new("a thread needs a `key`"))?;
                    valid_key(key)?;
                    if !self.keys.insert(key.to_owned()) {
                        return err(format!("two nodes or threads have the key `{}`", clean_label(key, 40)));
                    }
                    let a = m.get("anchor").and_then(Value::as_object).ok_or_else(|| ScreenError::new("a thread needs an `anchor`"))?;
                    let start = line_no(a.get("start"), 1).max(1);
                    let end = line_no(a.get("end"), start).max(1);
                    let (start, end) = if start <= end { (start, end) } else { (end, start) };
                    let child = m.get("child").ok_or_else(|| ScreenError::new("a thread needs a `child`"))?;
                    let child = self.node(child, depth + 1)?;
                    ov.threads.push(Thread {
                        key: key.to_owned(),
                        anchor: Anchor { side: Side::parse(a.get("side").and_then(Value::as_str)), start, end },
                        open: m.get("open").and_then(Value::as_bool).unwrap_or(false),
                        count: m.get("count").and_then(Value::as_u64).map_or(1, |c| c.clamp(1, 9999) as u32),
                        child: Box::new(child),
                    });
                }
            }
            Some(_) => return err("`threads` must be an array"),
        }
        match o.get("range_actions") {
            None | Some(Value::Null) => {}
            Some(Value::Array(a)) => {
                if a.len() > MAX_RANGE_ACTIONS {
                    return err(format!("more than {MAX_RANGE_ACTIONS} range actions"));
                }
                for r in a {
                    let m = r.as_object().ok_or_else(|| ScreenError::new("a range action is not an object"))?;
                    let id = m.get("id").and_then(Value::as_str).ok_or_else(|| ScreenError::new("a range action needs an `id`"))?;
                    valid_key(id)?;
                    if ov.range_actions.iter().any(|x| x.id == id) {
                        return err(format!("two range actions have the id `{}`", clean_label(id, 40)));
                    }
                    let l = label(m, "label");
                    ov.range_actions.push(RangeAction {
                        id: id.to_owned(),
                        label: if l.is_empty() { clean_label(id, 40) } else { l },
                        icon: m.get("icon").and_then(Value::as_str).map(|s| s.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').take(32).collect()).unwrap_or_default(),
                    });
                }
            }
            Some(_) => return err("`range_actions` must be an array"),
        }
        ov.scroll_to = o.get("scroll_to").and_then(Value::as_u64).map(|n| n.clamp(1, u64::from(MAX_LINE_NO)) as u32);
        let events = !ov.range_actions.is_empty() || !ov.threads.is_empty();
        Ok((ov, events))
    }

    /// The text of a `code` node, or why it is not shown.
    fn code_content(o: &Map<String, Value>) -> Res<CodeContent> {
        if let Some(b) = o.get("blob").filter(|b| !b.is_null()) {
            let lines = b.get("lines").and_then(Value::as_u64).unwrap_or(0);
            let bytes = b.get("bytes").and_then(Value::as_u64).unwrap_or(0);
            return Ok(CodeContent::Withheld(Withheld(format!("This file is too large to show here ({lines} lines, {}). Large files are not supported yet.", size_text(bytes as usize)))));
        }
        match o.get("text") {
            None | Some(Value::Null) => Ok(CodeContent::Text(Arc::new(Doc::new(String::new())))),
            Some(Value::String(s)) if s.len() > MAX_CODE_BYTES => Ok(CodeContent::Withheld(Withheld(format!("This file is too large to show here ({}; the limit is {}).", size_text(s.len()), size_text(MAX_CODE_BYTES))))),
            Some(Value::String(s)) => Ok(CodeContent::Text(Arc::new(Doc::new(s.clone())))),
            Some(_) => err("a code node's `text` must be a string"),
        }
    }

    fn diff_content(o: &Map<String, Value>) -> Res<DiffContent> {
        if let Some(b) = o.get("blob").filter(|b| !b.is_null()) {
            let bytes = b.get("bytes").and_then(Value::as_u64).unwrap_or(0);
            return Ok(DiffContent::Withheld(Withheld(format!("This diff is too large to show here ({}). Open the file instead.", size_text(bytes as usize)))));
        }
        match o.get("patch") {
            None | Some(Value::Null) => Ok(DiffContent::Patch(Arc::new(Patch::default()))),
            Some(Value::String(s)) if s.len() > MAX_CODE_BYTES => Ok(DiffContent::Withheld(Withheld(format!("This diff is too large to show here ({}; the limit is {}). Open the file instead.", size_text(s.len()), size_text(MAX_CODE_BYTES))))),
            Some(Value::String(s)) => Ok(DiffContent::Patch(Arc::new(diff::parse_unified(s)))),
            Some(_) => err("a diff node's `patch` must be a string"),
        }
    }

    pub(super) fn code(&mut self, o: &Map<String, Value>, depth: usize) -> Res<(CodeView, bool)> {
        let (overlay, events) = self.overlay(o, depth)?;
        let mut highlight_lines: Vec<u32> = match o.get("highlight_lines") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => {
                if a.len() > MAX_HIGHLIGHT_LINES {
                    return err(format!("more than {MAX_HIGHLIGHT_LINES} highlighted lines"));
                }
                a.iter().filter_map(Value::as_u64).map(|n| n.min(u64::from(MAX_LINE_NO)) as u32).collect()
            }
            Some(_) => return err("`highlight_lines` must be an array"),
        };
        highlight_lines.sort_unstable();
        highlight_lines.dedup();
        let view = CodeView {
            content: Self::code_content(o)?,
            language: short(o, "language", 40),
            path: short(o, "path", 1024),
            first_line: line_no(o.get("first_line"), 1).max(1),
            highlight_lines,
            overlay,
        };
        Ok((view, events))
    }

    pub(super) fn diff(&mut self, o: &Map<String, Value>, depth: usize) -> Res<(DiffView, bool)> {
        let (overlay, events) = self.overlay(o, depth)?;
        let view = DiffView {
            content: Self::diff_content(o)?,
            split: o.get("view").and_then(Value::as_str) == Some("split"),
            language: short(o, "language", 40),
            path: short(o, "path", 1024),
            overlay,
        };
        Ok((view, events))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doc_splits_lines_without_their_breaks() {
        let d = Doc::new("a\r\nb\n\nc".into());
        assert_eq!((d.len(), d.line(0), d.line(1), d.line(2), d.line(3), d.line(4)), (4, "a", "b", "", "c", ""));
        let d = Doc::new("a\n".into());
        assert_eq!((d.len(), d.line(0)), (1, "a"), "a final line break does not start a line");
        assert!(Doc::new(String::new()).is_empty());
        let d = Doc::new("x\r".into());
        assert_eq!(d.line(0), "x");
        let d = Doc::new("caf\u{e9}\n\u{4e2d}\u{6587}".into());
        assert_eq!((d.line(0), d.line(1)), ("caf\u{e9}", "\u{4e2d}\u{6587}"));
        assert_eq!(Doc::new("same".into()).hash(), Doc::new("same".into()).hash());
        assert_ne!(Doc::new("same".into()).hash(), Doc::new("other".into()).hash());
    }
}
