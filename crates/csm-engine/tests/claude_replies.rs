//! `claude_reply_list`, `claude_reply_get` and `claude_code_block_get`: Claude's replies read
//! back from the transcript the usage reader follows (with the usage add-on off, as by default).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{ClaudeRoots, Effects, IndexSetting, ToolError, ToolOutput};
use serde_json::{Value, json};

/// Several turns, tool use, thinking, a sidechain line, and a last reply with a long one-line
/// PowerShell command and a JSON block.
const FIXTURE: &str = include_str!("../../csm-core/src/claude_reply_fixture.jsonl");

fn harness(root: &Path) -> H {
    let index = Index::start(IndexOptions::memory()).unwrap();
    let roots = vec![root.to_path_buf()];
    H::start(test_app(), move |o| {
        o.index = IndexSetting::Handle(index);
        o.claude_roots = ClaudeRoots::Dirs(roots);
        o.usage_poll = Duration::from_millis(30);
    })
}

/// A live Claude session and where its transcript goes.
fn claude(h: &H, root: &Path) -> (String, PathBuf) {
    let id = h.session("claude");
    let v = h.wait_until("an id", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some() && s.live));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    let dir = root.join("projects").join("C--work-shop");
    std::fs::create_dir_all(&dir).unwrap();
    (id, dir.join(format!("{cid}.jsonl")))
}

/// Calls `tool` until it answers (the reader finds the transcript within a poll or two).
fn until_ok(h: &H, tool: &str, params: Value) -> ToolOutput {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match h.ui(tool, params.clone()) {
            Ok(o) => return o,
            Err(ToolError::NotFound(m)) if m.contains("no transcript") => {}
            Err(e) => panic!("{tool}: {e:?}"),
        }
        assert!(Instant::now() < deadline, "timed out waiting for the transcript");
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_last_reply_its_list_and_its_code_blocks_come_from_the_transcript() {
    let root = tempfile::tempdir().unwrap();
    let h = harness(root.path());
    let (id, path) = claude(&h, root.path());
    std::fs::write(&path, FIXTURE).unwrap();

    let last = until_ok(&h, "claude_reply_get", json!({"session_id": id}));
    let text = last.data["text"].as_str().unwrap();
    assert!(text.starts_with("Run this in PowerShell:\n\n```powershell\nGet-ChildItem -Path C:\\work -Recurse -File | Where-Object"), "{text}");
    assert!(!text.contains("tool_use") && !text.contains("private thoughts"), "{text}");
    assert_eq!(last.data["index"], 0);
    assert_eq!(last.data["id"], "msg_03");
    let lines = text.lines().count();
    assert_eq!(last.message, format!("Claude's last reply ({lines} lines)"));
    assert_eq!(last.data["code_blocks"][0]["lang"], "powershell");

    let list = h.ok("claude_reply_list", json!({"session_id": id}));
    let rows = list.data["replies"].as_array().unwrap();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[0]["preview"], "Run this in PowerShell:");
    assert_eq!(rows[1]["preview"], "I'll look at the folder first.");
    assert_eq!(rows[0]["code_blocks"].as_array().unwrap().len(), 2);
    assert_eq!(rows[0]["code_blocks"][1]["lang"], "json");
    assert_eq!(rows[2]["code_blocks"].as_array().unwrap().len(), 0);

    // By index and by id.
    let older = h.ok("claude_reply_get", json!({"session_id": id, "index": 2}));
    assert_eq!(older.data["text"], "Hello! What can I do for you?");
    let by_id = h.ok("claude_reply_get", json!({"session_id": id, "reply_id": rows[1]["id"]}));
    assert!(by_id.data["text"].as_str().unwrap().contains("- shop\n- blog"));
    assert_eq!(by_id.data["index"], 1);

    // The PowerShell command is one line, exactly as written.
    let block = h.ok("claude_code_block_get", json!({"session_id": id}));
    let code = block.data["code"].as_str().unwrap();
    assert!(!code.contains('\n') && code.ends_with("-First 20 FullName, Length"), "{code}");
    assert_eq!(block.data["lang"], "powershell");
    assert_eq!(block.message, "Code block 1 (powershell 1 line)");
    let json_block = h.ok("claude_code_block_get", json!({"session_id": id, "block_index": 1}));
    assert_eq!(json_block.data["code"], "{\n  \"a\": 1\n}");

    // Out of range, with a way on.
    match h.ui("claude_code_block_get", json!({"session_id": id, "reply_index": 1})) {
        Err(ToolError::NotFound(m)) => assert!(m.contains("no code block"), "{m}"),
        other => panic!("{other:?}"),
    }
    match h.ui("claude_code_block_get", json!({"session_id": id, "block_index": 5})) {
        Err(ToolError::NotFound(m)) => assert!(m.contains("2 code blocks"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(h.ui("claude_reply_get", json!({"session_id": id, "index": 7})), Err(ToolError::NotFound(_))));
    assert!(matches!(h.ui("claude_reply_get", json!({"session_id": id, "index": 500})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("claude_reply_list", json!({"session_id": id, "limit": 0})), Err(ToolError::InvalidParams(_))));

    // A new turn: index 0 follows it.
    let more = [
        json!({"type": "user", "message": {"role": "user", "content": "thanks"}}),
        json!({"type": "assistant", "message": {"id": "msg_04", "content": [{"type": "text", "text": "You're welcome."}]}}),
    ];
    let mut all = FIXTURE.trim_end().to_owned();
    for l in more {
        all.push('\n');
        all.push_str(&l.to_string());
    }
    all.push('\n');
    std::fs::write(&path, all).unwrap();
    let last = h.ok("claude_reply_get", json!({"session_id": id}));
    assert_eq!(last.data["text"], "You're welcome.");
    assert_eq!(last.message, "Claude's last reply (1 line)");
}

#[test]
fn read_only_session_content_and_only_for_local_claude_sessions() {
    let root = tempfile::tempdir().unwrap();
    let h = harness(root.path());
    let tools = h.ok("tool_list", json!({}));
    for id in ["claude_reply_list", "claude_reply_get", "claude_code_block_get"] {
        let t = tools.data.as_array().unwrap().iter().find(|t| t["id"] == id).unwrap_or_else(|| panic!("{id} is registered"));
        let effects: Effects = serde_json::from_value(t["effects"].clone()).unwrap();
        assert_eq!(effects, Effects::IDEMPOTENT | Effects::READS_SESSION_CONTENT, "{id}");
    }

    let ssh = h.ok("session_new", json!({"command": "claude", "kind": "ssh", "ssh": {"host": "box"}, "focus": false})).data["id"].as_str().unwrap().to_owned();
    match h.ui("claude_reply_get", json!({"session_id": ssh})) {
        Err(ToolError::Refused(m)) => assert!(m.starts_with("not available for this session"), "{m}"),
        other => panic!("{other:?}"),
    }
    let shell = h.session("pwsh");
    match h.ui("claude_reply_get", json!({"session_id": shell})) {
        Err(ToolError::Refused(m)) => assert!(m.contains("does not run Claude"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(h.ui("claude_reply_get", json!({"session_id": "nope"})), Err(ToolError::NotFound(_))));
    // A Claude session that has written nothing yet.
    let (id, _) = claude(&h, root.path());
    match h.ui("claude_reply_get", json!({"session_id": id})) {
        Err(ToolError::NotFound(m)) => assert!(m.contains("no transcript"), "{m}"),
        other => panic!("{other:?}"),
    }
}
