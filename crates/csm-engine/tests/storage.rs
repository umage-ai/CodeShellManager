//! Storage tools: `storage_info` reports the index, `storage_clean` prunes it (the retention
//! rules themselves are tested in csm-index), and the settings that drive it are checked.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{H, test_app};
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{IndexSetting, ToolError};
use serde_json::json;

fn with_index() -> (H, Index) {
    let ix = Index::start(IndexOptions::memory()).unwrap();
    let h = H::start(test_app(), {
        let ix = ix.clone();
        move |o| o.index = IndexSetting::Handle(ix)
    });
    (h, ix)
}

#[test]
fn storage_info_reports_the_index_and_its_limits() {
    let (h, _ix) = with_index();
    let out = h.ok("storage_info", json!({}));
    assert!(out.message.starts_with("Search index:"), "{}", out.message);
    assert!(out.data["index"]["file_bytes"].as_i64().is_some(), "{}", out.data);
    assert_eq!(out.data["limits"]["index_max_mb"], 1024);
    assert_eq!(out.data["limits"]["claude_usage_retention_days"], 400);
    assert_eq!(out.data["limits"]["closed_session_grace_days"], 30);
}

#[test]
fn storage_clean_removes_a_gone_sessions_old_output_and_keeps_the_open_ones() {
    let (h, ix) = with_index();
    let open = h.session("pwsh");
    // An old session that is no longer in the list, seen 90 days ago, and the open one, also old.
    let old = 1_700_000_000_000i64;
    for sid in ["gone", open.as_str()] {
        let sid = sid.to_owned();
        ix.call_blocking(move |db| db.insert_output(&sid, old, &[format!("{sid} said hello")], 1 << 20)).unwrap();
    }
    let out = h.ok("storage_clean", json!({}));
    assert_eq!(out.data["sessions_dropped"], 1, "{}", out.data);
    let left: Vec<String> = ix
        .call_blocking(|db| Ok(db.search_output("hello", 10, None)?.into_iter().map(|hit| hit.session_id).collect()))
        .unwrap();
    assert_eq!(left, vec![open.clone()], "the open session keeps its output whatever its age");
    // The engine's own clean-up stays scheduled; a second clean finds nothing more.
    assert_eq!(h.ok("storage_clean", json!({})).data["sessions_dropped"], 0);
}

#[test]
fn storage_settings_are_checked() {
    let (h, _ix) = with_index();
    h.ok("settings_set", json!({"key": "IndexMaxMb", "value": 2048}));
    assert!(matches!(h.ui("settings_set", json!({"key": "IndexMaxMb", "value": 10})), Err(ToolError::InvalidParams(_))));
    h.ok("settings_set", json!({"key": "ClaudeUsageRetentionDays", "value": 0}));
    assert!(matches!(h.ui("settings_set", json!({"key": "ClaudeUsageRetentionDays", "value": -1})), Err(ToolError::InvalidParams(_))));
    assert_eq!(h.ok("storage_info", json!({})).data["limits"]["index_max_mb"], 2048);
}

#[test]
fn without_an_index_the_tools_say_so() {
    let h = H::start(test_app(), |_| {});
    assert!(matches!(h.ui("storage_info", json!({})), Err(ToolError::Refused(_))));
    assert!(matches!(h.ui("storage_clean", json!({})), Err(ToolError::Refused(_))));
}
