//! The sidebar's bulk actions, its one-shot sort and Duplicate, through `Engine::call`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{H, osc7501, test_app};
use csm_core::models::{AppState, RunCommandItem, SessionGroup, ShellSession};
use csm_core::time::Timestamp;
use csm_engine::{Status, ToolError};
use serde_json::{Value, json};

fn order(h: &H) -> Vec<String> {
    h.engine.view().sessions.iter().map(|s| s.id.clone()).collect()
}

fn asleep(id: &str, name: &str, group: &str, folder: &str, active_ms: i64) -> ShellSession {
    ShellSession {
        id: id.into(),
        name: name.into(),
        group_id: group.into(),
        working_folder: folder.into(),
        command: "pwsh".into(),
        is_dormant: true,
        last_activity_at: Timestamp::from_epoch_ms(active_ms),
        ..Default::default()
    }
}

fn sleeping_app() -> AppState {
    let mut app = test_app();
    app.groups.push(SessionGroup { id: "g".into(), name: "G".into(), sort_order: 0, is_expanded: true });
    // Group g holds c, a, b, interleaved with ungrouped y, x.
    app.sessions = vec![
        asleep("c", "charlie", "g", r"C:\3", 300),
        asleep("y", "yankee", "", r"C:\9", 100),
        asleep("a", "Alpha", "g", r"C:\1", 200),
        asleep("x", "xray", "", r"C:\8", 400),
        asleep("b", "bravo", "g", r"C:\2", 100),
    ];
    app
}

#[test]
fn sort_reorders_once_by_field_direction_and_scope() {
    let h = H::start(sleeping_app(), |_| {});
    // One group: only its slots change.
    let out = h.ok("session_sort", json!({"by": "name", "group": "g"}));
    assert_eq!(out.data["changed"], true);
    assert_eq!(order(&h), ["a", "y", "b", "x", "c"]);
    // Everything, descending.
    h.ok("session_sort", json!({"by": "name", "descending": true}));
    assert_eq!(order(&h), ["y", "x", "c", "b", "a"]);
    // Last active defaults to newest first; equal times keep their order (stable).
    let out = h.ok("session_sort", json!({"by": "last_active"}));
    assert_eq!(out.data["descending"], true);
    assert_eq!(order(&h), ["x", "c", "a", "y", "b"]);
    // The ungrouped ones only (x and y swap; the group's rows stay put).
    h.ok("session_sort", json!({"by": "folder", "group": "", "descending": true}));
    assert_eq!(order(&h), ["y", "c", "a", "x", "b"]);
    // Sorting again changes nothing.
    let out = h.ok("session_sort", json!({"by": "folder", "group": "", "descending": true}));
    assert_eq!(out.data["changed"], false);
    // Bad input is refused, not guessed.
    assert!(matches!(h.ui("session_sort", json!({"by": "size"})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("session_sort", json!({"by": "name", "group": "nope"})), Err(ToolError::NotFound(_))));
}

#[test]
fn sort_by_git_branch_puts_sessions_without_one_last() {
    let h = H::new();
    let ids: Vec<String> = (0..3).map(|_| h.session("pwsh")).collect();
    // The second has no branch; the others push theirs (OSC 9001).
    h.pty(0).emit(b"\x1b]9001;git-branch=zeta\x1b\\");
    h.pty(2).emit(b"\x1b]9001;git-branch=alpha\x1b\\");
    h.wait_until("branches", |v| v.sessions.iter().filter(|s| s.branch.is_some()).count() == 2);
    h.ok("session_sort", json!({"by": "branch"}));
    assert_eq!(order(&h), [ids[2].clone(), ids[0].clone(), ids[1].clone()]);
    h.ok("session_sort", json!({"by": "branch", "descending": true}));
    assert_eq!(order(&h), [ids[0].clone(), ids[2].clone(), ids[1].clone()], "still last when descending");
}

#[test]
fn duplicate_copies_the_configuration_and_lands_right_after_the_original() {
    let h = H::new();
    let a = h.session("pwsh");
    let b = h.session("pwsh");
    h.ok("session_rename", json!({"session_id": a, "name": "api"}));
    h.ok("session_edit", json!({"session_id": a, "args": "-NoLogo"}));
    h.ok("run_edit", json!({"session_id": a, "runs": [{"label": "build", "command_line": "cargo build", "is_default": true}]}));
    let g = h.ok("group_new", json!({"name": "G"})).data["group_id"].as_str().unwrap().to_owned();
    h.ok("session_move_to_group", json!({"session_id": a, "group_id": g}));
    h.ok("session_move_to_group", json!({"session_id": b, "group_id": g}));
    let before = h.pty_count();

    let out = h.ok("session_duplicate", json!({"session_id": a}));
    let copy = out.data["session_id"].as_str().unwrap().to_owned();
    assert_eq!(out.data["name"], "api (2)");
    assert_eq!(order(&h), [a.clone(), copy.clone(), b.clone()], "placed right after the original");
    let v = h.wait_status(&copy, Status::Idle);
    assert!(h.pty_count() > before, "the copy started its own process");
    let s = v.session(&copy).unwrap();
    assert_eq!(s.group_id, g);
    assert_eq!(v.focused.as_deref(), Some(copy.as_str()), "the app focuses the copy");
    let cfg = h.ok("session_get", json!({"session_id": copy})).data;
    assert_eq!(cfg["config"]["command"], "pwsh");
    assert_eq!(cfg["config"]["args"], "-NoLogo");
    let runs = h.ok("run_list", json!({"session_id": copy})).data;
    let orig = h.ok("run_list", json!({"session_id": a})).data;
    let run_id = |d: &Value| d["runs"][0]["id"].as_str().map(str::to_owned).or_else(|| d[0]["id"].as_str().map(str::to_owned));
    assert!(run_id(&runs).is_some(), "the run commands came along: {runs}");
    assert_ne!(run_id(&runs), run_id(&orig), "with their own ids");

    // A second copy counts on.
    let out = h.ok("session_duplicate", json!({"session_id": a, "focus": false}));
    assert_eq!(out.data["name"], "api (3)");
    assert!(matches!(h.ui("session_duplicate", json!({"session_id": "nope"})), Err(ToolError::NotFound(_))));
}

#[test]
fn move_many_keeps_the_block_together_and_checks_every_id_first() {
    let h = H::new();
    let ids: Vec<String> = (0..5).map(|_| h.session("pwsh")).collect();
    let (a, b, c, d, e) = (&ids[0], &ids[1], &ids[2], &ids[3], &ids[4]);
    let g = h.ok("group_new", json!({"name": "G"})).data["group_id"].as_str().unwrap().to_owned();
    // To the end of an empty group: they keep their order.
    h.ok("session_move_many", json!({"session_ids": [d, b], "group_id": g}));
    let in_g = |h: &H| -> Vec<String> {
        h.ok("session_list", json!({"group": g})).data.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_owned()).collect()
    };
    assert_eq!(in_g(&h), [b.clone(), d.clone()]);
    // Before an anchor in the group.
    h.ok("session_move_many", json!({"session_ids": [e, a], "group_id": g, "before": d}));
    assert_eq!(in_g(&h), [b.clone(), a.clone(), e.clone(), d.clone()]);
    // An unknown id changes nothing.
    let r = h.ui("session_move_many", json!({"session_ids": [c, "nope"], "group_id": g}));
    assert!(matches!(r, Err(ToolError::NotFound(_))));
    assert_eq!(in_g(&h).len(), 4);
    // The anchor can't be one of the moved; an empty list is refused.
    let r = h.ui("session_move_many", json!({"session_ids": [a, b], "group_id": g, "after": a}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("session_move_many", json!({"session_ids": []})), Err(ToolError::InvalidParams(_))));
    // Back to Ungrouped, after c.
    h.ok("session_move_many", json!({"session_ids": [a, b], "group_id": null, "after": c}));
    let v = h.engine.view();
    let pos = |id: &str| v.sessions.iter().position(|s| s.id == id).unwrap();
    assert_eq!((pos(b), pos(a)), (pos(c) + 1, pos(c) + 2), "in sidebar order, right after c");
}

#[test]
fn sleep_wake_restart_and_close_many() {
    let h = H::new();
    let ids: Vec<String> = (0..3).map(|_| h.session("pwsh")).collect();
    let two = json!([ids[0], ids[1]]);

    let out = h.ok("session_sleep_many", json!({"session_ids": two}));
    assert_eq!(out.data["slept"].as_array().unwrap().len(), 2);
    h.wait_status(&ids[0], Status::Asleep);
    h.wait_status(&ids[1], Status::Asleep);
    let out = h.ok("session_sleep_many", json!({"session_ids": two}));
    assert_eq!(out.data["slept"].as_array().unwrap().len(), 0, "already asleep: skipped");

    let out = h.ok("session_wake_many", json!({"session_ids": [ids[0], ids[1], ids[2]]}));
    assert_eq!(out.data["woken"].as_array().unwrap().len(), 2, "the awake one is skipped");
    h.wait_status(&ids[0], Status::Idle);
    h.wait_status(&ids[1], Status::Idle);

    // More than one restart needs force (the app confirms first).
    let r = h.ui("session_restart_many", json!({"session_ids": two}));
    assert!(matches!(r, Err(ToolError::Refused(ref m)) if m.contains("force")), "{r:?}");
    let before = h.pty_count();
    let out = h.ok("session_restart_many", json!({"session_ids": two, "force": true}));
    assert_eq!(out.data["restarted"], 2, "{}", out.message);
    assert_eq!(h.pty_count(), before + 2);

    // A busy one refuses the close until forced.
    let busy = &ids[2];
    h.pty(2).emit(&osc7501("state=working"));
    h.wait_status(busy, Status::Working);
    let r = h.ui("session_close_many", json!({"session_ids": [ids[0], busy]}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    assert_eq!(h.engine.view().sessions.len(), 3, "nothing closed");
    let out = h.ok("session_close_many", json!({"session_ids": [ids[0], busy], "force": true}));
    assert_eq!(out.data["closed"].as_array().unwrap().len(), 2);
    assert_eq!(order(&h), [ids[1].clone()]);
    assert_eq!(h.ok("recently_closed_list", json!({})).data.as_array().unwrap().len(), 2);
}

#[test]
fn duplicate_keeps_the_run_commands_it_was_given() {
    // Not a fresh detection: the parent's list, verbatim apart from the ids.
    let mut app = test_app();
    app.sessions.push(ShellSession {
        id: "p".into(),
        name: "api (2)".into(),
        command: "pwsh".into(),
        working_folder: std::env::temp_dir().to_string_lossy().into_owned(),
        run_commands: vec![RunCommandItem { id: "r1".into(), label: "test".into(), command_line: "cargo test".into(), ..Default::default() }],
        is_dormant: true,
        ..Default::default()
    });
    let h = H::start(app, |_| {});
    let out = h.ok("session_duplicate", json!({"session_id": "p", "focus": false}));
    assert_eq!(out.data["name"], "api (3)");
    let copy = out.data["session_id"].as_str().unwrap().to_owned();
    h.wait_status(&copy, Status::Idle);
    assert_eq!(h.status("p"), Status::Asleep, "the original is left as it was");
    let runs = h.ok("run_list", json!({"session_id": copy})).data.to_string();
    assert!(runs.contains("cargo test") && !runs.contains("\"r1\""), "{runs}");
}
