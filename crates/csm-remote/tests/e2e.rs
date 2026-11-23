//! The client side end to end, in one process (design section 7, P1 step 8): the office's engine and
//! its `csm-peer` server, two laptops with a `RemoteBackend` each, one session they both type into,
//! a consent card answered from one laptop and closed on the office and the other laptop, and an
//! office restart that brings both laptops back to the restored session. Only the PTY is fake.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use csm_engine::{Caller, Event};
use csm_remote::{LinkState, RemoteBackend};
use csm_term::GridSize;
use csm_ui::backend::{Backend as UiBackend, ToolPort};
use csm_ui::terminal::TerminalSource;
use serde_json::json;

/// Polls both laptops until `ok` holds for each (the UI polls once a frame).
fn settle_both(a: &mut RemoteBackend, b: &mut RemoteBackend, what: &str, mut ok: impl FnMut(&RemoteBackend) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let _ = a.poll();
        let _ = b.poll();
        if ok(a) && ok(b) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits until a laptop's event satisfies `want`, and returns what it matched.
fn wait_event<T>(b: &mut RemoteBackend, what: &str, mut want: impl FnMut(&Event) -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, events) = b.poll();
        for e in &events {
            if let Some(found) = want(e) {
                return found;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn live(b: &RemoteBackend) -> bool {
    matches!(b.link_state(), LinkState::Live { .. })
}

/// Both panes show what the office's emulator shows.
fn lockstep(a: &mut RemoteBackend, b: &mut RemoteBackend, office: &Office, id: &str, what: &str) {
    let handle = wait_for("the office's terminal", || office.engine.terminal(id));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let _ = a.poll();
        let _ = b.poll();
        let same = |x: &RemoteBackend| x.terminal(id).is_some_and(|t| office.screen_of(&handle) == screen_of_pane(t.as_ref()));
        if same(a) && same(b) {
            return;
        }
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn text(term: &dyn TerminalSource) -> String {
    String::from_utf8_lossy(&screen_of_pane(term)).into_owned()
}

#[test]
fn two_laptops_type_into_one_session_and_both_panes_follow_the_office() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let (mut a, _) = connect(&office, None, "Laptop A");
    let (mut b, _) = connect(&office, None, "Laptop B");
    settle_both(&mut a, &mut b, "both connections", |x| live(x) && x.model().sessions.iter().any(|s| s.id == id));

    a.show_terminals(std::slice::from_ref(&id));
    b.show_terminals(std::slice::from_ref(&id));
    settle_both(&mut a, &mut b, "both panes", |x| x.terminal(&id).is_some());

    // Each laptop types; both lines reach the one PTY.
    let ta = a.terminal(&id).unwrap();
    ta.send_key(b"from A\r");
    wait_for("A's line at the PTY", || pty.written().ends_with(b"from A\r").then_some(()));
    let tb = b.terminal(&id).unwrap();
    tb.send_key(b"from B\r");
    wait_for("B's line at the PTY", || pty.written().ends_with(b"from B\r").then_some(()));

    // Output reaches both laptops, and both panes match the office's screen.
    pty.emit(b"both typed\r\n");
    lockstep(&mut a, &mut b, &office, &id, "both panes to follow the office");
    assert!(text(a.terminal(&id).unwrap().as_ref()).contains("both typed"));
    assert!(text(b.terminal(&id).unwrap().as_ref()).contains("both typed"));

    // The laptop that types drives the size: the PTY and both panes follow it.
    b.set_pane_driver(&id, true);
    tb.set_grid(GridSize::new(100, 30));
    wait_for("the PTY to take B's size", || (pty.size() == (100, 30)).then_some(()));
    pty.emit(b"after the resize\r\n");
    lockstep(&mut a, &mut b, &office, &id, "both panes after the resize");
}

#[test]
fn a_consent_card_shows_on_both_laptops_and_a_laptop_answer_closes_it_on_the_office() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let (mut a, _) = connect(&office, None, "Laptop A");
    let (mut b, _) = connect(&office, None, "Laptop B");
    settle_both(&mut a, &mut b, "both connections", |x| live(x) && x.model().sessions.iter().any(|s| s.id == id));
    let office_events = office.engine.subscribe();

    // An agent on the office asks to type into the session; it waits for the person.
    let engine = office.engine.clone();
    let asked = id.clone();
    let agent = std::thread::spawn(move || {
        engine.call(
            Caller::External { client: "agent".into(), session_id: None },
            "session_send_text",
            json!({"session_id": asked, "text": "npm test\r"}),
        )
    });

    // Both laptops show the same card; laptop A answers it.
    let request = wait_event(&mut a, "the card on laptop A", |e| match e {
        Event::ConsentRequested { request } => request["request_id"].as_u64(),
        _ => None,
    });
    let seen_by_b = wait_event(&mut b, "the card on laptop B", |e| match e {
        Event::ConsentRequested { request } => request["request_id"].as_u64(),
        _ => None,
    });
    assert_eq!(request, seen_by_b, "both laptops show the same card");

    a.submit("consent_answer", json!({"request_id": request, "answer": "allow_once"}));
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut answered, mut resolved_on_b) = (false, false);
    while !(answered && resolved_on_b) {
        let (replies, _) = a.poll();
        answered |= replies.iter().any(|(c, r)| c.id == "consent_answer" && r.is_ok());
        let (_, events) = b.poll();
        resolved_on_b |= events.iter().any(|e| matches!(e, Event::ConsentResolved { request_id } if *request_id == request));
        assert!(Instant::now() < deadline, "the answer to come back and the card to close on laptop B");
        std::thread::sleep(Duration::from_millis(5));
    }

    // The office closed the card too: the agent's call went ahead and the office saw the resolution.
    let ran = agent.join().unwrap();
    assert!(ran.is_ok(), "the agent's call went ahead once allowed: {ran:?}");
    wait_for("the agent's text at the PTY", || String::from_utf8_lossy(&pty.written()).contains("npm test").then_some(()));
    let resolved_on_office = office_events.try_iter().any(|e| matches!(e, Event::ConsentResolved { request_id } if request_id == request));
    assert!(resolved_on_office, "the office saw the card close");

    // A second answer, from the other laptop, finds nothing left to answer.
    b.submit("consent_answer", json!({"request_id": request, "answer": "deny"}));
    let deadline = Instant::now() + Duration::from_secs(20);
    let late = loop {
        let (replies, _) = b.poll();
        if let Some((_, r)) = replies.iter().find(|(c, _)| c.id == "consent_answer") {
            break r.is_ok();
        }
        assert!(Instant::now() < deadline, "the second answer to come back");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(!late, "the first answer won");
}

#[test]
fn an_office_restart_brings_both_laptops_back_on_the_restored_session() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let (mut a, _) = connect(&office, None, "Laptop A");
    let (mut b, _) = connect(&office, None, "Laptop B");
    settle_both(&mut a, &mut b, "both connections", |x| live(x) && x.model().sessions.iter().any(|s| s.id == id));
    a.show_terminals(std::slice::from_ref(&id));
    b.show_terminals(std::slice::from_ref(&id));
    settle_both(&mut a, &mut b, "both panes", |x| x.terminal(&id).is_some());
    pty.emit(b"before the restart\r\n");
    lockstep(&mut a, &mut b, &office, &id, "both panes before the restart");

    // The office app quits and starts again: both laptops lose the link and reconnect.
    let office = office.restart();
    settle_both(&mut a, &mut b, "both laptops to reconnect", live);
    assert!(office.engine.view().sessions.iter().any(|s| s.id == id), "the session was restored with its id");

    // The restored session is a new process (a new instance), so each laptop attaches afresh.
    let pty = wait_for("the restored session's PTY", || office.ptys.lock().unwrap().first().cloned());
    pty.emit(b"after the restart\r\n");
    lockstep(&mut a, &mut b, &office, &id, "both panes after the restart");
    let shown = text(a.terminal(&id).unwrap().as_ref());
    assert!(shown.contains("after the restart"), "{shown}");
    assert!(!shown.contains("before the restart"), "the new screen does not keep the old one");

    // Typing still works from either laptop, into the restored session.
    a.terminal(&id).unwrap().send_key(b"again\r");
    wait_for("the typed line at the restored PTY", || pty.written().ends_with(b"again\r").then_some(()));
}
