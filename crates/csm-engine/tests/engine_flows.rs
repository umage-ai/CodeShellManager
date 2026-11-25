//! The engine end to end through `Engine::call`, on a fake PTY.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use common::{H, osc7501, test_app};
use csm_core::models::{AppState, SessionGroup, ShellSession};
use csm_engine::{Caller, Event, GridSize, State, Status, ToolError};
use serde_json::json;

fn ext(session: Option<&str>) -> Caller {
    Caller::External { client: "test-mcp".into(), session_id: session.map(str::to_owned) }
}

#[test]
fn a_new_session_starts_and_its_status_follows_osc_7501() {
    let h = H::new();
    let id = h.session("claude");
    let pty = h.pty(0);
    assert!(pty.argv.iter().any(|a| a == "claude" || a.starts_with("claude ")), "{:?}", pty.argv);
    assert_eq!((pty.first_size.cols, pty.first_size.rows), (220, 50), "headless: the fallback size");

    // The probe is answered, before the DA1 that follows it (Claude's "no more replies" sentinel).
    pty.emit(b"\x1b]7501;?\x1b\\\x1b[c");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pty.written_text().contains("\x1b[?") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let w = pty.written_text();
    let probe = w.find("\x1b]7501;?\x1b\\").expect("probe answered");
    let da1 = w.find("\x1b[?").expect("DA1 answered");
    assert!(probe < da1, "{w:?}");

    pty.emit(&osc7501("state=working:progress=40"));
    let v = h.wait_status(&id, Status::Working);
    assert_eq!(v.session(&id).unwrap().progress, Some(40));
    assert!(v.session(&id).unwrap().reports_status);

    pty.emit(&osc7501("state=blocked:kind=permission"));
    let v = h.wait_status(&id, Status::NeedsYouApproval);
    assert_eq!(v.inbox, vec![id.clone()]);
    let inbox = h.ok("inbox_list", json!({}));
    assert_eq!(inbox.data[0]["id"], id);

    pty.emit(&osc7501("state=blocked:kind=question"));
    h.wait_status(&id, Status::NeedsYouQuestion);
    pty.emit(&osc7501("state=done"));
    h.wait_status(&id, Status::DoneUnseen);
    // Focusing acknowledges the result.
    h.ok("session_focus", json!({"session_id": id}));
    let v = h.wait_status(&id, Status::Idle);
    assert!(v.inbox.is_empty());
    assert_eq!(v.focused.as_deref(), Some(id.as_str()));

    // A malformed report is dropped whole; clear re-arms the heuristic.
    pty.emit(&osc7501("state=bogus"));
    pty.emit(&osc7501("state=working"));
    h.wait_status(&id, Status::Working);
    pty.emit(&osc7501("state=clear"));
    let v = h.wait_status(&id, Status::Idle);
    assert!(!v.session(&id).unwrap().reports_status);
}

#[test]
fn osc_9001_sets_branch_title_and_accent() {
    let h = H::new();
    let id = h.session("pwsh");
    h.pty(0).emit(b"\x1b]9001;git-branch=feat/x;git-dirty=1;title=Odulodo;color=#112233\x1b\\");
    let v = h.wait_until("9001 applied", |v| v.session(&id).is_some_and(|s| s.branch.as_deref() == Some("feat/x")));
    let s = v.session(&id).unwrap();
    assert!(s.git_dirty);
    assert_eq!(s.name, "Odulodo");
    assert_eq!(s.accent, "#112233");
    // Hostile values are rejected, not applied.
    h.pty(0).emit(b"\x1b]9001;color=red;title=\x01\x02\x1b\\");
    std::thread::sleep(Duration::from_millis(100));
    let v = h.engine.view();
    assert_eq!(v.session(&id).unwrap().accent, "#112233");
    assert_eq!(v.session(&id).unwrap().name, "Odulodo");
}

#[test]
fn the_first_pty_size_is_the_panes_measured_grid() {
    let h = H::start(test_app(), |o| o.first_size_timeout = Duration::from_secs(30));
    let folder = h.dir.path().to_string_lossy().into_owned();
    let id = h.ok("session_new", json!({"folder": folder, "command": "pwsh"})).data["id"].as_str().unwrap().to_owned();
    assert_eq!(h.status(&id), Status::Starting);
    let handle = h.wait_until("a terminal handle", |v| v.terminal(&id).is_some()).terminal(&id).cloned().unwrap();
    std::thread::sleep(Duration::from_millis(150));
    assert_eq!(h.pty_count(), 0, "no PTY before the pane is measured");
    handle.resize(GridSize::new(101, 33));
    let pty = h.pty(0);
    assert_eq!((pty.first_size.cols, pty.first_size.rows), (101, 33));
    h.wait_status(&id, Status::Idle);
    assert!(handle.is_started());
    handle.resize(GridSize::new(90, 20));
    assert_eq!(*pty.size.lock().unwrap(), csm_pty::PtySize::new(90, 20));
    // Keystrokes go straight to the PTY.
    handle.send_key(csm_engine::input::typed("ls", false));
    assert!(pty.written_text().contains("ls"));
}

#[test]
fn external_callers_need_consent_for_sensitive_tools_only() {
    let h = H::new();
    let id = h.session("pwsh");
    let events = h.engine.subscribe();
    assert!(h.engine.call(ext(None), "session_list", json!({})).is_ok(), "reading the list needs no consent");
    let rx = h.engine.submit(ext(None), "session_send_text", json!({"session_id": id, "text": "hi"}));
    let deadline = Instant::now() + Duration::from_secs(5);
    let request = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ConsentRequested { request }) => break request,
            Ok(_) if Instant::now() < deadline => continue,
            other => panic!("expected a consent request, got {other:?}"),
        }
    };
    assert_eq!(request["tool"], "session_send_text");
    assert!(rx.try_recv().is_err(), "must wait for the person");
    for tool in ["session_read_screen", "session_close", "session_new", "session_restart"] {
        let r = h.engine.submit(ext(None), tool, json!({"session_id": id}));
        std::thread::sleep(Duration::from_millis(30));
        assert!(r.try_recv().is_err(), "{tool} must wait for consent");
    }
    h.ok("consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"}));
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert!(h.pty(0).written_text().contains("hi"));
}

#[test]
fn a_session_cannot_type_into_itself_even_before_consent() {
    let h = H::new();
    let id = h.session("claude");
    let events = h.engine.subscribe();
    let r = h.engine.call(ext(Some(&id)), "session_send_text", json!({"session_id": id, "text": "rm -rf /"}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    assert!(!matches!(events.try_recv(), Ok(Event::ConsentRequested { .. })), "no card for a refused call");
    assert!(!h.pty(0).written_text().contains("rm -rf"));
}

#[test]
fn send_text_pastes_bracketed_submits_separately_and_refuses_mid_turn() {
    let h = H::new();
    let id = h.session("claude");
    let pty = h.pty(0);
    pty.emit(b"\x1b[?2004h");
    pty.emit(&osc7501("state=working"));
    h.wait_status(&id, Status::Working);
    let r = h.ui("session_send_text", json!({"session_id": id, "text": "hello"}));
    assert!(matches!(r, Err(ToolError::Refused(m)) if m.contains("working")));
    h.ok("session_send_text", json!({"session_id": id, "text": "hello\x1b[201~evil", "submit": true, "force": true}));
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pty.written_text().ends_with('\r') && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let w = pty.written_text();
    assert!(w.contains("\x1b[200~hello[201~evil\x1b[201~\r"), "{w:?}");
}

#[test]
fn close_refuses_a_busy_session_without_force_and_reopen_brings_it_back() {
    let h = H::new();
    let id = h.session("claude");
    h.pty(0).emit(&osc7501("state=working"));
    h.wait_status(&id, Status::Working);
    let r = h.ui("session_close", json!({"session_id": id}));
    assert!(matches!(r, Err(ToolError::Refused(m)) if m.contains("busy")));
    h.ok("session_close", json!({"session_id": id, "force": true}));
    h.wait_until("closed", |v| v.session(&id).is_none());
    let pty = h.pty(0);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pty.has_exited_fake() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(pty.closed.load(Ordering::SeqCst), "a Claude session is closed gracefully first");
    let rc = h.ok("recently_closed_list", json!({}));
    assert_eq!(rc.data.as_array().unwrap().len(), 1);
    let again = h.ok("session_reopen_closed", json!({}));
    let new_id = again.data["session_id"].as_str().unwrap().to_owned();
    assert_ne!(new_id, id);
    h.wait_status(&new_id, Status::Idle);
    assert_eq!(h.ok("recently_closed_list", json!({})).data.as_array().unwrap().len(), 0);

    // A session that does not speak 7501 is busy while it prints.
    let id2 = h.session("pwsh");
    h.pty(h.pty_count() - 1).emit(b"compiling...\r\n");
    std::thread::sleep(Duration::from_millis(50));
    let r = h.ui("session_close", json!({"session_id": id2}));
    assert!(matches!(r, Err(ToolError::Refused(m)) if m.contains("producing output")));
}

trait Exited {
    fn has_exited_fake(&self) -> bool;
}
impl Exited for common::FakePty {
    fn has_exited_fake(&self) -> bool {
        csm_pty::Pty::has_exited(self)
    }
}

#[test]
fn sleep_wake_and_restart_keep_the_session_and_order_its_processes() {
    let h = H::new();
    let id = h.session("claude");
    let first = h.pty(0);
    h.ok("session_focus", json!({"session_id": id}));

    h.ok("session_sleep", json!({"session_id": id}));
    let v = h.wait_status(&id, Status::Asleep);
    assert!(v.terminal(&id).is_none());
    assert!(v.session(&id).unwrap().asleep);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !first.killed.load(Ordering::SeqCst) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(first.closed.load(Ordering::SeqCst) && first.killed.load(Ordering::SeqCst));
    assert!(matches!(h.ui("session_send_text", json!({"session_id": id, "text": "x"})), Err(ToolError::Refused(_))));

    h.ok("session_wake", json!({"session_id": id}));
    h.wait_status(&id, Status::Idle);
    let second = h.pty(1);

    // Restart: the old process is closed and has exited before the new one is created.
    let out = h.ok("session_restart", json!({"session_id": id}));
    assert!(out.message.starts_with("Restarted"), "{}", out.message);
    let third = h.pty(2);
    let closed_at = second.closed_at.lock().unwrap().expect("closed gracefully");
    assert!(closed_at <= third.created, "the new process started before the old one was closed");
    assert!(csm_pty::Pty::has_exited(second.as_ref()));
    let v = h.wait_status(&id, Status::Idle);
    assert_eq!(v.focused.as_deref(), Some(id.as_str()), "focus survives a restart");
    assert_eq!(v.sessions.len(), 1, "same session, same id");

    // A Claude that ignores the close is killed after the exit wait, then relaunched.
    third.exit_on_close.store(false, Ordering::SeqCst);
    let t0 = Instant::now();
    h.ok("session_restart", json!({"session_id": id}));
    assert!(t0.elapsed() >= Duration::from_millis(1400), "waited for the exit first");
    assert!(third.killed.load(Ordering::SeqCst));
    h.pty(3);
}

#[test]
fn a_failed_start_wake_or_restart_leaves_the_session_asleep_not_deleted() {
    let fail = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let f2 = fail.clone();
    let h = H::start(test_app(), move |o| {
        let inner = o.pty.clone();
        o.pty = std::sync::Arc::new(move |opts| {
            if f2.load(Ordering::SeqCst) { Err(std::io::Error::new(std::io::ErrorKind::NotFound, "program not found")) } else { inner(opts) }
        });
    });
    let id = h.session("claude");
    // An edit that cannot launch is refused before anything changes.
    assert!(h.ui("session_edit", json!({"session_id": id, "kind": "ssh"})).is_err());

    fail.store(true, Ordering::SeqCst);
    let r = h.ui("session_restart", json!({"session_id": id}));
    assert!(matches!(&r, Err(ToolError::Failed(m)) if m.contains("asleep")), "{r:?}");
    let v = h.wait_status(&id, Status::Asleep);
    assert_eq!(v.sessions.len(), 1, "kept, asleep and editable");

    h.ok("session_wake", json!({"session_id": id}));
    h.wait_status(&id, Status::Asleep);
    fail.store(false, Ordering::SeqCst);
    h.ok("session_wake", json!({"session_id": id}));
    h.wait_status(&id, Status::Idle);

    // A brand-new session that cannot start is kept asleep too (with a toast), not lost.
    fail.store(true, Ordering::SeqCst);
    let folder = h.dir.path().to_string_lossy().into_owned();
    let nid = h.ok("session_new", json!({"folder": folder, "command": "nosuch"})).data["id"].as_str().unwrap().to_owned();
    h.wait_status(&nid, Status::Asleep);
}

#[test]
fn the_screen_heuristic_raises_needs_you_when_no_status_is_reported() {
    let h = H::new();
    // Only Claude sessions are judged by the screen heuristic (an old Claude that doesn't speak 7501).
    let id = h.session("claude");
    h.pty(0).emit(b"Do you want to continue? (y/N) ");
    let v = h.wait_until("heuristic alert", |v| v.session(&id).is_some_and(|s| s.status.needs_you()));
    assert_eq!(v.inbox, vec![id.clone()]);
    // A real keystroke acknowledges it.
    h.engine.terminal(&id).unwrap().send_key(csm_engine::input::typed("y", false));
    h.wait_status(&id, Status::Idle);
}

#[test]
fn a_restored_session_is_not_judged_by_the_screen_heuristic_until_it_is_addressed() {
    // Restore is not news: a resumed Claude reprints its last reply, which may end in a question
    // (ConPTY's cursor jumps and all). That raised "needs you", and a notification, for every
    // restored session at start-up.
    let mut app = test_app();
    app.sessions.push(ShellSession { id: "a".into(), command: "claude".into(), working_folder: ".".into(), ..Default::default() });
    let h = H::start(app, |_| {});
    h.wait_status("a", Status::Idle);
    h.pty(0).emit(b"\x1b[40;1H\xe2\x97\x8f\x1b[1CShall\x1b[1CI\x1b[1Cadd\x1b[1Cthis\x1b[1Cto\x1b[1Cthe\x1b[1Cplan?");
    // Past the heuristic's 1.5 s idle window.
    std::thread::sleep(Duration::from_millis(2500));
    let v = h.engine.view();
    assert_eq!(v.session("a").map(|s| s.status), Some(Status::Idle), "resumed output is not a question to the person");
    assert!(v.inbox.is_empty());
    // Once the person has typed into it, a question it then asks is judged again, with its spaces.
    h.engine.terminal("a").unwrap().send_key(csm_engine::input::typed("y", false));
    std::thread::sleep(Duration::from_millis(100));
    h.pty(0).emit(b"\r\n\x1b[41;1HDo\x1b[1Cyou\x1b[1Cwant\x1b[1Cto\x1b[1Ccontinue?");
    let v = h.wait_until("heuristic alert", |v| v.session("a").is_some_and(|s| s.status.needs_you()));
    assert_eq!(v.session("a").and_then(|s| s.status_message.as_deref()), Some("Do you want to continue?"));
}

#[test]
fn a_shell_waiting_at_its_prompt_is_idle_not_needs_you() {
    // The WPF heuristic matched any line ending in `>` or `(y/N)`, so every idle shell showed as
    // "needs you". Plain shells are no longer judged by the screen heuristic.
    let h = H::new();
    let id = h.session("cmd");
    h.pty(0).emit(b"Microsoft Windows\r\nC:\\work> ");
    h.pty(0).emit(b"Overwrite? (y/N) ");
    // Past the heuristic's 1.5 s idle window.
    std::thread::sleep(Duration::from_millis(2500));
    let v = h.engine.view();
    let s = v.session(&id).expect("session");
    assert!(!s.status.needs_you(), "a shell prompt must not be needs-you, got {:?}", s.status);
    assert!(v.inbox.is_empty());
}

#[test]
fn restart_is_refused_while_the_restore_loop_runs_and_while_another_restart_runs() {
    let mut app = test_app();
    for id in ["a", "b"] {
        app.sessions.push(ShellSession { id: id.into(), command: "claude".into(), working_folder: ".".into(), ..Default::default() });
    }
    let h = H::start(app, |o| o.first_size_timeout = Duration::from_secs(30));
    let v = h.wait_until("restore running", |v| v.restore.is_some());
    assert_eq!(v.restore.unwrap().total, 2);
    let r = h.ui("session_restart", json!({"session_id": "a"}));
    assert!(matches!(r, Err(ToolError::Refused(m)) if m.contains("restored")), "restart during restore");
    // The panes report their sizes: the restore completes.
    for id in ["a", "b"] {
        h.engine.terminal(id).unwrap().resize(GridSize::new(80, 25));
    }
    h.wait_until("restore done", |v| v.restore.is_none());
    // A long restart (the old Claude ignores the close) blocks a second one.
    h.pty(0).exit_on_close.store(false, Ordering::SeqCst);
    h.pty(1).exit_on_close.store(false, Ordering::SeqCst);
    let first = h.engine.submit(Caller::Ui, "session_restart", json!({"session_id": "a"}));
    std::thread::sleep(Duration::from_millis(100));
    let r = h.ui("session_restart", json!({"session_id": "b"}));
    assert!(matches!(r, Err(ToolError::Refused(m)) if m.contains("already")), "concurrent restart");
    // The relaunched pane is measured by the UI (no fallback in this test): play the UI.
    let deadline = Instant::now() + Duration::from_secs(10);
    let reply = loop {
        if let Ok(r) = first.try_recv() {
            break r;
        }
        assert!(Instant::now() < deadline, "the restart never finished");
        if let Some(t) = h.engine.terminal("a").filter(|t| !t.is_started()) {
            t.resize(GridSize::new(80, 25));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    reply.unwrap();
}

#[test]
fn restore_relaunches_live_sessions_and_keeps_dormant_ones_asleep() {
    let mut app = test_app();
    app.sessions.push(ShellSession { id: "live".into(), command: "pwsh".into(), ..Default::default() });
    app.sessions.push(ShellSession { id: "zz".into(), command: "pwsh".into(), is_dormant: true, ..Default::default() });
    app.sessions.push(ShellSession { id: "bad".into(), kind: csm_core::models::SessionKind::Ssh, ..Default::default() });
    let h = H::start(app, |_| {});
    h.wait_status("live", Status::Idle);
    assert_eq!(h.status("zz"), Status::Asleep);
    assert_eq!(h.status("bad"), Status::Asleep, "an invalid session falls back to asleep");
    assert_eq!(h.pty_count(), 1);
}

#[test]
fn move_to_group_uses_anchors_and_lands_where_asked() {
    let h = H::new();
    let ids: Vec<String> = (0..4).map(|_| h.session("pwsh")).collect();
    let (a, b, c, d) = (&ids[0], &ids[1], &ids[2], &ids[3]);
    let g = h.ok("group_new", json!({"name": "Odulodo"})).data["group_id"].as_str().unwrap().to_owned();
    h.ok("session_move_to_group", json!({"session_id": a, "group_id": g}));
    h.ok("session_move_to_group", json!({"session_id": c, "group_id": g, "after": a}));
    h.ok("session_move_to_group", json!({"session_id": d, "group_id": g, "before": a}));
    h.ok("session_move_to_group", json!({"session_id": b, "group_id": g}));
    let order = |h: &H| -> Vec<String> {
        h.ok("session_list", json!({"group": g})).data.as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap().to_owned()).collect()
    };
    assert_eq!(order(&h), vec![d.clone(), a.clone(), c.clone(), b.clone()]);
    // Downward move to just after the last one: no overshoot.
    h.ok("session_move_to_group", json!({"session_id": d, "group_id": g, "after": c}));
    assert_eq!(order(&h), vec![a.clone(), c.clone(), d.clone(), b.clone()]);
    // An anchor in another group is refused.
    h.ok("session_move_to_group", json!({"session_id": b, "group_id": null}));
    let r = h.ui("session_move_to_group", json!({"session_id": a, "group_id": g, "after": b}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))));
    // Deleting the group ungroups, never closes.
    h.ok("group_delete", json!({"group_id": g}));
    let v = h.engine.view();
    assert_eq!(v.sessions.len(), 4);
    assert!(v.sessions.iter().all(|s| s.group_id.is_empty()));
}

#[test]
fn the_inbox_is_oldest_first_and_inbox_next_follows_it() {
    let h = H::new();
    let s1 = h.session("claude");
    let s2 = h.session("claude");
    let s3 = h.session("claude");
    h.pty(1).emit(&osc7501("state=blocked:kind=question"));
    h.wait_status(&s2, Status::NeedsYouQuestion);
    std::thread::sleep(Duration::from_millis(20));
    h.pty(2).emit(&osc7501("state=done"));
    h.wait_status(&s3, Status::DoneUnseen);
    std::thread::sleep(Duration::from_millis(20));
    h.pty(0).emit(&osc7501("state=blocked:kind=permission"));
    let v = h.wait_status(&s1, Status::NeedsYouApproval);
    // Needs you first (oldest first), then unseen results.
    assert_eq!(v.inbox, vec![s2.clone(), s1.clone(), s3.clone()]);
    h.ok("inbox_next", json!({}));
    let v = h.wait_until("focus s2", |v| v.focused.as_deref() == Some(s2.as_str()));
    assert_eq!(v.session(&s2).unwrap().status, Status::NeedsYouQuestion, "focus does not answer the question");
    h.ok("inbox_next", json!({}));
    h.wait_until("focus s1", |v| v.focused.as_deref() == Some(s1.as_str()));
}

#[test]
fn inbox_next_declares_that_it_can_wake_a_session_so_mcp_callers_are_asked() {
    let h = H::new();
    let spec = h.engine.registry().get("inbox_next").map(|t| t.effects).unwrap();
    assert!(spec.contains(csm_engine::Effects::LAUNCHES_PROCESS | csm_engine::Effects::CHANGES_UI_STATE), "{spec:?}");
    assert!(spec.needs_consent(), "an agent that calls it may start a process");
}

#[test]
fn session_wait_completes_on_the_status_and_times_out() {
    let h = H::new();
    let id = h.session("claude");
    h.pty(0).emit(&osc7501("state=working"));
    h.wait_status(&id, Status::Working);
    let rx = h.engine.submit(Caller::Ui, "session_wait", json!({"session_id": id, "until": "done", "timeout_s": 10}));
    std::thread::sleep(Duration::from_millis(100));
    assert!(rx.try_recv().is_err());
    h.pty(0).emit(&osc7501("state=done"));
    let out = rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(out.data["status"], "done_unseen");
    // Already reached: immediate.
    assert!(h.ui("session_wait", json!({"session_id": id, "until": "idle"})).is_ok());
    let t0 = Instant::now();
    let r = h.ui("session_wait", json!({"session_id": id, "until": "needs_you", "timeout_s": 0.2}));
    assert!(matches!(r, Err(ToolError::TimedOut(_))), "{r:?}");
    assert!(t0.elapsed() < Duration::from_secs(3));
    // A wait on a session that gets closed ends.
    let rx = h.engine.submit(Caller::Ui, "session_wait", json!({"session_id": id, "until": "any_change", "timeout_s": 10}));
    std::thread::sleep(Duration::from_millis(50));
    h.ok("session_close", json!({"session_id": id, "force": true}));
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err(ToolError::NotFound(_))));
}

#[test]
fn read_screen_and_output_return_plain_text() {
    let h = H::new();
    let id = h.session("pwsh");
    h.pty(0).emit(b"\x1b[31mhello\x1b[0m world\r\nsecond line\r\n");
    std::thread::sleep(Duration::from_millis(50));
    let screen = h.ok("session_read_screen", json!({"session_id": id}));
    let text = screen.data["text"].as_str().unwrap();
    assert!(text.contains("hello world") && text.contains("second line"), "{text:?}");
    let out = h.ok("session_read_output", json!({"session_id": id, "lines": 1}));
    assert_eq!(out.data["text"], "second line");
    assert!(h.ui("session_read_output", json!({"session_id": id, "lines": 5000})).is_err());
}

#[test]
fn state_round_trips_and_falls_back_to_the_backup() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let ptys = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let opts = || {
        let mut o = common::fast_options(&ptys);
        o.save_path = Some(path.clone());
        o
    };
    {
        std::fs::write(&path, test_app().to_json().unwrap()).unwrap();
        let (state, _) = State::load(opts());
        let e = csm_engine::Engine::start(state, vec![]).unwrap();
        let folder = dir.path().to_string_lossy().into_owned();
        e.call(Caller::Ui, "group_new", json!({"name": "G"})).unwrap();
        e.call(Caller::Ui, "session_new", json!({"folder": folder, "command": "pwsh", "name": "kept"})).unwrap();
        e.call(Caller::Ui, "layout_set", json!({"preset": "3x2"})).unwrap();
        e.shutdown();
    }
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"CsmRsVersion\": 1"), "{text}");
    let (state, report) = State::load(opts());
    assert_eq!(report.source, Some(csm_core::state::LoadSource::Primary));
    assert_eq!(state.app.sessions.len(), 1);
    assert_eq!(state.app.sessions[0].name, "kept");
    assert_eq!(state.app.groups.len(), 1);
    assert_eq!(state.app.ui.layouts.get("__ALL__").unwrap().grid_cols, 3);
    // Tear the primary: the previous generation (.bak) is used.
    std::fs::write(&path, "{\"Sessions\": [").unwrap();
    let (state, report) = State::load(opts());
    assert_eq!(report.source, Some(csm_core::state::LoadSource::Backup));
    assert!(state.app.sessions.len() <= 1);
    // --clean: nothing restored, nothing written.
    let before = std::fs::read(&path).unwrap();
    let mut o = opts();
    o.clean = true;
    let (state, _) = State::load(o);
    assert!(state.app.sessions.is_empty());
    let e = csm_engine::Engine::start(state, vec![]).unwrap();
    e.call(Caller::Ui, "group_new", json!({"name": "not saved"})).unwrap();
    e.shutdown();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn the_wpf_state_imports_as_sleeping_sessions() {
    let h = H::new();
    let mut wpf = AppState::default();
    wpf.groups.push(SessionGroup { id: "g1".into(), name: "Client".into(), ..Default::default() });
    wpf.groups.push(SessionGroup { id: "g2".into(), name: "Ops".into(), sort_order: 1, ..Default::default() });
    wpf.sessions.push(ShellSession {
        id: "w1".into(),
        name: "one".into(),
        group_id: "g1".into(),
        working_folder: r"C:\src\one".into(),
        ..Default::default()
    });
    let mut ssh = ShellSession { id: "w2".into(), ssh_host: "dev".into(), ..Default::default() };
    ssh.set_is_remote(true);
    wpf.sessions.push(ssh);
    let path = h.dir.path().join("wpf-state.json");
    std::fs::write(&path, wpf.to_json().unwrap()).unwrap();
    let out = h.ok("state_import_wpf", json!({"path": path.to_string_lossy()}));
    assert_eq!(out.data["sessions"], 2);
    let v = h.engine.view();
    assert_eq!(v.sessions.len(), 2);
    assert!(v.sessions.iter().all(|s| s.status == Status::Asleep));
    assert_eq!(v.session("w2").unwrap().kind, "ssh");
    assert_eq!(v.groups.len(), 2);
    assert_eq!(h.pty_count(), 0, "importing starts nothing");
    let again = h.ok("state_import_wpf", json!({"path": path.to_string_lossy()}));
    assert_eq!(again.data["sessions"], 0);
    assert!(h.ui("state_import_wpf", json!({"path": h.dir.path().join("nope.json").to_string_lossy()})).is_err());
}

#[test]
fn quit_closes_everything_and_signals_ready() {
    let h = H::new();
    let a = h.session("claude");
    let _b = h.session("pwsh");
    let events = h.engine.subscribe();
    h.ok("app_quit", json!({}));
    let mut ready = false;
    while let Ok(e) = events.recv_timeout(Duration::from_secs(5)) {
        if e == Event::QuitReady {
            ready = true;
            break;
        }
    }
    assert!(ready);
    assert!(h.pty(0).closed.load(Ordering::SeqCst), "Claude closed gracefully");
    assert!(csm_pty::Pty::has_exited(h.pty(1).as_ref()));
    assert!(matches!(h.ui("session_restart", json!({"session_id": a})), Err(ToolError::Refused(_))));
    assert!(matches!(h.ui("session_new", json!({"folder": ".", "command": "x"})), Err(ToolError::Refused(_))));
}

#[test]
fn custom_grids_are_remembered_deduplicated_capped_forgotten_and_saved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, test_app().to_json().unwrap()).unwrap();
    let ptys = Default::default();
    let mut o = common::fast_options(&ptys);
    o.save_path = Some(path.clone());
    let (state, _) = State::load(o.clone());
    let e = csm_engine::Engine::start(state, vec![]).unwrap();
    let call = |id: &str, p: serde_json::Value| e.call(Caller::Ui, id, p).unwrap();
    let grids = |e: &csm_engine::Engine| e.view().custom_grids.clone();

    // The new presets are presets: by name, and as cols/rows they are not remembered.
    for (name, g) in [("4x1", (4u64, 1u64)), ("6x1", (6, 1)), ("6x2", (6, 2))] {
        let l = call("layout_set", json!({"preset": name}));
        assert_eq!((l.data["layout"]["GridCols"].as_u64(), l.data["layout"]["GridRows"].as_u64()), (Some(g.0), Some(g.1)));
        call("layout_set", json!({"cols": g.0, "rows": g.1}));
    }
    assert!(grids(&e).is_empty(), "a preset is never a custom");

    // A custom size is remembered, newest first; setting it again moves it, never duplicates it.
    call("layout_set", json!({"mode": "grid", "cols": 5, "rows": 1}));
    let out = call("layout_set", json!({"mode": "grid", "cols": 4, "rows": 3}));
    assert_eq!(out.data["custom_grids"], json!([[4, 3], [5, 1]]));
    call("layout_set", json!({"mode": "grid", "cols": 5, "rows": 1}));
    assert_eq!(grids(&e), vec![(5, 1), (4, 3)]);
    // Only one side given: the remembered size is the resulting grid.
    call("layout_set", json!({"rows": 4}));
    assert_eq!(grids(&e).first(), Some(&(5, 4)));
    // At most six; the oldest goes.
    for (c, r) in [(1, 2), (1, 3), (2, 3), (7, 7), (8, 8)] {
        call("layout_set", json!({"cols": c, "rows": r}));
    }
    assert_eq!(grids(&e), vec![(8, 8), (7, 7), (2, 3), (1, 3), (1, 2), (5, 4)]);
    assert_eq!(call("layout_get", json!({})).data["custom_grids"].as_array().unwrap().len(), 6);

    // Forget one; forgetting it again is quiet.
    let f = call("grid_preset_forget", json!({"cols": 7, "rows": 7}));
    assert_eq!(f.data["forgot"], true);
    assert_eq!(grids(&e), vec![(8, 8), (2, 3), (1, 3), (1, 2), (5, 4)]);
    assert_eq!(call("grid_preset_forget", json!({"cols": 7, "rows": 7})).data["forgot"], false);
    assert!(e.call(Caller::Ui, "grid_preset_forget", json!({"cols": 7})).is_err(), "both sides are required");
    assert!(e.call(Caller::Ui, "layout_set", json!({"cols": 9})).is_err());
    e.shutdown();

    // Saved with the UI state: a new engine on the same file offers them again.
    let (state, _) = State::load(o);
    let e = csm_engine::Engine::start(state, vec![]).unwrap();
    assert_eq!(e.view().custom_grids, vec![(8, 8), (2, 3), (1, 3), (1, 2), (5, 4)]);
    e.shutdown();
}

#[test]
fn layout_view_and_settings_tools() {
    let h = H::new();
    let g = h.ok("group_new", json!({"name": "Odulodo"})).data["group_id"].as_str().unwrap().to_owned();
    let id = h.session("pwsh");
    h.ok("view_set", json!({"view": "rail", "rail_tab": g}));
    h.ok("layout_set", json!({"mode": "side"}));
    h.ok("layout_pin", json!({"session_id": id}));
    let l = h.ok("layout_get", json!({"group": g}));
    assert_eq!(l.data["layout"]["Pinned"][0], id);
    // Focusing a session in another group switches the rail tab.
    h.ok("session_focus", json!({"session_id": id}));
    assert_eq!(h.engine.view().rail_tab, None);
    h.ok("inspector_show", json!({"tab": "git"}));
    h.ok("inbox_panel_set", json!({"open": false, "height": 180.0}));
    let v = h.engine.view();
    assert!(!v.inbox_open && v.inbox_height == Some(180.0));
    assert!(h.ui("layout_set", json!({"preset": "7x7"})).is_err());
    // Free mode's tree is the UI's: stored as given, never interpreted; objects only, capped.
    let free = json!({"tree": {"leaf": {"session": id}}, "floats": []});
    h.ok("layout_set", json!({"group": g, "free": free}));
    let l = h.ok("layout_get", json!({"group": g}));
    assert_eq!(l.data["layout"]["FreeTree"], free);
    assert!(h.ui("layout_set", json!({"free": [1, 2]})).is_err());
    assert!(h.ui("layout_set", json!({"free": {"x": "y".repeat(70_000)}})).is_err());
    // Settings: by snake_case or PascalCase; secrets hidden; launch settings app-only.
    h.ok("settings_set", json!({"key": "show_git_branch", "value": false}));
    assert_eq!(h.ok("settings_get", json!({"key": "ShowGitBranch"})).data["value"], false);
    h.ok("settings_set", json!({"key": "AnthropicApiKey", "value": "sk-secret"}));
    assert_eq!(h.ok("settings_get", json!({"key": "anthropic_api_key"})).data["value"], "(set)");
    let r = h.engine.call(ext(None), "settings_set", json!({"key": "DefaultCommand", "value": "calc"}));
    assert!(matches!(r, Err(ToolError::Refused(_))));
    assert!(h.ui("settings_set", json!({"key": "Nope", "value": 1})).is_err());
    assert!(h.ui("settings_set", json!({"key": "Theme", "value": 5})).is_err());
    assert!(h.ok("app_info", json!({})).data["version"].is_string());
}

#[test]
fn every_tool_has_an_object_schema_and_the_catalogue_is_complete() {
    let h = H::new();
    let ids: Vec<&str> = h.engine.registry().all().iter().map(|s| s.id).collect();
    for want in [
        "session_list",
        "session_get",
        "session_new",
        "session_focus",
        "session_rename",
        "session_edit",
        "session_close",
        "session_sleep",
        "session_wake",
        "session_restart",
        "session_reopen_closed",
        "session_send_text",
        "session_read_screen",
        "session_read_output",
        "session_wait",
        "session_status_clear",
        "inbox_list",
        "inbox_next",
        "group_list",
        "group_new",
        "group_rename",
        "group_delete",
        "session_move_to_group",
        "group_restart_all",
        "group_sleep_all",
        "group_close_all",
        "group_tear_off",
        "group_dock",
        "view_get",
        "view_set",
        "layout_get",
        "layout_set",
        "layout_pin",
        "layout_unpin",
        "inspector_show",
        "inbox_panel_set",
        "window_placement_set",
        "recently_closed_list",
        "app_info",
        "settings_get",
        "settings_set",
        "toolset_enable",
        "toolset_disable",
        "state_import_wpf",
        "app_quit",
        "grant_list",
        "grant_revoke",
        "session_accent_reset",
    ] {
        assert!(ids.contains(&want), "missing {want}");
    }
    for s in h.engine.registry().all() {
        assert_eq!((s.input_schema)()["type"], "object", "{}", s.id);
    }
}

#[test]
fn force_quit_stops_waiting_for_a_wedged_claude() {
    let h = H::start(test_app(), |o| o.shutdown_budget = Duration::from_secs(30));
    let _a = h.session("claude");
    h.pty(0).exit_on_close.store(false, Ordering::SeqCst);
    let events = h.engine.subscribe();
    let t0 = Instant::now();
    // The reply comes when the quit is done, so don't wait for it here.
    let first = h.engine.submit(Caller::Ui, "app_quit", json!({}));
    // The quit is waiting on the wedged Claude: a second plain quit is refused, a forced one
    // cuts the wait short.
    std::thread::sleep(Duration::from_millis(150));
    assert!(matches!(h.ui("app_quit", json!({})), Err(ToolError::Refused(_))));
    h.ok("app_quit", json!({"force": true}));
    let mut ready = false;
    let mut forced = false;
    while let Ok(e) = events.recv_timeout(Duration::from_secs(5)) {
        match e {
            Event::QuitProgress { phase, .. } if phase == "forced" => forced = true,
            Event::QuitReady => {
                ready = true;
                break;
            }
            _ => {}
        }
    }
    assert!(ready && forced, "ready {ready}, forced {forced}");
    assert!(t0.elapsed() < Duration::from_secs(5), "force must not wait out the 30 s budget: {:?}", t0.elapsed());
    assert!(csm_pty::Pty::has_exited(h.pty(0).as_ref()));
    first.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
}

#[test]
fn a_forced_quit_reports_live_claudes_as_forced_not_clean() {
    // Forced from the start, so the worker reaches each Claude with force already set: it kills
    // it straight away, and must still say "forced" (a kill always leaves the PTY exited).
    let h = H::start(test_app(), |o| o.shutdown_budget = Duration::from_secs(30));
    let _a = h.session("claude");
    let _b = h.session("claude");
    for i in 0..2 {
        h.pty(i).exit_on_close.store(false, Ordering::SeqCst);
    }
    let events = h.engine.subscribe();
    let done = h.engine.submit(Caller::Ui, "app_quit", json!({"force": true}));
    let mut phases = Vec::new();
    while let Ok(e) = events.recv_timeout(Duration::from_secs(5)) {
        match e {
            Event::QuitProgress { phase, .. } if phase != "closing" => phases.push(phase),
            Event::QuitReady => break,
            _ => {}
        }
    }
    assert_eq!(phases, vec!["forced", "forced"]);
    done.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
}

#[test]
fn grants_persist_and_can_be_revoked() {
    let h = H::new();
    let id = h.session("pwsh");
    let events = h.engine.subscribe();
    let rx = h.engine.submit(ext(None), "session_read_screen", json!({"session_id": id}));
    let request = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ConsentRequested { request }) => break request,
            Ok(_) => continue,
            other => panic!("expected a consent request, got {other:?}"),
        }
    };
    h.ok("consent_answer", json!({"request_id": request["request_id"], "answer": "allow_always"}));
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    let list = h.ok("grant_list", json!({}));
    assert_eq!(list.data, json!([{"client": "test-mcp", "tool": "session_read_screen"}]));
    // Granted: runs without a card. The grant is part of the persisted state.
    assert!(h.engine.call(ext(None), "session_read_screen", json!({"session_id": id})).is_ok());
    let saved = h.ok("settings_get", json!({}));
    assert!(saved.data.get("McpGrants").is_none(), "grants are not settings");
    let out = h.ok("grant_revoke", json!({"client": "test-mcp", "tool": "session_read_screen"}));
    assert_eq!(out.data["removed"], 1);
    assert_eq!(h.ok("grant_list", json!({})).data, json!([]));
    let r = h.engine.submit(ext(None), "session_read_screen", json!({"session_id": id}));
    std::thread::sleep(Duration::from_millis(50));
    assert!(r.try_recv().is_err(), "revoked: asks again");
    // Round trip through state.json.
    let mut app = AppState::default();
    app.mcp_grants.push(csm_core::models::McpGrant { client: "c".into(), tool: "t".into() });
    let back: AppState = serde_json::from_str(&app.to_json().unwrap()).unwrap();
    assert_eq!(back.mcp_grants, app.mcp_grants);
    assert!(!AppState::default().to_json().unwrap().contains("McpGrants"), "absent when empty (WPF files stay clean)");
}

#[test]
fn an_osc_9001_accent_can_be_reset() {
    let h = H::new();
    let id = h.session("pwsh");
    let folder_colour = h.engine.view().session(&id).unwrap().accent.clone();
    h.pty(0).emit(b"]9001;color=#112233\\");
    let v = h.wait_until("accent pushed", |v| v.session(&id).is_some_and(|s| s.accent_override));
    assert_eq!(v.session(&id).unwrap().accent, "#112233");
    let out = h.ok("session_accent_reset", json!({"session_id": id}));
    assert_eq!(out.data["changed"], true);
    let v = h.engine.view();
    assert!(!v.session(&id).unwrap().accent_override);
    assert_eq!(v.session(&id).unwrap().accent, folder_colour);
    assert_eq!(h.ok("session_accent_reset", json!({"session_id": id})).data["changed"], false);
}

#[test]
fn wsl_distros_answers_off_the_engine_thread_and_needs_no_consent() {
    let h = H::new();
    // An MCP caller gets the answer without a consent card: it is declared read-only.
    let r = h.engine.call(ext(None), "wsl_distros", json!({}));
    let out = r.expect("wsl_distros is read-only, so no consent");
    assert!(out.data.is_array(), "{:?}", out.data);
    if !cfg!(windows) {
        assert_eq!(out.data.as_array().unwrap().len(), 0);
    }
}

#[test]
fn window_placements_persist_per_window_and_never_under_clean() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let ptys = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let opts = || {
        let mut o = common::fast_options(&ptys);
        o.save_path = Some(path.clone());
        o
    };
    let set = |e: &csm_engine::Engine, p: serde_json::Value| e.call(Caller::Ui, "window_placement_set", p);
    let (gid, sid) = {
        let (state, _) = State::load(opts());
        let e = csm_engine::Engine::start(state, vec![]).unwrap();
        let folder = dir.path().to_string_lossy().into_owned();
        let gid = e.call(Caller::Ui, "group_new", json!({"name": "G"})).unwrap().data["group_id"].as_str().unwrap().to_owned();
        let sid = e.call(Caller::Ui, "session_new", json!({"folder": folder, "command": "pwsh"})).unwrap().data["id"].as_str().unwrap().to_owned();
        set(&e, json!({"window": "main", "left": 2200, "top": 300, "width": 2400, "height": 1500, "dpi": 144, "maximized": true})).unwrap();
        set(&e, json!({"window": format!("group:{gid}"), "left": -1700, "top": 10, "width": 900, "height": 600, "dpi": 96})).unwrap();
        set(&e, json!({"window": format!("pane:{sid}"), "left": 5, "top": 6, "width": 700, "height": 500, "dpi": 120})).unwrap();
        // The snapshot carries them for the UI.
        let view = e.view();
        assert_eq!(view.windows["main"].placement.width, 2400);
        assert!(view.windows["main"].maximized);
        assert_eq!(view.windows[&format!("group:{gid}")].placement.left, -1700);
        // Garbage and unknown windows are refused.
        let bad = |p: serde_json::Value| set(&e, p).unwrap_err();
        assert!(matches!(bad(json!({"window": "main", "left": 0, "top": 0, "width": 0, "height": 10, "dpi": 96})), ToolError::InvalidParams(_)));
        assert!(matches!(bad(json!({"window": "main", "left": 0, "top": 0, "width": 10, "height": 10, "dpi": 0})), ToolError::InvalidParams(_)));
        assert!(matches!(bad(json!({"window": "side", "left": 0, "top": 0, "width": 10, "height": 10, "dpi": 96})), ToolError::InvalidParams(_)));
        assert!(matches!(bad(json!({"window": "group:nope", "left": 0, "top": 0, "width": 10, "height": 10, "dpi": 96})), ToolError::NotFound(_)));
        e.shutdown();
        (gid, sid)
    };
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("\"LastPlacement\""), "the main window uses the state file's own fields: {text}");
    let (state, _) = State::load(opts());
    let p = state.app.last_placement.unwrap();
    assert_eq!((p.left, p.top, p.width, p.height, p.dpi), (2200, 300, 2400, 1500, 144));
    assert!(state.app.window_maximized);
    let g = state.app.ui.windows[&csm_core::models::group_window_key(&gid)];
    assert_eq!((g.placement.left, g.placement.width, g.placement.dpi, g.maximized), (-1700, 900, 96, false));
    let s = state.app.ui.windows[&csm_core::models::pane_window_key(&sid)];
    assert_eq!((s.placement.top, s.placement.height, s.placement.dpi), (6, 500, 120));

    // A deleted group's window is forgotten at the next save of any window.
    let e = csm_engine::Engine::start(state, vec![]).unwrap();
    e.call(Caller::Ui, "group_delete", json!({"group_id": gid})).unwrap();
    set(&e, json!({"window": format!("pane:{sid}"), "left": 1, "top": 1, "width": 10, "height": 10, "dpi": 96})).unwrap();
    assert!(!e.view().windows.contains_key(&format!("group:{gid}")));
    e.shutdown();

    // --clean: placements are taken but nothing is written.
    let before = std::fs::read(&path).unwrap();
    let mut o = opts();
    o.clean = true;
    let (state, _) = State::load(o);
    let e = csm_engine::Engine::start(state, vec![]).unwrap();
    set(&e, json!({"window": "main", "left": 1, "top": 2, "width": 300, "height": 200, "dpi": 96})).unwrap();
    e.shutdown();
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

#[test]
fn a_new_claude_session_runs_and_stores_its_own_conversation_id() {
    let h = H::new();
    let id = h.session("claude");
    let pty = h.pty(0);
    let v = h.wait_until("the conversation id stored", |v| v.session(&id).is_some_and(|s| s.claude_session_id.is_some()));
    let cid = v.session(&id).unwrap().claude_session_id.clone().unwrap();
    assert_eq!(cid.len(), 36, "a UUID: {cid}");
    let line = pty.argv.join(" ");
    assert!(line.contains(&format!("--session-id {cid}")), "{line}");

    // A copy starts its own conversation.
    let copy = h.ok("session_duplicate", json!({"session_id": id})).data["session_id"].as_str().unwrap().to_owned();
    let v = h.wait_until("the copy's id", |v| v.session(&copy).is_some_and(|s| s.claude_session_id.is_some()));
    assert_ne!(v.session(&copy).unwrap().claude_session_id, Some(cid.clone()));

    // A restart with auto-resume continues the stored id: no transcript exists in this test, so
    // it starts that conversation again under the same id rather than guessing another.
    h.ok("settings_set", json!({"key": "AutoResumeClaude", "value": true}));
    let before = h.pty_count();
    h.ok("session_restart", json!({"session_id": id}));
    let p2 = h.pty(before);
    let line = p2.argv.join(" ");
    assert!(line.contains(&cid), "the restart names the stored conversation: {line}");
    assert_eq!(h.engine.view().session(&id).unwrap().claude_session_id.as_deref(), Some(cid.as_str()));

    // Editing the folder forgets it.
    let other = tempfile::tempdir().unwrap();
    h.ok("session_edit", json!({"session_id": id, "folder": other.path().to_string_lossy()}));
    assert_eq!(h.engine.view().session(&id).unwrap().claude_session_id, None);
}

#[test]
fn an_ssh_config_alias_session_runs_ssh_with_the_file_and_the_remote_command_as_argv() {
    let h = H::new();
    let out = h.ok(
        "session_new",
        json!({"kind": "ssh", "name": "box", "resume": "rerun", "focus": false,
               "ssh": {"host": "alias-1", "config_file": "conf/ssh cfg", "remote_argv": ["tmux", "new-session", "-A", "-s", "my main"]}}),
    );
    let id = out.data["id"].as_str().unwrap().to_owned();
    let argv = h.pty(0).argv.clone();
    assert_eq!(argv[..3], ["ssh", "-F", "conf/ssh cfg"], "{argv:?}");
    assert_eq!(argv[argv.len() - 2], "alias-1");
    assert_eq!(argv.last().unwrap(), "tmux new-session -A -s 'my main'");
    // The settings read back, and a hostile element is refused before anything starts.
    let cfg = h.ok("session_get", json!({"session_id": id})).data["config"]["ssh"].clone();
    assert_eq!(cfg["config_file"], "conf/ssh cfg");
    assert_eq!(cfg["remote_argv"][4], "my main");
    let e = h.ui("session_new", json!({"kind": "ssh", "ssh": {"host": "h", "remote_argv": ["a\nb"]}})).unwrap_err();
    assert!(matches!(e, ToolError::InvalidParams(_)), "{e}");
    let e = h.ui("session_new", json!({"kind": "ssh", "ssh": {"host": "h", "remote_argv": vec!["x"; 65]}})).unwrap_err();
    assert!(matches!(e, ToolError::InvalidParams(_)), "{e}");
}

#[test]
fn a_rerun_session_comes_back_without_a_claude_resume_flag() {
    let mut app = test_app();
    let mut s = ShellSession { id: "r".into(), command: "claude".into(), args: "--model x".into(), resume_rerun: true, claude_session_id: Some("11111111-2222-3333-4444-555555555555".into()), ..Default::default() };
    s.working_folder = std::env::temp_dir().to_string_lossy().into_owned();
    app.sessions.push(s);
    let h = H::start(app, |_| {});
    h.wait_status("r", Status::Idle);
    let argv = h.pty_of("r").argv.join(" ");
    assert!(!argv.contains("--resume") && !argv.contains("--session-id"), "{argv}");
    assert!(argv.contains("--model x"), "{argv}");
}

#[test]
fn each_claude_gets_its_own_quit_wait_and_the_board_gets_the_whole_budget() {
    // Two wedged Claudes and a per-session wait of 400 ms: each is forced after its own wait
    // (the first used to eat a shared budget and leave the second none), and the snapshot
    // carries the whole budget, 2 x (wait + stagger), from the start.
    let h = H::start(test_app(), |o| o.shutdown_budget = Duration::from_millis(400));
    // One slot is the old behaviour: one Claude after another.
    h.ok("settings_set", json!({"key": "QuitClaudeConcurrency", "value": 1}));
    let _a = h.session("claude");
    let _b = h.session("claude");
    for i in 0..2 {
        h.pty(i).exit_on_close.store(false, Ordering::SeqCst);
    }
    let events = h.engine.subscribe();
    let done = h.engine.submit(Caller::Ui, "app_quit", json!({}));
    let snap = h.wait_until("the quit to start", |s| s.quit.is_some());
    assert_eq!(snap.quit.map(|q| q.total), Some(2), "the total is known at once");
    let budget = snap.quit_budget_ms.expect("a budget");
    let settings = h.ok("settings_get", json!({"key": "ClaudeLaunchStaggerMs"})).data["value"].as_u64().unwrap_or(0);
    assert_eq!(budget, 2 * (400 + settings), "per-session wait plus stagger, times the Claudes");
    let mut forced = Vec::new();
    while let Ok(e) = events.recv_timeout(Duration::from_secs(10)) {
        match e {
            Event::QuitProgress { phase, elapsed_ms, .. } if phase == "forced" => forced.push(elapsed_ms),
            Event::QuitReady => break,
            _ => {}
        }
    }
    assert_eq!(forced.len(), 2, "both wedged Claudes are forced");
    for ms in &forced {
        assert!(*ms >= 350, "each waited its own time before being forced: {forced:?}");
    }
    done.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
}

#[test]
fn rail_tab_and_pane_steps_wrap_round_the_tabs_and_the_awake_sessions() {
    let h = H::new();
    let one = h.ok("group_new", json!({"name": "One"})).data["group_id"].as_str().unwrap().to_owned();
    let two = h.ok("group_new", json!({"name": "Two"})).data["group_id"].as_str().unwrap().to_owned();
    let a = h.session("pwsh");
    let b = h.session("pwsh");
    h.ok("session_move_to_group", json!({"session_id": a, "group_id": one}));
    h.ok("session_move_to_group", json!({"session_id": b, "group_id": two}));
    // The rail's tabs are All, One, Two; the steps wrap round them.
    assert_eq!(h.engine.view().rail_tab, None);
    h.ok("rail_tab_next", json!({}));
    assert_eq!(h.engine.view().rail_tab.as_deref(), Some(one.as_str()));
    h.ok("rail_tab_next", json!({}));
    h.ok("rail_tab_next", json!({}));
    assert_eq!(h.engine.view().rail_tab, None, "Next from the last tab wraps to All");
    h.ok("rail_tab_prev", json!({}));
    assert_eq!(h.engine.view().rail_tab.as_deref(), Some(two.as_str()), "Previous from All wraps to the last group");
    // Switching to Two focuses its awake session, so the pane steps stay inside Two.
    assert_eq!(h.engine.view().focused.as_deref(), Some(b.as_str()));
    h.ok("pane_focus_next", json!({}));
    assert_eq!(h.engine.view().focused.as_deref(), Some(b.as_str()), "the only awake session of the tab stays focused");
    // In All the steps go round both sessions.
    h.ok("view_set", json!({"rail_tab": "all"}));
    let first = h.engine.view().focused.clone();
    h.ok("pane_focus_next", json!({}));
    let second = h.engine.view().focused.clone();
    assert_ne!(first, second, "Next moves to the other session");
    h.ok("pane_focus_next", json!({}));
    assert_eq!(h.engine.view().focused, first, "and wraps back");
    h.ok("pane_focus_prev", json!({}));
    assert_eq!(h.engine.view().focused, second, "Previous goes back");
}

#[test]
fn quit_claude_concurrency_is_a_setting_with_a_default_and_a_range() {
    let h = H::new();
    assert_eq!(h.ok("settings_get", json!({"key": "QuitClaudeConcurrency"})).data["value"], json!(4));
    h.ok("settings_set", json!({"key": "QuitClaudeConcurrency", "value": 2}));
    assert_eq!(h.ok("settings_get", json!({"key": "QuitClaudeConcurrency"})).data["value"], json!(2));
    for bad in [json!(0), json!(9), json!(-1), json!("many")] {
        assert!(matches!(h.ui("settings_set", json!({"key": "QuitClaudeConcurrency", "value": bad})), Err(ToolError::InvalidParams(_))), "{bad}");
    }
    assert_eq!(h.ok("settings_get", json!({"key": "QuitClaudeConcurrency"})).data["value"], json!(2), "a refused value changes nothing");
}

#[test]
fn quit_closes_claude_sessions_in_a_bounded_pool_and_the_board_gets_the_slot_count() {
    // Five wedged Claudes, two slots, 300 ms each: never more than two closing at once, all five
    // forced after their own wait, and the budget is three waves of waits, not five.
    let h = H::start(test_app(), |o| o.shutdown_budget = Duration::from_millis(300));
    h.ok("settings_set", json!({"key": "QuitClaudeConcurrency", "value": 2}));
    for _ in 0..5 {
        h.session("claude");
    }
    for i in 0..5 {
        h.pty(i).exit_on_close.store(false, Ordering::SeqCst);
    }
    let events = h.engine.subscribe();
    let done = h.engine.submit(Caller::Ui, "app_quit", json!({}));
    let snap = h.wait_until("the quit to start", |s| s.quit.is_some());
    assert_eq!(snap.quit_slots, Some(2));
    assert_eq!(snap.quit_budget_ms, Some(3 * 300));
    let (mut now, mut max, mut forced) = (0u32, 0u32, 0u32);
    while let Ok(e) = events.recv_timeout(Duration::from_secs(10)) {
        match e {
            Event::QuitProgress { phase, .. } if phase == "closing" => {
                now += 1;
                max = max.max(now);
            }
            Event::QuitProgress { phase, .. } => {
                now -= 1;
                forced += u32::from(phase == "forced");
            }
            Event::QuitReady => break,
            _ => {}
        }
    }
    assert_eq!((max, forced), (2, 5));
    done.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
}

/// A quit over `n` Claude sessions that "exit" after rewriting `~/.claude.json` as `rewrite` (a
/// stand-in for the racing CLI); returns the file's text afterwards.
fn quit_with_claude_json(initial: Option<&str>, rewrite: &str, n: usize) -> (String, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("home").join(".claude.json");
    std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
    if let Some(t) = initial {
        std::fs::write(&cfg, t).unwrap();
    }
    let state = dir.path().join("data").join("state.json");
    let (cfg2, state2) = (cfg.clone(), state.clone());
    let h = H::start(test_app(), move |o| {
        o.save_path = Some(state2);
        o.claude_configs = csm_engine::claude_config::ClaudeConfigs::Files(vec![cfg2]);
    });
    for _ in 0..n {
        h.session("claude");
    }
    let ptys: Vec<_> = (0..n).map(|i| h.pty(i)).collect();
    for p in &ptys {
        p.exit_on_close.store(false, Ordering::SeqCst);
    }
    // Each Claude, once asked to close, rewrites the file and exits.
    let (cfg3, text) = (cfg.clone(), rewrite.to_owned());
    let helpers: Vec<_> = ptys
        .into_iter()
        .map(|p| {
            let (cfg, text) = (cfg3.clone(), text.clone());
            std::thread::spawn(move || {
                let deadline = Instant::now() + Duration::from_secs(10);
                while !p.closed.load(Ordering::SeqCst) && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                let _ = std::fs::write(&cfg, &text);
                p.exit(Some(0));
            })
        })
        .collect();
    let events = h.engine.subscribe();
    let done = h.engine.submit(Caller::Ui, "app_quit", json!({}));
    while let Ok(e) = events.recv_timeout(Duration::from_secs(10)) {
        if e == Event::QuitReady {
            break;
        }
    }
    done.recv_timeout(Duration::from_secs(10)).unwrap().unwrap();
    for t in helpers {
        t.join().unwrap();
    }
    let text = std::fs::read_to_string(&cfg).unwrap_or_default();
    (text, dir.keep())
}

#[test]
fn a_claude_json_torn_by_the_exits_is_restored_after_the_quit() {
    let good = r#"{"numStartups": 3}"#;
    let (text, dir) = quit_with_claude_json(Some(good), r#"{"numStartups": 4, "proj"#, 3);
    assert_eq!(text, good);
    let backups = std::fs::read_dir(dir.join("data").join("claude-config-backups")).unwrap().count();
    assert!(backups >= 1, "the backup lives in the data folder");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_claude_json_the_exits_left_valid_is_not_touched() {
    let updated = r#"{"numStartups": 4}"#;
    let (text, dir) = quit_with_claude_json(Some(r#"{"numStartups": 3}"#), updated, 3);
    assert_eq!(text, updated, "lost updates are accepted; a good file is never replaced");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_missing_claude_json_is_ignored_by_the_quit() {
    let (text, dir) = quit_with_claude_json(None, r#"{"numStartups": 1}"#, 2);
    assert_eq!(text, r#"{"numStartups": 1}"#);
    assert!(!dir.join("data").join("claude-config-backups").exists(), "no file before, no backup");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_session_whose_process_ended_is_exited_not_working_and_never_counts_as_busy() {
    // The quit prompt once counted sessions left in Working/Needs-you by a process that was gone.
    let h = H::new();
    let id = h.session("claude");
    h.pty(0).emit(&osc7501("state=working"));
    h.wait_status(&id, Status::Working);
    assert_eq!(h.engine.view().busy_sessions(), vec![id.clone()], "a live working session is busy");
    h.pty(0).exit(Some(1));
    let v = h.wait_status(&id, Status::Exited);
    assert!(v.busy_sessions().is_empty(), "an exited session is not busy");

    // The same for a screen-heuristic alert and a blocked record left behind by a dead process.
    let id2 = h.session("claude");
    let n = h.pty_count() - 1;
    h.pty(n).emit(&osc7501("state=blocked:kind=question"));
    h.wait_status(&id2, Status::NeedsYouQuestion);
    assert_eq!(h.engine.view().busy_sessions(), vec![id2.clone()], "blocked mid-turn is busy");
    h.pty(n).exit(Some(0));
    let v = h.wait_status(&id2, Status::Exited);
    assert!(v.busy_sessions().is_empty());
}

#[test]
fn a_stale_heuristic_needs_you_and_a_sleeping_session_are_not_busy() {
    let h = H::new();
    let id = h.session("claude");
    h.pty(0).emit(b"Do you want to continue? (y/N) ");
    h.wait_until("heuristic alert", |v| v.session(&id).is_some_and(|s| s.status.needs_you()));
    // It still needs you, but it has been quiet: closing the app cuts nothing off.
    std::thread::sleep(Duration::from_millis(4300));
    let v = h.engine.view();
    assert!(v.session(&id).is_some_and(|s| s.status.needs_you()));
    assert!(v.busy_sessions().is_empty(), "a heuristic question on a quiet screen is not busy");

    let other = h.session("claude");
    h.pty(h.pty_count() - 1).emit(&osc7501("state=working"));
    h.wait_status(&other, Status::Working);
    h.ok("session_sleep", json!({"session_id": other}));
    let v = h.wait_status(&other, Status::Asleep);
    assert!(v.busy_sessions().is_empty(), "a sleeping session has no process to cut off");
}
