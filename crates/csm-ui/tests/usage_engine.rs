//! Claude usage on the real engine, through the UI: a Claude session (fake PTY) whose transcript
//! is written to a temporary Claude home; the Usage tab asks `usage_session`, the sidebar's chips
//! come from the periodic `usage_context`, and nothing toasts while it polls.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{Caller, ClaudeRoots, Engine, IndexSetting, State, StateOptions};
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

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64
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
fn the_usage_tab_and_the_sidebar_chip_follow_a_real_transcript() {
    let root = tempfile::tempdir().unwrap();
    let folder = tempfile::tempdir().unwrap();
    let options = StateOptions {
        pty: Arc::new(|_: SpawnOptions| Ok(Arc::new(FakePty { exited: AtomicBool::new(false), callbacks: Mutex::new(Vec::new()) }) as Arc<dyn Pty>)),
        first_size_timeout: Duration::from_millis(30),
        restore_on_start: false,
        index: IndexSetting::Handle(Index::start(IndexOptions::memory()).unwrap()),
        claude_roots: ClaudeRoots::Dirs(vec![root.path().to_path_buf()]),
        usage_poll: Duration::from_millis(30),
        ..StateOptions::default()
    };
    let mut app = csm_core::models::AppState::default();
    app.settings.claude_launch_stagger_ms = 0;
    app.settings.auto_resume_claude = false;
    // The built-in Claude usage add-on is off by default.
    app.settings.addon_approvals.insert(csm_engine::addons::CLAUDE_USAGE_ID.into(), csm_engine::addons::BUILTIN_DIGEST.into());
    let engine = Engine::start(State::new(app, options), csm_ui::app_tools()).unwrap();
    let out = engine.call(Caller::Ui, "session_new", json!({"folder": folder.path().to_string_lossy(), "command": "claude"})).unwrap();
    let id = out.data["id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(10);
    let cid = loop {
        if let Some(c) = engine.view().session(&id).and_then(|s| s.claude_session_id.clone()) {
            break c;
        }
        assert!(Instant::now() < deadline, "no conversation id");
        std::thread::sleep(Duration::from_millis(5));
    };
    let dir = root.path().join("projects").join("some-folder");
    std::fs::create_dir_all(&dir).unwrap();
    let line = json!({"type": "assistant", "timestamp": csm_core::time::Timestamp::from_epoch_ms(now_ms() - 1000).as_str(), "cwd": folder.path().to_string_lossy(),
        "message": {"id": "m1", "model": "claude-opus-5-5", "usage": {"input_tokens": 1000, "output_tokens": 100, "cache_read_input_tokens": 140_000, "cache_creation_input_tokens": 9_000}}});
    std::fs::write(dir.join(format!("{cid}.jsonl")), line.to_string() + "\n").unwrap();

    let backend = EngineBackend::new(engine.clone(), EngineOptions { folders: vec![folder.path().to_string_lossy().into_owned()], ..EngineOptions::default() });
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let mut app = CsmApp::new(Box::new(csm_ui::EnginePort::new(engine.clone())), Box::new(backend), TerminalRenderer::new(fonts));
    app.ui.no_dialog_delay = true;
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("session_focus", json!({"session_id": id})));
    // The focus itself answers with a "Focused ..." toast. Wait for that reply and drop it, so the
    // "polling never toasts" check below sees only what polling made (no timing involved).
    until(&mut h, "the focus reply", |h| h.state().ui.toasts.iter().any(|t| t.text.starts_with("Focused ")));
    h.state_mut().ui.toasts.clear();
    h.state_mut().run(&ctx, Action::Tool("inspector_show", json!({"tab": "usage"})));
    until(&mut h, "the Usage tab", |h| h.state().model.inspector == Some(InspectorTab::Usage));
    // The tab asks again every few seconds until the reader has stored the request.
    until(&mut h, "the session's numbers", |h| h.state().model.usage.sessions.get(&id).is_some_and(|u| u.usage.total.requests == 1));
    assert!(h.state().submitted().iter().any(|c| c.id == "usage_session" && c.params == json!({"session_id": id})));
    // 150k of 200k: past the warning ratio.
    assert!(h.query_by_label("Context fill 75%").is_some(), "the gauge");
    assert!(h.query_by_label("150k of 200k · 75%").is_some());
    assert!(h.query_by_label("Refresh usage").is_some());
    // The sidebar's chip comes from the periodic `usage_context`.
    until(&mut h, "the sidebar chip", |h| h.state().model.usage.chips.get(&id).is_some_and(|c| c.context.is_some()));
    assert!(h.state().submitted().iter().any(|c| c.id == "usage_context"));
    assert!(h.state().ui.toasts.is_empty(), "polling never toasts: {:?}", h.state().ui.toasts.iter().map(|t| t.text.clone()).collect::<Vec<_>>());

    // Settings → Usage asks for today's and this week's cost per project.
    h.state_mut().run(&ctx, Action::OpenSettings);
    until(&mut h, "the settings", |h| h.query_by_label("Settings: Usage").is_some());
    h.get_by_label("Settings: Usage").click();
    until(&mut h, "the week's summary", |h| h.state().model.usage.summary.contains_key("week"));
    let asked: Vec<_> = h.state().submitted().iter().filter(|c| c.id == "usage_summary").map(|c| c.params["from"].clone()).collect();
    assert!(asked.contains(&json!("today")) && asked.contains(&json!("week")), "{asked:?}");
    assert_eq!(h.state().model.usage.summary["week"].rows.len(), 1);

    // The dashboard: the real `usage_report`, a range switch, and a drill into the session.
    h.state_mut().run(&ctx, Action::OpenUsage(None));
    until(&mut h, "the report", |h| h.state().model.usage.report.as_ref().is_some_and(|r| r.report.total.requests == 1));
    assert!(h.query_by_label("Claude usage").is_some());
    h.get_by_label("Range: 30 days").click();
    until(&mut h, "the 30-day report", |h| h.state().model.usage.report.as_ref().is_some_and(|r| r.params["from"] == "30d"));
    let report = h.state().model.usage.report.clone().unwrap().report;
    assert_eq!(report.days.len(), 30);
    assert_eq!(report.rows[0].key, id, "the conversation is keyed by its CSM session");
    let label = report.rows[0].label.clone();
    h.get_by_label(&format!("Usage row: {label}")).click();
    until(&mut h, "the session's models", |h| h.state().model.usage.report.as_ref().is_some_and(|r| r.params["session"] == json!(id)));
    assert!(h.query_by_label("Usage row: opus-5-5").is_some());
    let hit = h.state().model.usage.report.as_ref().unwrap().report.total.cache_hit_rate.unwrap();
    assert!((hit - 140_000.0 / 150_000.0).abs() < 1e-9, "{hit}");
    assert!(h.state().ui.toasts.is_empty());
    engine.shutdown();
}
