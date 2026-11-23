//! The remote window end to end (design section 7, P1 step 6): a real engine with a fake PTY, the
//! real `csm-peer` server on 127.0.0.1, and a `RemoteBackend` connected to it, through a TCP proxy
//! the test can cut. Only the PTY is fake.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::*;
use csm_engine::{Caller, ToolError};
use csm_remote::{LinkState, RemoteBackend};
use csm_term::GridSize;
use csm_ui::terminal::SourceState;

use csm_ui::backend::{Backend as UiBackend, ToolPort};

use serde_json::json;

#[test]
fn the_laptop_connects_and_sees_the_office_sessions() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let (mut b, _me) = connect(&office, None, "Laptop");

    settle(&mut b, "the connection", live);
    settle(&mut b, "the session list", |b| b.model().sessions.iter().any(|s| s.id == id));

    settle(&mut b, "the host's tool list", |b| !b.tools().is_empty());
    let tools = b.tools();
    let rename = tools.iter().find(|t| t.id == "session_rename").expect("the host's tools are listed");
    assert!(rename.effects.contains(csm_engine::Effects::MUTATES_STATE));
}

#[test]
fn a_tool_call_is_proxied_and_its_delta_reaches_the_mirror() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the session list", |b| live(b) && b.model().sessions.iter().any(|s| s.id == id));

    let out = call(&mut b, "session_rename", json!({"session_id": id, "name": "Renamed from the laptop"})).unwrap();
    assert!(!out.message.is_empty());
    settle(&mut b, "the renamed session in the mirror", |b| b.model().sessions.iter().any(|s| s.id == id && s.name == "Renamed from the laptop"));
    // The office has it too: the call ran there.
    let office_name = office.engine.view().sessions.iter().find(|s| s.id == id).map(|s| s.name.clone());
    assert_eq!(office_name.as_deref(), Some("Renamed from the laptop"));
}

#[test]
fn a_view_call_stays_on_the_laptop_and_a_focus_reaches_the_office() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the session list", |b| live(b) && b.model().sessions.iter().any(|s| s.id == id));
    let office_view_before = office.engine.view().view;

    call(&mut b, "view_set", json!({"view": "headers"})).unwrap();
    assert_eq!(b.model().group_mode, csm_ui::model::GroupMode::Headers, "the laptop shows its own view");
    assert_eq!(office.engine.view().view, office_view_before, "the office keeps its own view");

    let missing = call(&mut b, "layout_set", json!({"group": "no-such-group"}));
    assert!(missing.is_err(), "an unknown group is refused as the engine refuses it");

    call(&mut b, "session_focus", json!({"session_id": id})).unwrap();
    assert_eq!(b.model().active.as_deref(), Some(id.as_str()));
    assert_eq!(office.engine.view().focused.as_deref(), Some(id.as_str()), "the focus reached the office");
    assert!(b.submitted().iter().any(|c| c.id == "session_focus"));
    assert!(!b.busy(), "nothing is left waiting");
}

#[test]
fn a_terminal_follows_the_office_types_pastes_and_resizes() {
    let office = Office::new();
    let (id, pty, handle) = office.session();
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the session list", |b| live(b) && b.model().sessions.iter().any(|s| s.id == id));

    b.show_terminals(std::slice::from_ref(&id));
    settle(&mut b, "the pane's first screen", |b| b.terminal(&id).is_some_and(|t| t.state() == SourceState::Running));

    pty.emit(b"\x1b[31mred\x1b[0m words\r\nsecond line\r\n");
    assert_lockstep(&mut b, &office, &handle, &id, "the emulators to agree after output");
    let shown = screen_of_pane(b.terminal(&id).unwrap().as_ref());
    assert!(text_of(&shown).contains("second line"), "the laptop draws the office's output");

    // Typing goes to the PTY.
    let term = b.terminal(&id).unwrap();
    term.send_key(b"ls\r");
    settle(&mut b, "the typed bytes at the PTY", |_| pty.written().ends_with(b"ls\r"));

    // Pasted text is bracketed by the office when the program asked for it; here it at least arrives.
    term.paste("pasted text");
    settle(&mut b, "the pasted text at the PTY", |_| {
        let w = pty.written();
        w.windows(b"pasted text".len()).any(|x| x == b"pasted text")
    });

    // Resize: the pane that types drives the PTY size, and the laptop's emulator follows the office.
    b.set_pane_driver(&id, true);
    term.set_grid(GridSize::new(100, 30));
    settle(&mut b, "the PTY to take the pane's size", |_| pty.size() == (100, 30));
    settle(&mut b, "the laptop's emulator to take the new size", |b| b.terminal(&id).is_some_and(|t| pane_size(t.as_ref()) == (100, 30)));
    pty.emit(b"after the resize\r\n");
    assert_lockstep(&mut b, &office, &handle, &id, "the emulators to agree after a resize");

    // A pane that is hidden is unsubscribed: its emulator is no longer offered.
    b.show_terminals(&[]);
    assert!(b.terminal(&id).is_none());
}

#[test]
fn a_dropped_connection_resumes_the_terminal_where_it_left_off() {
    let office = Office::new();
    let (id, pty, handle) = office.session();
    let proxy = Proxy::start(office.server.local_addr().unwrap());
    let (mut b, _me) = connect(&office, Some(proxy.port()), "Laptop");
    settle(&mut b, "the connection", live);
    settle(&mut b, "the session list", |b| b.model().sessions.iter().any(|s| s.id == id));

    b.show_terminals(std::slice::from_ref(&id));
    pty.emit(b"before the drop\r\n");
    assert_lockstep(&mut b, &office, &handle, &id, "the emulators to agree before the drop");

    // The link goes down; the office keeps running and its output keeps coming.
    proxy.cut();
    settle(&mut b, "the laptop to notice the drop", |b| !live(b));
    pty.emit(b"output while the laptop was away\r\n");
    settle(&mut b, "the laptop to reconnect", live);

    assert_lockstep(&mut b, &office, &handle, &id, "the emulators to agree after the resume");
    let shown = screen_of_pane(b.terminal(&id).unwrap().as_ref());
    assert!(text_of(&shown).contains("output while the laptop was away"), "the gap is filled in");
}

#[test]
fn a_revoked_device_ends_the_window_as_refused() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the session list", |b| live(b) && b.model().sessions.iter().any(|s| s.id == id));

    office.ui("peer_device_revoke", json!({"all": true})).unwrap();
    settle(&mut b, "the window to be refused", |b| matches!(b.link_state(), LinkState::Refused { .. }));
    let LinkState::Refused { reason } = b.link_state().clone() else { panic!("refused") };
    assert!(reason.contains("removed"), "the reason says what happened: {reason}");
    assert!(!b.busy());
}

/// Polls the backend until an event satisfies `want`, and returns what it matched.
fn wait_event<T>(b: &mut RemoteBackend, what: &str, mut want: impl FnMut(&csm_engine::Event) -> Option<T>) -> T {
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

#[test]
fn a_consent_card_from_an_office_agent_shows_on_the_laptop_and_the_first_answer_wins() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the session list", |b| live(b) && b.model().sessions.iter().any(|s| s.id == id));

    // An agent (an MCP client) on the office asks to type into the session. It waits for the person.
    let engine = office.engine.clone();
    let asked = id.clone();
    let agent = std::thread::spawn(move || {
        engine.call(
            Caller::External { client: "agent".into(), session_id: None },
            "session_send_text",
            json!({"session_id": asked, "text": "rm -rf"}),
        )
    });

    // The card reaches the laptop and can be answered there.
    let request_id = wait_event(&mut b, "the consent card on the laptop", |e| match e {
        csm_engine::Event::ConsentRequested { request } => request["request_id"].as_u64(),
        _ => None,
    });
    // The answer and the resolution both come back over the link. This loop keeps the events, so the
    // resolution is not lost while the answer's reply is awaited.
    b.submit("consent_answer", json!({"request_id": request_id, "answer": "deny"}));
    let deadline = Instant::now() + Duration::from_secs(20);
    let (mut answered, mut resolved) = (false, false);
    while !(answered && resolved) {
        let (replies, events) = b.poll();
        for (c, reply) in &replies {
            if c.id == "consent_answer" {
                assert!(reply.is_ok(), "the device may answer its card: {reply:?}");
                answered = true;
            }
        }
        resolved |= events.iter().any(|e| matches!(e, csm_engine::Event::ConsentResolved { request_id: r } if *r == request_id));
        assert!(Instant::now() < deadline, "the answer and the resolution to come back");
        std::thread::sleep(Duration::from_millis(5));
    }

    // The agent is declined, and the card is gone everywhere.
    assert!(matches!(agent.join().unwrap(), Err(ToolError::Declined(_))));
    assert!(!pty.written().windows(b"rm -rf".len()).any(|w| w == b"rm -rf"), "nothing was typed");

    // The second answer, from the office, finds nothing left to answer.
    let late = office.ui("consent_answer", json!({"request_id": request_id, "answer": "allow_once"}));
    assert!(late.is_err(), "the first answer won");
}

#[test]
fn a_window_shares_one_connection_between_its_port_and_its_view() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let (b, _me) = connect(&office, None, "Laptop");
    let (mut port, view, handle) = csm_remote::shared(b);

    // The port's calls go to the office and come back; the view sees the same link and mirror.
    wait_for("the link", || match view.remote()?.link {
        csm_ui::remote_window::RemoteLink::Connected => Some(()),
        _ => {
            port.poll();
            None
        }
    });
    let status = view.remote().expect("a remote window always has a status");
    assert_eq!(status.link, csm_ui::remote_window::RemoteLink::Connected);
    port.submit("session_rename", json!({"session_id": id, "name": "Shared"}));
    wait_for("the rename's reply", || {
        let (done, _) = port.poll();
        done.iter().any(|(c, r)| c.id == "session_rename" && r.is_ok()).then_some(())
    });
    // The mirror is rebuilt when the port is polled (the UI does both once a frame).
    wait_for("the renamed session in the model", || {
        let _ = port.poll();
        view.model().sessions.iter().any(|s| s.id == id && s.name == "Shared").then_some(())
    });
    assert!(view.model().remote, "the model says it is a remote window");
    assert_eq!(port.submitted().last().map(|c| c.id.as_str()), Some("session_rename"));
    assert_eq!(handle.overlay().theme, None, "a fresh window has no theme of its own yet");
}
