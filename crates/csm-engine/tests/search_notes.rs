//! Notes and search through `Engine::call`: output typed into a fake PTY is searchable, notes are
//! shared per folder, hostile queries come back as plain messages, `--clean` never writes.

mod common;

use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{Caller, IndexSetting, Status, ToolError};
use serde_json::{Value, json};

fn with_index() -> H {
    H::start(test_app(), |o| o.index = IndexSetting::Handle(Index::start(IndexOptions::memory()).unwrap()))
}

fn named(h: &H, name: &str) -> String {
    let folder = h.dir.path().to_string_lossy().into_owned();
    let out = h.ok("session_new", json!({"folder": folder, "command": "pwsh", "name": name}));
    let id = out.data["id"].as_str().unwrap().to_owned();
    h.wait_status(&id, Status::Idle);
    id
}

fn results(h: &H, tool: &str, params: Value) -> Vec<Value> {
    h.ok(tool, params).data["results"].as_array().unwrap().clone()
}

#[test]
fn output_is_indexed_and_found_with_the_live_session_name() {
    let h = with_index();
    let id = named(&h, "Odulodo");
    h.pty(0).emit(b"\x1b[1t\x1b[c\x1b[?1004h\x1b[32mcompiling\x1b[0m zebra-crossing-widget\r\nerror: linker failed\r\n");
    let r = results(&h, "search_output", json!({"query": "zebra-crossing-widget"}));
    assert_eq!(r.len(), 1, "{r:?}");
    assert_eq!(r[0]["session_id"], id.as_str());
    assert_eq!(r[0]["session_name"], "Odulodo");
    assert_eq!(r[0]["exists"], true);
    assert!(r[0]["excerpt"].as_str().unwrap().contains("zebra-crossing-widget"));
    assert!(!r[0]["excerpt"].as_str().unwrap().contains('\u{1b}'));
    assert_eq!(r[0]["matches"].as_array().unwrap().len(), 1);
    // The bundled ConPTY's opening bytes are not text.
    assert!(results(&h, "search_output", json!({"query": "1004h"})).is_empty());
    // A rename shows up in later results.
    h.ok("session_rename", json!({"session_id": id, "name": "Renamed"}));
    assert_eq!(results(&h, "search_output", json!({"query": "linker failed"}))[0]["session_name"], "Renamed");
}

#[test]
fn filters_by_session_and_group_and_limit() {
    let h = with_index();
    let a = named(&h, "A");
    let b = named(&h, "B");
    let g = h.ok("group_new", json!({"name": "Work"})).data["group_id"].as_str().unwrap().to_owned();
    h.ok("session_move_to_group", json!({"session_id": a, "group_id": g}));
    h.pty(0).emit(b"shared-token one\r\n");
    h.pty(1).emit(b"shared-token two\r\n");
    assert_eq!(results(&h, "search_output", json!({"query": "shared-token"})).len(), 2);
    let only_a = results(&h, "search_output", json!({"query": "shared-token", "session_id": a}));
    assert!(only_a.len() == 1 && only_a[0]["session_id"] == a.as_str());
    let grp = results(&h, "search_output", json!({"query": "shared-token", "group_id": g}));
    assert!(grp.len() == 1 && grp[0]["session_id"] == a.as_str());
    assert_eq!(results(&h, "search_output", json!({"query": "shared-token", "limit": 1})).len(), 1);
    assert!(matches!(h.ui("search_output", json!({"query": "x", "group_id": "nope"})), Err(ToolError::NotFound(_))));
    let _ = b;
}

#[test]
fn hostile_queries_are_plain_messages_never_a_crash() {
    let h = with_index();
    named(&h, "A");
    h.pty(0).emit(b"hello world AND more\r\n");
    for q in ["\"", "AND", "(", "NEAR(", "col:x", "*", "a\u{0}b", "'; DROP TABLE output; --"] {
        let r = h.ui("search_output", json!({"query": q}));
        assert!(r.is_ok(), "literal {q:?}: {r:?}");
    }
    assert_eq!(results(&h, "search_output", json!({"query": "world AND more"})).len(), 1, "literal phrase containing AND");
    // Advanced mode passes FTS5 syntax through; a mistake is a plain message, not an internal error.
    assert_eq!(results(&h, "search_output", json!({"query": "hello OR nothing", "advanced": true})).len(), 1);
    match h.ui("search_output", json!({"query": "hello AND", "advanced": true})) {
        Err(ToolError::InvalidParams(m)) => assert!(m.contains("isn't valid search syntax"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(h.ui("search_output", json!({"query": "   "})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("search_output", json!({"query": "x".repeat(600)})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("search_output", json!({"query": "x", "bogus": 1})), Err(ToolError::InvalidParams(_))));
}

#[test]
fn two_sessions_in_one_folder_share_a_note() {
    let h = with_index();
    let a = named(&h, "A");
    let b = named(&h, "B");
    let set = h.ok("note_set", json!({"session_id": a, "text": "remember the milk\nand the eggs"}));
    assert_eq!(set.data["shared_by"], 2);
    let got = h.ok("note_get", json!({"session_id": b}));
    assert_eq!(got.data["text"], "remember the milk\nand the eggs");
    assert_eq!(got.data["shared_by"], 2);
    // The same folder spelled another way is the same note.
    let spelled = h.dir.path().to_string_lossy().replace('\\', "/").to_uppercase();
    if cfg!(windows) {
        assert_eq!(h.ok("note_get", json!({"folder": format!("{spelled}/")})).data["text"], "remember the milk\nand the eggs");
    }
    // Search finds it and says which sessions live there.
    let r = results(&h, "search_notes", json!({"query": "MILK"}));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["type"], "note");
    assert_eq!(r[0]["session_ids"].as_array().unwrap().len(), 2);
    // Blank deletes; control characters are dropped.
    h.ok("note_set", json!({"folder": h.dir.path().to_string_lossy(), "text": "a\u{1b}[31mb\tc"}));
    assert_eq!(h.ok("note_get", json!({"session_id": a})).data["text"], "a[31mb\tc");
    h.ok("note_set", json!({"session_id": a, "text": "  "}));
    assert_eq!(h.ok("note_get", json!({"session_id": a})).data["text"], "");
    assert!(results(&h, "search_notes", json!({"query": "milk"})).is_empty());
}

#[test]
fn notes_refuse_ssh_and_bad_params() {
    let h = with_index();
    let out = h.ok("session_new", json!({"kind": "ssh", "ssh": {"host": "dev.example", "remote_folder": "/srv/app"}, "command": "bash"}));
    let ssh = out.data["id"].as_str().unwrap().to_owned();
    for r in [h.ui("note_get", json!({"session_id": ssh})), h.ui("note_set", json!({"session_id": ssh, "text": "x"}))] {
        assert!(matches!(&r, Err(ToolError::Refused(m)) if m.contains("SSH")), "{r:?}");
    }
    assert!(matches!(h.ui("note_get", json!({})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("note_get", json!({"folder": "a", "session_id": ssh})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("note_get", json!({"folder": "  "})), Err(ToolError::InvalidParams(_))));
    assert!(matches!(h.ui("note_get", json!({"session_id": "nope"})), Err(ToolError::NotFound(_))));
    assert!(matches!(h.ui("note_set", json!({"folder": "/x", "text": "x".repeat(300 * 1024)})), Err(ToolError::InvalidParams(_))));
}

#[test]
fn without_an_index_the_tools_say_so() {
    let h = H::new();
    for (tool, p) in [("note_get", json!({"folder": "/x"})), ("search_output", json!({"query": "x"})), ("search_notes", json!({"query": "x"}))] {
        assert!(matches!(h.ui(tool, p), Err(ToolError::Refused(m)) if m.contains("no search index")), "{tool}");
    }
}

#[test]
fn reading_output_and_notes_needs_consent_for_mcp_clients() {
    let h = with_index();
    named(&h, "A");
    let events = h.engine.subscribe();
    let ext = Caller::External { client: "agent".into(), session_id: None };
    let rx = h.engine.submit(ext, "search_output", json!({"query": "x"}));
    assert!(matches!(events.recv_timeout(Duration::from_secs(5)), Ok(csm_engine::Event::ConsentRequested { .. })));
    assert!(rx.try_recv().is_err());
}

#[test]
fn a_closed_session_found_by_search_can_be_reopened() {
    let h = with_index();
    let id = named(&h, "Ghost");
    h.pty(0).emit(b"ghostly-marker output\r\n");
    h.ok("session_close", json!({"session_id": id, "force": true}));
    let r = results(&h, "search_output", json!({"query": "ghostly-marker"}));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0]["exists"], false);
    assert_eq!(r[0]["reopenable"], true);
    assert_eq!(r[0]["session_name"], "Ghost");
    let out = h.ok("search_reopen_session", json!({"session_id": id}));
    let new_id = out.data["session_id"].as_str().unwrap().to_owned();
    assert_ne!(new_id, id);
    let v = h.wait_until("the reopened session", |v| v.session(&new_id).is_some());
    assert_eq!(v.session(&new_id).unwrap().name, "Ghost");
    // The ring was left as it was, and an unknown or still-open session is refused.
    assert!(matches!(h.ui("search_reopen_session", json!({"session_id": new_id})), Err(ToolError::Refused(_))));
    assert!(matches!(h.ui("search_reopen_session", json!({"session_id": "never-existed"})), Err(ToolError::NotFound(_))));
}

#[test]
fn clean_runs_index_in_memory_and_write_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let loc = csm_engine::index_location(Some(&state_path), true);
    assert_eq!(loc, csm_engine::csm_index::IndexLocation::Memory);
    let h = H::start(test_app(), |o| {
        o.clean = true;
        o.save_path = Some(state_path.clone());
        o.index = IndexSetting::Auto;
    });
    named(&h, "A");
    h.pty(0).emit(b"clean-run-marker\r\n");
    h.ok("note_set", json!({"folder": "/some/where", "text": "kept in memory"}));
    assert_eq!(results(&h, "search_output", json!({"query": "clean-run-marker"})).len(), 1);
    h.engine.shutdown();
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0, "--clean must leave the state dir empty, index included");
}

#[test]
fn an_auto_index_beside_a_custom_state_file_persists_across_runs() {
    let dir = tempfile::tempdir().unwrap();
    let state_path = dir.path().join("state.json");
    let start = || {
        H::start(test_app(), |o| {
            o.save_path = Some(state_path.clone());
            o.index = IndexSetting::Auto;
        })
    };
    let h = start();
    h.ok("note_set", json!({"folder": "/some/where", "text": "persisted note"}));
    h.engine.shutdown();
    assert!(dir.path().join(csm_engine::INDEX_FILE).is_file());
    let h2 = start();
    assert_eq!(h2.ok("note_get", json!({"folder": "/some/where"})).data["text"], "persisted note");
    h2.engine.shutdown();
}

#[test]
fn notes_import_from_a_wpf_database() {
    let h = with_index();
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("output.db");
    {
        let c = rusqlite::Connection::open(&p).unwrap();
        c.execute_batch("CREATE TABLE project_notes (folder_path TEXT PRIMARY KEY, content TEXT, updated_at INTEGER); INSERT INTO project_notes VALUES ('/home/u/proj', 'imported text', 10);").unwrap();
    }
    let out = h.ok("notes_import_wpf", json!({"path": p.to_string_lossy()}));
    assert_eq!(out.data["imported"], 1);
    assert_eq!(h.ok("note_get", json!({"folder": "/home/u/proj"})).data["text"], "imported text");
    assert!(matches!(h.ui("notes_import_wpf", json!({"path": dir.path().join("none.db").to_string_lossy()})), Err(ToolError::NotFound(_))));
}

#[test]
fn the_tools_are_registered_in_their_toolsets() {
    let h = H::new();
    let list = h.ok("tool_list", json!({}));
    let tools = list.data.as_array().unwrap();
    for (id, set) in [
        ("note_get", "notes"),
        ("note_set", "notes"),
        ("notes_import_wpf", "notes"),
        ("search_output", "search"),
        ("search_notes", "search"),
        ("search_reopen_session", "search"),
    ] {
        let t = tools.iter().find(|t| t["id"] == id).unwrap_or_else(|| panic!("{id} missing"));
        assert_eq!(t["toolset"], set, "{id}");
    }
}

#[test]
fn queued_output_never_blocks_the_reader() {
    // The PTY reader thread feeds the index; with a tiny queue it drops and counts, and keeps going.
    let mut o = IndexOptions::memory();
    o.queue_bytes = 64;
    let ix = Index::start(o).unwrap();
    let h = H::start(test_app(), |opts| opts.index = IndexSetting::Handle(ix.clone()));
    named(&h, "A");
    let start = Instant::now();
    let chunk = vec![b'x'; 4096];
    for _ in 0..200 {
        h.pty(0).emit(&chunk);
    }
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(ix.dropped() > 0, "a 64-byte queue must have overflowed");
}
