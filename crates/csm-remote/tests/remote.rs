//! The remote window end to end (design section 7, P1 step 6): a real engine with a fake PTY, the
//! real `csm-peer` server on 127.0.0.1, and a `RemoteBackend` connected to it, through a TCP proxy
//! the test can cut. Only the PTY is fake.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    // The office hears it (to wake the session and mark it seen); its own focus stays its own
    // (a_laptops_focus_leaves_the_offices_own_view_as_it_is).
    assert!(b.submitted().iter().any(|c| c.id == "session_focus"), "the focus reached the office");
    assert!(!b.busy(), "nothing is left waiting");
}

#[test]
fn a_laptops_focus_leaves_the_offices_own_view_as_it_is() {
    let office = Office::new();
    let (mine, _p1, _h1) = office.session();
    let (theirs, _p2, _h2) = office.session();
    let home = office.ui("group_new", json!({"name": "Home"})).unwrap().data["group_id"].as_str().unwrap().to_owned();
    let work = office.ui("group_new", json!({"name": "Work"})).unwrap().data["group_id"].as_str().unwrap().to_owned();
    office.ui("session_move_to_group", json!({"session_id": mine, "group_id": home})).unwrap();
    office.ui("session_move_to_group", json!({"session_id": theirs, "group_id": work})).unwrap();
    // The office sits on Home with its own session in front.
    office.ui("view_set", json!({"rail_tab": home})).unwrap();
    office.ui("session_focus", json!({"session_id": mine})).unwrap();
    assert_eq!(office.engine.view().rail_tab.as_deref(), Some(home.as_str()));
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "both sessions in the mirror", |b| live(b) && !b.tools().is_empty() && [&mine, &theirs].iter().all(|id| b.model().sessions.iter().any(|s| &s.id == *id)));

    // The laptop switches to Work (focusing its session there) and then focuses it directly.
    call(&mut b, "view_set", json!({"rail_tab": work})).unwrap();
    settle(&mut b, "the focus answered", |b| !b.busy());
    call(&mut b, "session_focus", json!({"session_id": theirs})).unwrap();
    settle(&mut b, "the second focus answered", |b| !b.busy());
    assert_eq!(b.model().rail, csm_ui::model::Scope::Group(work.clone()), "the laptop is on Work");
    assert_eq!(b.model().active.as_deref(), Some(theirs.as_str()));
    let view = office.engine.view();
    assert_eq!(view.rail_tab.as_deref(), Some(home.as_str()), "the office stays on Home");
    assert_eq!(view.focused.as_deref(), Some(mine.as_str()), "and keeps its own session in front");
}

#[test]
fn a_rail_tab_switches_the_laptops_rail_and_the_office_only_hears_the_focus() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let gid = office.ui("group_new", json!({"name": "Work"})).unwrap().data["group_id"].as_str().unwrap().to_owned();
    office.ui("session_move_to_group", json!({"session_id": id, "group_id": gid})).unwrap();
    let (mut b, _me) = connect(&office, None, "Laptop");
    settle(&mut b, "the group in the mirror", |b| live(b) && b.model().groups.iter().any(|g| g.id == gid) && b.model().sessions.iter().any(|s| s.id == id));
    settle(&mut b, "the host's tool list", |b| !b.tools().is_empty());
    let office_rail_before = office.engine.view().rail_tab.clone();
    assert_eq!(b.model().rail, csm_ui::model::Scope::All);
    // Focus an ungrouped session first, so the group tab has something to switch to.
    let (other, _p2, _h2) = office.session();
    settle(&mut b, "the second session", |b| b.model().sessions.iter().any(|s| s.id == other));
    call(&mut b, "session_focus", json!({"session_id": other})).unwrap();
    let sent_before = b.submitted().len();

    // A group tab click: the laptop switches, and the office gets the focus but keeps its own tab.
    call(&mut b, "view_set", json!({"rail_tab": gid})).unwrap();
    assert_eq!(b.model().rail, csm_ui::model::Scope::Group(gid.clone()), "the laptop's rail switched");
    assert_eq!(b.model().active.as_deref(), Some(id.as_str()), "and focused the group's session");
    assert_eq!(office.engine.view().rail_tab, office_rail_before, "the office's rail tab did not follow");
    let sent: Vec<_> = b.submitted()[sent_before..].iter().map(|c| (c.id.clone(), c.params.clone())).collect();
    assert_eq!(sent, vec![("session_focus".to_owned(), json!({"session_id": id}))], "only the focus was sent");
    // The office answers the focus (results seen, last used) but keeps its own session in front.
    settle(&mut b, "the office's answer", |b| !b.busy());
    assert_ne!(office.engine.view().focused.as_deref(), Some(id.as_str()), "the office's own focus did not follow");

    // Next from the group wraps to All: nothing to focus, so nothing is sent at all.
    let sent_before = b.submitted().len();
    call(&mut b, "rail_tab_next", json!({})).unwrap();
    assert_eq!(b.model().rail, csm_ui::model::Scope::All, "the laptop wrapped to All");
    assert_eq!(office.engine.view().rail_tab, office_rail_before, "the office's rail tab is untouched");
    assert!(b.submitted()[sent_before..].iter().all(|c| c.id != "session_focus"), "a tab with no focus sends nothing to the office");
    call(&mut b, "rail_tab_next", json!({})).unwrap();
    assert_eq!(b.model().rail, csm_ui::model::Scope::Group(gid), "and the next step is the group again");
    assert_eq!(office.engine.view().rail_tab, office_rail_before);

    let unknown = call(&mut b, "view_set", json!({"rail_tab": "no-such-group"}));
    assert!(matches!(unknown, Err(ToolError::NotFound(_))), "an unknown group is refused locally: {unknown:?}");
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

/// What a frame does, through the UI's own trait: `show_panes` with the panes it drew and the
/// focused one, then the pane's terminal is drawn from `Backend::terminal`. This is the path the
/// real remote window takes (no test calls `show_terminals` by hand).
#[test]
fn a_pane_the_ui_draws_gets_the_offices_grid_through_the_backend_trait() {
    let office = Office::new();
    let (id, pty, handle) = office.session();
    let (b, _me) = connect(&office, None, "Laptop");
    let (mut port, mut view, _handle) = csm_remote::shared(b);
    wait_for("the session list", || {
        let _ = port.poll();
        view.model().sessions.iter().any(|s| s.id == id).then_some(())
    });
    // Nothing is drawn yet: no terminal is subscribed.
    assert!(view.terminal(&id).is_none(), "a session with no pane has no terminal");

    // The frame showed this pane, and it has the focus (so it types and drives the PTY size).
    view.show_panes(std::slice::from_ref(&id), Some(&id));
    wait_for("the pane's first screen", || {
        let _ = port.poll();
        view.terminal(&id).is_some_and(|t| t.state() == SourceState::Running).then_some(())
    });
    pty.emit(b"drawn through the trait\r\n");
    wait_for("the pane to draw the office's grid", || {
        let _ = port.poll();
        let t = view.terminal(&id)?;
        (screen_of_pane(t.as_ref()) == office.screen_of(&handle)).then_some(())
    });
    let shown = screen_of_pane(view.terminal(&id).unwrap().as_ref());
    assert!(text_of(&shown).contains("drawn through the trait"));

    // The focused pane is the driver: its resize reaches the PTY.
    view.terminal(&id).unwrap().set_grid(GridSize::new(100, 30));
    wait_for("the PTY to take the focused pane's size", || {
        let _ = port.poll();
        (pty.size() == (100, 30)).then_some(())
    });

    // A frame that no longer shows the pane unsubscribes it.
    view.show_panes(&[], None);
    assert!(view.terminal(&id).is_none(), "a hidden pane is unsubscribed");
}

/// A host that is out of reach keeps its last snapshot on screen, marked stale; the mark goes when
/// the link is back.
#[test]
fn an_office_out_of_reach_keeps_its_last_snapshot_marked_stale() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let proxy = Proxy::start(office.server.local_addr().unwrap());
    let (mut b, _me) = connect(&office, Some(proxy.port()), "Laptop");
    settle(&mut b, "the connection", live);
    settle(&mut b, "the session list", |b| b.model().sessions.iter().any(|s| s.id == id));
    assert!(!b.model().stale, "a live window is not stale");

    proxy.cut();
    settle(&mut b, "the snapshot marked stale", |b| b.model().stale);
    assert!(b.model().sessions.iter().any(|s| s.id == id), "the last snapshot is still shown");
    assert!(b.remote().is_some_and(|s| matches!(s.link, csm_ui::remote_window::RemoteLink::Offline)));

    settle(&mut b, "the laptop to reconnect", live);
    settle(&mut b, "the stale mark to go", |b| !b.model().stale);
}

/// Quitting a remote window stops its link: the window's worker ends and does not come back, and the
/// office is not told to quit (its session keeps running and a laptop can connect again).
#[test]
fn stopping_the_window_ends_the_link_and_leaves_the_office_running() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let (b, _me) = connect(&office, None, "Laptop");
    let (mut port, view, handle) = csm_remote::shared(b);
    wait_for("the link", || {
        let _ = port.poll();
        (view.remote()?.link == csm_ui::remote_window::RemoteLink::Connected).then_some(())
    });
    handle.stop();
    // Longer than the longest first retry: a link that was not stopped would be back by now.
    let until = Instant::now() + Duration::from_millis(2500);
    while Instant::now() < until {
        let _ = port.poll();
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_ne!(view.remote().map(|s| s.link), Some(csm_ui::remote_window::RemoteLink::Connected), "the window stays stopped");
    assert!(office.engine.view().sessions.iter().any(|s| s.id == id), "the office keeps its session");
    let (mut again, _me2) = connect(&office, None, "Second laptop");
    settle(&mut again, "the second laptop to connect", live);
}

#[test]
fn a_delta_that_does_not_apply_is_fixed_with_a_snapshot_on_the_same_link() {
    // The office sends one delta whose base this laptop does not have. The laptop asks for the whole
    // snapshot on the link it has: no reconnect, and the session that came with the bad delta shows.
    let corrupt = Arc::new(AtomicBool::new(false));
    let office = Office::new_corruptible(corrupt.clone());
    let (first, _pty, _handle) = office.session();
    let proxy = Proxy::start(office.server.local_addr().unwrap());
    let (mut b, _me) = connect(&office, Some(proxy.port()), "Laptop");
    settle(&mut b, "the connection", live);
    settle(&mut b, "the session list", |b| b.model().sessions.iter().any(|s| s.id == first));
    let links_before = proxy.conns.lock().unwrap().len();

    corrupt.store(true, Ordering::Release);
    let (second, _pty2, _handle2) = office.session();
    settle(&mut b, "the new session, from the fresh snapshot", |b| b.model().sessions.iter().any(|s| s.id == second));
    assert!(!corrupt.load(Ordering::Acquire), "the bad delta was sent, so the resync was exercised");
    assert!(live(&b), "the link is still up");
    assert_eq!(proxy.conns.lock().unwrap().len(), links_before, "no reconnect: the laptop asked on the same link");
    assert!(!b.model().stale, "a resync is not an outage");
}

#[test]
fn a_terminal_is_veiled_while_the_link_is_down_and_clear_again_after() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let proxy = Proxy::start(office.server.local_addr().unwrap());
    let (mut b, _me) = connect(&office, Some(proxy.port()), "Laptop");
    settle(&mut b, "the connection", live);
    settle(&mut b, "the session list", |b| b.model().sessions.iter().any(|s| s.id == id));
    b.show_terminals(std::slice::from_ref(&id));
    let state = |b: &RemoteBackend| b.terminal(&id).map(|t| t.state());
    settle(&mut b, "the terminal to run", |b| state(b) == Some(SourceState::Running));

    proxy.cut();
    settle(&mut b, "the terminal to be veiled", |b| state(b) == Some(SourceState::Stale));
    settle(&mut b, "the link to come back", live);
    settle(&mut b, "the terminal to run again", |b| state(b) == Some(SourceState::Running));
}
