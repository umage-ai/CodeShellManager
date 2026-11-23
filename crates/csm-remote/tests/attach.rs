//! `csm attach` against a real office (P1 step 8): the console draws the session's grid, then the live
//! bytes, types into the session, follows the console's size, detaches without touching the session,
//! and reports a session that ends or a link that drops. The console is a channel the test drives.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use common::*;
use csm_peer::client::{self, ClientConfig};
use csm_peer_proto::msg::TermClosedReason;
use csm_pty::Pty;
use csm_remote::attach::{self, AttachEnd, Console, ConsoleEvent, DETACH_KEY};
use serde_json::json;

/// A console whose events come from the test, and whose output the test reads.
struct Tty {
    rx: Receiver<ConsoleEvent>,
    out: Arc<Mutex<Vec<u8>>>,
    size: Arc<Mutex<(u16, u16)>>,
}

impl Console for Tty {
    fn size(&self) -> (u16, u16) {
        *self.size.lock().unwrap()
    }
    fn write(&mut self, bytes: &[u8]) {
        self.out.lock().unwrap().extend_from_slice(bytes);
    }
    fn next(&mut self, wait: Duration) -> Option<ConsoleEvent> {
        if wait.is_zero() { self.rx.try_recv().ok() } else { self.rx.recv_timeout(wait).ok() }
    }
}

/// One attach running on its own thread, as `csm attach` runs it.
struct Attached {
    events: Sender<ConsoleEvent>,
    out: Arc<Mutex<Vec<u8>>>,
    size: Arc<Mutex<(u16, u16)>>,
    end: JoinHandle<AttachEnd>,
}

impl Attached {
    fn key(&self, bytes: &[u8]) {
        self.events.send(ConsoleEvent::Key(bytes.to_vec())).unwrap();
    }
    fn resize(&self, cols: u16, rows: u16) {
        *self.size.lock().unwrap() = (cols, rows);
        self.events.send(ConsoleEvent::Resized { cols, rows }).unwrap();
    }
    fn output(&self) -> Vec<u8> {
        self.out.lock().unwrap().clone()
    }
    fn detach(self) -> AttachEnd {
        self.key(&[DETACH_KEY, b'd']);
        self.end.join().unwrap()
    }
}

/// Attaches a laptop's console to `id` on the office, with the console at `size`.
fn attach(office: &Office, id: &str, size: (u16, u16)) -> Attached {
    let (me, host) = office.pair("Console");
    let conn = client::connect(&host.address, &host.host_public, &me, &ClientConfig::new("Console")).unwrap();
    let (events, rx) = mpsc::channel();
    let out = Arc::new(Mutex::new(Vec::new()));
    let shown = Arc::new(Mutex::new(size));
    let mut tty = Tty { rx, out: out.clone(), size: shown.clone() };
    let session = id.to_owned();
    let end = std::thread::spawn(move || {
        let end = attach::run(&conn, &session, &mut tty);
        conn.close();
        end
    });
    Attached { events, out, size: shown, end }
}

fn wait_output(a: &Attached, what: &str, needle: &str) {
    wait_for(what, || String::from_utf8_lossy(&a.output()).contains(needle).then_some(()));
}

fn wait_pty(pty: &FakePty, what: &str, ok: impl Fn(&FakePty) -> bool) {
    wait_for(what, || ok(pty).then_some(()));
}

#[test]
fn attach_draws_the_grid_then_the_live_output_and_types_into_the_session() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    pty.emit(b"before the attach\r\n");
    let size = pty.size();
    let a = attach(&office, &id, size);

    wait_output(&a, "the grid", "before the attach");
    assert!(a.output().starts_with(b"\x1b[H"), "the console clears first, then draws");

    a.key(b"ls\r");
    wait_pty(&pty, "the typed line at the PTY", |p| p.written().ends_with(b"ls\r"));

    pty.emit(b"live output\r\n");
    wait_output(&a, "the live output", "live output");

    assert_eq!(a.detach(), AttachEnd::Detached);
    // The session is untouched: still listed, still running.
    let listed = office.engine.view().sessions.iter().any(|s| s.id == id);
    assert!(listed, "the session is still there");
    assert!(!pty.has_exited(), "the detach did not end the process");
}

#[test]
fn ctrl_bracket_twice_types_one_ctrl_bracket_and_a_later_key_is_sent_as_typed() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let a = attach(&office, &id, pty.size());
    a.key(&[DETACH_KEY, DETACH_KEY]);
    wait_pty(&pty, "one Ctrl+] at the PTY", |p| p.written().ends_with(&[DETACH_KEY]));
    a.key(b"x");
    wait_pty(&pty, "the x at the PTY", |p| p.written().ends_with(b"x"));
    assert_eq!(a.detach(), AttachEnd::Detached);
}

#[test]
fn a_pasted_text_reaches_the_session_bracketed_by_the_office() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let a = attach(&office, &id, pty.size());
    a.events.send(ConsoleEvent::Paste("pasted from the console".into())).unwrap();
    wait_pty(&pty, "the pasted text", |p| String::from_utf8_lossy(&p.written()).contains("pasted from the console"));
    assert_eq!(a.detach(), AttachEnd::Detached);
}

#[test]
fn the_console_size_becomes_the_session_size_on_attach_and_on_each_change() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let want = (100, 30);
    assert_ne!(pty.size(), want, "the test needs a size the session does not have yet");

    let a = attach(&office, &id, want);
    wait_pty(&pty, "the session to take the console's size", |p| p.size() == want);

    a.resize(120, 40);
    wait_pty(&pty, "the session to follow a console resize", |p| p.size() == (120, 40));
    assert_eq!(a.detach(), AttachEnd::Detached);
}

#[test]
fn a_session_that_closes_ends_the_attach_and_the_console_says_why() {
    let office = Office::new();
    let (id, _pty, _handle) = office.session();
    let a = attach(&office, &id, (80, 24));
    wait_for("the first draw", || (!a.output().is_empty()).then_some(()));
    office.ui("session_close", json!({"session_id": id, "force": true})).unwrap();
    let end = a.end.join().unwrap();
    assert!(matches!(end, AttachEnd::SessionEnded(_)), "the attach ends with the session: {end:?}");
    if let AttachEnd::SessionEnded(reason) = end {
        assert!(matches!(reason, TermClosedReason::SessionGone | TermClosedReason::NotRunning), "{reason:?}");
    }
}

#[test]
fn a_dropped_link_ends_the_attach_as_lost_and_the_session_keeps_running() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let proxy = Proxy::start(office.server.local_addr().unwrap());
    let (me, host) = office.pair("Console");
    let addr = csm_peer_proto::addr::PeerAddr { host: csm_peer_proto::addr::Host::Ip("127.0.0.1".parse().unwrap()), port: proxy.port() };
    let conn = client::connect(&addr, &host.host_public, &me, &ClientConfig::new("Console")).unwrap();
    let (events, rx) = mpsc::channel();
    let _keep = events;
    let mut tty = Tty { rx, out: Arc::new(Mutex::new(Vec::new())), size: Arc::new(Mutex::new((80, 24))) };
    let session = id.clone();
    let end = std::thread::spawn(move || attach::run(&conn, &session, &mut tty));
    std::thread::sleep(Duration::from_millis(100));
    proxy.cut();
    let end = end.join().unwrap();
    assert!(matches!(end, AttachEnd::LinkLost(ref why) if !why.is_empty()), "{end:?}");
    assert!(!pty.has_exited(), "a lost console does not end the session");
}

fn count_of(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

#[test]
fn two_consoles_of_different_sizes_settle_instead_of_resizing_each_other() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let clear = b"\x1b[H\x1b[2J\x1b[3J";

    let a = attach(&office, &id, (80, 24));
    wait_pty(&pty, "the session to take the first console's size", |p| p.size() == (80, 24));
    let b = attach(&office, &id, (120, 40));
    wait_pty(&pty, "the session to take the second console's size", |p| p.size() == (120, 40));

    // Each console draws the grid a few times while the size is handed over. Then it must stop:
    // the same count after a quiet period means no console is redrawing the other.
    let draws = || (count_of(&a.output(), clear), count_of(&b.output(), clear));
    std::thread::sleep(Duration::from_millis(400));
    let settled = draws();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(draws(), settled, "the consoles stop redrawing once the size is handed over");
    assert!(settled.0 <= 4 && settled.1 <= 4, "a handful of draws, not a loop: {settled:?}");
    assert_eq!(pty.size(), (120, 40), "the last console to change size keeps it");

    // Both consoles still type into the one session.
    a.key(b"a");
    wait_pty(&pty, "the first console's key", |p| p.written().ends_with(b"a"));
    b.key(b"b");
    wait_pty(&pty, "the second console's key", |p| p.written().ends_with(b"b"));

    assert_eq!(a.detach(), AttachEnd::Detached);
    assert_eq!(b.detach(), AttachEnd::Detached);
}

#[test]
fn a_console_resize_claims_the_size_once_and_the_other_console_follows_without_a_loop() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let clear = b"\x1b[H\x1b[2J\x1b[3J";
    let a = attach(&office, &id, (80, 24));
    let b = attach(&office, &id, (80, 24));
    wait_pty(&pty, "the session at the shared size", |p| p.size() == (80, 24));
    std::thread::sleep(Duration::from_millis(200));
    let before = count_of(&b.output(), clear);

    a.resize(100, 30);
    wait_pty(&pty, "the session to follow the resize", |p| p.size() == (100, 30));
    std::thread::sleep(Duration::from_millis(400));
    let after = count_of(&b.output(), clear);
    assert!(after <= before + 2, "the other console redraws a bounded number of times: {before} -> {after}");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(count_of(&b.output(), clear), after, "and then stops");
    assert_eq!(pty.size(), (100, 30));
    assert_eq!(a.detach(), AttachEnd::Detached);
    assert_eq!(b.detach(), AttachEnd::Detached);
}
