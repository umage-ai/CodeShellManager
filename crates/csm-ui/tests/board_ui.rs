//! The Board layout in the real app: `CsmApp` on `EngineBackend` over a real engine (only the PTY
//! is fake). Columns, the drag of a backlog task onto an agent card (a confirm with the exact
//! prompt, then `task_assign`), Review's Accept, and an idle card's "+ task".
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, State, StateOptions};
use csm_pty::{Backend as PtyBackend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::{CsmApp, EngineBackend, EngineOptions};
use egui::{Event, Modifiers, PointerButton, Pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::{Value, json};

#[derive(Default)]
struct FakePty {
    written: Mutex<Vec<u8>>,
    exited: AtomicBool,
    callbacks: Mutex<Vec<ExitCallback>>,
}

impl Pty for FakePty {
    fn subscribe(&self, _: OutputSink) {}
    fn on_exit(&self, cb: ExitCallback) {
        self.callbacks.lock().unwrap().push(cb);
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
        self.close();
    }
    fn close(&self) {
        if !self.exited.swap(true, Ordering::SeqCst) {
            for cb in std::mem::take(&mut *self.callbacks.lock().unwrap()) {
                cb(Some(0));
            }
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
    group: String,
    alpha: String,
    beta: String,
    _dir: tempfile::TempDir,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.engine.shutdown();
    }
}

impl Rig {
    /// Steps the UI until `f` holds (bounded by time: the engine answers on its own thread).
    fn until(&mut self, what: &str, f: impl Fn(&mut Self) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
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

    fn ui(&self, id: &str, params: Value) -> Value {
        self.engine.call(Caller::Ui, id, params).unwrap_or_else(|e| panic!("{id}: {e}")).data
    }

    fn task(&self, id: &str) -> Option<csm_core::tasks::Task> {
        self.engine.view().tasks.iter().find(|t| t.id == id).cloned()
    }

    fn center(&self, label: &str) -> Pos2 {
        self.h.get_by_label(label).rect().center()
    }

    /// Press, move in steps, release: a real drag, as egui sees one.
    fn drag(&mut self, from: Pos2, to: Pos2) {
        self.h.event(Event::PointerMoved(from));
        self.h.step();
        self.h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        self.h.step();
        for i in 1..=8 {
            self.h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
            self.h.step();
        }
        self.h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
        self.h.step();
        self.h.step();
    }

    fn written_anywhere(&self, text: &str) -> usize {
        self.ptys.lock().unwrap().iter().filter(|p| String::from_utf8_lossy(&p.written.lock().unwrap()).contains(text)).count()
    }
}

/// Two live sessions (Alpha, Beta) in group Proj, on the Board.
fn rig() -> Rig {
    let ptys: Ptys = Arc::new(Mutex::new(Vec::new()));
    let list = ptys.clone();
    let options = StateOptions {
        pty: Arc::new(move |_: SpawnOptions| {
            let p = Arc::new(FakePty::default());
            list.lock().unwrap().push(p.clone());
            Ok(p as Arc<dyn Pty>)
        }),
        // The Board draws no panes, so nothing measures a grid: sessions start at the fallback.
        first_size_timeout: Duration::from_millis(30),
        exit_wait: Duration::from_millis(300),
        shutdown_budget: Duration::from_millis(300),
        restore_on_start: false,
        ..StateOptions::default()
    };
    let mut app = csm_core::models::AppState::default();
    app.settings.claude_launch_stagger_ms = 0;
    app.settings.auto_resume_claude = false;
    let engine = Engine::start(State::new(app, options), csm_ui::app_tools()).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().display().to_string();
    let call = |id: &str, p: Value| engine.call(Caller::Ui, id, p).unwrap_or_else(|e| panic!("{id}: {e}")).data;
    let group = call("group_new", json!({"name": "Proj"}))["group_id"].as_str().unwrap().to_owned();
    let alpha = call("session_new", json!({"folder": folder, "command": "pwsh", "name": "Alpha", "group": group}))["id"].as_str().unwrap().to_owned();
    let beta = call("session_new", json!({"folder": folder, "command": "pwsh", "name": "Beta", "group": group}))["id"].as_str().unwrap().to_owned();
    call("view_set", json!({"rail_tab": group}));
    let backend = EngineBackend::new(engine.clone(), EngineOptions { launch_commands: vec!["pwsh".into()], ..EngineOptions::default() });
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let app = CsmApp::new(Box::new(csm_ui::EnginePort::new(engine.clone())), Box::new(backend), TerminalRenderer::new(fonts));
    let h = Harness::builder().with_size(vec2(1600.0, 900.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app);
    let mut r = Rig { h, engine, ptys, group, alpha, beta, _dir: dir };
    r.until("the layout bar", |r| r.has("Layout: Board"));
    r.h.get_by_label("Layout: Board").click();
    r.until("the board", |r| r.has_contains("Board column: Backlog"));
    let (a, b) = (r.alpha.clone(), r.beta.clone());
    r.until("both sessions idle", |r| {
        let v = r.engine.view();
        [&a, &b].iter().all(|id| v.session(id).is_some_and(|s| s.status == csm_engine::Status::Idle))
    });
    r
}

#[test]
fn the_board_shows_its_columns_and_agent_cards_by_status() {
    let mut r = rig();
    assert_eq!(r.engine.view().current_layout().mode, csm_core::models::LayoutMode::Board, "layout_set mode=board");
    for c in ["Backlog (0)", "Working (0)", "Needs you (0)", "Review (0)", "Idle (2)", "Done (0)"] {
        r.until(c, |r| r.has(&format!("Board column: {c}")));
    }
    assert!(r.has("Agent: Alpha") && r.has("Agent: Beta"));
    assert!(r.has("New task for Alpha"), "an idle card offers + task");
    // Every session view has a time in its status, not just the inbox's.
    assert!(r.h.state().model.sessions.iter().all(|s| s.status_since_ms.is_some()));
    // A task in another group's project is not on this board.
    r.ui("task_new", json!({"project": "group:elsewhere", "title": "Not here"}));
    r.ui("task_new", json!({"session_id": r.alpha, "title": "Write the docs"}));
    r.until("the backlog card", |r| r.has("Task: Write the docs"));
    assert!(!r.has("Task: Not here"));
    r.until("the count", |r| r.has("Board column: Backlog (1)"));
    // A click focuses the session and switches to Focus.
    r.h.get_by_label("Agent: Beta").click();
    let beta = r.beta.clone();
    r.until("Beta focused", |r| r.engine.view().focused.as_deref() == Some(beta.as_str()));
    r.until("Focus layout", |r| r.engine.view().current_layout().mode == csm_core::models::LayoutMode::Focus);
    let _ = &r.group;
}

#[test]
fn dragging_a_backlog_task_onto_an_agent_asks_with_the_prompt_then_assigns_and_sends() {
    let mut r = rig();
    let id = r.ui("task_new", json!({"session_id": r.alpha, "title": "Write the docs", "body": "Start with the README"}))["id"]
        .as_str()
        .unwrap()
        .to_owned();
    r.until("the backlog card", |r| r.has("Task: Write the docs"));
    let (from, to) = (r.center("Task: Write the docs"), r.center("Agent: Beta"));
    r.drag(from, to);
    r.until("the confirm", |r| r.has("Assign and send"));
    assert!(r.has_contains(&format!("Task {id} from the user: Write the docs")), "the confirm shows the exact prompt");
    assert!(r.has_contains("Start with the README"));
    assert_eq!(r.task(&id).unwrap().state, csm_core::tasks::TaskState::Backlog, "nothing happens before the answer");
    r.h.get_by_label("Assign and send").click();
    let beta = r.beta.clone();
    r.until("the task assigned to Beta", |r| r.task(&id).is_some_and(|t| t.assignee.as_deref() == Some(beta.as_str())));
    r.until("the prompt in Beta's PTY", |r| r.written_anywhere(&format!("Task {id} from the user: Write the docs")) == 1);
    let call = r.h.state().submitted().iter().rev().find(|c| c.id == "task_assign").cloned().unwrap();
    assert_eq!(call.params, json!({"task_id": id, "session_id": beta, "send": true, "force": false}));
    // Beta's card shows the task it works on; the backlog is empty.
    r.until("the empty backlog", |r| r.has("Board column: Backlog (0)"));

    // Done → Review → Accept → Done.
    r.ui("task_done", json!({"task_id": id, "note": "README rewritten"}));
    let accept = format!("Accept {id}");
    r.until("the review card", |r| r.has(&accept));
    r.h.get_by_label(&accept).click();
    r.until("accepted", |r| r.task(&id).is_some_and(|t| t.state == csm_core::tasks::TaskState::Done));
    r.until("the done column", |r| r.has("Board column: Done (1)"));
}

#[test]
fn plus_task_on_an_idle_card_creates_the_task_and_sends_it() {
    let mut r = rig();
    r.h.get_by_label("New task for Alpha").click();
    r.until("the prompt", |r| r.has("Add and send"));
    r.h.event(Event::Text("Run the linter".into()));
    r.h.step();
    r.h.get_by_label("Add and send").click();
    let alpha = r.alpha.clone();
    r.until("the task, assigned to Alpha", |r| {
        r.engine.view().tasks.iter().any(|t| t.title == "Run the linter" && t.assignee.as_deref() == Some(alpha.as_str()))
    });
    r.until("the prompt in Alpha's PTY", |r| r.written_anywhere("from the user: Run the linter") == 1);
}
