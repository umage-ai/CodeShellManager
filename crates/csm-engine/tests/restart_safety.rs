//! A restart whose worker thread cannot be started must not leave the session stuck or the
//! restart guard held (COR-8).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{H, test_app};
use csm_engine::ToolError;
use serde_json::json;

#[test]
fn a_restart_that_cannot_start_its_worker_fails_cleanly_and_releases_the_guard() {
    let h = H::start(test_app(), |o| o.fail_worker_spawn = true);
    let id = h.session("pwsh");
    let err = h.ui("session_restart", json!({"session_id": id})).unwrap_err();
    assert!(format!("{err:?}").contains("restart worker"), "{err:?}");
    // Asleep, not stuck in "starting"; the old process was killed inline.
    h.wait_until("the session asleep", |v| v.sessions.iter().any(|s| s.id == id && s.status == csm_engine::Status::Asleep));
    assert!(h.pty_of(&id).killed.load(std::sync::atomic::Ordering::Acquire));
    // The guard is free: the next restart is not refused with "already running".
    let err = h.ui("session_restart", json!({"session_id": id})).unwrap_err();
    assert!(!matches!(&err, ToolError::Refused(m) if m.contains("already running")), "{err:?}");
}
