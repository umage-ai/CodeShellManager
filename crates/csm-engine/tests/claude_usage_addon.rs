//! The built-in Claude usage add-on: off by default (the reader keeps only a light, in-memory tail
//! for context fill, subagents and Remote Control, and stores nothing), turned on and off with
//! `addon_enable` / `addon_disable` without a restart, the "Read past Claude history" setting,
//! `usage_forget_history`, and `--clean` never reading the history.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_engine::addons::{BUILTIN_DIGEST, BUILTIN_VERSION, CLAUDE_USAGE_ID};
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{ClaudeRoots, Effects, IndexSetting, ToolError, ToolOutput};
use serde_json::{Value, json};

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn asst(id: &str, ts: i64, input: u64) -> String {
    json!({"type": "assistant", "timestamp": csm_core::time::Timestamp::from_epoch_ms(ts).as_str(), "cwd": "C:\\work\\shop",
        "message": {"id": id, "model": "claude-opus-5-5", "usage": {"input_tokens": input, "output_tokens": 5, "cache_read_input_tokens": 1_000}}})
    .to_string()
        + "\n"
}

fn on_app() -> csm_core::models::AppState {
    let mut app = test_app();
    app.settings.addon_approvals.insert(CLAUDE_USAGE_ID.into(), BUILTIN_DIGEST.into());
    app
}

/// A harness over `root` with an index the test can look into.
fn harness(app: csm_core::models::AppState, root: &Path, clean: bool) -> (H, Index) {
    let index = Index::start(IndexOptions::memory()).unwrap();
    let ix = index.clone();
    let roots = vec![root.to_path_buf()];
    let h = H::start(app, move |o| {
        o.index = IndexSetting::Handle(ix);
        o.claude_roots = ClaudeRoots::Dirs(roots);
        o.usage_poll = Duration::from_millis(30);
        o.clean = clean;
    });
    (h, index)
}

/// A conversation CSM never ran.
fn history_file(root: &Path) {
    let d = root.join("projects").join("C--work-blog");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("hist-0001.jsonl"), asst("h1", now_ms() - 60_000, 700)).unwrap();
}

fn transcript_for(root: &Path, cid: &str) -> PathBuf {
    let dir = root.join("projects").join("C--work-shop");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{cid}.jsonl"))
}

/// A live Claude session and its transcript path.
fn claude(h: &H, root: &Path) -> (String, PathBuf) {
    let id = h.session("claude");
    let v = h.wait_until("an id", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some() && s.live));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    (id, transcript_for(root, &cid))
}

fn until(h: &H, tool: &str, params: Value, what: &str, ok: impl Fn(&ToolOutput) -> bool) -> ToolOutput {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let out = h.ok(tool, params.clone());
        if ok(&out) {
            return out;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}; last: {}", out.data);
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn fill_of(out: &ToolOutput, id: &str) -> Value {
    out.data["sessions"].as_array().unwrap().iter().find(|s| s["session_id"] == json!(id)).map_or(Value::Null, |s| s["context"]["fill_tokens"].clone())
}

fn stored_requests(ix: &Index) -> usize {
    ix.call_blocking(|db| db.claude_report_rows(i64::MIN, i64::MAX)).unwrap().len()
}

fn stored_files(ix: &Index) -> usize {
    ix.call_blocking(|db| db.claude_files_all()).unwrap().len()
}

fn refused_off(r: Result<ToolOutput, ToolError>) {
    match r {
        Err(ToolError::Refused(m)) => assert!(m.contains("Claude usage is off") && m.contains("Add-ons"), "{m}"),
        other => panic!("{other:?}"),
    }
}

fn report_sessions(h: &H) -> u64 {
    h.ok("usage_report", json!({"from": "all"})).data["sessions"].as_u64().unwrap()
}

#[test]
fn off_by_default_the_reader_keeps_context_subagents_and_remote_control_but_stores_nothing() {
    let root = tempfile::tempdir().unwrap();
    history_file(root.path());
    let (h, ix) = harness(test_app(), root.path(), false);

    // Listed with the add-ons, built in, off.
    let list = h.ok("addon_list", json!({}));
    let a = list.data["builtin"].as_array().unwrap().iter().find(|a| a["id"] == CLAUDE_USAGE_ID).unwrap().clone();
    assert_eq!((a["kind"].as_str(), a["state"].as_str(), a["version"].as_str()), (Some("builtin"), Some("disabled"), Some(BUILTIN_VERSION)));
    assert!(a["description"].as_str().unwrap().contains("Built in"));
    assert!(h.engine.view().builtin_addons.iter().any(|a| a.id == CLAUDE_USAGE_ID), "the snapshot lists it for the UI");

    let (id, path) = claude(&h, root.path());
    let now = now_ms();
    std::fs::write(&path, asst("m1", now - 1_000, 42_000)).unwrap();
    let sub = path.with_extension("").join("subagents");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("agent-a1.jsonl"), asst("s1", now - 500, 9)).unwrap();
    let url = "https://claude.ai/code/session_abc";
    let rc = json!({"type": "system", "subtype": "bridge_status", "content": format!("/remote-control is active · Continue at {url}"), "url": url, "timestamp": csm_core::time::Timestamp::from_epoch_ms(now + 10).as_str()});
    std::fs::OpenOptions::new().append(true).open(&path).and_then(|mut f| std::io::Write::write_all(&mut f, (rc.to_string() + "\n").as_bytes())).unwrap();

    // The sidebar chip's context fill, without cost.
    let out = until(&h, "usage_context", json!({}), "the fill", |o| fill_of(o, &id) == json!(43_000));
    let row = out.data["sessions"].as_array().unwrap().iter().find(|s| s["session_id"] == json!(id)).unwrap().clone();
    assert!(row["cost_usd"].is_null(), "{row}");
    assert_eq!(out.data["usage"], false);
    // Remote Control.
    let v = h.wait_until("remote control", |v| v.session(&id).is_some_and(|s| s.remote_control));
    assert_eq!(v.session(&id).unwrap().remote_control_url.as_deref(), Some(url));
    // Subagents, without their cost.
    let subs = until(&h, "session_subagents", json!({"session_id": id}), "the subagent", |o| o.data["agents"].as_array().is_some_and(|a| a.len() == 1));
    assert!(subs.message.contains("Claude usage is off"), "{}", subs.message);

    // The usage tools say why they cannot answer.
    refused_off(h.ui("usage_session", json!({"session_id": id})));
    refused_off(h.ui("usage_summary", json!({})));
    refused_off(h.ui("usage_report", json!({})));

    // Nothing was stored: not the live session's requests, not the history.
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(stored_requests(&ix), 0);
    assert_eq!(stored_files(&ix), 0);
}

#[test]
fn enabling_starts_reading_at_once_and_disabling_keeps_what_was_stored() {
    let root = tempfile::tempdir().unwrap();
    history_file(root.path());
    let (h, ix) = harness(test_app(), root.path(), false);
    let (id, path) = claude(&h, root.path());
    std::fs::write(&path, asst("m1", now_ms() - 1_000, 10)).unwrap();
    until(&h, "usage_context", json!({}), "the light tail", |o| fill_of(o, &id) == json!(1_010));

    // A wrong version or digest is refused; the shown ones enable it.
    assert!(matches!(h.ui("addon_enable", json!({"id": CLAUDE_USAGE_ID, "version": "9", "digest": BUILTIN_DIGEST})), Err(ToolError::Refused(_))));
    let on = h.ok("addon_enable", json!({"id": CLAUDE_USAGE_ID, "version": BUILTIN_VERSION, "digest": BUILTIN_DIGEST}));
    assert_eq!(on.data["enabled"], true);
    assert!(h.engine.view().builtin_addons.iter().any(|a| a.id == CLAUDE_USAGE_ID && a.state == csm_engine::addons::AddonState::Loaded));
    // The live session is accounted, and the history is imported, without a restart.
    until(&h, "usage_session", json!({"session_id": id}), "the live request", |o| o.data["total"]["requests"] == 1);
    until(&h, "usage_report", json!({"from": "all"}), "the history", |o| o.data["sessions"] == 2 && o.data["history_on"] == true);
    let ctx = h.ok("usage_context", json!({}));
    assert!(ctx.data["sessions"][0]["cost_usd"].as_f64().unwrap() > 0.0, "cost once it is on");

    // Off again: refused, the stored rows stay, and nothing more is stored.
    h.ok("addon_disable", json!({"id": CLAUDE_USAGE_ID}));
    refused_off(h.ui("usage_report", json!({})));
    let kept = stored_requests(&ix);
    assert_eq!(kept, 2);
    std::fs::OpenOptions::new().append(true).open(&path).and_then(|mut f| std::io::Write::write_all(&mut f, asst("m2", now_ms(), 20).as_bytes())).unwrap();
    until(&h, "usage_context", json!({}), "the light tail again", |o| fill_of(o, &id) == json!(1_020));
    assert_eq!(stored_requests(&ix), kept, "off stores nothing");

    // On again: carries on from the stored positions and picks up what was written meanwhile.
    h.ok("addon_enable", json!({"id": CLAUDE_USAGE_ID, "version": BUILTIN_VERSION, "digest": BUILTIN_DIGEST}));
    until(&h, "usage_session", json!({"session_id": id}), "the request written while off", |o| o.data["total"]["requests"] == 2);
}

#[test]
fn without_past_history_only_the_sessions_csm_runs_are_counted_and_forgetting_needs_it_off() {
    let root = tempfile::tempdir().unwrap();
    history_file(root.path());
    let mut app = on_app();
    app.settings.claude_usage_history = false;
    let (h, ix) = harness(app, root.path(), false);
    let (id, path) = claude(&h, root.path());
    std::fs::write(&path, asst("m1", now_ms() - 1_000, 10)).unwrap();
    until(&h, "usage_session", json!({"session_id": id}), "the live request", |o| o.data["total"]["requests"] == 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(report_sessions(&h), 1, "the history was not read");
    assert_eq!(h.ok("usage_report", json!({})).data["history_on"], false);
    assert_eq!(h.ok("settings_get", json!({"key": "ClaudeUsageHistory"})).data["value"], false);

    // Turned on (a setting, no restart): the history comes in.
    h.ok("settings_set", json!({"key": "ClaudeUsageHistory", "value": true}));
    until(&h, "usage_report", json!({"from": "all"}), "the history", |o| o.data["sessions"] == 2);

    // Forgetting while it is read would read it again: refused.
    assert!(matches!(h.ui("usage_forget_history", json!({})), Err(ToolError::Refused(_))));
    let registry = h.engine.registry();
    assert!(registry.get("usage_forget_history").unwrap().effects.contains(Effects::MAY_DISCARD_DATA));

    // Off, then forget: the imported history goes, the live session's rows stay.
    h.ok("settings_set", json!({"key": "ClaudeUsageHistory", "value": false}));
    let deadline = Instant::now() + Duration::from_secs(10);
    let out = loop {
        match h.ui("usage_forget_history", json!({})) {
            Ok(o) => break o,
            Err(ToolError::Refused(_)) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => panic!("{e:?}"),
        }
    };
    assert_eq!(out.data["requests"], 1);
    assert_eq!(report_sessions(&h), 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(report_sessions(&h), 1, "not read again while the setting is off");
    assert_eq!(stored_requests(&ix), 1);

    // On again: read again from scratch (the reader forgot its positions too).
    h.ok("settings_set", json!({"key": "ClaudeUsageHistory", "value": true}));
    until(&h, "usage_report", json!({"from": "all"}), "the history again", |o| o.data["sessions"] == 2);
}

#[test]
fn a_clean_run_never_reads_past_history_and_leaves_the_add_on_as_it_was() {
    let root = tempfile::tempdir().unwrap();
    history_file(root.path());
    let (h, _ix) = harness(on_app(), root.path(), true);
    let (id, path) = claude(&h, root.path());
    std::fs::write(&path, asst("m1", now_ms() - 1_000, 10)).unwrap();
    until(&h, "usage_session", json!({"session_id": id}), "the session started in this run", |o| o.data["total"]["requests"] == 1);
    std::thread::sleep(Duration::from_millis(300));
    let rep = h.ok("usage_report", json!({"from": "all"}));
    assert_eq!(rep.data["sessions"], 1, "only this run's session: {}", rep.data);
    assert_eq!((rep.data["history_on"].as_bool(), rep.data["history_allowed"].as_bool()), (Some(false), Some(false)));
    // The stored on/off state and the history setting are as they were.
    let s = h.ok("settings_get", json!({}));
    assert_eq!(s.data["ClaudeUsageHistory"], true);
    assert_eq!(s.data["AddonApprovals"][CLAUDE_USAGE_ID], BUILTIN_DIGEST);
}
