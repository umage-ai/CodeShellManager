//! The Docs tab on the real engine: a session in a temporary folder; the tab lists the docs
//! (`project_docs_list`), reads the shown one (`project_doc_get`), follows an edit on disk within
//! the poll period, and follows a link to a sibling doc, without toasting.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, State, StateOptions};
use csm_pty::{Backend as PtyBackend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_ui::model::InspectorTab;
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::{Action, CsmApp, EngineBackend, EngineOptions};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

struct FakePty {
    exited: AtomicBool,
    callbacks: Mutex<Vec<ExitCallback>>,
}

impl Pty for FakePty {
    fn subscribe(&self, _: OutputSink) {}
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

fn until(h: &mut Harness<'static, CsmApp>, what: &str, f: impl Fn(&Harness<'static, CsmApp>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f(h) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..3 {
        h.step();
    }
}

#[test]
fn the_docs_tab_reads_the_folder_and_follows_edits() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("README.md"), "# Readme\n\nSee [the rules](CLAUDE.md).\n").unwrap();
    std::fs::write(folder.path().join("CLAUDE.md"), "# First rules\n").unwrap();
    let options = StateOptions {
        pty: Arc::new(|_: SpawnOptions| Ok(Arc::new(FakePty { exited: AtomicBool::new(false), callbacks: Mutex::new(Vec::new()) }) as Arc<dyn Pty>)),
        first_size_timeout: Duration::from_millis(30),
        restore_on_start: false,
        ..StateOptions::default()
    };
    let engine = Engine::start(State::new(csm_core::models::AppState::default(), options), csm_ui::app_tools()).unwrap();
    let out = engine.call(Caller::Ui, "session_new", json!({"folder": folder.path().to_string_lossy(), "command": "pwsh"})).unwrap();
    let id = out.data["id"].as_str().unwrap().to_owned();

    let backend = EngineBackend::new(engine.clone(), EngineOptions { folders: vec![folder.path().to_string_lossy().into_owned()], ..EngineOptions::default() });
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let app = CsmApp::new(Box::new(csm_ui::EnginePort::new(engine.clone())), Box::new(backend), TerminalRenderer::new(fonts));
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("session_focus", json!({"session_id": id})));
    h.state_mut().run(&ctx, Action::Tool("inspector_show", json!({"tab": "docs"})));
    until(&mut h, "the Docs tab", |h| h.state().model.inspector == Some(InspectorTab::Docs));
    until(&mut h, "CLAUDE.md's text", |h| h.query_by_label("First rules").is_some());
    assert!(h.state().submitted().iter().any(|c| c.id == "project_docs_list" && c.params == json!({"session_id": id})));
    assert!(h.state().submitted().iter().any(|c| c.id == "project_doc_get" && c.params == json!({"session_id": id, "file": "CLAUDE.md"})));

    // An edit on disk shows up at the next poll (every 2 s of UI time).
    std::fs::write(folder.path().join("CLAUDE.md"), "# Second, longer rules\n").unwrap();
    until(&mut h, "the edited text", |h| h.query_by_label("Second, longer rules").is_some());

    // A link to a sibling doc switches the picker.
    h.get_by_label("Doc: README.md").click();
    until(&mut h, "README.md", |h| h.query_by_label("Readme").is_some());
    h.get_by_label("Link: the rules").click();
    until(&mut h, "back on CLAUDE.md", |h| h.query_by_label("Second, longer rules").is_some());
    let toasts: Vec<String> = h.state().ui.toasts.iter().map(|t| t.text.clone()).filter(|t| !t.starts_with("Focused")).collect();
    assert!(toasts.is_empty(), "polling never toasts: {toasts:?}");
    engine.shutdown();
}
