//! Claude usage end to end: a Claude session started on a fake PTY gets its own conversation id,
//! the transcript reader follows `<root>/projects/<dir>/<id>.jsonl` (and its subagents) into the
//! index, and the usage tools report it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{ClaudeRoots, IndexSetting, ToolError, ToolOutput};
use serde_json::{Value, json};

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn iso(ms: i64) -> String {
    csm_core::time::Timestamp::from_epoch_ms(ms).as_str().to_owned()
}

fn asst(id: &str, ts: i64, model: &str, usage: Value, extra: Value) -> String {
    let mut v = json!({
        "type": "assistant", "sessionId": "x", "timestamp": iso(ts), "cwd": "C:\\work\\shop",
        "message": {"id": id, "model": model, "role": "assistant", "content": [{"type": "text", "text": "secret reply"}], "usage": usage}
    });
    if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
        for (k, x) in e {
            o.insert(k.clone(), x.clone());
        }
    }
    v.to_string() + "\n"
}

fn usage_harness(root: &Path) -> H {
    let roots = vec![root.to_path_buf()];
    H::start(test_app(), move |o| {
        o.index = IndexSetting::Handle(Index::start(IndexOptions::memory()).unwrap());
        o.claude_roots = ClaudeRoots::Dirs(roots);
        o.usage_poll = Duration::from_millis(40);
    })
}

/// Calls `tool` until `ok` accepts the answer (the reader runs on its own thread).
fn until(h: &H, tool: &str, params: Value, what: &str, ok: impl Fn(&ToolOutput) -> bool) -> ToolOutput {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let out = h.ok(tool, params.clone());
        if ok(&out) {
            return out;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for {what}; last answer: {}", serde_json::to_string_pretty(&out.data).unwrap());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn transcript_for(root: &Path, cid: &str) -> PathBuf {
    let dir = root.join("projects").join("C--work-shop");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{cid}.jsonl"))
}

#[test]
fn a_claude_sessions_transcript_is_followed_and_reported() {
    let root = tempfile::tempdir().unwrap();
    let h = usage_harness(root.path());
    let id = h.session("claude");
    let v = h.wait_until("an id", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some()));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    let path = transcript_for(root.path(), &cid);
    let t0 = now_ms() - 2_000;
    let mut text = String::new();
    text += &asst("m1", t0, "claude-opus-5-5", json!({"input_tokens": 100, "output_tokens": 50, "cache_creation_input_tokens": 30_000}), json!({}));
    // The same response again, further along: one request, the max per field.
    text += &asst("m1", t0 + 10, "claude-opus-5-5", json!({"input_tokens": 100, "output_tokens": 80, "cache_creation_input_tokens": 30_000}), json!({}));
    text += &asst("m2", t0 + 20_000, "claude-opus-5-5", json!({"input_tokens": 10, "output_tokens": 20, "cache_read_input_tokens": 30_000}), json!({}));
    text += "{broken\n";
    text += &json!({"type": "system", "subtype": "compact_boundary", "timestamp": iso(t0 + 30_000), "compactMetadata": {"trigger": "auto", "preTokens": 150_000, "postTokens": 9_000}}).to_string();
    text += "\n";
    std::fs::write(&path, &text).unwrap();
    let sub_dir = path.with_extension("").join("subagents");
    std::fs::create_dir_all(&sub_dir).unwrap();
    std::fs::write(sub_dir.join("agent-a1.jsonl"), asst("s1", t0 + 5_000, "qwen3.8:27b", json!({"input_tokens": 500}), json!({"agentId": "a1"}))).unwrap();

    let out = until(&h, "usage_session", json!({"session_id": id}), "3 requests", |o| o.data["total"]["requests"] == 3);
    let d = &out.data;
    assert_eq!(d["claude_session_id"], json!(cid));
    assert_eq!(d["main"]["requests"], 2);
    assert_eq!(d["subagents"]["requests"], 1);
    assert_eq!(d["subagents"]["unpriced_models"], json!(["qwen3.8:27b"]), "unpriced is reported, not $0");
    // m1: 100*4 + 80*20 + 30000*5 = 152000 → $0.152; m2: 10*4 + 20*20 + 30000*0.2 = 6440 → $0.00644
    let cost = d["total"]["cost_usd"].as_f64().unwrap();
    assert!((cost - 0.15844).abs() < 1e-9, "{cost}");
    assert_eq!(d["context"]["fill_tokens"], 30_010);
    assert_eq!(d["context"]["window_tokens"], 200_000);
    assert_eq!(d["context"]["level"], "ok");
    assert_eq!(d["compactions"]["count"], 1);
    assert_eq!(d["compactions"]["last"]["post_tokens"], 9_000);
    assert_eq!(d["data_quality"]["bad_lines"], 1);
    assert_eq!(d["today"]["requests"], 3, "all of it ran just now (\"today\" is local)");
    assert!(out.message.contains("API-equivalent"), "{}", out.message);
    assert!(!serde_json::to_string(d).unwrap().contains("secret reply"), "no content is kept");

    // Appended lines are read incrementally; a cache miss after a long idle gap is reported.
    let mut f = std::fs::OpenOptions::new().append(true).open(&path).unwrap();
    f.write_all(asst("m3", t0 + 20_000 + 400_000, "claude-opus-5-5", json!({"input_tokens": 10, "output_tokens": 5, "cache_creation_input_tokens": 31_000}), json!({})).as_bytes())
        .unwrap();
    drop(f);
    let out = until(&h, "usage_session", json!({"session_id": id}), "the appended request", |o| o.data["total"]["requests"] == 4);
    assert_eq!(out.data["last_cache_miss"]["idle_gap_ms"], 400_000);
    assert_eq!(out.data["last_cache_miss"]["cache_write"], 31_000);

    let out = until(&h, "usage_context", json!({}), "the live fill", |o| o.data["sessions"][0]["context"]["fill_tokens"] == 31_010);
    assert_eq!(out.data["sessions"][0]["session_id"], json!(id));
    assert!(out.data["sessions"][0]["cost_usd"].as_f64().unwrap() > 0.15);

    let out = h.ok("usage_summary", json!({"from": "all", "group_by": "project"}));
    assert_eq!(out.data["rows"][0]["label"], "shop", "work is a generic parent folder");
    assert_eq!(out.data["rows"][0]["requests"], 4);
    let out = h.ok("usage_summary", json!({"from": "today", "group_by": "session"}));
    assert_eq!(out.data["group_by"], "session");
    let out = h.ok("usage_summary", json!({"from": "all", "group_by": "model"}));
    assert!(out.data["rows"].as_array().unwrap().iter().any(|r| r["key"] == "qwen3.8:27b" && r["unpriced_requests"] == 1));

    // A rewritten (shorter) transcript is read again from the start.
    std::fs::write(&path, asst("m9", t0, "claude-opus-5-5", json!({"input_tokens": 1}), json!({}))).unwrap();
    let out = until(&h, "usage_session", json!({"session_id": id}), "the rewrite", |o| o.data["main"]["requests"] == 1);
    assert_eq!(out.data["context"]["fill_tokens"], 1);
}

#[test]
fn usage_tools_refuse_what_they_cannot_answer() {
    let root = tempfile::tempdir().unwrap();
    let h = usage_harness(root.path());
    let shell = h.session("pwsh");
    assert!(matches!(h.ui("usage_session", json!({"session_id": shell})), Err(ToolError::Refused(_))));
    assert!(matches!(h.ui("usage_session", json!({"session_id": "nope"})), Err(ToolError::NotFound(_))));
    assert!(matches!(h.ui("usage_summary", json!({"from": "soon"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("usage_summary", json!({"group_by": "colour"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("usage_context", json!({"x": 1})), Err(ToolError::InvalidParams(_))));
    // A Claude session with nothing recorded yet answers, with no fill.
    let id = h.session("claude");
    let out = h.ok("usage_session", json!({"session_id": id}));
    assert!(out.data["context"].is_null());
    assert_eq!(out.data["total"]["requests"], 0);

    // No index: refused with a reason.
    let plain = H::new();
    let c = plain.session("claude");
    assert!(matches!(plain.ui("usage_session", json!({"session_id": c})), Err(ToolError::Refused(_))));
    assert!(matches!(plain.ui("usage_summary", json!({})), Err(ToolError::Refused(_))));
}
