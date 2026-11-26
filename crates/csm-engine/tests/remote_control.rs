//! Claude Code Remote Control end to end: the transcript reader sees Claude's `bridge_status` line
//! and the session reports `remote_control` (with its link) through `session_list` /
//! `session_get`; a disconnect line, or the process ending, turns it off; a line from before the
//! process started and garbage do not turn it on.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use common::{H, test_app};
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{ClaudeRoots, IndexSetting};
use serde_json::json;

const URL: &str = "https://claude.ai/code/session_01W2nwy51AsMgseViHRgKABa";

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
}

fn iso(ms: i64) -> String {
    csm_core::time::Timestamp::from_epoch_ms(ms).as_str().to_owned()
}

fn harness(root: &Path) -> H {
    let roots = vec![root.to_path_buf()];
    H::start(test_app(), move |o| {
        o.index = IndexSetting::Handle(Index::start(IndexOptions::memory()).unwrap());
        o.claude_roots = ClaudeRoots::Dirs(roots);
        o.usage_poll = Duration::from_millis(40);
    })
}

fn transcript_for(root: &Path, cid: &str) -> PathBuf {
    let dir = root.join("projects").join("C--work-shop");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(format!("{cid}.jsonl"))
}

fn on_line(ts: i64, url: &str) -> String {
    json!({"type": "system", "subtype": "bridge_status", "content": format!("/remote-control is active \u{b7} Continue here, on your phone, or at {url}"),
        "url": url, "isMeta": false, "timestamp": iso(ts), "cwd": "C:\\work\\shop"})
    .to_string()
        + "\n"
}

fn off_line(ts: i64) -> String {
    json!({"type": "system", "subtype": "local_command", "content": "<local-command-stdout>Remote Control disconnected.</local-command-stdout>",
        "level": "info", "timestamp": iso(ts)})
    .to_string()
        + "\n"
}

fn append(path: &Path, text: &str) {
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

/// A marker line the reader has certainly seen once the usage tools count it.
fn marker(ts: i64, id: &str) -> String {
    json!({"type": "assistant", "timestamp": iso(ts), "message": {"id": id, "model": "claude-opus-5-5", "usage": {"input_tokens": 1}}}).to_string() + "\n"
}

/// Waits until the reader has read the marker: its context fill (`usage_context` answers it with
/// the Claude usage add-on off, as here: Remote Control does not need the add-on).
fn read_through(h: &H, id: &str, fill: u64) {
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    let filled = || {
        let out = h.ok("usage_context", json!({}));
        out.data["sessions"].as_array().unwrap().iter().any(|s| s["session_id"] == json!(id) && s["context"]["fill_tokens"] == fill)
    };
    while !filled() {
        assert!(std::time::Instant::now() < deadline, "the reader did not get to a fill of {fill}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn remote_control_follows_the_transcript_and_the_process() {
    let root = tempfile::tempdir().unwrap();
    let h = harness(root.path());
    let id = h.session("claude");
    let v = h.wait_until("an id", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some() && s.live));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    let path = transcript_for(root.path(), &cid);
    let now = now_ms();

    // From an earlier run of the conversation (before this process started), plus garbage: off.
    append(&path, &on_line(now - 3_600_000, URL));
    append(&path, "{\"type\":\"system\",\"subtype\":\"bridge_status\",\"url\":42}\n{broken\n\u{1b}]8;;\u{7}\n");
    append(&path, &json!({"type": "system", "subtype": "bridge_status", "url": URL}).to_string());
    append(&path, "\n");
    append(&path, &marker(now, "m1"));
    read_through(&h, &id, 1);
    std::thread::sleep(Duration::from_millis(150));
    let s = h.ok("session_get", json!({"session_id": id}));
    assert_eq!(s.data["session"]["remote_control"], false, "{}", s.data["session"]);

    // Turned on in this run: on, with the link.
    append(&path, &on_line(now_ms() + 10, URL));
    let v = h.wait_until("remote control on", |v| v.session(&id).is_some_and(|s| s.remote_control));
    assert_eq!(v.session(&id).unwrap().remote_control_url.as_deref(), Some(URL));
    let list = h.ok("session_list", json!({}));
    let row = list.data.as_array().unwrap().iter().find(|r| r["id"] == json!(id)).unwrap().clone();
    assert_eq!(row["remote_control"], true);
    assert_eq!(row["remote_control_url"], URL);

    // Disconnected: off.
    append(&path, &off_line(now_ms() + 20));
    h.wait_until("remote control off", |v| v.session(&id).is_some_and(|s| !s.remote_control && s.remote_control_url.is_none()));

    // On again, with a link that is not http(s): on, without a link.
    append(&path, &on_line(now_ms() + 30, "javascript:alert(1)"));
    let v = h.wait_until("on without a link", |v| v.session(&id).is_some_and(|s| s.remote_control));
    assert_eq!(v.session(&id).unwrap().remote_control_url, None);

    // The process ends: off.
    h.pty_of(&id).exit(Some(0));
    h.wait_until("off with the process", |v| v.session(&id).is_some_and(|s| !s.remote_control));
}

#[test]
fn a_shell_session_never_reports_remote_control() {
    let root = tempfile::tempdir().unwrap();
    let h = harness(root.path());
    let id = h.session("pwsh");
    h.wait_until("live", |v| v.session(&id).is_some_and(|s| s.live));
    let s = h.ok("session_get", json!({"session_id": id}));
    assert_eq!(s.data["session"]["remote_control"], false);
    assert_eq!(s.data["session"]["remote_control_url"], serde_json::Value::Null);
}

/// Claude's live per-process file, `<config dir>/sessions/<pid>.json`.
fn sessions_file(root: &Path, pid: u32, cid: &str, started_ms: i64, bridge: Option<&str>) {
    let dir = root.join("sessions");
    std::fs::create_dir_all(&dir).unwrap();
    let mut v = json!({"pid": pid, "sessionId": cid, "cwd": "C:/work/shop", "startedAt": started_ms, "version": "2.1.332", "kind": "interactive", "status": "idle"});
    if let Some(b) = bridge {
        v["bridgeSessionId"] = json!(b);
    }
    std::fs::write(dir.join(format!("{pid}.json")), v.to_string()).unwrap();
    // Claude keeps other files in the folder; they are not read.
    std::fs::write(dir.join(format!("{pid}.abc123.key")), "not json").unwrap();
}

#[test]
fn a_resumed_session_is_marked_from_claudes_sessions_file() {
    let root = tempfile::tempdir().unwrap();
    let h = harness(root.path());
    let id = h.session("claude");
    let v = h.wait_until("an id", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some() && s.live));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    let path = transcript_for(root.path(), &cid);
    let now = now_ms();

    // The transcript only has the line of an earlier run: not on by the transcript rule.
    append(&path, &on_line(now - 3_600_000, "https://claude.ai/code/session_old"));
    append(&path, &marker(now, "m1"));
    read_through(&h, &id, 1);
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(h.ok("session_get", json!({"session_id": id})).data["session"]["remote_control"], false);

    // The resumed process reconnected silently: its sessions file says so.
    sessions_file(root.path(), 4242, &cid, now_ms(), Some("session_01RESUMEDabc"));
    let v = h.wait_until("on from the sessions file", |v| v.session(&id).is_some_and(|s| s.remote_control));
    assert_eq!(v.session(&id).unwrap().remote_control_url.as_deref(), Some("https://claude.ai/code/session_01RESUMEDabc"));

    // Remote Control turned off: the file loses its bridge id.
    sessions_file(root.path(), 4242, &cid, now_ms(), None);
    h.wait_until("off from the sessions file", |v| v.session(&id).is_some_and(|s| !s.remote_control && s.remote_control_url.is_none()));

    // The process ends: off, whatever the file says.
    sessions_file(root.path(), 4242, &cid, now_ms(), Some("session_01RESUMEDabc"));
    h.wait_until("on again", |v| v.session(&id).is_some_and(|s| s.remote_control));
    h.pty_of(&id).exit(Some(0));
    h.wait_until("off with the process", |v| v.session(&id).is_some_and(|s| !s.remote_control));
}

#[test]
fn a_stale_sessions_file_is_ignored_and_the_transcript_rule_applies() {
    let root = tempfile::tempdir().unwrap();
    let h = harness(root.path());
    let id = h.session("claude");
    let v = h.wait_until("an id", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some() && s.live));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    let path = transcript_for(root.path(), &cid);
    let now = now_ms();

    // A file from a process that started an hour ago (a crashed earlier run): not this one.
    sessions_file(root.path(), 111, &cid, now - 3_600_000, Some("session_01STALEabc"));
    append(&path, &marker(now, "m1"));
    read_through(&h, &id, 1);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(h.ok("session_get", json!({"session_id": id})).data["session"]["remote_control"], false);

    // The transcript rule still works beside it.
    append(&path, &on_line(now_ms() + 10, URL));
    let v = h.wait_until("on from the transcript", |v| v.session(&id).is_some_and(|s| s.remote_control));
    assert_eq!(v.session(&id).unwrap().remote_control_url.as_deref(), Some(URL));
}
