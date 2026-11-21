//! The screen's view tree (docs/addons.md, "Screens"): a pure function from a [`Model`] to JSON.
//! No colours, sizes or fonts: only the semantic choices the protocol offers (`tone`, `kind`,
//! `gap`). Everything the person can do is either an event the host handles or a button bound to a
//! tool (`review.set_status`, `review.delete`, `session_send_text`), so a button here does nothing
//! an agent could not do with the same tool.

// serde_json::Value indexing reads Null for an absent key, and every write here is to an object this module built.
#![allow(clippy::indexing_slicing)]

use std::collections::{HashMap, HashSet};

use serde_json::{Value, json};

use crate::prompt::one_line;
use crate::store::{Status, Thread};

/// Which list of files the left column shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Working,
    Staged,
    Files,
}

impl Source {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Source::Working => "working",
            Source::Staged => "staged",
            Source::Files => "files",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Source> {
        match s {
            "working" => Some(Source::Working),
            "staged" => Some(Source::Staged),
            "files" => Some(Source::Files),
            _ => None,
        }
    }
}

/// One row of the file list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRow {
    pub path: String,
    /// `M A D R C U ?`, none for the plain file list.
    pub status: Option<char>,
    pub old_path: Option<String>,
}

/// What the middle column shows for the selected file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Body {
    /// No file picked yet.
    Nothing,
    /// A note instead of content (binary, too large, an error).
    Note(String),
    Diff { patch: String, truncated: bool },
    Code { text: String, truncated: bool },
}

/// Where a thread is now, in the selected file (or `Elsewhere` for a thread of another file).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Where {
    Exact(u32, u32),
    Moved(u32, u32),
    Outdated,
    Elsewhere,
}

impl Where {
    #[must_use]
    pub fn range(self) -> Option<(u32, u32)> {
        match self {
            Where::Exact(s, e) | Where::Moved(s, e) => Some((s, e)),
            _ => None,
        }
    }
}

/// A thread with what the screen needs to draw it.
#[derive(Debug, Clone)]
pub struct ThreadView {
    pub thread: Thread,
    pub at: Where,
    /// The "Send to agent" text for this thread alone (empty: no session to send to).
    pub prompt: String,
}

/// A comment being written, anchored to a selection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub path: String,
    /// `new` or `old`; ignored for a whole-file comment.
    pub side: String,
    pub start: u32,
    pub end: u32,
    pub whole_file: bool,
}

pub struct Model<'a> {
    pub repo: &'a str,
    pub session_id: Option<&'a str>,
    /// Live sessions in this repository (id, name) the review may be sent to.
    pub sessions: &'a [(String, String)],
    pub project: &'a str,
    pub source: Source,
    pub split: bool,
    pub files: &'a [FileRow],
    pub files_note: Option<&'a str>,
    pub untracked: &'a HashSet<String>,
    pub selected: Option<&'a str>,
    pub body: &'a Body,
    pub threads: &'a [ThreadView],
    pub draft: Option<&'a Draft>,
    pub open: &'a HashMap<String, bool>,
    pub scroll_to: Option<u32>,
    pub loading: bool,
    pub error: Option<&'a str>,
    /// "Send to agent" for every open thread (empty: nothing to send).
    pub prompt_all: &'a str,
    pub persistent: bool,
}

fn tone_of(s: Status) -> &'static str {
    match s {
        Status::Open => "warning",
        Status::Addressed => "info",
        Status::Resolved => "success",
    }
}

fn status_rank(s: Status) -> u8 {
    match s {
        Status::Open => 0,
        Status::Addressed => 1,
        Status::Resolved => 2,
    }
}

/// `path:12-14`, `path:12`, `path (file)`.
#[must_use]
pub fn place(t: &Thread, at: Where) -> String {
    if t.anchor.kind == "file" {
        return format!("{} (file)", t.anchor.path);
    }
    let (s, e) = at.range().unwrap_or((t.anchor.start, t.anchor.end));
    if s == e { format!("{}:{s}", t.anchor.path) } else { format!("{}:{s}-{e}", t.anchor.path) }
}

fn button(key: &str, label: &str, kind: &str) -> Value {
    json!({"type": "button", "key": key, "label": label, "kind": kind, "event": true})
}

fn action(label: &str, kind: &str, tool: &str, params: &Value) -> Value {
    json!({"type": "button", "label": label, "kind": kind, "action": {"tool": tool, "params": params}})
}

fn status_buttons(m: &Model<'_>, tv: &ThreadView) -> Vec<Value> {
    let id = &tv.thread.id;
    let set = |label: &str, kind: &str, status: &str| action(label, kind, "review.set_status", &json!({"thread_id": id, "status": status, "project": m.project}));
    let mut row = Vec::new();
    match tv.thread.status {
        Status::Open => row.push(set("Resolve", "secondary", "resolved")),
        Status::Addressed => {
            row.push(set("Resolve", "primary", "resolved"));
            row.push(set("Reopen", "secondary", "open"));
        }
        Status::Resolved => row.push(set("Reopen", "secondary", "open")),
    }
    if let Some(sid) = m.session_id.filter(|_| !tv.prompt.is_empty()) {
        row.push(action("Send to agent", "ghost", "session_send_text", &json!({"session_id": sid, "text": tv.prompt, "submit": false})));
    }
    row.push(action("Delete", "danger", "review.delete", &json!({"thread_id": id, "project": m.project})));
    row
}

/// The thread's block: status, where it is, the comments, a reply box and its buttons.
fn thread_block(m: &Model<'_>, tv: &ThreadView) -> Value {
    let t = &tv.thread;
    let mut badges = vec![json!({"type": "badge", "text": t.status.name(), "tone": tone_of(t.status)})];
    match tv.at {
        Where::Moved(..) => badges.push(json!({"type": "badge", "text": "moved", "tone": "muted"})),
        Where::Outdated => badges.push(json!({"type": "badge", "text": "outdated", "tone": "danger"})),
        _ => {}
    }
    badges.push(json!({"type": "text", "style": "caption", "tone": "muted", "text": place(t, tv.at)}));
    let mut children = vec![json!({"type": "row", "gap": "sm", "children": badges})];
    if tv.at == Where::Outdated && !t.anchor.snippet.is_empty() {
        let fence = format!("```\n{}\n```", t.anchor.snippet.replace("```", "'''"));
        let note = if t.anchor.commit.is_empty() { "The code changed. As written:".to_owned() } else { format!("The code changed. As written at {}:", t.anchor.commit.chars().take(8).collect::<String>()) };
        children.push(json!({"type": "text", "style": "caption", "tone": "muted", "text": note}));
        children.push(json!({"type": "markdown", "source": fence}));
    }
    for c in &t.comments {
        children.push(json!({
            "type": "comment",
            "author": {"name": c.author.name, "kind": if c.author.is_agent() { "agent" } else { "person" }},
            "time_ms": c.created_ms,
            "body": c.body,
        }));
    }
    children.push(json!({
        "type": "text_input", "key": format!("reply-{}-{}", t.id, t.comments.len()), "value": "", "placeholder": "Reply...",
        "multiline": true, "max_len": 16384, "submit_label": "Reply"
    }));
    children.push(json!({"type": "row", "gap": "sm", "children": status_buttons(m, tv)}));
    json!({"type": "column", "key": format!("tb-{}", t.id), "gap": "xs", "children": children})
}

fn is_open(m: &Model<'_>, t: &Thread) -> bool {
    m.open.get(&t.id).copied().unwrap_or(t.status != Status::Resolved)
}

fn draft_block(d: &Draft) -> Value {
    let what = if d.whole_file {
        format!("Comment on {}", d.path)
    } else if d.start == d.end {
        format!("Comment on line {}", d.start)
    } else {
        format!("Comment on lines {}-{}", d.start, d.end)
    };
    json!({"type": "column", "key": "draft-block", "gap": "xs", "children": [
        {"type": "text", "style": "caption", "tone": "muted", "text": what},
        {"type": "text_input", "key": "draft-input", "value": "", "placeholder": "Write a comment...", "multiline": true, "max_len": 16384, "submit_label": "Comment"},
        {"type": "row", "children": [button("draft-cancel", "Cancel", "ghost")]}
    ]})
}

/// The `threads` array of a code or diff node: the threads that sit on a line of the shown file
/// (and the draft).
fn node_threads(m: &Model<'_>, path: &str, diff: bool) -> Vec<Value> {
    let mut out = Vec::new();
    for tv in m.threads {
        let t = &tv.thread;
        if t.anchor.path != path || t.anchor.kind == "file" {
            continue;
        }
        let old = t.anchor.side == "old";
        if old && !diff {
            continue;
        }
        let Some((s, e)) = tv.at.range() else { continue };
        let mut anchor = json!({"start": s, "end": e});
        if diff {
            anchor["side"] = json!(if old { "old" } else { "new" });
        }
        out.push(json!({"key": t.id, "anchor": anchor, "open": is_open(m, t), "count": t.comments.len(), "child": thread_block(m, tv)}));
    }
    if let Some(d) = m.draft.filter(|d| d.path == path && !d.whole_file) {
        let mut anchor = json!({"start": d.start, "end": d.end});
        if diff {
            anchor["side"] = json!(d.side);
        }
        out.push(json!({"key": "draft", "anchor": anchor, "open": true, "count": 1, "child": draft_block(d)}));
    }
    out.truncate(500);
    out
}

/// Whole-file and outdated threads of the shown file, and a whole-file draft: they have no line.
fn loose_threads(m: &Model<'_>, path: &str) -> Vec<Value> {
    let mut out = Vec::new();
    if let Some(d) = m.draft.filter(|d| d.path == path && d.whole_file) {
        out.push(draft_block(d));
    }
    for tv in m.threads {
        let t = &tv.thread;
        let loose = t.anchor.path == path && (t.anchor.kind == "file" || tv.at == Where::Outdated || (t.anchor.side == "old" && matches!(m.body, Body::Code { .. })));
        if loose {
            out.push(thread_block(m, tv));
            out.push(json!({"type": "divider"}));
        }
    }
    out
}

/// The title, the controls and what the person should know before reading (above the file).
fn head(m: &Model<'_>) -> Vec<Value> {
    let mut col = vec![
        json!({"type": "row", "gap": "sm", "children": [
            {"type": "text", "style": "title", "text": format!("Review: {}", m.repo)},
            {"type": "text", "style": "caption", "tone": "muted", "text": "view only; comments are kept in the add-on's data"}
        ]}),
        toolbar(m),
    ];
    if !m.persistent {
        col.push(json!({"type": "text", "style": "caption", "tone": "warning", "text": "Comments are kept in memory only in this run (no data folder)."}));
    }
    if let Some(e) = m.error {
        col.push(json!({"type": "text", "tone": "danger", "text": e}));
    }
    if m.loading {
        col.push(json!({"type": "progress", "label": "Loading..."}));
    }
    col
}

fn middle(m: &Model<'_>) -> Value {
    let mut col: Vec<Value> = head(m);
    let Some(path) = m.selected else {
        col.push(json!({"type": "empty", "title": "Pick a file", "body": "Choose a file on the left to read it, then select lines to comment.", "icon": "layers"}));
        return json!({"type": "column", "gap": "xs", "children": col});
    };
    let status = m.files.iter().find(|f| f.path == path).and_then(|f| f.status);
    col.push(json!({"type": "row", "gap": "sm", "children": [
        {"type": "text", "style": "mono", "text": path},
        {"type": "text", "style": "caption", "tone": "muted", "text": match status { Some('A') => "added", Some('D') => "deleted", Some('R') => "renamed", Some('?') => "untracked", Some('M') => "modified", _ => "" }},
        button("file-comment", "Comment on file", "ghost")
    ]}));
    let loose = loose_threads(m, path);
    if !loose.is_empty() {
        col.push(json!({"type": "section", "key": "loose", "title": "File comments and outdated comments", "children": loose}));
    }
    let range = json!([{"id": "comment", "label": "+ Comment"}]);
    match m.body {
        Body::Nothing => {}
        Body::Note(n) => col.push(json!({"type": "empty", "title": n, "icon": "layers"})),
        Body::Diff { patch, truncated } => {
            if *truncated {
                col.push(json!({"type": "text", "style": "caption", "tone": "warning", "text": "The diff was cut: it is longer than this view shows."}));
            }
            let mut d = json!({"type": "diff", "key": "diff", "patch": patch, "view": if m.split { "split" } else { "unified" }, "path": path, "range_actions": range, "threads": node_threads(m, path, true)});
            if let Some(n) = m.scroll_to {
                d["scroll_to"] = json!(n);
            }
            col.push(d);
        }
        Body::Code { text, truncated } => {
            if *truncated {
                col.push(json!({"type": "text", "style": "caption", "tone": "warning", "text": "The file was cut: it is longer than this view shows."}));
            }
            let mut c = json!({"type": "code", "key": "code", "text": text, "path": path, "range_actions": range, "threads": node_threads(m, path, false)});
            if let Some(n) = m.scroll_to {
                c["scroll_to"] = json!(n);
            }
            col.push(c);
        }
    }
    json!({"type": "column", "gap": "xs", "children": col})
}

fn left(m: &Model<'_>) -> Value {
    let mut badge_of: HashMap<&str, usize> = HashMap::new();
    for tv in m.threads {
        if tv.thread.status != Status::Resolved {
            *badge_of.entry(tv.thread.anchor.path.as_str()).or_default() += 1;
        }
    }
    let files: Vec<Value> = m
        .files
        .iter()
        .take(20_000)
        .map(|f| {
            let mut o = json!({"path": f.path});
            if let Some(s) = f.status {
                o["status"] = json!(s.to_string());
            }
            if let Some(old) = &f.old_path {
                o["old_path"] = json!(old);
            }
            if let Some(n) = badge_of.get(f.path.as_str()) {
                o["badges"] = json!([{"text": n.to_string(), "tone": "warning"}]);
            }
            o
        })
        .collect();
    let mut col = vec![source_picker(m)];
    if files.is_empty() {
        let title = match m.source {
            Source::Working => "No changes",
            Source::Staged => "Nothing staged",
            Source::Files => "No files",
        };
        col.push(json!({"type": "empty", "title": title, "body": m.files_note.unwrap_or(""), "icon": "layers"}));
    } else {
        let mut f = json!({"type": "file_list", "key": "files", "view": if m.source == Source::Files { "tree" } else { "flat" }, "files": files});
        if let Some(s) = m.selected {
            f["selected"] = json!(s);
        }
        col.push(f);
        if let Some(n) = m.files_note {
            col.push(json!({"type": "text", "style": "caption", "tone": "muted", "text": n}));
        }
    }
    json!({"type": "column", "gap": "xs", "children": col})
}

fn right(m: &Model<'_>) -> Value {
    let mut sorted: Vec<&ThreadView> = m.threads.iter().collect();
    sorted.sort_by_key(|a| (status_rank(a.thread.status), std::cmp::Reverse(a.thread.updated_ms)));
    let items: Vec<Value> = sorted
        .iter()
        .take(500)
        .map(|tv| {
            let t = &tv.thread;
            let mut badges = vec![json!({"text": t.status.name(), "tone": tone_of(t.status)})];
            if tv.at == Where::Outdated {
                badges.push(json!({"text": "outdated", "tone": "danger"}));
            }
            if t.comments.last().is_some_and(|c| c.author.is_agent()) {
                badges.push(json!({"text": "agent", "tone": "accent"}));
            }
            json!({"key": t.id, "title": place(t, tv.at), "subtitle": one_line(t.comments.first().map(|c| c.body.as_str()).unwrap_or_default(), 90), "badges": badges})
        })
        .collect();
    let open = m.threads.iter().filter(|t| t.thread.status == Status::Open).count();
    let mut col = Vec::new();
    if items.is_empty() {
        col.push(json!({"type": "empty", "title": "No comments yet", "body": "Select lines in a file, then press + Comment.", "icon": "layers"}));
    } else {
        col.push(json!({"type": "list", "key": "threads", "items": items}));
    }
    json!({"type": "section", "title": "Threads", "badge": open.to_string(), "children": col})
}

fn source_picker(m: &Model<'_>) -> Value {
    json!({"type": "segmented", "key": "source", "selected": m.source.name(), "options": [
        {"value": "working", "label": "Working tree"}, {"value": "staged", "label": "Staged"}, {"value": "files", "label": "Files"}]})
}

fn toolbar(m: &Model<'_>) -> Value {
    let mut kids = Vec::new();
    if m.source != Source::Files {
        kids.push(json!({"type": "segmented", "key": "mode", "selected": if m.split { "split" } else { "unified" }, "options": [
            {"value": "unified", "label": "Unified"}, {"value": "split", "label": "Split"}]}));
    }
    if m.sessions.len() > 1 {
        let opts: Vec<Value> = m.sessions.iter().take(50).map(|(id, name)| json!({"value": id, "label": name})).collect();
        kids.push(json!({"type": "select", "key": "session", "selected": m.session_id.unwrap_or(""), "options": opts}));
    }
    kids.push(button("refresh", "Refresh", "ghost"));
    match (m.session_id, m.prompt_all.is_empty()) {
        (Some(sid), false) => kids.push(action("Send to agent", "primary", "session_send_text", &json!({"session_id": sid, "text": m.prompt_all, "submit": false}))),
        _ => kids.push(json!({"type": "button", "label": "Send to agent", "kind": "primary", "disabled": true})),
    }
    json!({"type": "row", "gap": "sm", "children": kids})
}

/// The whole screen.
#[must_use]
pub fn build(m: &Model<'_>) -> Value {
    // The root is a split: a screen whose root is a column scrolls as a whole, which would give
    // the diff a fixed height instead of the room the window has.
    json!({"type": "split", "key": "cols", "ratio": 0.22, "children": [
        left(m),
        {"type": "split", "key": "rest", "ratio": 0.72, "children": [middle(m), right(m)]}
    ]})
}

/// A tree shown before the first data arrives, and when there is nothing to show.
#[must_use]
pub fn note(title: &str, body: &str) -> Value {
    json!({"type": "column", "key": "root", "children": [{"type": "empty", "title": title, "body": body, "icon": "layers"}]})
}
