//! The real app: `CsmApp` on [`EngineBackend`] over a real engine with the app's tools, driven
//! through the UI. Only the PTY is fake (the engine's `PtyFactory` seam), so nothing is spawned
//! and the tests are deterministic; everything else (tool registry, consent, quit) is the real
//! code path.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, State, StateOptions};
use csm_pty::{Backend as PtyBackend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_ui::model::{LayoutMode, ShutdownRowState};
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::{CsmApp, EngineBackend, EngineOptions};
use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

/// A PTY that records what it was given and exits when told to.
struct FakePty {
    argv: Vec<String>,
    first: PtySize,
    sink: Mutex<Option<OutputSink>>,
    written: Mutex<Vec<u8>>,
    exited: AtomicBool,
    callbacks: Mutex<Vec<ExitCallback>>,
    /// Exits when the console is closed (a well-behaved program); off = a wedged Claude.
    exit_on_close: bool,
}

impl FakePty {
    fn exit(&self, code: Option<i32>) {
        if self.exited.swap(true, Ordering::SeqCst) {
            return;
        }
        for cb in std::mem::take(&mut *self.callbacks.lock().unwrap()) {
            cb(code);
        }
    }

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
        if self.exited.load(Ordering::SeqCst) {
            cb(Some(0));
        } else {
            self.callbacks.lock().unwrap().push(cb);
        }
    }
    fn write(&self, bytes: &[u8]) -> io::Result<()> {
        self.written.lock().unwrap().extend_from_slice(bytes);
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
        self.exit(Some(1));
    }
    fn close(&self) {
        if self.exit_on_close {
            self.exit(Some(0));
        }
    }
    fn backend(&self) -> PtyBackend {
        PtyBackend::BundledConPty
    }
    fn pid(&self) -> Option<u32> {
        Some(1)
    }
}

type Ptys = Arc<Mutex<Vec<Arc<FakePty>>>>;

struct Rig {
    h: Harness<'static, CsmApp>,
    engine: Engine,
    ptys: Ptys,
    folder: String,
}

/// The app as `codeshell` builds it, minus the window and the IPC bridge.
fn rig(exit_on_close: bool) -> Rig {
    rig_with(exit_on_close, Duration::from_millis(1200))
}

fn rig_with(exit_on_close: bool, shutdown_budget: Duration) -> Rig {
    let ptys: Ptys = Arc::new(Mutex::new(Vec::new()));
    let list = ptys.clone();
    let options = StateOptions {
        pty: Arc::new(move |o: SpawnOptions| {
            let p = Arc::new(FakePty {
                argv: o.argv.iter().map(|a| a.to_string_lossy().into_owned()).collect(),
                first: o.size,
                sink: Mutex::new(None),
                written: Mutex::new(Vec::new()),
                exited: AtomicBool::new(false),
                callbacks: Mutex::new(Vec::new()),
                exit_on_close,
            });
            list.lock().unwrap().push(p.clone());
            Ok(p as Arc<dyn Pty>)
        }),
        // Long, so only a measured pane can be what starts the PTY.
        first_size_timeout: Duration::from_secs(30),
        shutdown_budget,
        exit_wait: Duration::from_millis(500),
        restore_on_start: false,
        ..StateOptions::default()
    };
    let mut app = csm_core::models::AppState::default();
    app.settings.claude_launch_stagger_ms = 0;
    app.settings.auto_resume_claude = false;
    let engine = Engine::start(State::new(app, options), csm_ui::app_tools()).unwrap();
    let folder = std::env::temp_dir().display().to_string();
    let opts = EngineOptions {
        launch_commands: vec!["claude".into(), "pwsh -NoLogo".into()],
        folders: vec![folder.clone()],
        wpf_import: Some("C:\\Users\\someone\\AppData\\Roaming\\CodeShellManager\\state.json".into()),
        shutdown_budget_s: 1.2,
        ..EngineOptions::default()
    };
    let backend = EngineBackend::new(engine.clone(), opts);
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let app = CsmApp::new(engine.clone(), Box::new(backend), TerminalRenderer::new(fonts));
    let h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app);
    Rig { h, engine, ptys, folder }
}

impl Rig {
    /// Steps the UI until `f` holds (bounded): the engine answers on its own thread.
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

    fn has(&self, label: &str) -> bool {
        self.h.query_by_label(label).is_some()
    }

    fn has_contains(&self, label: &str) -> bool {
        self.h.query_by_label_contains(label).is_some()
    }

    /// Toasts are painted text: read them from the app.
    fn toast_contains(&self, text: &str) -> bool {
        self.h.state().ui.toasts.iter().any(|t| t.text.contains(text))
    }

    fn board_rows(&self) -> Vec<ShutdownRowState> {
        self.h.state().model.shutdown.as_ref().map(|b| b.rows.iter().map(|r| r.state).collect()).unwrap_or_default()
    }

    /// The empty screen's button (the sidebar has a "New session" of its own).
    fn empty_screen_new_session(&self) -> egui_kittest::Node<'_> {
        self.h.get_all_by_label("New session").max_by(|a, b| a.rect().center().x.total_cmp(&b.rect().center().x)).unwrap()
    }

    fn pty(&self, n: usize) -> Option<Arc<FakePty>> {
        self.ptys.lock().unwrap().get(n).cloned()
    }

    fn only_session(&self) -> String {
        let v = self.engine.view();
        assert_eq!(v.sessions.len(), 1);
        v.sessions[0].id.clone()
    }

    /// Opens the quick-open from the empty screen and starts the folder's session with the
    /// command at `cmd_index` (Tab cycles it).
    fn new_session(&mut self, tabs: usize) -> String {
        self.empty_screen_new_session().click();
        self.until("the quick-open", |r| r.has_contains("Folder: "));
        for _ in 0..tabs {
            self.h.key_press(egui::Key::Tab);
            self.h.step();
        }
        self.h.key_press(egui::Key::Enter);
        self.until("the session in the engine", |r| r.engine.view().sessions.len() == 1);
        self.only_session()
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.shutdown();
    }
}

#[test]
fn first_run_shows_the_empty_screen_with_new_session_and_wpf_import() {
    let mut r = rig(true);
    r.until("the first frames", |r| r.has("No sessions yet"));
    assert!(r.empty_screen_new_session().rect().center().x > 400.0, "in the work area");
    assert!(r.has("Import from CodeShellManager (WPF)"));
    // The import is a real engine tool: a missing file is reported, not faked.
    r.h.get_by_label("Import from CodeShellManager (WPF)").click();
    r.until("the engine's error, shown as a toast", |r| r.toast_contains("could not read"));
    assert_eq!(r.h.state().submitted().last().map(|c| c.id.as_str()), Some("state_import_wpf"));
}

#[test]
fn quick_open_starts_a_session_that_appears_in_the_sidebar_at_the_measured_size() {
    let mut r = rig(true);
    r.until("the empty screen", |r| r.has("No sessions yet"));
    let id = r.new_session(1); // Tab: claude → "pwsh -NoLogo"
    let call = r.h.state().submitted().iter().rev().find(|c| c.id == "session_new").cloned().unwrap();
    assert_eq!(call.params["command"], "pwsh", "the program and its arguments go separately");
    assert_eq!(call.params["args"], "-NoLogo");
    assert_eq!(call.params["folder"], r.folder.as_str());

    let name = r.engine.view().session(&id).unwrap().name.clone();
    r.until("the sidebar row", |r| r.has_contains(&format!("Session: {name}")));
    r.until("the pane", |r| r.has_contains(&format!("Pane: {name}")));
    // The pane measured its grid and that started the PTY (the fallback is 30 s away).
    r.until("the PTY", |r| r.pty(0).is_some());
    let pty = r.pty(0).unwrap();
    assert_ne!((pty.first.cols, pty.first.rows), (220, 50), "not the fallback size");
    let term = r.engine.terminal(&id).unwrap();
    let size = term.terminal().lock().unwrap().size();
    assert_eq!((pty.first.cols, pty.first.rows), (size.cols, size.rows), "the PTY starts at the grid the pane drew");
    assert!(pty.first.cols > 40 && pty.first.rows > 10, "a real pane, not a degenerate rect: {:?}", pty.first);
    assert!(pty.argv.iter().any(|a| a.contains("pwsh")), "argv: {:?}", pty.argv);

    // Output reaches the pane; the status follows the engine.
    pty.emit(b"hello from the fake pty\r\n");
    r.until("Idle", |r| r.h.state().model.session(&id).is_some_and(|s| s.status == csm_ui::model::Status::Idle));
}

#[test]
fn buttons_submit_real_engine_tools_and_the_ui_follows_the_snapshot() {
    let mut r = rig(true);
    r.until("the empty screen", |r| r.has("No sessions yet"));
    r.new_session(1);
    r.until("the layout bar", |r| r.has("Layout: Focus"));

    r.h.get_by_label("Layout: Focus").click();
    r.until("the engine's layout", |r| r.engine.view().current_layout().mode == csm_core::models::LayoutMode::Focus);
    r.until("the UI's layout", |r| r.h.state().model.layout(&r.h.state().scope()).mode == LayoutMode::Focus);

    r.h.get_by_label("Group view: layers").click();
    r.until("the engine's view", |r| r.engine.view().view == csm_engine::GroupView::Headers);
    r.until("the UI's view", |r| r.h.state().model.group_mode == csm_ui::model::GroupMode::Headers);

    // A tool's error comes back to the person as a toast instead of being swallowed.
    let ctx = r.h.ctx.clone();
    r.h.state_mut().run(&ctx, csm_ui::Action::Tool("session_focus", json!({"session_id": "no-such-session"})));
    r.until("the tool's error", |r| r.toast_contains("no-such-session") || r.toast_contains("No session"));
}

#[test]
fn a_consent_card_from_a_real_external_call_allows_it_and_the_call_completes() {
    let mut r = rig(true);
    r.until("the empty screen", |r| r.has("No sessions yet"));
    let id = r.new_session(1);
    r.until("the PTY", |r| r.pty(0).is_some());
    // An MCP client wants to type into the session: the engine parks the call and asks.
    let caller = Caller::External { client: "test-client".into(), session_id: None };
    let reply = r.engine.submit(caller, "session_send_text", json!({"session_id": id, "text": "echo hi", "submit": true}));
    r.until("the card", |r| r.has("Allow once"));
    assert!(reply.try_recv().is_err(), "nothing runs before the person answers");
    r.h.get_by_label("Allow once").click();
    r.until("the answer", |r| !r.has("Allow once"));
    let out = reply.recv_timeout(Duration::from_secs(5)).expect("the external call completes");
    assert!(out.is_ok(), "{out:?}");
    let written = String::from_utf8_lossy(&r.pty(0).unwrap().written.lock().unwrap()).into_owned();
    assert!(written.contains("echo hi"), "the text reached the PTY: {written:?}");
}

#[test]
fn closing_the_window_shows_the_shutdown_board_until_quit_ready() {
    // A Claude session that ignores the console close: the board shows it closing, then forced.
    let mut r = rig(false);
    r.until("the empty screen", |r| r.has("No sessions yet"));
    let id = r.new_session(0); // claude
    r.until("the PTY", |r| r.pty(0).is_some());
    r.until("live", |r| r.engine.view().session(&id).is_some_and(|s| s.status != csm_engine::Status::Starting));

    // The window's close button: cancelled, and the quit starts instead.
    r.h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    r.h.step();
    assert!(!r.h.state().quit_ready, "the window does not close before the sessions are down");
    r.until("the board", |r| r.has("Closing CodeShellManager"));
    r.until("the row closing", |r| r.board_rows().iter().any(|s| matches!(s, ShutdownRowState::Closing | ShutdownRowState::Forced)));
    r.until("QuitReady", |r| r.h.state().quit_ready);
    assert_eq!(r.board_rows(), vec![ShutdownRowState::Forced], "the wedged session is marked, not hidden");
    assert!(r.pty(0).unwrap().has_exited(), "every process is gone");
}

#[test]
fn stand_ins_do_not_collide_with_engine_tools() {
    // Engine::start refuses duplicate ids, so building the rig is the collision check.
    let r = rig(true);
    let reg = r.engine.registry();
    for id in [
        "session_new",
        "app_quit",
        "state_import_wpf",
        "pane_float",
        "pane_dock",
        "pane_tear_off",
        "run_start",
        "git_fetch",
        "note_set",
        "search_output",
        "open_with",
        "grant_list",
    ] {
        assert!(reg.get(id).is_some(), "{id} is registered");
    }
    assert!(reg.get("demo_set_status").is_none(), "the review's status switcher is demo-only");
    // pane_* are real: they change the engine's stored layout.
    let folder = r.folder.clone();
    let id = r.engine.call(Caller::Ui, "session_new", json!({"folder": folder, "command": "pwsh", "focus": true})).unwrap().data["id"]
        .as_str()
        .unwrap()
        .to_owned();
    r.engine.call(Caller::Ui, "pane_float", json!({"session_id": id})).unwrap();
    let l = r.engine.view().current_layout();
    assert_eq!(l.mode, csm_core::models::LayoutMode::Free);
    assert_eq!(l.free_tree.unwrap()["floats"][0]["session"], id.as_str());
}

#[test]
fn minimize_maximize_restore_are_real_and_focus_restores() {
    let r = rig(true);
    let folder = r.folder.clone();
    let new = |r: &Rig| {
        r.engine.call(Caller::Ui, "session_new", json!({"folder": folder, "command": "pwsh", "focus": true})).unwrap().data["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (a, b) = (new(&r), new(&r));
    // Minimized is stored in the engine's layout (so it persists with it).
    r.engine.call(Caller::Ui, "pane_minimize", json!({"session_id": a})).unwrap();
    assert_eq!(r.engine.view().current_layout().minimized, vec![a.clone()]);
    // Focusing it from anywhere (sidebar, palette, inbox, CLI) brings it back.
    r.engine.call(Caller::Ui, "session_focus", json!({"session_id": a})).unwrap();
    assert!(r.engine.view().current_layout().minimized.is_empty());
    // Maximize, then focusing a different session restores the layout so it can be seen.
    r.engine.call(Caller::Ui, "pane_maximize", json!({"session_id": a})).unwrap();
    assert_eq!(r.engine.view().current_layout().maximized.as_deref(), Some(a.as_str()));
    r.engine.call(Caller::Ui, "session_focus", json!({"session_id": b})).unwrap();
    assert_eq!(r.engine.view().current_layout().maximized, None);
    // pane_restore undoes either.
    r.engine.call(Caller::Ui, "pane_maximize", json!({"session_id": b})).unwrap();
    let out = r.engine.call(Caller::Ui, "pane_restore", json!({"session_id": b})).unwrap();
    assert_eq!(out.data["maximized"], serde_json::Value::Null);
    assert_eq!(r.engine.view().current_layout().maximized, None);
    // Closing a minimized session forgets it.
    r.engine.call(Caller::Ui, "pane_minimize", json!({"session_id": b})).unwrap();
    r.engine.call(Caller::Ui, "session_close", json!({"session_id": b, "force": true})).unwrap();
    assert!(r.engine.view().current_layout().minimized.is_empty());
    // Effects are declared honestly: UI state only.
    for id in ["pane_minimize", "pane_maximize", "pane_restore"] {
        assert_eq!(r.engine.registry().get(id).unwrap().effects, csm_engine::Effects::UI, "{id}");
    }
}

#[test]
fn editing_a_live_session_asks_restart_now_and_restarts_it() {
    let mut r = rig(true);
    r.until("the empty screen", |r| r.has("No sessions yet"));
    let id = r.new_session(1); // pwsh -NoLogo
    r.until("the PTY", |r| r.pty(0).is_some());
    let ctx = r.h.ctx.clone();
    r.h.state_mut().run(&ctx, csm_ui::Action::EditSession(id.clone()));
    // session_get fills the panel in: the command with its arguments.
    r.until("the edit panel, filled in", |r| r.h.state().ui.quick.loaded);
    assert_eq!(r.h.state().ui.quick.cmd, 1, "pwsh -NoLogo is the second launch command");
    assert_eq!(r.h.state().ui.quick.query, r.folder);
    r.h.key_press(egui::Key::Tab); // → claude: a new command needs a new process
    r.h.step();
    r.h.key_press(egui::Key::Enter);
    r.until("the question", |r| r.has("Restart now"));
    let saved = r.engine.call(Caller::Ui, "session_get", json!({"session_id": id})).unwrap();
    assert_eq!(saved.data["config"]["command"], "claude", "saved before asking");
    r.h.get_by_label("Restart now").click();
    r.until("the restart", |r| r.pty(1).is_some());
    assert!(r.pty(1).unwrap().argv.iter().any(|a| a.contains("claude")), "{:?}", r.pty(1).unwrap().argv);
    assert!(r.h.state().submitted().iter().any(|c| c.id == "session_restart"));
}

#[test]
fn force_close_now_stops_waiting_for_a_wedged_claude() {
    // A Claude that ignores the console close, and a 30 s budget: without force, the board
    // would wait it out.
    let mut r = rig_with(false, Duration::from_secs(30));
    r.until("the empty screen", |r| r.has("No sessions yet"));
    let id = r.new_session(0); // claude
    r.until("the PTY", |r| r.pty(0).is_some());
    r.until("live", |r| r.engine.view().session(&id).is_some_and(|s| s.status != csm_engine::Status::Starting));
    let ctx = r.h.ctx.clone();
    r.h.state_mut().run(&ctx, csm_ui::Action::AskQuit);
    r.until("the board", |r| r.has("Force close now"));
    let t0 = Instant::now();
    r.h.get_by_label("Force close now").click();
    r.until("QuitReady", |r| r.h.state().quit_ready);
    assert!(t0.elapsed() < Duration::from_secs(5), "force must not wait out the budget: {:?}", t0.elapsed());
    let call = r.h.state().submitted().iter().rev().find(|c| c.id == "app_quit").cloned().unwrap();
    assert_eq!(call.params, json!({"force": true}));
    assert_eq!(r.board_rows(), vec![ShutdownRowState::Forced]);
    assert!(r.pty(0).unwrap().has_exited());
}

#[test]
fn an_always_grant_is_listed_in_settings_and_revoked_there() {
    let mut r = rig(true);
    r.until("the empty screen", |r| r.has("No sessions yet"));
    let id = r.new_session(1);
    r.until("the PTY", |r| r.pty(0).is_some());
    let caller = Caller::External { client: "test-client".into(), session_id: None };
    let reply = r.engine.submit(caller.clone(), "session_read_screen", json!({"session_id": id}));
    r.until("the card", |r| r.has("Always for this client"));
    r.h.get_by_label("Always for this client").click();
    r.until("the answer", |r| !r.has("Always for this client"));
    reply.recv_timeout(Duration::from_secs(5)).unwrap().unwrap();
    assert_eq!(r.engine.call(Caller::Ui, "grant_list", json!({})).unwrap().data.as_array().unwrap().len(), 1);

    let ctx = r.h.ctx.clone();
    r.h.state_mut().run(&ctx, csm_ui::Action::OpenSettings);
    r.until("the page", |r| r.has("Settings: MCP"));
    r.h.get_by_label("Settings: MCP").click();
    r.until("the grant", |r| r.has("Revoke test-client · session_read_screen"));
    r.h.get_by_label("Revoke test-client · session_read_screen").click();
    r.until("the empty list", |r| r.h.state().ui.settings.grants.is_empty() && r.h.state().submitted().iter().any(|c| c.id == "grant_revoke"));
    assert_eq!(r.engine.call(Caller::Ui, "grant_list", json!({})).unwrap().data, json!([]));
    // Revoked: the next call asks again.
    let again = r.engine.submit(caller, "session_read_screen", json!({"session_id": id}));
    r.until("a new card", |r| r.has("Allow once"));
    assert!(again.try_recv().is_err());
}

#[test]
fn settings_change_through_the_page_and_the_engine_keeps_them() {
    let mut r = rig(true);
    r.until("the empty screen", |r| r.has("No sessions yet"));
    let ctx = r.h.ctx.clone();
    r.h.state_mut().run(&ctx, csm_ui::Action::OpenSettings);
    r.until("the values", |r| r.has("Warn when closing busy sessions"));
    r.h.get_by_label("Warn when closing busy sessions").click();
    r.until("the engine's answer", |r| !r.h.state().prefs.warn_on_close_when_busy);
    let v = r.engine.call(Caller::Ui, "settings_get", json!({"key": "WarnOnCloseWhenBusy"})).unwrap();
    assert_eq!(v.data["value"], false);
    r.h.get_by_label("Settings: About").click();
    r.until("app_info", |r| r.has_contains("CodeShellManager 0."));
}
