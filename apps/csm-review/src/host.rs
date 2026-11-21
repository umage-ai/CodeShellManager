//! The add-on's process: the JSON-RPC loop, the screen, and the tools.
//!
//! One thread does everything. CSM's answers to `csm/call` are read inline (messages that arrive
//! meanwhile wait in a queue), so no state is shared and the store has exactly one writer.
//! Everything from CSM (event values, caller, git output, file text) is treated as untrusted:
//! no indexing, no unwrap, caps on every list and string.

// serde_json::Value indexing reads Null for an absent key, and every write here is to an object this module built.
#![allow(clippy::indexing_slicing)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, Lines, StdinLock, Write};
use std::path::PathBuf;

use serde_json::{Value, json};

use crate::anchor::{self, Anchor, Location};
use crate::prompt;
use crate::store::{Author, Status, Store, Thread};
use crate::view::{self, Body, Draft, FileRow, Model, Source, ThreadView, Where};

/// One file's text or one patch asked of git: well under the 1 MiB a `csm/call` answer may be
/// once JSON escaping has had its way with it.
const READ_BYTES: usize = 300 * 1024;
const MAX_SCREENS: usize = 8;
const MAX_PENDING: usize = 2000;
/// Paths whose current text a `review.list` fetches to re-anchor (the rest say `unchecked`).
const MAX_LOCATE_PATHS: usize = 25;
const MAX_FILES_SHOWN: usize = 20_000;
const MAX_LIST: usize = 200;

pub const SCREEN_ID: &str = "review.changes";

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn cut(s: &str, cap: usize) -> String {
    s.chars().take(cap).collect()
}

fn text_of<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str)
}

fn u32_of(v: &Value, k: &str) -> Option<u32> {
    v.get(k).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok())
}

/// Who is calling a tool, from the `caller` CSM puts on every `tools/call`.
#[derive(Debug, Clone, Default)]
struct Who {
    kind: String,
    client: String,
    session_id: Option<String>,
    session_name: String,
    project: Option<String>,
}

impl Who {
    fn of(params: &Value) -> Who {
        let c = params.get("caller").cloned().unwrap_or(Value::Null);
        let s = |k: &str| text_of(&c, k).map(str::to_owned);
        Who { kind: s("kind").unwrap_or_default(), client: s("client").unwrap_or_default(), session_id: s("session_id"), session_name: s("session_name").unwrap_or_default(), project: s("project") }
    }

    /// The person: the app's UI or `csm <tool>`. Only they resolve or delete.
    fn is_person(&self) -> bool {
        matches!(self.kind.as_str(), "ui" | "cli")
    }

    fn is_agent(&self) -> bool {
        self.kind == "external"
    }
}

/// A tool's refusal or failure, as the JSON-RPC error it becomes.
pub struct Fail(pub String);

impl From<crate::store::StoreError> for Fail {
    fn from(e: crate::store::StoreError) -> Fail {
        Fail(e.to_string())
    }
}

type ToolResult = Result<(String, Value), Fail>;

fn fail<T>(m: impl Into<String>) -> Result<T, Fail> {
    Err(Fail(m.into()))
}

/// One open screen.
struct Screen {
    project: String,
    root: String,
    folder: String,
    session_id: Option<String>,
    sessions: Vec<(String, String, String)>,
    source: Source,
    split: bool,
    files: Vec<FileRow>,
    untracked: HashSet<String>,
    files_note: Option<String>,
    head: String,
    selected: Option<String>,
    body: Body,
    new_text: Option<(String, String)>,
    old_text: Option<(String, String)>,
    draft: Option<Draft>,
    open: HashMap<String, bool>,
    scroll_to: Option<u32>,
    rev: u64,
    error: Option<String>,
    loading: bool,
}

/// What `locate` needs for one file: the current text and blob of a side.
type Text = (String, String);

pub struct Host {
    lines: Lines<StdinLock<'static>>,
    out: io::Stdout,
    pending: VecDeque<Value>,
    next_id: u64,
    store: Store,
    screens: HashMap<String, Screen>,
    user: String,
    eof: bool,
}

pub fn run() {
    let stdin = io::stdin();
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).map(|u| cut(&u.chars().filter(|c| !c.is_control()).collect::<String>(), 40)).ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| "Reviewer".into());
    let mut host = Host { lines: stdin.lock().lines(), out: io::stdout(), pending: VecDeque::new(), next_id: 1000, store: Store::new(None), screens: HashMap::new(), user, eof: false };
    while !host.eof {
        let Some(msg) = host.next_message() else { break };
        host.dispatch(&msg);
    }
}

impl Host {
    fn next_message(&mut self) -> Option<Value> {
        if let Some(m) = self.pending.pop_front() {
            return Some(m);
        }
        loop {
            let line = match self.lines.next() {
                Some(Ok(l)) => l,
                _ => return None,
            };
            if let Ok(v) = serde_json::from_str::<Value>(&line) {
                return Some(v);
            }
        }
    }

    fn send(&mut self, v: &Value) {
        let mut out = self.out.lock();
        if writeln!(out, "{v}").and_then(|()| out.flush()).is_err() {
            self.eof = true;
        }
    }

    fn reply(&mut self, id: &Value, result: Value) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn reply_err(&mut self, id: &Value, code: i64, message: &str) {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": cut(message, 400)}}));
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    /// A CSM tool through `csm/call`: its `data`, or why it failed.
    fn csm_call(&mut self, tool: &str, params: Value) -> Result<Value, String> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": "csm/call", "params": {"tool": tool, "params": params}}));
        loop {
            if self.eof {
                return Err("CSM closed the connection".into());
            }
            let line = match self.lines.next() {
                Some(Ok(l)) => l,
                _ => {
                    self.eof = true;
                    return Err("CSM closed the connection".into());
                }
            };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
            if msg.get("method").is_none() && msg.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(r) = msg.get("result") {
                    return Ok(r.get("data").cloned().unwrap_or(Value::Null));
                }
                let why = msg.pointer("/error/data/message").or_else(|| msg.pointer("/error/message")).and_then(Value::as_str).unwrap_or("the call failed");
                return Err(cut(why, 300));
            }
            if self.pending.len() < MAX_PENDING {
                self.pending.push_back(msg);
            }
        }
    }

    fn dispatch(&mut self, msg: &Value) {
        let method = text_of(msg, "method").unwrap_or_default().to_owned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match (method.as_str(), msg.get("id")) {
            ("initialize", Some(id)) => self.initialize(id, &params),
            ("tools/call", Some(id)) => {
                let name = text_of(&params, "name").unwrap_or_default().to_owned();
                let args = params.get("arguments").cloned().unwrap_or(Value::Null);
                let who = Who::of(&params);
                match self.tool(&name, &args, &who) {
                    Ok((message, data)) => self.reply(id, json!({"message": cut(&message, 480), "data": data})),
                    Err(Fail(m)) if m == "no such tool" => self.reply_err(id, -32601, &m),
                    Err(Fail(m)) => self.reply_err(id, -32000, &m),
                }
            }
            ("screen/open", Some(id)) => self.screen_open(id, &params),
            ("screen/event", None) => self.screen_event(&params),
            ("screen/closed", None) => {
                if let Some(i) = text_of(&params, "instance") {
                    self.screens.remove(i);
                }
            }
            ("screen/refresh", None) => {
                if let Some(i) = text_of(&params, "instance").map(str::to_owned) {
                    self.render(&i);
                }
            }
            ("screen/error", None) => eprintln!("CSM refused a view: {}", cut(text_of(&params, "message").unwrap_or_default(), 300)),
            (_, Some(id)) => self.reply_err(id, -32601, "unknown method"),
            _ => {}
        }
    }

    fn initialize(&mut self, id: &Value, params: &Value) {
        let screens_ok = params.pointer("/capabilities/screens").and_then(Value::as_u64).unwrap_or(0) >= 1;
        let dir = text_of(params, "data_dir").filter(|d| !d.is_empty()).map(PathBuf::from);
        self.store = Store::new(dir);
        let obj = |extra: Value| json!({"type": "object", "properties": extra, "additionalProperties": false});
        let project = json!({"type": "string", "description": "The project id (as the review screen and session_list give it). An agent's own project is used when it calls from a session."});
        let tid = json!({"type": "string", "description": "A thread id from review.list."});
        let tools = json!([
            {"id": "review.list", "title": "List review comments", "summary": "Review threads of a project: id, status, file, current lines (re-anchored, or outdated), first comment. Default: open and addressed.", "effects": [],
             "input_schema": obj(json!({"project": project, "status": {"type": "string", "enum": ["open", "addressed", "resolved", "all"]}, "path": {"type": "string"}, "since_ms": {"type": "integer"}}))},
            {"id": "review.get", "title": "Read a review thread", "summary": "The whole thread (every comment), where it is now (exact, moved or outdated) and the code on those lines.", "effects": [],
             "input_schema": obj(json!({"thread_id": tid, "project": project}))},
            {"id": "review.reply", "title": "Reply to a review thread", "summary": "Add a comment to a thread. An agent may set status `addressed` in the same call (\"fixed in a1b2c3\"); only the person resolves.", "effects": ["stores_data"],
             "input_schema": obj(json!({"thread_id": tid, "body": {"type": "string"}, "status": {"type": "string", "enum": ["open", "addressed", "resolved"]}, "project": project}))},
            {"id": "review.set_status", "title": "Set a thread's status", "summary": "open or addressed (an agent), or resolved (only the person).", "effects": ["stores_data"],
             "input_schema": obj(json!({"thread_id": tid, "status": {"type": "string", "enum": ["open", "addressed", "resolved"]}, "project": project}))},
            {"id": "review.create", "title": "Start a review thread", "summary": "Comment on lines `start`..`end` of a file (or on the whole file when they are left out). Marked as written by the agent when an agent calls it.", "effects": ["stores_data"],
             "input_schema": obj(json!({"path": {"type": "string"}, "start": {"type": "integer"}, "end": {"type": "integer"}, "body": {"type": "string"}, "side": {"type": "string", "enum": ["new", "old"]}, "source": {"type": "string", "enum": ["working", "staged", "file"]}, "project": project}))},
            {"id": "review.delete", "title": "Delete a review thread", "summary": "Only the person deletes a thread.", "effects": ["stores_data"],
             "input_schema": obj(json!({"thread_id": tid, "project": project}))},
            {"id": "review.open", "title": "Open the review", "summary": "Show the review screen to the person, on a thread or a file.", "effects": [],
             "input_schema": obj(json!({"thread_id": tid, "path": {"type": "string"}, "session_id": {"type": "string"}, "project": project}))},
            {"id": "review.prompt", "title": "Build the review prompt", "summary": "The text \"Send to agent\" pastes: one line per open thread and the tools to use.", "effects": [],
             "input_schema": obj(json!({"thread_ids": {"type": "array", "items": {"type": "string"}}, "project": project}))}
        ]);
        let mut answer = json!({"api": 1, "name": "Review", "tools": tools, "subscriptions": []});
        if screens_ok {
            answer["screens"] = json!([{"id": SCREEN_ID, "title": "Review", "icon": "layers", "scope": "project", "summary": "Changed files and diffs, with comment threads that agents can read and answer"}]);
        }
        self.reply(id, answer);
    }

    // ── Identity and scope ───────────────────────────────────────────────────────────────

    /// The project a call works on, and where git runs for it. An agent in one of our sessions
    /// works only in its own project; anyone else names one.
    fn scope(&mut self, who: &Who, args: &Value, thread_id: Option<&str>) -> Result<(String, Value), Fail> {
        let given = text_of(args, "project").filter(|p| !p.is_empty()).map(str::to_owned);
        let project = if who.is_agent() && who.session_id.is_some() {
            let own = who.project.clone().ok_or_else(|| Fail("CSM did not say which project your session is in".into()))?;
            if given.as_ref().is_some_and(|g| *g != own) {
                return fail("an agent works in its own project only; leave `project` out");
            }
            own
        } else if let Some(g) = given {
            g
        } else if let Some(id) = thread_id.filter(|_| who.is_person()) {
            self.store.locate_project(None, id, now_ms()).ok_or_else(|| Fail(format!("no thread `{id}`")))?
        } else {
            return fail("give `project` (the project id the review screen or session_list shows)");
        };
        let target = match (&who.session_id, who.is_agent()) {
            (Some(sid), true) => json!({"session_id": sid}),
            _ => {
                let root = project.strip_prefix("repo:").map(str::to_owned).or_else(|| self.store.root_of(&project, now_ms())).unwrap_or_default();
                if root.is_empty() { Value::Null } else { json!({"folder": root}) }
            }
        };
        Ok((project, target))
    }

    fn author(&self, who: &Who) -> Author {
        if who.is_agent() {
            let name = if who.session_name.is_empty() { who.client.clone() } else { who.session_name.clone() };
            Author { kind: "agent".into(), name: cut(&name, 80), session_id: who.session_id.clone(), client: Some(cut(&who.client, 80)).filter(|c| !c.is_empty()) }
        } else {
            Author::person(&self.user)
        }
    }

    // ── Tools ────────────────────────────────────────────────────────────────────────────

    fn tool(&mut self, name: &str, args: &Value, who: &Who) -> ToolResult {
        if who.kind == "addon" && !matches!(name, "review.list" | "review.get" | "review.prompt") {
            return fail("another add-on may read review threads, not change them");
        }
        let id_arg = text_of(args, "thread_id").map(str::to_owned);
        match name {
            "review.list" => self.tool_list(who, args),
            "review.get" => {
                let id = id_arg.ok_or_else(|| Fail("`thread_id` is required".into()))?;
                self.tool_get(who, args, &id)
            }
            "review.reply" => {
                let id = id_arg.ok_or_else(|| Fail("`thread_id` is required".into()))?;
                self.tool_reply(who, args, &id)
            }
            "review.set_status" => {
                let id = id_arg.ok_or_else(|| Fail("`thread_id` is required".into()))?;
                self.tool_set_status(who, args, &id)
            }
            "review.create" => self.tool_create(who, args),
            "review.delete" => {
                let id = id_arg.ok_or_else(|| Fail("`thread_id` is required".into()))?;
                self.tool_delete(who, args, &id)
            }
            "review.open" => self.tool_open(who, args),
            "review.prompt" => self.tool_prompt(who, args),
            _ => fail("no such tool"),
        }
    }

    /// The current text of a side of `path` in the checkout `target` names, or `None`.
    fn fetch_text(&mut self, target: &Value, path: &str, side: &str) -> Option<Text> {
        self.fetch_full(target, path, side).map(|(text, blob, _)| (text, blob))
    }

    /// Text, blob id and HEAD of a side of `path`, or `None` (no checkout, binary, unreadable).
    fn fetch_full(&mut self, target: &Value, path: &str, side: &str) -> Option<(String, String, String)> {
        if target.is_null() {
            return None;
        }
        let mut p = target.clone();
        p["path"] = json!(path);
        p["max_bytes"] = json!(READ_BYTES);
        if side == "old" {
            p["rev"] = json!("HEAD");
        }
        let d = self.csm_call("git_show_file", p).ok()?;
        if d.get("binary").and_then(Value::as_bool).unwrap_or(false) {
            return None;
        }
        Some((text_of(&d, "text")?.to_owned(), text_of(&d, "blob").unwrap_or_default().to_owned(), text_of(&d, "head").unwrap_or_default().to_owned()))
    }

    /// Where each thread is now. Fetches the text of at most [`MAX_LOCATE_PATHS`] files.
    fn locate_all(&mut self, target: &Value, threads: &[Thread]) -> HashMap<String, Where> {
        let mut cache: HashMap<(String, String), Option<Text>> = HashMap::new();
        let mut out = HashMap::new();
        for t in threads {
            if t.anchor.kind == "file" {
                out.insert(t.id.clone(), Where::Elsewhere);
                continue;
            }
            let key = (t.anchor.path.clone(), t.anchor.side.clone());
            if !cache.contains_key(&key) {
                if cache.len() >= MAX_LOCATE_PATHS {
                    out.insert(t.id.clone(), Where::Elsewhere);
                    continue;
                }
                let text = self.fetch_text(target, &key.0, &key.1);
                cache.insert(key.clone(), text);
            }
            let w = match cache.get(&key).and_then(Option::as_ref) {
                Some((text, blob)) => to_where(anchor::locate(&t.anchor, blob, text)),
                None => Where::Elsewhere,
            };
            out.insert(t.id.clone(), w);
        }
        out
    }

    fn thread_json(t: &Thread, at: Where, full: bool) -> Value {
        let last = t.comments.last().map(|c| json!({"kind": c.author.kind, "name": c.author.name}));
        let (state, lines) = match at {
            Where::Exact(s, e) => ("exact", Some((s, e))),
            Where::Moved(s, e) => ("moved", Some((s, e))),
            Where::Outdated => ("outdated", None),
            Where::Elsewhere => ("unchecked", Some((t.anchor.start, t.anchor.end))),
        };
        let mut o = json!({
            "id": t.id, "status": t.status.name(), "path": t.anchor.path, "kind": t.anchor.kind, "side": t.anchor.side,
            "location": {"state": state, "start": lines.map(|l| l.0), "end": lines.map(|l| l.1)},
            "snippet": cut(&t.anchor.snippet, 600), "commit": t.anchor.commit,
            "comment_count": t.comments.len(), "last_author": last, "created_ms": t.created_ms, "updated_ms": t.updated_ms,
        });
        if full {
            o["comments"] = json!(t.comments.iter().map(|c| json!({"id": c.id, "author": c.author, "body": c.body, "created_ms": c.created_ms})).collect::<Vec<_>>());
        } else {
            o["first_comment"] = json!(t.comments.first().map(|c| cut(&c.body, 300)));
        }
        o
    }

    fn tool_list(&mut self, who: &Who, args: &Value) -> ToolResult {
        let (project, target) = self.scope(who, args, None)?;
        let want = text_of(args, "status").unwrap_or("");
        let status_ok = |s: Status| match want {
            "" => s != Status::Resolved,
            "all" => true,
            w => Status::parse(w) == Some(s),
        };
        if !matches!(want, "" | "all" | "open" | "addressed" | "resolved") {
            return fail("`status` is open, addressed, resolved or all");
        }
        let path = text_of(args, "path").map(clean_path).transpose()?;
        let since = args.get("since_ms").and_then(Value::as_u64).unwrap_or(0);
        let mut threads: Vec<Thread> = self.store.threads(&project, now_ms()).iter().filter(|t| status_ok(t.status) && t.updated_ms >= since && path.as_ref().is_none_or(|p| *p == t.anchor.path)).cloned().collect();
        threads.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
        let total = threads.len();
        threads.truncate(MAX_LIST);
        let at = self.locate_all(&target, &threads);
        let list: Vec<Value> = threads.iter().map(|t| Self::thread_json(t, at.get(&t.id).copied().unwrap_or(Where::Elsewhere), false)).collect();
        Ok((format!("{total} thread(s)."), json!({"project": project, "threads": list, "total": total})))
    }

    fn tool_get(&mut self, who: &Who, args: &Value, id: &str) -> ToolResult {
        let (project, target) = self.scope(who, args, Some(id))?;
        let t = self.store.thread(&project, id, now_ms()).cloned().ok_or_else(|| Fail(format!("no thread `{id}`")))?;
        let at = self.locate_all(&target, std::slice::from_ref(&t)).get(id).copied().unwrap_or(Where::Elsewhere);
        let mut data = Self::thread_json(&t, at, true);
        if t.anchor.kind == "lines"
            && let Some((s, e)) = at.range()
            && let Some((text, _)) = self.fetch_text(&target, &t.anchor.path, &t.anchor.side)
        {
            let lines = anchor::split_lines(&text);
            let (s0, e0) = (usize::try_from(s).unwrap_or(1).saturating_sub(1), usize::try_from(e).unwrap_or(0));
            let now = lines.get(s0..e0.min(lines.len())).unwrap_or(&[]).join("\n");
            data["current_lines"] = json!(cut(&now, 4096));
        }
        Ok((format!("{} on {} ({}).", t.status.name(), t.anchor.path, data["location"]["state"].as_str().unwrap_or("")), data))
    }

    fn tool_reply(&mut self, who: &Who, args: &Value, id: &str) -> ToolResult {
        let (project, _) = self.scope(who, args, Some(id))?;
        let status = text_of(args, "status").map(|s| Status::parse(s).ok_or_else(|| Fail("`status` is open, addressed or resolved".into()))).transpose()?;
        if status == Some(Status::Resolved) && !who.is_person() {
            return fail("only the person resolves a thread; reply with status `addressed` and they will check it");
        }
        if status.is_some() && !who.is_person() && self.store.thread(&project, id, now_ms()).is_some_and(|t| t.status == Status::Resolved) {
            return fail("the person resolved this thread; only they change its status (a reply without `status` reopens it)");
        }
        let author = self.author(who);
        let body = text_of(args, "body").unwrap_or_default();
        let t = self.store.reply(&project, id, body, author, status, now_ms())?;
        self.changed(&project);
        Ok((format!("Replied; the thread is {}.", t.status.name()), json!({"thread_id": t.id, "status": t.status.name(), "comments": t.comments.len()})))
    }

    fn tool_set_status(&mut self, who: &Who, args: &Value, id: &str) -> ToolResult {
        let (project, _) = self.scope(who, args, Some(id))?;
        let status = text_of(args, "status").and_then(Status::parse).ok_or_else(|| Fail("`status` is open, addressed or resolved".into()))?;
        if status == Status::Resolved && !who.is_person() {
            return fail("only the person resolves a thread; mark it `addressed` and they will check it");
        }
        if !who.is_person() && self.store.thread(&project, id, now_ms()).is_some_and(|t| t.status == Status::Resolved) {
            return fail("the person resolved this thread; only they change its status");
        }
        let t = self.store.set_status(&project, id, status, now_ms())?;
        self.changed(&project);
        Ok((format!("The thread is {}.", t.status.name()), json!({"thread_id": t.id, "status": t.status.name()})))
    }

    fn tool_delete(&mut self, who: &Who, args: &Value, id: &str) -> ToolResult {
        if !who.is_person() {
            return fail("only the person deletes a thread");
        }
        let (project, _) = self.scope(who, args, Some(id))?;
        self.store.delete(&project, id, now_ms())?;
        self.changed(&project);
        Ok(("Deleted.".into(), json!({"thread_id": id})))
    }

    fn tool_create(&mut self, who: &Who, args: &Value) -> ToolResult {
        let (project, target) = self.scope(who, args, None)?;
        let path = clean_path(text_of(args, "path").unwrap_or_default())?;
        let side = match text_of(args, "side").unwrap_or("new") {
            s @ ("new" | "old") => s.to_owned(),
            _ => return fail("`side` is new or old"),
        };
        let source = match text_of(args, "source").unwrap_or("working") {
            s @ ("working" | "staged" | "file") => s.to_owned(),
            _ => return fail("`source` is working, staged or file"),
        };
        let (start, end) = (u32_of(args, "start"), u32_of(args, "end"));
        let body = text_of(args, "body").unwrap_or_default().to_owned();
        let author = self.author(who);
        let worktree = target.get("folder").and_then(Value::as_str).unwrap_or_default().to_owned();
        let root = project.strip_prefix("repo:").unwrap_or(&worktree).to_owned();
        let anchor = match (start, end) {
            (None, None) => anchor::file_anchor(&path, "file", "", &worktree),
            (Some(s), e) => {
                let e = e.unwrap_or(s);
                let text = self.fetch_full(&target, &path, &side).ok_or_else(|| Fail(format!("cannot read `{path}` to anchor the comment (is it a text file in the repository?)")))?;
                anchor::lines_anchor(&path, &side, &source, &text.2, &worktree, &text.1, &text.0, s, e).ok_or_else(|| Fail(format!("lines {s}-{e} are not in `{path}`")))?
            }
            (None, Some(_)) => return fail("give `start` too, or leave both out for a comment on the whole file"),
        };
        let t = self.store.create(&project, &root, anchor, &body, author, now_ms())?;
        self.changed(&project);
        Ok((format!("Started thread {} on {}.", t.id, path), json!({"thread_id": t.id, "status": "open"})))
    }

    fn tool_prompt(&mut self, who: &Who, args: &Value) -> ToolResult {
        let (project, target) = self.scope(who, args, None)?;
        let ids: Option<HashSet<String>> = args.get("thread_ids").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_owned)).take(200).collect());
        let threads: Vec<Thread> = self.store.threads(&project, now_ms()).iter().filter(|t| ids.as_ref().map_or(t.status == Status::Open, |s| s.contains(&t.id))).cloned().collect();
        let at = self.locate_all(&target, &threads);
        let lines: Vec<prompt::Line> = threads.iter().map(|t| line_of(t, at.get(&t.id).copied().unwrap_or(Where::Elsewhere))).collect();
        let repo = project.strip_prefix("repo:").map(repo_name).unwrap_or_default();
        let text = prompt::build(&repo, "open", &lines);
        Ok((if text.is_empty() { "Nothing to send.".into() } else { format!("A prompt for {} thread(s).", lines.len()) }, json!({"text": text, "count": lines.len()})))
    }

    fn tool_open(&mut self, who: &Who, args: &Value) -> ToolResult {
        let id = text_of(args, "thread_id").map(str::to_owned);
        let (project, _) = self.scope(who, args, id.as_deref())?;
        let mut ctx = json!({"project": project});
        let sid = who.session_id.clone().filter(|_| who.is_agent()).or_else(|| text_of(args, "session_id").map(str::to_owned));
        if let Some(s) = sid {
            ctx["session_id"] = json!(s);
        }
        let mut a = serde_json::Map::new();
        if let Some(i) = &id {
            a.insert("thread_id".into(), json!(i));
        }
        if let Some(p) = text_of(args, "path") {
            a.insert("path".into(), json!(clean_path(p)?));
        }
        ctx["args"] = Value::Object(a);
        let d = self.csm_call("screen_open", json!({"screen": SCREEN_ID, "context": ctx, "focus": true})).map_err(Fail)?;
        Ok(("Opened the review.".into(), json!({"instance": d.get("instance")})))
    }

    // ── Screen ───────────────────────────────────────────────────────────────────────────

    /// Something about a project's threads changed: show it in every open screen of the project.
    fn changed(&mut self, project: &str) {
        let ids: Vec<String> = self.screens.iter().filter(|(_, s)| s.project == project).map(|(i, _)| i.clone()).collect();
        for i in ids {
            self.render(&i);
        }
    }

    fn screen_open(&mut self, id: &Value, params: &Value) {
        let instance = text_of(params, "instance").unwrap_or_default().to_owned();
        let ctx = params.get("context").cloned().unwrap_or(Value::Null);
        let folder = text_of(&ctx, "folder").unwrap_or_default().to_owned();
        let session_id = text_of(&ctx, "session_id").map(str::to_owned);
        let Some(project) = text_of(&ctx, "project").filter(|p| !p.is_empty()).map(str::to_owned) else {
            self.reply(id, json!({"rev": 1, "root": view::note("Open the review on a session", "The review needs a session in a repository: use Open Review from a session.")}));
            return;
        };
        if self.screens.len() >= MAX_SCREENS && !self.screens.contains_key(&instance) {
            self.reply_err(id, -32000, "too many review screens are open");
            return;
        }
        let root = project.strip_prefix("repo:").map(str::to_owned).unwrap_or_else(|| folder.clone());
        let screen = Screen {
            project,
            root,
            folder,
            session_id,
            sessions: Vec::new(),
            source: Source::Working,
            split: false,
            files: Vec::new(),
            untracked: HashSet::new(),
            files_note: None,
            head: String::new(),
            selected: None,
            body: Body::Nothing,
            new_text: None,
            old_text: None,
            draft: None,
            open: HashMap::new(),
            scroll_to: None,
            rev: 1,
            error: None,
            loading: true,
        };
        self.screens.insert(instance.clone(), screen);
        self.reply(id, json!({"rev": 1, "root": view::note("Loading the review...", "Reading the changed files.")}));
        let Some(mut s) = self.screens.remove(&instance) else { return };
        self.load_sessions(&mut s);
        self.load_files(&mut s);
        let deep = ctx.get("args").cloned().unwrap_or(Value::Null);
        self.deep_link(&mut s, &deep);
        if s.selected.is_some() {
            self.load_body(&mut s);
        }
        s.loading = false;
        self.screens.insert(instance.clone(), s);
        self.render(&instance);
    }

    /// A thread or a path the screen was asked to open on.
    fn deep_link(&mut self, s: &mut Screen, args: &Value) {
        if let Some(tid) = text_of(args, "thread_id") {
            let found = self.store.thread(&s.project, tid, now_ms()).cloned();
            if let Some(t) = found {
                self.goto_thread(s, &t);
                return;
            }
        }
        if let Some(p) = text_of(args, "path").and_then(|p| clean_path(p).ok()) {
            if !s.files.iter().any(|f| f.path == p) {
                s.source = Source::Files;
                self.load_files(s);
            }
            if s.files.iter().any(|f| f.path == p) {
                s.selected = Some(p);
                self.load_body(s);
            }
        }
    }

    fn goto_thread(&mut self, s: &mut Screen, t: &Thread) {
        let path = t.anchor.path.clone();
        if !s.files.iter().any(|f| f.path == path) {
            s.source = Source::Files;
            self.load_files(s);
        }
        s.selected = Some(path);
        s.draft = None;
        s.open.insert(t.id.clone(), true);
        self.load_body(s);
        s.scroll_to = self.thread_views(s).iter().find(|v| v.thread.id == t.id).and_then(|v| v.at.range()).map(|r| r.0).or(Some(t.anchor.start).filter(|n| *n > 0));
    }

    fn target_of(s: &Screen) -> Value {
        match &s.session_id {
            Some(sid) => json!({"session_id": sid}),
            None => json!({"folder": s.folder}),
        }
    }

    fn load_sessions(&mut self, s: &mut Screen) {
        let Ok(d) = self.csm_call("session_list", json!({"include_asleep": false})) else { return };
        let repo = s.root.clone();
        let list: Vec<(String, String, String)> = d
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| v.get("asleep").and_then(Value::as_bool) != Some(true) && text_of(v, "kind") != Some("ssh"))
            .filter(|v| text_of(v, "repo_root").is_some_and(|r| r == repo) || text_of(v, "id") == s.session_id.as_deref())
            .filter_map(|v| Some((text_of(v, "id")?.to_owned(), cut(text_of(v, "name").unwrap_or("session"), 60), text_of(v, "folder").unwrap_or_default().to_owned())))
            .take(50)
            .collect();
        if s.folder.is_empty()
            && let Some(f) = list.iter().find(|(i, _, _)| Some(i) == s.session_id.as_ref()).map(|(_, _, f)| f.clone())
        {
            s.folder = f;
        }
        s.sessions = list;
    }

    fn load_files(&mut self, s: &mut Screen) {
        s.error = None;
        s.files_note = None;
        s.untracked.clear();
        let target = Self::target_of(s);
        let result = match s.source {
            Source::Files => self.csm_call("git_ls_files", target).map(|d| {
                let rows: Vec<FileRow> = d["files"].as_array().into_iter().flatten().filter_map(|f| f.as_str()).take(MAX_FILES_SHOWN).map(|p| FileRow { path: p.to_owned(), status: None, old_path: None }).collect();
                (rows, d["truncated"].as_bool().unwrap_or(false), String::new(), Vec::new())
            }),
            Source::Working | Source::Staged => {
                let mut p = target;
                p["source"] = json!(s.source.name());
                p["patch"] = json!(false);
                self.csm_call("git_diff", p).map(|d| {
                    let mut rows: Vec<FileRow> = d["files"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|f| Some(FileRow { path: text_of(f, "path")?.to_owned(), status: text_of(f, "status").and_then(|x| x.chars().next()), old_path: text_of(f, "old_path").map(str::to_owned) }))
                        .collect();
                    let untracked: Vec<String> = d["untracked"].as_array().into_iter().flatten().filter_map(|f| f.as_str().map(str::to_owned)).collect();
                    for u in &untracked {
                        rows.push(FileRow { path: u.clone(), status: Some('?'), old_path: None });
                    }
                    rows.truncate(MAX_FILES_SHOWN);
                    (rows, d["files_truncated"].as_bool().unwrap_or(false), text_of(&d, "head").unwrap_or_default().to_owned(), untracked)
                })
            }
        };
        match result {
            Ok((rows, truncated, head, untracked)) => {
                s.files = rows;
                s.head = head;
                s.untracked = untracked.into_iter().collect();
                if truncated {
                    s.files_note = Some("The list was cut.".into());
                }
            }
            Err(e) => {
                s.files.clear();
                s.error = Some(format!("Could not read the repository: {}", cut(&e, 300)));
            }
        }
        if s.selected.as_ref().is_some_and(|p| !s.files.iter().any(|f| &f.path == p)) {
            s.selected = None;
            s.body = Body::Nothing;
            s.new_text = None;
            s.old_text = None;
            s.draft = None;
        }
    }

    fn load_body(&mut self, s: &mut Screen) {
        s.new_text = None;
        s.old_text = None;
        s.scroll_to = None;
        let Some(path) = s.selected.clone() else {
            s.body = Body::Nothing;
            return;
        };
        let row = s.files.iter().find(|f| f.path == path).cloned();
        let status = row.as_ref().and_then(|r| r.status);
        let target = Self::target_of(s);
        let as_code = s.source == Source::Files || status == Some('?');
        if as_code {
            let mut p = target;
            p["path"] = json!(path);
            p["max_bytes"] = json!(READ_BYTES);
            s.body = match self.csm_call("git_show_file", p) {
                Ok(d) if d["binary"].as_bool() == Some(true) => Body::Note("A binary file is not shown.".into()),
                Ok(d) => {
                    let text = text_of(&d, "text").unwrap_or_default().to_owned();
                    s.new_text = Some((text.clone(), text_of(&d, "blob").unwrap_or_default().to_owned()));
                    Body::Code { text, truncated: d["truncated"].as_bool().unwrap_or(false) }
                }
                Err(e) => Body::Note(format!("Could not read the file: {}", cut(&e, 200))),
            };
            return;
        }
        let mut p = target.clone();
        p["source"] = json!(s.source.name());
        let mut paths = vec![json!(path)];
        if let Some(old) = row.and_then(|r| r.old_path) {
            paths.push(json!(old));
        }
        p["paths"] = json!(paths);
        p["max_bytes"] = json!(READ_BYTES);
        s.body = match self.csm_call("git_diff", p) {
            Ok(d) => {
                let patch = text_of(&d, "patch").unwrap_or_default().to_owned();
                if patch.trim().is_empty() { Body::Note("No textual changes in this file.".into()) } else { Body::Diff { patch, truncated: d["truncated"].as_bool().unwrap_or(false) } }
            }
            Err(e) => Body::Note(format!("Could not read the diff: {}", cut(&e, 200))),
        };
        if status != Some('D') {
            let mut q = target;
            q["path"] = json!(path);
            q["max_bytes"] = json!(READ_BYTES);
            if s.source == Source::Staged {
                q["rev"] = json!(":0");
            }
            if let Ok(d) = self.csm_call("git_show_file", q)
                && d["binary"].as_bool() != Some(true)
            {
                s.new_text = Some((text_of(&d, "text").unwrap_or_default().to_owned(), text_of(&d, "blob").unwrap_or_default().to_owned()));
            }
        }
        let has_old = self.store.threads(&s.project, now_ms()).iter().any(|t| t.anchor.path == path && t.anchor.side == "old");
        if has_old && status != Some('A') {
            self.load_old(s, &path);
        }
    }

    /// HEAD's text of the selected file, for old-side anchors.
    fn load_old(&mut self, s: &mut Screen, path: &str) {
        let old_path = s.files.iter().find(|f| f.path == path).and_then(|f| f.old_path.clone()).unwrap_or_else(|| path.to_owned());
        let target = Self::target_of(s);
        s.old_text = self.fetch_text(&target, &old_path, "old");
    }

    fn thread_views(&mut self, s: &Screen) -> Vec<ThreadView> {
        let repo = repo_name(&s.root);
        let threads: Vec<Thread> = self.store.threads(&s.project, now_ms()).to_vec();
        threads
            .into_iter()
            .map(|t| {
                let on_file = s.selected.as_deref() == Some(t.anchor.path.as_str());
                let at = if !on_file || t.anchor.kind == "file" {
                    Where::Elsewhere
                } else {
                    let text = if t.anchor.side == "old" { s.old_text.as_ref() } else { s.new_text.as_ref() };
                    match text {
                        Some((text, blob)) => to_where(anchor::locate(&t.anchor, blob, text)),
                        None if t.anchor.side == "new" && matches!(s.body, Body::Diff { .. } | Body::Note(_)) && s.files.iter().any(|f| Some(&f.path) == s.selected.as_ref() && f.status == Some('D')) => Where::Outdated,
                        None => Where::Elsewhere,
                    }
                };
                let prompt = if s.session_id.is_some() { prompt::build(&repo, t.status.name(), &[line_of(&t, at)]) } else { String::new() };
                ThreadView { thread: t, at, prompt }
            })
            .collect()
    }

    fn render(&mut self, instance: &str) {
        let Some(mut s) = self.screens.remove(instance) else { return };
        let views = self.thread_views(&s);
        let repo = repo_name(&s.root);
        let open_lines: Vec<prompt::Line> = views.iter().filter(|v| v.thread.status == Status::Open).map(|v| line_of(&v.thread, v.at)).collect();
        let prompt_all = if s.session_id.is_some() { prompt::build(&repo, "open", &open_lines) } else { String::new() };
        let session_pairs: Vec<(String, String)> = s.sessions.iter().map(|(i, n, _)| (i.clone(), n.clone())).collect();
        let persistent = self.store.persistent();
        s.rev += 1;
        let model = Model {
            repo: &repo,
            session_id: s.session_id.as_deref(),
            sessions: &session_pairs,
            project: &s.project,
            source: s.source,
            split: s.split,
            files: &s.files,
            files_note: s.files_note.as_deref(),
            untracked: &s.untracked,
            selected: s.selected.as_deref(),
            body: &s.body,
            threads: &views,
            draft: s.draft.as_ref(),
            open: &s.open,
            scroll_to: s.scroll_to,
            loading: s.loading,
            error: s.error.as_deref(),
            prompt_all: &prompt_all,
            persistent,
        };
        let mut root = view::build(&model);
        if serde_json::to_string(&root).map_or(true, |t| t.len() > 700_000) {
            root = view::note("Too much to show at once", "Pick a smaller file, or narrow the changes.");
        }
        let rev = s.rev;
        self.screens.insert(instance.to_owned(), s);
        self.notify("screen/render", json!({"instance": instance, "rev": rev, "root": root}));
    }

    fn screen_event(&mut self, p: &Value) {
        let Some(instance) = text_of(p, "instance").map(str::to_owned) else { return };
        let Some(mut s) = self.screens.remove(&instance) else { return };
        let key = text_of(p, "key").unwrap_or_default().to_owned();
        let kind = text_of(p, "kind").unwrap_or_default().to_owned();
        let value = p.get("value").cloned().unwrap_or(Value::Null);
        s.error = None;
        self.handle(&mut s, &key, &kind, &value);
        self.screens.insert(instance.clone(), s);
        self.render(&instance);
    }

    fn handle(&mut self, s: &mut Screen, key: &str, kind: &str, value: &Value) {
        match (key, kind) {
            ("source", "select") => {
                if let Some(src) = text_of(value, "value").and_then(Source::parse) {
                    s.source = src;
                    s.draft = None;
                    self.load_files(s);
                    self.load_body(s);
                }
            }
            ("mode", "select") => s.split = text_of(value, "value") == Some("split"),
            ("session", "select") => {
                if let Some((id, _, folder)) = text_of(value, "value").and_then(|v| s.sessions.iter().find(|(i, _, _)| i == v)).cloned() {
                    s.session_id = Some(id);
                    if !folder.is_empty() {
                        s.folder = folder;
                    }
                    self.load_files(s);
                    self.load_body(s);
                }
            }
            ("refresh", "click") => {
                self.load_sessions(s);
                self.load_files(s);
                self.load_body(s);
            }
            ("files", "select") => {
                if let Some(p) = text_of(value, "path").and_then(|p| clean_path(p).ok()).filter(|p| s.files.iter().any(|f| &f.path == p)) {
                    s.selected = Some(p);
                    s.draft = None;
                    self.load_body(s);
                }
            }
            ("threads", "select") => {
                if let Some(t) = text_of(value, "key").and_then(|k| self.store.thread(&s.project, k, now_ms()).cloned()) {
                    self.goto_thread(s, &t);
                }
            }
            (_, "thread_toggle") => {
                if let Some(k) = text_of(value, "key") {
                    let now = s.open.get(k).copied().or_else(|| self.store.thread(&s.project, k, now_ms()).map(|t| t.status != Status::Resolved)).unwrap_or(true);
                    s.open.insert(k.to_owned(), !now);
                }
            }
            (_, "range_action") => {
                let (Some(path), Some(start), Some(end)) = (s.selected.clone(), u32_of(value, "start"), u32_of(value, "end")) else { return };
                let side = if text_of(value, "side") == Some("old") { "old" } else { "new" };
                s.draft = Some(Draft { path, side: side.into(), start: start.min(end), end: start.max(end), whole_file: false });
            }
            ("file-comment", "click") => {
                if let Some(path) = s.selected.clone() {
                    s.draft = Some(Draft { path, side: "new".into(), start: 0, end: 0, whole_file: true });
                }
            }
            ("draft-cancel", "click") => s.draft = None,
            ("draft-input", "submit") => self.submit_draft(s, text_of(value, "value").unwrap_or_default()),
            (k, "submit") if k.starts_with("reply-") => {
                let tid = k.strip_prefix("reply-").and_then(|r| r.rsplit_once('-')).map(|(t, _)| t.to_owned()).unwrap_or_default();
                let author = Author::person(&self.user);
                match self.store.reply(&s.project, &tid, text_of(value, "value").unwrap_or_default(), author, None, now_ms()) {
                    Ok(_) => {
                        s.open.insert(tid, true);
                    }
                    Err(e) => s.error = Some(e.to_string()),
                }
            }
            _ => {}
        }
    }

    fn submit_draft(&mut self, s: &mut Screen, body: &str) {
        let Some(d) = s.draft.clone() else { return };
        let source = match (s.source, s.untracked.contains(&d.path)) {
            (Source::Files, _) => "file",
            (Source::Staged, _) => "staged",
            (Source::Working, _) => "working",
        };
        let worktree = s.folder.clone();
        let a = if d.whole_file {
            anchor::file_anchor(&d.path, source, &s.head, &worktree)
        } else {
            if d.side == "old" && s.old_text.is_none() {
                let path = d.path.clone();
                self.load_old(s, &path);
            }
            let text = if d.side == "old" { s.old_text.as_ref() } else { s.new_text.as_ref() };
            let built = text.and_then(|(t, b)| anchor::lines_anchor(&d.path, &d.side, source, &s.head, &worktree, b, t, d.start, d.end));
            // Without the text (a cut file, a deleted one) the comment is kept by its numbers.
            built.unwrap_or(Anchor { kind: "lines".into(), path: d.path.clone(), worktree: worktree.clone(), source: source.into(), commit: s.head.clone(), side: d.side.clone(), start: d.start, end: d.end, ..Anchor::default() })
        };
        let author = Author::person(&self.user);
        match self.store.create(&s.project, &s.root, a, body, author, now_ms()) {
            Ok(t) => {
                s.draft = None;
                s.open.insert(t.id, true);
            }
            Err(e) => s.error = Some(e.to_string()),
        }
    }
}

fn to_where(l: Location) -> Where {
    match l {
        Location::Exact { start, end } => Where::Exact(start, end),
        Location::Moved { start, end } => Where::Moved(start, end),
        Location::Outdated => Where::Outdated,
    }
}

fn line_of(t: &Thread, at: Where) -> prompt::Line {
    prompt::Line::of(t, at.range().filter(|_| t.anchor.kind == "lines").or(Some((t.anchor.start, t.anchor.end)).filter(|r| r.0 > 0 && at != Where::Outdated)), at == Where::Outdated)
}

/// The repository's folder name.
fn repo_name(root: &str) -> String {
    let leaf = root.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(root);
    cut(&leaf.chars().filter(|c| !c.is_control()).collect::<String>(), 80)
}

/// A repository-relative path as a tool argument: forward slashes, no `..`, not absolute.
pub fn clean_path(p: &str) -> Result<String, Fail> {
    let p = p.replace('\\', "/");
    if p.is_empty() || p.len() > 4096 || p.chars().any(char::is_control) || p.starts_with('/') || p.as_bytes().get(1) == Some(&b':') {
        return fail("a path is relative to the repository root, with forward slashes");
    }
    let mut parts = Vec::new();
    for part in p.split('/') {
        match part {
            "" | "." => {}
            ".." => return fail("a path cannot contain `..`"),
            x => parts.push(x),
        }
    }
    if parts.is_empty() {
        return fail("a path names no file");
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_checked_and_normalised() {
        assert_eq!(clean_path("src\\a/./b.rs").ok().as_deref(), Some("src/a/b.rs"));
        for bad in ["", "/etc", "C:/x", "../x", "a/../../b", "a\nb", "."] {
            assert!(clean_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_caller_decides_who_may_resolve() {
        let ui = Who::of(&json!({"caller": {"kind": "ui"}}));
        let cli = Who::of(&json!({"caller": {"kind": "cli"}}));
        let agent = Who::of(&json!({"caller": {"kind": "external", "client": "claude-code", "session_id": "s1", "project": "repo:C:/x"}}));
        let addon = Who::of(&json!({"caller": {"kind": "addon", "addon": "other"}}));
        let none = Who::of(&json!({}));
        assert!(ui.is_person() && cli.is_person());
        assert!(!agent.is_person() && agent.is_agent());
        assert!(!addon.is_person() && !addon.is_agent());
        assert!(!none.is_person() && !none.is_agent(), "no caller is nobody");
    }

    #[test]
    fn repo_names() {
        assert_eq!(repo_name("C:\\code\\shop"), "shop");
        assert_eq!(repo_name("/home/a/shop/"), "shop");
    }
}
