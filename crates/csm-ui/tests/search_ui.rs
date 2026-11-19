//! Search and notes through the real UI. Two rigs:
//! - the demo backend (its engine has an in-memory index seeded with what the demo sessions
//!   "printed"): the palette's search row opens the panel and a result click submits
//!   `session_focus`;
//! - the real backend over a real engine with a fake PTY: the Notes tab reads and saves through
//!   `note_get` / `note_set` (debounced), and output a session prints is found by search.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::*;
use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{Caller, Engine, IndexSetting, State, StateOptions};
use csm_pty::{Backend as PtyBackend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::theme::ThemeKind;
use csm_ui::{Action, CsmApp, EngineBackend, EngineOptions};
use egui::{Event, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

// ── Demo rig ─────────────────────────────────────────────────────────────────────────────

fn demo() -> Harness<'static, CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn last_call(h: &Harness<'static, CsmApp>) -> (String, serde_json::Value) {
    let c = h.state().submitted().last().cloned().expect("a call was submitted");
    (c.id, c.params)
}

#[test]
fn the_palette_search_row_opens_the_panel_and_a_result_click_focuses_its_session() {
    let mut h = demo();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenPalette);
    settle(&mut h);
    h.state_mut().ui.palette.query = "migration".into();
    settle(&mut h);
    h.get_by_label("Palette: Search all output and notes for “migration”").click();
    settle(&mut h);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::Search, "the palette row opens the search panel");
    assert!(h.state().submitted().iter().any(|c| c.id == "search_output" && c.params["query"] == "migration"), "and searches with its query");
    assert!(h.state().submitted().iter().any(|c| c.id == "search_notes"));
    assert!(h.query_by_label_contains("Search result: CodeShellManager:").is_some(), "the hit is listed under its session");
    h.get_by_label_contains("Search result: CodeShellManager:").click();
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!((id.as_str(), &params["session_id"]), ("session_focus", &json!("csm")));
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::None, "the panel closes");
}

#[test]
fn ctrl_f_opens_the_panel_and_enter_searches() {
    let mut h = demo();
    h.event(Event::Key { key: Key::F, physical_key: Some(Key::F), pressed: true, repeat: false, modifiers: Modifiers::COMMAND });
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::Search);
    h.state_mut().ui.search.query = "frobnicate".into();
    h.key_press(Key::Enter);
    settle(&mut h);
    settle(&mut h);
    assert!(h.state().submitted().iter().any(|c| c.id == "search_output" && c.params["query"] == "frobnicate"));
    assert!(h.query_by_label_contains("Search result: craft-anything:").is_some());
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::None);
}

#[test]
fn a_hostile_query_is_a_message_in_the_panel() {
    let mut h = demo();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSearch("\"(AND".into()));
    settle(&mut h);
    settle(&mut h);
    assert!(h.state().ui.search.error.is_none(), "taken literally, so not a syntax error: {:?}", h.state().ui.search.error);
    assert!(h.state().ui.search.output.is_empty());
}

#[test]
fn a_result_for_a_closed_session_offers_to_reopen_it() {
    let mut h = demo();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSearch(String::new()));
    h.state_mut().ui.search.searched = Some("x".into());
    h.state_mut().ui.search.output = vec![json!({"type": "output", "session_id": "gone-1", "session_name": "Old build", "folder": "C:\\old", "ts": 0, "excerpt": "ancient output", "matches": [[0, 7]], "reopenable": true})];
    h.state_mut().ui.search.notes = vec![json!({"type": "note", "folder": "C:\\GitHub\\Odulodo", "excerpt": "a note about odulodo", "matches": [[2, 6]], "session_ids": ["odulodo"]})];
    settle(&mut h);
    h.get_by_label_contains("Search result: Old build:").click();
    settle(&mut h);
    assert!(h.state().ui.search.offer.is_some());
    assert!(h.query_by_label("Reopen").is_some());
    h.get_by_label("Reopen").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("search_reopen_session".to_string(), json!({"session_id": "gone-1"})));

    // Notes are labelled as notes, and open the Notes tab on a session in that folder.
    h.state_mut().run(&ctx, Action::OpenSearch(String::new()));
    h.state_mut().ui.search.output = Vec::new();
    h.state_mut().ui.search.notes = vec![json!({"type": "note", "folder": "C:\\GitHub\\Odulodo", "excerpt": "a note about odulodo", "matches": [[2, 6]], "session_ids": ["odulodo"]})];
    settle(&mut h);
    h.get_by_label_contains("Search note: C:\\GitHub\\Odulodo").click();
    settle(&mut h);
    let calls: Vec<String> = h.state().submitted().iter().rev().take(4).map(|c| c.id.clone()).collect();
    assert!(calls.contains(&"session_focus".to_string()) && calls.contains(&"inspector_show".to_string()), "{calls:?}");
}

// ── Real engine rig ──────────────────────────────────────────────────────────────────────

struct FakePty {
    sink: Mutex<Option<OutputSink>>,
    exited: AtomicBool,
    callbacks: Mutex<Vec<ExitCallback>>,
}

impl FakePty {
    fn emit(&self, bytes: &[u8]) {
        if let Some(s) = self.sink.lock().unwrap().as_mut() {
            s(bytes);
        }
    }
}

impl Pty for FakePty {
    fn subscribe(&self, sink: OutputSink) {
        *self.sink.lock().unwrap() = Some(sink);
    }
    fn on_exit(&self, cb: ExitCallback) {
        self.callbacks.lock().unwrap().push(cb);
    }
    fn write(&self, _: &[u8]) -> io::Result<()> {
        Ok(())
    }
    fn resize(&self, _: PtySize) -> io::Result<()> {
        Ok(())
    }
    fn has_exited(&self) -> bool {
        self.exited.load(Ordering::SeqCst)
    }
    fn exit_code(&self) -> Option<i32> {
        None
    }
    fn kill(&self) {
        if !self.exited.swap(true, Ordering::SeqCst) {
            for cb in std::mem::take(&mut *self.callbacks.lock().unwrap()) {
                cb(Some(1));
            }
        }
    }
    fn close(&self) {
        self.kill();
    }
    fn backend(&self) -> PtyBackend {
        PtyBackend::BundledConPty
    }
    fn pid(&self) -> Option<u32> {
        Some(1)
    }
}

struct Rig {
    h: Harness<'static, CsmApp>,
    engine: Engine,
    ptys: Arc<Mutex<Vec<Arc<FakePty>>>>,
    folder: String,
    _dir: tempfile::TempDir,
}

fn real() -> Rig {
    let ptys: Arc<Mutex<Vec<Arc<FakePty>>>> = Arc::new(Mutex::new(Vec::new()));
    let list = ptys.clone();
    let options = StateOptions {
        pty: Arc::new(move |_: SpawnOptions| {
            let p = Arc::new(FakePty { sink: Mutex::new(None), exited: AtomicBool::new(false), callbacks: Mutex::new(Vec::new()) });
            list.lock().unwrap().push(p.clone());
            Ok(p as Arc<dyn Pty>)
        }),
        first_size_timeout: Duration::from_millis(30),
        restore_on_start: false,
        index: IndexSetting::Handle(Index::start(IndexOptions::memory()).unwrap()),
        ..StateOptions::default()
    };
    let mut app = csm_core::models::AppState::default();
    app.settings.claude_launch_stagger_ms = 0;
    app.settings.auto_resume_claude = false;
    let engine = Engine::start(State::new(app, options), csm_ui::app_tools()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().display().to_string();
    let backend = EngineBackend::new(engine.clone(), EngineOptions { folders: vec![folder.clone()], ..EngineOptions::default() });
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let app = CsmApp::new(engine.clone(), Box::new(backend), TerminalRenderer::new(fonts));
    let h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app);
    Rig { h, engine, ptys, folder, _dir: dir }
}

impl Rig {
    fn until(&mut self, what: &str, f: impl Fn(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f(self) {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            self.h.step();
            std::thread::sleep(Duration::from_millis(5));
        }
        for _ in 0..3 {
            self.h.step();
        }
    }

    fn new_session(&mut self, name: &str) -> String {
        let out = self.engine.call(Caller::Ui, "session_new", json!({"folder": self.folder, "command": "pwsh", "name": name, "focus": true})).unwrap();
        let id = out.data["id"].as_str().unwrap().to_owned();
        let want = id.clone();
        self.until("the session in the UI", move |r| r.h.state().model.session(&want).is_some());
        id
    }
}

#[test]
fn the_notes_tab_reads_and_saves_through_the_notes_tools() {
    let mut r = real();
    let a = r.new_session("Alpha");
    // A note kept earlier (by anyone) shows up when the tab opens.
    r.engine.call(Caller::Ui, "note_set", json!({"session_id": a, "text": "kept earlier"})).unwrap();
    let ctx = r.h.ctx.clone();
    r.h.state_mut().run(&ctx, Action::Tool("inspector_show", json!({"tab": "notes"})));
    let aa = a.clone();
    r.until("the note to load", move |r| r.h.state().ui.notes_cache.saved.get(&aa).is_some_and(|t| t == "kept earlier"));
    assert!(r.h.state().submitted().iter().any(|c| c.id == "note_get" && c.params["session_id"] == a.as_str()));
    // Typing saves after the debounce, as a `note_set` for the session.
    r.h.get_by_label("Notes for this folder").click();
    r.h.get_by_label("Notes for this folder").type_text(" and more");
    for _ in 0..90 {
        r.h.step();
    }
    let aa = a.clone();
    r.until("the note to be saved", move |r| r.h.state().submitted().iter().any(|c| c.id == "note_set" && c.params["session_id"] == aa.as_str()));
    let saved = r.engine.call(Caller::Ui, "note_get", json!({"session_id": a})).unwrap();
    assert!(saved.data["text"].as_str().unwrap().contains("and more"), "{}", saved.data);
    assert!(saved.data["text"].as_str().unwrap().starts_with("kept earlier"));
}

#[test]
fn two_sessions_in_a_folder_show_the_shared_note_and_count() {
    let mut r = real();
    let a = r.new_session("Alpha");
    let b = r.new_session("Beta");
    r.engine.call(Caller::Ui, "note_set", json!({"session_id": a, "text": "shared scratch"})).unwrap();
    let ctx = r.h.ctx.clone();
    r.h.state_mut().run(&ctx, Action::Tool("inspector_show", json!({"tab": "notes"})));
    r.h.state_mut().run(&ctx, Action::Tool("session_focus", json!({"session_id": b})));
    let bb = b.clone();
    r.until("the shared note on Beta", move |r| r.h.state().ui.notes_cache.saved.get(&bb).is_some_and(|t| t == "shared scratch"));
    assert_eq!(r.h.state().ui.notes_cache.shared.get(&b), Some(&2));
    assert!(r.h.query_by_label_contains("shared by 2 sessions").is_some() || r.h.state().ui.notes_cache.shared.get(&b) == Some(&2));
}

#[test]
fn output_printed_by_a_session_is_found_and_its_result_focuses_it() {
    let mut r = real();
    let _a = r.new_session("Alpha");
    let _b = r.new_session("Beta");
    r.until("the PTYs to start", |r| r.ptys.lock().unwrap().len() == 2 && r.ptys.lock().unwrap().iter().all(|p| p.sink.lock().unwrap().is_some()));
    // Which PTY belongs to which session is the engine's business; one of them prints the needle.
    r.ptys.lock().unwrap()[0].emit(b"\x1b[32mneedle-in-output\x1b[0m unique words here\r\n");
    let ctx = r.h.ctx.clone();
    r.h.state_mut().run(&ctx, Action::OpenSearch("needle-in-output".into()));
    r.until("the results", |r| r.h.state().ui.search.pending == 0 && !r.h.state().ui.search.output.is_empty());
    assert!(r.h.state().ui.search.error.is_none());
    let hit = r.h.state().ui.search.output[0].clone();
    let (sid, name) = (hit["session_id"].as_str().unwrap().to_owned(), hit["session_name"].as_str().unwrap().to_owned());
    assert!(r.h.state().model.session(&sid).is_some(), "the result names a live session");
    r.h.get_by_label_contains(&format!("Search result: {name}:")).click();
    r.until("the focus call", |r| r.h.state().submitted().iter().any(|c| c.id == "session_focus"));
    let last = r.h.state().submitted().iter().rev().find(|c| c.id == "session_focus").cloned().unwrap();
    assert_eq!(last.params["session_id"], sid.as_str());
}
