//! `csm attach` from the laptop's own client folder (P1 step 8): the office is paired the way the app
//! pairs it (a laptop engine's `peer_client_pair`, with its `ClientPeer` writing `peer-client/`), and
//! then a console attaches through the folder alone. The console never pairs, an unpaired office is
//! refused with nothing written, and the attached console types into the office's session.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use csm_engine::{Caller, Engine, PeerClientHost, State, StateOptions};
use csm_peer::ClientPeer;
use csm_pty::Pty;
use csm_remote::attach::{self, AttachEnd, Console, ConsoleEvent, DETACH_KEY};
use csm_remote::paired_offices;
use serde_json::json;

/// The laptop's own engine: its client folder is `dir/peer-client`, and it writes nothing else.
fn laptop(dir: &Path) -> Engine {
    let options = StateOptions {
        restore_on_start: false,
        peer_client: Arc::new(ClientPeer::new(dir.join("peer-client"))) as Arc<dyn PeerClientHost>,
        ..StateOptions::default()
    };
    let (state, _) = State::load(options);
    Engine::start(state, vec![]).unwrap()
}

/// A console that plays a fixed script of events, and keeps what the office draws.
struct Script {
    started: std::time::Instant,
    events: VecDeque<ConsoleEvent>,
    out: Arc<Mutex<Vec<u8>>>,
}

impl Console for Script {
    fn size(&self) -> (u16, u16) {
        (80, 24)
    }
    fn write(&mut self, bytes: &[u8]) {
        self.out.lock().unwrap().extend_from_slice(bytes);
    }
    /// The detach waits until the grid has been drawn, so the test sees the attach's first screen.
    fn next(&mut self, _wait: Duration) -> Option<ConsoleEvent> {
        let detaching = matches!(self.events.front(), Some(ConsoleEvent::Key(k)) if k.first() == Some(&DETACH_KEY));
        let drawn = String::from_utf8_lossy(&self.out.lock().unwrap()).contains("from the office");
        if detaching && !drawn && self.started.elapsed() < Duration::from_secs(15) {
            return None;
        }
        self.events.pop_front()
    }
}

#[test]
fn a_console_attaches_through_the_client_folder_to_a_paired_office_and_never_pairs() {
    let office = Office::new();
    let (id, pty, _handle) = office.session();
    let cfg = tempfile::tempdir().unwrap();
    let client_dir = cfg.path().join("peer-client");

    // Nothing is paired: the console is refused, and writes nothing.
    let refused = attach::connect(&client_dir, "ABC234").expect_err("nothing is paired");
    assert!(refused.contains("no device key"), "{refused}");
    assert!(!client_dir.exists(), "a console never makes the client folder");

    // The person pairs the office in the laptop's app: the office offers, the laptop pairs, the office
    // shows the check code and the person allows it.
    let me = laptop(cfg.path());
    let link = office.ui("peer_pair_start", json!({})).unwrap().data["link"].as_str().unwrap().to_owned();
    me.call(Caller::Ui, "peer_client_pair", json!({"link": link, "name": "Office"})).unwrap();
    let code = wait_for("the check code on the office", || office.status()["pending_pair"]["check_code"].as_str().map(str::to_owned));
    office.ui("peer_pair_answer", json!({"allow": true, "check_code": code})).unwrap();
    let host_id = wait_for("the office in the laptop's list", || {
        let offices = paired_offices(&client_dir).ok()?;
        offices.first().map(|o| o.host_id.clone())
    });

    // The console connects through the folder, lists the sessions and types into one.
    let (_name, conn) = attach::connect(&client_dir, &host_id).expect("the paired office accepts the console");
    let sessions = attach::list_sessions(&conn).expect("the session list");
    assert!(sessions.iter().any(|s| s["id"].as_str() == Some(id.as_str())), "the session is listed");

    pty.emit(b"from the office\r\n");
    let out = Arc::new(Mutex::new(Vec::new()));
    let mut console = Script {
        started: std::time::Instant::now(),
        events: VecDeque::from(vec![ConsoleEvent::Key(b"typed here\r".to_vec()), ConsoleEvent::Key(vec![DETACH_KEY, b'd'])]),
        out: out.clone(),
    };
    let end = attach::run(&conn, &id, &mut console);
    conn.close();
    assert_eq!(end, AttachEnd::Detached);
    wait_for("the typed line at the PTY", || pty.written().ends_with(b"typed here\r").then_some(()));
    assert!(!pty.has_exited(), "detaching leaves the session running");
    assert!(String::from_utf8_lossy(&out.lock().unwrap()).contains("from the office"), "the grid was drawn");

    // The console changed nothing in the folder: the same one office, still paired.
    assert_eq!(paired_offices(&client_dir).unwrap().len(), 1);
    let unknown = attach::connect(&client_dir, "ZZZZZZ").expect_err("an unpaired id is refused");
    assert!(unknown.contains("not paired"), "{unknown}");
}
