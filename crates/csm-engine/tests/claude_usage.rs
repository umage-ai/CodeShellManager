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

/// Settings with the built-in Claude usage add-on on (it is off by default).
fn usage_app() -> csm_core::models::AppState {
    let mut app = test_app();
    app.settings.addon_approvals.insert(csm_engine::addons::CLAUDE_USAGE_ID.into(), csm_engine::addons::BUILTIN_DIGEST.into());
    app
}

fn usage_harness(root: &Path) -> H {
    let roots = vec![root.to_path_buf()];
    H::start(usage_app(), move |o| {
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
    // Claude over SSH: its transcripts are on the other machine.
    let ssh = h.ok("session_new", json!({"command": "claude", "kind": "ssh", "ssh": {"host": "box"}, "focus": false}));
    let ssh = ssh.data["id"].as_str().unwrap().to_owned();
    match h.ui("usage_session", json!({"session_id": ssh})) {
        Err(ToolError::Refused(m)) => assert!(m.contains("SSH"), "{m}"),
        other => panic!("{other:?}"),
    }
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

#[test]
fn session_subagents_reports_workflow_runs_and_states() {
    let root = tempfile::tempdir().unwrap();
    let h = usage_harness(root.path());
    let id = h.session("claude");
    let v = h.wait_until("an id", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some()));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    let path = transcript_for(root.path(), &cid);
    let t0 = now_ms() - 1_000;
    std::fs::write(&path, asst("m1", t0, "claude-opus-5-5", json!({"input_tokens": 10, "output_tokens": 5}), json!({}))).unwrap();
    let sub = path.with_extension("").join("subagents");
    let run = sub.join("workflows").join("wf_test");
    std::fs::create_dir_all(&run).unwrap();
    // A standalone agent that finished, and a hostile meta next to it.
    std::fs::write(sub.join("agent-ex1.jsonl"), asst("e1", t0, "claude-opus-5-5", json!({"input_tokens": 1000}), json!({"agentId": "ex1"}))).unwrap();
    std::fs::write(sub.join("agent-ex1.meta.json"), r#"{"agentType":"Explore","description":"Inventory features","model":"claude-opus-5-5","finished":true}"#).unwrap();
    std::fs::write(sub.join("agent-bad.jsonl"), asst("b1", t0, "claude-opus-5-5", json!({"input_tokens": 1}), json!({"agentId": "bad"}))).unwrap();
    std::fs::write(sub.join("agent-bad.meta.json"), "[[[ not json \u{0}").unwrap();
    // A workflow with two agents in one phase, one with a result, and a third only in the journal.
    for (a, label) in [("w1", "impl:core"), ("w2", "review:core:1")] {
        std::fs::write(run.join(format!("agent-{a}.jsonl")), asst(&format!("{a}m"), t0 + 10, "claude-sonnet-5", json!({"input_tokens": 100}), json!({"agentId": a}))).unwrap();
        std::fs::write(
            run.join(format!("agent-{a}.meta.json")),
            format!(r#"{{"agentType":"workflow-subagent","description":"{label}","workflowPhase":"Screens core","model":"sonnet","inWorkflowRun":true}}"#),
        )
        .unwrap();
    }
    std::fs::write(
        run.join("journal.jsonl"),
        "{\"type\":\"launched\"}\n{\"type\":\"started\",\"agentId\":\"w1\",\"label\":\"impl:core\",\"phase\":\"Screens core\"}\n\
         {\"type\":\"started\",\"agentId\":\"w2\",\"label\":\"review:core:1\",\"phase\":\"Screens core\"}\n\
         {\"type\":\"started\",\"agentId\":\"w3\",\"label\":\"impl:ui\",\"phase\":\"Screens ui\"}\n{\"type\":\"result\",\"agentId\":\"w1\",\"result\":{}}\n",
    )
    .unwrap();

    let out = until(&h, "session_subagents", json!({"session_id": id}), "the subagents with usage", |o| {
        o.data["agents"].as_array().is_some_and(|a| a.len() == 4) && o.data["agents"].as_array().unwrap().iter().all(|a| a["requests"] == 1)
    });
    let d = &out.data;
    assert_eq!((d["running"].as_u64(), d["done"].as_u64(), d["stale"].as_u64()), (Some(2), Some(2), Some(0)));
    let by = |id: &str| d["agents"].as_array().unwrap().iter().find(|a| a["id"] == id).unwrap().clone();
    assert_eq!(by("ex1")["state"], "done");
    assert_eq!(by("ex1")["label"], "Inventory features");
    assert_eq!(by("w1")["state"], "done", "the journal's result marks it done");
    assert_eq!(by("w2")["state"], "running");
    assert_eq!(by("w2")["phase"], "Screens core");
    assert_eq!(by("w2")["run"], "wf_test");
    assert_eq!(by("bad")["state"], "running", "a hostile meta shows less, it does not fail");
    assert_eq!(by("w2")["tokens"], 100);
    assert!(by("w2")["cost_usd"].as_f64().is_some());
    let run = &d["runs"][0];
    assert_eq!(run["id"], "wf_test");
    assert_eq!((run["done"].as_u64(), run["total"].as_u64()), (Some(1), Some(3)));
    assert_eq!(run["phases"][0], json!({"name": "Screens core", "done": 1, "total": 2, "running": 1}));
    assert_eq!(run["phases"][1]["name"], "Screens ui");
    assert!(out.message.contains("4 subagents"), "{}", out.message);
    // The snapshot carries the same report, and the counts.
    let v = h.wait_until("the view has them", |v| v.session(&id).is_some_and(|s| s.subagents_total == 4));
    assert_eq!(v.session(&id).unwrap().subagents_running, 2);
    assert_eq!(v.session(&id).unwrap().subagents.as_ref().unwrap().runs.len(), 1);

    // The journal grows: the rest of the phase finishes.
    let mut f = std::fs::OpenOptions::new().append(true).open(run_journal(&path)).unwrap();
    f.write_all(b"{\"type\":\"result\",\"agentId\":\"w2\"}\n").unwrap();
    drop(f);
    let out = until(&h, "session_subagents", json!({"session_id": id}), "w2 done", |o| o.data["runs"][0]["phases"][0]["done"] == 2);
    assert_eq!(out.data["running"], 1, "only the hostile-meta agent still runs");
}

fn run_journal(main: &Path) -> PathBuf {
    main.with_extension("").join("subagents").join("workflows").join("wf_test").join("journal.jsonl")
}

#[test]
fn session_subagents_is_empty_for_a_session_without_any() {
    let root = tempfile::tempdir().unwrap();
    let h = usage_harness(root.path());
    let id = h.session("claude");
    let out = h.ok("session_subagents", json!({"session_id": id}));
    assert_eq!(out.data["agents"], json!([]));
    assert_eq!(out.data["following"], true);
    assert!(out.message.contains("no subagents"), "{}", out.message);
    assert!(h.ui("session_subagents", json!({"session_id": "nope"})).is_err());
    assert!(h.ui("session_subagents", json!({"session_id": id, "extra": 1})).is_err());
}

/// A conversation CSM never ran, in its own project folder.
fn history_file(root: &Path, dir: &str, cid: &str, lines: &[String]) {
    let d = root.join("projects").join(dir);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join(format!("{cid}.jsonl")), lines.concat()).unwrap();
}

#[test]
fn usage_report_reads_the_history_and_drills_down() {
    let root = tempfile::tempdir().unwrap();
    let now = now_ms();
    let day = 86_400_000;
    let opus = |id: &str, ts: i64, u: Value| asst(id, ts, "claude-opus-5-5", u, json!({}));
    // Written before CSM starts: read by the history reader, not by a followed session.
    history_file(
        root.path(),
        "C--work-shop",
        "aaaa-1111",
        &[
            opus("h1", now - 60_000, json!({"input_tokens": 100, "output_tokens": 10, "cache_creation_input_tokens": 40_000, "cache_creation": {"ephemeral_1h_input_tokens": 40_000}})),
            opus("h2", now - 30_000, json!({"input_tokens": 10, "output_tokens": 10, "cache_read_input_tokens": 40_000})),
            asst("h3", now - 20_000, "claude-sonnet-5-5", json!({"input_tokens": 1_000, "output_tokens": 100}), json!({})),
            opus("h4", now - 10 * day, json!({"input_tokens": 1, "output_tokens": 1})),
            opus("h5", now - 40 * day, json!({"input_tokens": 2, "output_tokens": 2})),
        ],
    );
    history_file(root.path(), "C--work-blog", "bbbb-2222", &[asst("b1", now - 50_000, "qwen3.8:27b", json!({"input_tokens": 700}), json!({}))]);
    let sub = root.path().join("projects").join("C--work-shop").join("aaaa-1111").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("agent-x1.jsonl"), asst("s1", now - 40_000, "claude-sonnet-5-5", json!({"input_tokens": 5}), json!({"agentId": "x1"}))).unwrap();
    let h = usage_harness(root.path());

    let out = until(&h, "usage_report", json!({"from": "all"}), "the history", |o| o.data["total"]["requests"] == 7);
    let d = &out.data;
    assert_eq!(d["history"]["scanned"], true);
    assert_eq!(d["group_by"], "session");
    assert_eq!(d["sessions"], 2);
    assert_eq!(d["rows"][0]["key"], "aaaa-1111", "a conversation CSM didn't run is keyed by its id");
    assert_eq!(d["rows"][0]["label"], "shop · aaaa-111");
    assert_eq!(d["rows"][0]["projects"], json!(["shop"]));
    assert_eq!(d["rows"][1]["metrics"]["unpriced_models"], json!(["qwen3.8:27b"]));
    assert!(d["days"].as_array().unwrap().len() >= 40, "all: from the first request's day");
    assert!(d["definitions"].as_str().unwrap().contains("cache_hit_rate"));
    assert_eq!(d["price_version"], "2026-11-15");
    assert!(out.message.contains("API-equivalent"), "{}", out.message);
    assert_eq!(d["total"]["tokens"]["cache_write_1h"], 40_000, "1h writes are kept apart");

    // Ranges: today, 7 days, 30 days.
    let n = |from: &str| h.ok("usage_report", json!({"from": from})).data["total"]["requests"].as_u64().unwrap();
    assert_eq!(n("today"), 5, "h1, h2, h3, the subagent and b1 (unless the test straddles midnight)");
    assert_eq!(n("7d"), 5);
    assert_eq!(n("30d"), 6);
    let week = h.ok("usage_report", json!({"from": "7d"}));
    let days = week.data["days"].as_array().unwrap();
    assert_eq!(days.len(), 7);
    assert_eq!(days[6]["day"], json!(jiff::Zoned::now().date().to_string()));
    let hit = week.data["total"]["cache_hit_rate"].as_f64().unwrap();
    assert!((hit - 40_000.0 / (100.0 + 40_000.0 + 10.0 + 40_000.0 + 1_000.0 + 5.0 + 700.0)).abs() < 1e-9, "{hit}");
    assert!(week.data["total"]["cache_miss_cost_usd"].as_f64().unwrap() > 0.0);
    // A custom range: just the day ten days ago.
    let ten = (jiff::Zoned::now().date() - jiff::Span::new().days(10)).to_string();
    let custom = h.ok("usage_report", json!({"from": ten, "to": ten}));
    assert_eq!(custom.data["total"]["requests"], 1);
    assert_eq!(custom.data["days"].as_array().unwrap().len(), 1);

    // Drill down: a session's models, then a model's sessions.
    let models = h.ok("usage_report", json!({"from": "7d", "group_by": "model", "session": "aaaa-1111"}));
    let keys: Vec<&str> = models.data["rows"].as_array().unwrap().iter().map(|r| r["key"].as_str().unwrap()).collect();
    assert_eq!(keys, ["claude-opus-5-5", "claude-sonnet-5-5"]);
    assert_eq!(models.data["filter"]["session"], "aaaa-1111");
    let sessions = h.ok("usage_report", json!({"from": "7d", "group_by": "session", "model": "claude-sonnet-5-5"}));
    assert_eq!(sessions.data["rows"].as_array().unwrap().len(), 1);
    assert_eq!(sessions.data["rows"][0]["metrics"]["requests"], 2, "h3 and the subagent");
    let by_project = h.ok("usage_report", json!({"from": "7d", "group_by": "project"}));
    assert_eq!(by_project.data["rows"][0]["key"], "shop");
    let by_day = h.ok("usage_report", json!({"from": "30d", "group_by": "day"}));
    assert_eq!(by_day.data["rows"].as_array().unwrap().len(), 2, "only days with requests");

    // Bad params are refused before any work.
    assert!(matches!(h.ui("usage_report", json!({"from": "soon"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("usage_report", json!({"from": "2026-11-10", "to": "2026-11-01"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("usage_report", json!({"group_by": "colour"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("usage_report", json!({"nope": 1})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(H::new().ui("usage_report", json!({})), Err(ToolError::Refused(_))));
}

#[test]
fn a_followed_session_takes_over_its_conversation_read_from_history() {
    let root = tempfile::tempdir().unwrap();
    let h = usage_harness(root.path());
    let id = h.session("claude");
    let v = h.wait_until("an id", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some()));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    let path = transcript_for(root.path(), &cid);
    std::fs::write(&path, asst("m1", now_ms() - 1000, "claude-opus-5-5", json!({"input_tokens": 10, "output_tokens": 5}), json!({}))).unwrap();
    // Whichever reader gets it first, the rows end up under the CSM session.
    until(&h, "usage_session", json!({"session_id": id}), "the request", |o| o.data["total"]["requests"] == 1);
    let rep = until(&h, "usage_report", json!({"from": "today"}), "the session row", |o| o.data["rows"][0]["key"] == json!(id));
    assert_eq!(rep.data["rows"].as_array().unwrap().len(), 1);
    assert_eq!(rep.data["rows"][0]["csm_session"], json!(id));
}
