//! The Board's tasks through `Engine::call`: `tasks.json` persistence, `--clean`, the status log,
//! and the agent rules (own project, own tasks, the cap, consent for assign and delete).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{Caller, Event, IndexSetting, State, Status, ToolError};
use serde_json::{Value, json};

fn agent(session: &str) -> Caller {
    Caller::External { client: "claude-code".into(), session_id: Some(session.into()) }
}

fn outsider() -> Caller {
    Caller::External { client: "some-ide".into(), session_id: None }
}

/// A live `pwsh` session in `group`.
fn session_in(h: &H, group: &str) -> String {
    let folder = h.dir.path().to_string_lossy().into_owned();
    let out = h.ok("session_new", json!({"folder": folder, "command": "pwsh", "group": group}));
    let id = out.data["id"].as_str().unwrap().to_owned();
    h.wait_status(&id, Status::Idle);
    id
}

fn group(h: &H, name: &str) -> String {
    h.ok("group_new", json!({"name": name})).data["group_id"].as_str().unwrap().to_owned()
}

fn refused(r: Result<csm_engine::ToolOutput, ToolError>) -> String {
    match r {
        Err(ToolError::Refused(m)) => m,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

fn task_state(h: &H, id: &str) -> Value {
    h.engine.view().tasks.iter().find(|t| t.id == id).map(|t| serde_json::to_value(t).unwrap()).unwrap_or(Value::Null)
}

fn wait_file(path: &std::path::Path, what: &str, f: impl Fn(&str) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if std::fs::read_to_string(path).is_ok_and(|t| f(&t)) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn tasks_json_round_trips_beside_the_state_and_clean_runs_never_touch_it() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    let tasks = dir.path().join("tasks.json");
    let h = H::start(test_app(), |o| o.save_path = Some(state.clone()));
    let g = group(&h, "Client X");
    let t1 = h.ok("task_new", json!({"project": format!("group:{g}"), "title": "Write the release notes", "body": "Line one\nLine two"}));
    let id = t1.data["id"].as_str().unwrap().to_owned();
    assert_eq!(t1.data["project_name"], "Client X");
    h.ok("task_new", json!({"project": format!("group:{g}"), "title": "Second"}));
    wait_file(&tasks, "tasks.json with both tasks", |t| t.contains("Second"));
    h.ok("task_update", json!({"task_id": "t2", "progress": "half way"}));
    h.engine.shutdown();
    // Saved on exit, with a backup of the previous generation.
    assert!(std::fs::read_to_string(&tasks).unwrap().contains("half way"));
    assert!(dir.path().join("tasks.json.bak").is_file());

    // A new run reads them back.
    let opts = csm_engine::StateOptions { save_path: Some(state.clone()), restore_on_start: false, ..csm_engine::StateOptions::default() };
    let (st, _) = State::load(opts.clone());
    let e = csm_engine::Engine::start(st, vec![]).unwrap();
    let v = e.view();
    assert_eq!(v.tasks.len(), 2);
    let first = v.tasks.iter().find(|t| t.id == id).unwrap();
    assert_eq!((first.title.as_str(), first.body.as_str()), ("Write the release notes", "Line one\nLine two"));
    assert_eq!(v.tasks.iter().find(|t| t.id == "t2").unwrap().progress.as_deref(), Some("half way"));
    // Ids keep counting.
    let t3 = e.call(Caller::Ui, "task_new", json!({"project": "group:", "title": "Third"})).unwrap();
    assert_eq!(t3.data["id"], "t3");
    e.shutdown();

    // --clean: nothing read, nothing written.
    let before = std::fs::read(&tasks).unwrap();
    let (st, _) = State::load(csm_engine::StateOptions { clean: true, ..opts });
    let e = csm_engine::Engine::start(st, vec![]).unwrap();
    assert!(e.view().tasks.is_empty(), "a clean run starts without tasks");
    e.call(Caller::Ui, "task_new", json!({"project": "group:", "title": "Throwaway"})).unwrap();
    std::thread::sleep(Duration::from_millis(100));
    e.shutdown();
    assert_eq!(std::fs::read(&tasks).unwrap(), before, "a clean run never writes tasks.json");
}

#[test]
fn an_unreadable_tasks_file_is_kept_aside_and_the_backup_used() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path().join("state.json");
    std::fs::write(dir.path().join("tasks.json"), "{\"tasks\": [").unwrap();
    std::fs::write(
        dir.path().join("tasks.json.bak"),
        r#"{"version":1,"next_id":2,"tasks":[{"id":"t1","project":"group:","title":"From the backup","state":"backlog","created_by":"person","created_ms":0,"updated_ms":0}]}"#,
    )
    .unwrap();
    let (st, _) = State::load(csm_engine::StateOptions { save_path: Some(state), restore_on_start: false, ..csm_engine::StateOptions::default() });
    let e = csm_engine::Engine::start(st, vec![]).unwrap();
    assert_eq!(e.view().tasks.first().map(|t| t.title.clone()).as_deref(), Some("From the backup"));
    e.shutdown();
    let kept = std::fs::read_dir(dir.path()).unwrap().filter_map(Result::ok).any(|f| f.file_name().to_string_lossy().starts_with("tasks.json.bad-"));
    assert!(kept, "the torn file is kept where no save can overwrite it");
}

#[test]
fn the_person_runs_a_task_through_the_board() {
    let h = H::new();
    let g = group(&h, "G");
    let a = session_in(&h, &g);
    let project = h.engine.view().session(&a).unwrap().project.clone();
    assert_eq!(project, format!("group:{g}"), "outside a repository the project is the group");
    let t = h.ok("task_new", json!({"session_id": a, "title": "Fix the login", "body": "It 500s\non Safari"}));
    let id = t.data["id"].as_str().unwrap().to_owned();
    assert_eq!(t.data["project"], project);
    // Assign pastes "Task <id> from <creator>: <title>" and the body, bracketed, then Enter.
    let out = h.ok("task_assign", json!({"task_id": id, "session_id": a}));
    assert!(out.message.contains("sent"), "{}", out.message);
    let pty = h.pty_of(&a);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !pty.written_text().ends_with('\r') {
        assert!(Instant::now() < deadline, "no Enter after the paste: {:?}", pty.written_text());
        std::thread::sleep(Duration::from_millis(5));
    }
    let written = pty.written_text();
    // A paste sends newlines as CR, as a terminal does.
    assert!(written.contains(&format!("Task {id} from the user: Fix the login\rIt 500s\ron Safari")), "{written:?}");
    assert_eq!(task_state(&h, &id)["state"], "assigned");
    assert_eq!(task_state(&h, &id)["assignee"], a.as_str());
    h.ok("task_done", json!({"task_id": id, "note": "fixed the cookie"}));
    assert_eq!(task_state(&h, &id)["state"], "review");
    h.ok("task_reopen", json!({"task_id": id, "note": "Safari still fails"}));
    assert_eq!(task_state(&h, &id)["state"], "assigned", "back to the same session");
    h.ok("task_done", json!({"task_id": id}));
    h.ok("task_accept", json!({"task_id": id}));
    assert_eq!(task_state(&h, &id)["state"], "done");
    // Done is closed: the default list leaves it out.
    assert_eq!(h.ok("task_list", json!({})).data.as_array().unwrap().len(), 0);
    assert_eq!(h.ok("task_list", json!({"include_closed": true})).data.as_array().unwrap().len(), 1);

    // An asleep session refuses the paste, and the task stays where it was.
    let t2 = h.ok("task_new", json!({"session_id": a, "title": "Second"})).data["id"].as_str().unwrap().to_owned();
    h.ok("session_sleep", json!({"session_id": a}));
    h.wait_status(&a, Status::Asleep);
    refused(h.ui("task_assign", json!({"task_id": t2, "session_id": a})));
    assert_eq!(task_state(&h, &t2)["state"], "backlog");
    // Without send, nothing is pasted and it is assigned anyway.
    h.ok("task_assign", json!({"task_id": t2, "session_id": a, "send": false}));
    assert_eq!(task_state(&h, &t2)["state"], "assigned");
    // Closing the session gives its assigned task back.
    h.ok("session_close", json!({"session_id": a, "force": true}));
    h.wait_until("the session to go", |v| v.session(&a).is_none());
    assert_eq!(task_state(&h, &t2)["state"], "backlog");
    assert_eq!(task_state(&h, &t2)["assignee"], Value::Null);
}

#[test]
fn agents_keep_to_their_own_project_and_their_own_tasks_without_asking() {
    let h = H::new();
    let (g1, g2) = (group(&h, "One"), group(&h, "Two"));
    let (a, b, c) = (session_in(&h, &g1), session_in(&h, &g1), session_in(&h, &g2));
    let events = h.engine.subscribe();

    // Bookkeeping in its own project runs at once (no consent card).
    let mine = h.engine.call(agent(&a), "task_new", json!({"title": "Add tests\u{1b}[2J", "body": "x\u{1b}[201~y"})).unwrap();
    let mine = mine.data["id"].as_str().unwrap().to_owned();
    let t = task_state(&h, &mine);
    assert_eq!(
        (t["project"].as_str().unwrap(), t["title"].as_str().unwrap(), t["body"].as_str().unwrap()),
        (&*format!("group:{g1}"), "Add tests[2J", "x[201~y")
    );
    assert_eq!(t["created_by"], format!("session:{a}"));
    h.engine.call(agent(&a), "task_claim", json!({"task_id": mine})).unwrap();
    h.engine.call(agent(&a), "task_update", json!({"task_id": mine, "progress": "two of five"})).unwrap();

    // Not in another project: neither creating, nor reading, nor claiming there.
    refused(h.engine.call(agent(&a), "task_new", json!({"title": "x", "project": format!("group:{g2}")})));
    let theirs = h.ok("task_new", json!({"session_id": c, "title": "Elsewhere"})).data["id"].as_str().unwrap().to_owned();
    refused(h.engine.call(agent(&a), "task_get", json!({"task_id": theirs})));
    refused(h.engine.call(agent(&a), "task_claim", json!({"task_id": theirs})));
    let listed = h.engine.call(agent(&a), "task_list", json!({})).unwrap();
    assert!(listed.data.as_array().unwrap().iter().all(|t| t["project"] == format!("group:{g1}")), "{}", listed.data);

    // Not another session's task in its own project.
    let bs = h.ok("task_new", json!({"session_id": b, "title": "B's"})).data["id"].as_str().unwrap().to_owned();
    h.ok("task_claim", json!({"task_id": bs, "session_id": b}));
    for (tool, params) in [
        ("task_update", json!({"task_id": bs, "title": "mine now"})),
        ("task_done", json!({"task_id": bs})),
        ("task_handoff", json!({"task_id": bs})),
        ("task_claim", json!({"task_id": bs})),
    ] {
        refused(h.engine.call(agent(&a), tool, params));
    }
    // The person's calls only, and the agent can't cancel.
    h.engine.call(agent(&a), "task_done", json!({"task_id": mine, "note": "done"})).unwrap();
    refused(h.engine.call(agent(&a), "task_accept", json!({"task_id": mine})));
    refused(h.engine.call(agent(&a), "task_reopen", json!({"task_id": mine})));
    refused(h.engine.call(agent(&a), "task_update", json!({"task_id": bs, "cancel": true})));
    // Assigning to itself would type into its own prompt.
    let free = h.ok("task_new", json!({"session_id": a, "title": "Free"})).data["id"].as_str().unwrap().to_owned();
    refused(h.engine.call(agent(&a), "task_assign", json!({"task_id": free, "session_id": a})));
    // Assigning across projects is refused before anyone is asked.
    refused(h.engine.call(agent(&a), "task_assign", json!({"task_id": free, "session_id": c})));
    // An outsider (not in one of our sessions) does no bookkeeping.
    refused(h.engine.call(outsider(), "task_new", json!({"title": "x", "project": "group:"})));
    refused(h.engine.call(outsider(), "task_claim", json!({"task_id": free})));
    // None of that asked the person.
    while let Ok(e) = events.try_recv() {
        assert!(!matches!(e, Event::ConsentRequested { .. }), "a refused or bookkeeping call asked: {e:?}");
    }
    assert_eq!(task_state(&h, &bs)["title"], "B's");
    assert_eq!(task_state(&h, &bs)["assignee"], b.as_str());

    // At most fifty open tasks per session.
    let made = h.engine.view().tasks.iter().filter(|t| t.created_by == format!("session:{a}") && t.state.is_open()).count();
    for i in made..50 {
        h.engine.call(agent(&a), "task_new", json!({"title": format!("n{i}")})).unwrap();
    }
    let m = refused(h.engine.call(agent(&a), "task_new", json!({"title": "one too many"})));
    assert!(m.contains("50"), "{m}");
    // Untrusted text is capped.
    let long = "x".repeat(201);
    assert!(matches!(h.engine.call(agent(&b), "task_new", json!({"title": long})), Err(ToolError::InvalidParams(_))));
}

#[test]
fn assign_and_delete_ask_the_person_first() {
    let h = H::new();
    let g = group(&h, "G");
    let (a, b) = (session_in(&h, &g), session_in(&h, &g));
    let t = h.ok("task_new", json!({"session_id": a, "title": "Review the PR"})).data["id"].as_str().unwrap().to_owned();
    let events = h.engine.subscribe();
    let rx = h.engine.submit(agent(&a), "task_assign", json!({"task_id": t, "session_id": b}));
    let request = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ConsentRequested { request }) => break request,
            Ok(_) => continue,
            Err(e) => panic!("no consent card: {e}"),
        }
    };
    assert_eq!(request["tool"], "task_assign");
    assert!(rx.try_recv().is_err(), "nothing happens before the answer");
    assert_eq!(task_state(&h, &t)["state"], "backlog");
    h.ok("consent_answer", json!({"request_id": request["request_id"], "answer": "allow_once"}));
    rx.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(task_state(&h, &t)["assignee"], b.as_str());
    let pty_b = h.pty(1);
    assert!(pty_b.written_text().contains(&format!("Task {t} from the user: Review the PR")), "{:?}", pty_b.written_text());

    let rx = h.engine.submit(agent(&a), "task_delete", json!({"task_id": t}));
    let request = loop {
        match events.recv_timeout(Duration::from_secs(5)) {
            Ok(Event::ConsentRequested { request }) => break request,
            Ok(_) => continue,
            Err(e) => panic!("no consent card: {e}"),
        }
    };
    h.ok("consent_answer", json!({"request_id": request["request_id"], "answer": "deny"}));
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err(ToolError::Declined(_))));
    assert_eq!(task_state(&h, &t)["title"], "Review the PR", "a denied delete keeps the task");
}

#[test]
fn every_session_has_a_status_since_and_changes_reach_the_status_log() {
    let ix = Index::start(IndexOptions::memory()).unwrap();
    let h = H::start(test_app(), |o| o.index = IndexSetting::Handle(ix.clone()));
    let a = h.session("pwsh");
    let v = h.engine.view();
    let s = v.session(&a).unwrap();
    assert!(s.status_since_ms.is_some(), "idle sessions have one too");
    assert!(s.inbox_since_ms.is_none());
    h.ok("session_sleep", json!({"session_id": a}));
    h.wait_status(&a, Status::Asleep);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let id = a.clone();
        let spans = ix.call_blocking(move |db| db.status_log(Some(&id), 0)).unwrap();
        let statuses: Vec<&str> = spans.iter().map(|s| s.status.as_str()).collect();
        if statuses.last() == Some(&"asleep") {
            assert!(statuses.len() <= 3, "starting flickers are merged: {statuses:?}");
            break;
        }
        assert!(Instant::now() < deadline, "the status log has {statuses:?}");
        std::thread::sleep(Duration::from_millis(20));
    }
}
