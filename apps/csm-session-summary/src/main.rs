//! A sample process add-on (docs/addons.md, "Process add-ons"): one tool, `session-summary.summarise`.
//!
//! It shows the process side end to end: the `initialize` handshake, a declared tool, and a tool
//! call that calls back into the app with `csm/call` (`session_list`) and answers with a short
//! text summary: counts by status, which sessions need you, and the sessions grouped by repo.
//!
//! Honest effects: `session_list` is read-only and returns no terminal text, so both the add-on
//! and its tool declare `effects = []`. The program reads stdin and writes stdout only; it opens
//! no file and no socket. It is a console program that the host starts with no window.
//!
//! Every value from the host is treated as untrusted: no indexing, no unwrap, and an answer of
//! an unexpected shape produces an error reply, never a panic.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

use std::collections::{BTreeMap, VecDeque};
use std::io::{self, BufRead, Write};

use serde_json::{Value, json};

const TOOL: &str = "session-summary.summarise";
/// The host cuts a tool's `message` at 500 characters; stay under it so nothing is lost.
const MESSAGE_CAP: usize = 480;
/// Statuses that mean the person has something to do, in the order they are reported.
const NEEDS_YOU: &[&str] = &["needs_you_approval", "needs_you_question", "done_unseen", "error"];

/// One session, reduced to what the summary uses.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    name: String,
    status: String,
    repo: String,
    asleep: bool,
}

/// The repo a session belongs to: the main working tree's folder name, else the sidebar subtitle.
fn repo_of(s: &Value) -> String {
    let text = |k: &str| s.get(k).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty());
    if let Some(root) = text("repo_root") {
        let leaf = root.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().unwrap_or(root);
        if !leaf.is_empty() {
            return leaf.to_owned();
        }
    }
    text("folder_short").unwrap_or("(no folder)").to_owned()
}

/// Reads the `data` of a `session_list` answer. Entries that are not objects are skipped.
fn rows(data: &Value) -> Vec<Row> {
    data.as_array()
        .into_iter()
        .flatten()
        .filter(|s| s.is_object())
        .map(|s| Row {
            name: s.get("name").and_then(Value::as_str).unwrap_or("(unnamed)").to_owned(),
            status: s.get("status").and_then(Value::as_str).unwrap_or("unknown").to_owned(),
            repo: repo_of(s),
            asleep: s.get("asleep").and_then(Value::as_bool).unwrap_or(false),
        })
        .collect()
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn cut(s: &str, cap: usize) -> String {
    if s.chars().count() <= cap {
        return s.to_owned();
    }
    let mut out: String = s.chars().take(cap.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// The summary: a short `message` (one paragraph, under the host's cap) and a `data` object with
/// the full detail.
fn summarise(data: &Value, include_asleep: bool) -> (String, Value) {
    let all = rows(data);
    let shown: Vec<&Row> = all.iter().filter(|r| include_asleep || !r.asleep).collect();
    let mut by_status: BTreeMap<&str, usize> = BTreeMap::new();
    let mut by_repo: BTreeMap<&str, (usize, usize)> = BTreeMap::new();
    let mut needs: Vec<&Row> = Vec::new();
    for r in &shown {
        *by_status.entry(r.status.as_str()).or_default() += 1;
        let wants_you = NEEDS_YOU.contains(&r.status.as_str());
        let e = by_repo.entry(r.repo.as_str()).or_default();
        e.0 += 1;
        e.1 += usize::from(wants_you);
        if wants_you {
            needs.push(r);
        }
    }
    needs.sort_by_key(|r| NEEDS_YOU.iter().position(|s| *s == r.status).unwrap_or(usize::MAX));

    let message = if shown.is_empty() {
        "No sessions.".to_owned()
    } else {
        let counts: Vec<String> = by_status.iter().map(|(s, n)| format!("{n} {}", s.replace('_', " "))).collect();
        let mut m = format!("{} in {}: {}.", plural(shown.len(), "session", "sessions"), plural(by_repo.len(), "repo", "repos"), counts.join(", "));
        if needs.is_empty() {
            m.push_str(" Nothing needs you.");
        } else {
            let names: Vec<String> = needs.iter().take(5).map(|r| format!("{} ({})", r.name, r.status.replace('_', " "))).collect();
            let more = needs.len().saturating_sub(5);
            let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
            m.push_str(&format!(" Needs you: {}{tail}.", names.join(", ")));
        }
        m
    };
    let message = cut(&message, MESSAGE_CAP);

    let repos: Vec<Value> = by_repo.iter().map(|(repo, (n, w))| json!({"repo": repo, "sessions": n, "needs_you": w})).collect();
    let needs_json: Vec<Value> = needs.iter().map(|r| json!({"name": r.name, "status": r.status, "repo": r.repo})).collect();
    let detail = json!({
        "total": shown.len(),
        "by_status": by_status,
        "needs_you": needs_json,
        "by_repo": repos,
    });
    (message, detail)
}

fn send(out: &mut impl Write, v: &Value) -> io::Result<()> {
    writeln!(out, "{v}")?;
    out.flush()
}

fn ok(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn err(id: &Value, code: i64, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn initialize(id: &Value) -> Value {
    ok(
        id,
        json!({
            "api": 1,
            "name": "Session summary",
            "tools": [{
                "id": TOOL,
                "title": "Summarise sessions",
                "summary": "A short text summary of your sessions: counts by status, which ones need you, and the sessions per repo.",
                "input_schema": {
                    "type": "object",
                    "properties": {"include_asleep": {"type": "boolean", "description": "Count sleeping sessions too (default false)."}},
                    "additionalProperties": false
                },
                "effects": []
            }],
            "subscriptions": []
        }),
    )
}

/// The most requests held while one is waiting on the app's answer; past it they are refused.
const MAX_PENDING: usize = 64;

/// Reads lines until the answer to request `want` arrives. A request that arrives meanwhile (a
/// second `tools/call`) is queued in `pending` to be served next, never dropped; notifications and
/// stray answers are ignored. `None` when stdin closes.
fn wait_for(lines: &mut impl Iterator<Item = io::Result<String>>, want: u64, pending: &mut VecDeque<Value>) -> Option<Value> {
    for line in lines {
        let line = line.ok()?;
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        if msg.get("method").is_none() {
            if msg.get("id").and_then(Value::as_u64) == Some(want) {
                return Some(msg);
            }
        } else if msg.get("id").is_some() && pending.len() < MAX_PENDING {
            pending.push_back(msg);
        }
    }
    None
}

fn main() {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let stdout = io::stdout();
    serve(&mut lines, &mut stdout.lock());
}

fn serve(lines: &mut impl Iterator<Item = io::Result<String>>, out: &mut impl Write) {
    let mut next_id: u64 = 1000;
    let mut pending: VecDeque<Value> = VecDeque::new();
    loop {
        let msg = match pending.pop_front() {
            Some(m) => m,
            None => match lines.next() {
                Some(Ok(line)) => match serde_json::from_str::<Value>(&line) {
                    Ok(m) => m,
                    Err(_) => continue,
                },
                _ => break,
            },
        };
        let method = msg.get("method").and_then(Value::as_str).unwrap_or_default();
        let Some(id) = msg.get("id").cloned() else { continue };
        let reply = match method {
            "initialize" => initialize(&id),
            "tools/call" => {
                let params = msg.get("params").cloned().unwrap_or(Value::Null);
                if params.get("name").and_then(Value::as_str) != Some(TOOL) {
                    err(&id, -32601, "no such tool")
                } else {
                    let include_asleep = params.get("arguments").and_then(|a| a.get("include_asleep")).and_then(Value::as_bool).unwrap_or(false);
                    next_id += 1;
                    let call = json!({"jsonrpc": "2.0", "id": next_id, "method": "csm/call", "params": {"tool": "session_list", "params": {}}});
                    if send(out, &call).is_err() {
                        break;
                    }
                    match wait_for(lines, next_id, &mut pending) {
                        None => break,
                        Some(answer) => match answer.get("result") {
                            Some(result) => {
                                let (message, data) = summarise(result.get("data").unwrap_or(&Value::Null), include_asleep);
                                ok(&id, json!({"message": message, "data": data}))
                            }
                            None => {
                                let why = answer.pointer("/error/message").and_then(Value::as_str).unwrap_or("session_list failed");
                                err(&id, -32000, &cut(why, 200))
                            }
                        },
                    }
                }
            }
            _ => err(&id, -32601, "unknown method"),
        };
        if send(out, &reply).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!([
            {"name": "api", "status": "working", "repo_root": "C:\\code\\shop", "folder_short": "shop"},
            {"name": "web", "status": "needs_you_approval", "repo_root": "/home/a/shop", "folder_short": "web"},
            {"name": "docs", "status": "idle", "folder_short": "docs"},
            {"name": "old", "status": "asleep", "asleep": true, "folder_short": "docs"},
            "not an object", 7
        ])
    }

    #[test]
    fn counts_needs_you_and_repos() {
        let (m, d) = summarise(&sample(), false);
        assert_eq!(d["total"], 3);
        assert_eq!(d["by_status"]["working"], 1);
        assert_eq!(d["needs_you"][0]["name"], "web");
        assert_eq!(d["by_repo"].as_array().unwrap().len(), 2);
        assert!(m.contains("3 sessions in 2 repos") && m.contains("web (needs you approval)"), "{m}");
        let (_, with_asleep) = summarise(&sample(), true);
        assert_eq!(with_asleep["total"], 4);
    }

    #[test]
    fn empty_and_hostile_input_never_panics() {
        assert_eq!(summarise(&Value::Null, false).0, "No sessions.");
        assert_eq!(summarise(&json!({"x": 1}), false).0, "No sessions.");
        let long: Vec<Value> = (0..500).map(|i| json!({"name": format!("session-number-{i}"), "status": "error"})).collect();
        assert!(summarise(&Value::Array(long), true).0.chars().count() <= MESSAGE_CAP);
    }

    #[test]
    fn a_call_that_arrives_while_one_waits_is_queued_and_answered() {
        let call = |id: u64| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": TOOL, "arguments": {}}}).to_string();
        let answer = |id: u64| json!({"jsonrpc": "2.0", "id": id, "result": {"data": sample()}}).to_string();
        // The second call lands before the app answers the first call's `csm/call`.
        let input = [call(1), call(2), answer(1001), answer(1002)];
        let mut lines = input.into_iter().map(Ok);
        let mut out = Vec::new();
        serve(&mut lines, &mut out);
        let text = String::from_utf8(out).unwrap();
        let replies: Vec<Value> = text.lines().map(|l| serde_json::from_str(l).unwrap()).filter(|v: &Value| v.get("method").is_none()).collect();
        let ids: Vec<u64> = replies.iter().filter_map(|r| r["id"].as_u64()).collect();
        assert_eq!(ids, vec![1, 2], "{text}");
        assert!(replies.iter().all(|r| r.get("result").is_some()), "{text}");
    }
}
