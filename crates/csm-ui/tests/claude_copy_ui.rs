//! Copying from Claude panes: "Copy last reply" / "Copy reply…" and Ctrl+Shift+C on the real
//! engine (Claude's replies from a fixture transcript), and the cleaned (or, with Alt, raw) copy
//! of a selection in the terminal view.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_engine::csm_index::{Index, IndexOptions};
use csm_engine::{Caller, ClaudeRoots, Engine, IndexSetting, State, StateOptions};
use csm_pty::{Backend as PtyBackend, ExitCallback, OutputSink, Pty, PtySize, SpawnOptions};
use csm_term::GridSize;
use csm_ui::app::Menu;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::terminal::{StaticTerminal, TerminalRenderer, TerminalView, ViewOptions};
use csm_ui::{Action, CsmApp, EngineBackend, EngineOptions};
use egui::{Event, Id, Key, Modifiers, OutputCommand, PointerButton, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

const TRANSCRIPT: &str = include_str!("../../csm-core/src/claude_reply_fixture.jsonl");
const PS: &str = "Get-ChildItem -Path C:\\work -Recurse -File | Where-Object { $_.Length -gt 1MB } | Sort-Object Length -Descending | Select-Object -First 20 FullName, Length";

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

/// The app, and every text it put on the clipboard.
struct Ui {
    app: CsmApp,
    copied: Vec<String>,
}

fn copies(ctx: &egui::Context) -> Vec<String> {
    ctx.output(|o| o.commands.iter().filter_map(|c| if let OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None }).collect())
}

fn until(h: &mut Harness<'static, Ui>, what: &str, f: impl Fn(&Harness<'static, Ui>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f(h) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    h.step();
}

fn toasts(h: &Harness<'static, Ui>) -> Vec<String> {
    h.state().app.ui.toasts.iter().map(|t| t.text.clone()).collect()
}

/// A real engine with a live Claude session (fake PTY) whose transcript is the fixture, and the
/// app over it with that session focused.
fn claude_app() -> (Harness<'static, Ui>, Engine, String, tempfile::TempDir, tempfile::TempDir) {
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
    std::fs::write(dir.join(format!("{cid}.jsonl")), TRANSCRIPT).unwrap();
    // The reader has found it.
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.call(Caller::Ui, "claude_reply_get", json!({"session_id": id})).is_err() {
        assert!(Instant::now() < deadline, "the transcript was not found");
        std::thread::sleep(Duration::from_millis(10));
    }

    let backend = EngineBackend::new(engine.clone(), EngineOptions { folders: vec![folder.path().to_string_lossy().into_owned()], ..EngineOptions::default() });
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let mut app = CsmApp::new(Box::new(csm_ui::EnginePort::new(engine.clone())), Box::new(backend), TerminalRenderer::new(fonts));
    app.ui.no_dialog_delay = true;
    let mut h = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(
        |ui, s: &mut Ui| {
            s.app.frame(ui);
            s.copied.extend(copies(ui.ctx()));
        },
        Ui { app, copied: Vec::new() },
    );
    let ctx = h.ctx.clone();
    h.state_mut().app.run(&ctx, Action::Tool("session_focus", json!({"session_id": id})));
    let sid = id.clone();
    until(&mut h, "the session", move |h| h.state().app.model.active.as_deref() == Some(sid.as_str()));
    (h, engine, id, root, folder)
}

#[test]
fn the_pane_menu_copies_the_last_reply_and_the_picker_a_code_block() {
    let (mut h, engine, id, _root, _folder) = claude_app();
    // The pane menu is taller than the 800-pixel window the app opens here, so it scrolls; this test
    // clicks its last rows, so it takes a window that holds the whole menu.
    h.set_size(vec2(1280.0, 1000.0));
    h.step();
    let ctx = h.ctx.clone();
    h.state_mut().app.run(&ctx, Action::OpenMenu(Menu::Pane { session: id.clone(), at: pos2(900.0, 120.0) }));
    h.step();
    assert!(h.query_by_label("Copy reply…").is_some());
    h.get_by_label("Copy last reply").click();
    until(&mut h, "the copy", |h| !h.state().copied.is_empty());
    let text = h.state().copied.last().unwrap().clone();
    assert!(text.starts_with("Run this in PowerShell:\n\n```powershell\n"), "{text}");
    let lines = text.lines().count();
    assert!(toasts(&h).contains(&format!("Copied Claude's last reply ({lines} lines)")), "{:?}", toasts(&h));
    assert!(h.state().app.ui.menu.is_none());

    // "Copy reply…": the picker, in the menu's place, lists the replies and their code blocks.
    h.state_mut().copied.clear();
    h.state_mut().app.run(&ctx, Action::OpenMenu(Menu::Pane { session: id.clone(), at: pos2(900.0, 120.0) }));
    h.step();
    h.get_by_label("Copy reply…").click();
    until(&mut h, "the reply list", |h| h.query_by_label("Run this in PowerShell:").is_some());
    assert!(matches!(&h.state().app.ui.menu, Some(Menu::ClaudeReplies { session, .. }) if *session == id));
    assert!(h.query_by_label("I'll look at the folder first.").is_some());
    assert!(h.query_by_label("Hello! What can I do for you?").is_some());
    assert!(h.query_by_label("Copy code block 2: json {").is_some());
    let block = h.query_all_by_label_contains("Copy code block 1: powershell Get-ChildItem").count();
    assert_eq!(block, 1);
    h.get_by_label_contains("Copy code block 1: powershell").click();
    until(&mut h, "the code block", |h| !h.state().copied.is_empty());
    assert_eq!(h.state().copied.last().unwrap(), PS, "the command, one line, as Claude wrote it");
    assert!(toasts(&h).contains(&"Copied code block 1 (1 line)".to_string()), "{:?}", toasts(&h));
    let asked: Vec<_> = h.state().app.submitted().iter().filter(|c| c.id == "claude_code_block_get").map(|c| c.params.clone()).collect();
    assert_eq!(asked, [json!({"session_id": id, "reply_id": "msg_03", "block_index": 0})]);

    // An older reply, picked by its id.
    h.state_mut().copied.clear();
    h.state_mut().app.run(&ctx, Action::OpenClaudeReplies(id.clone()));
    until(&mut h, "the reply list", |h| h.query_by_label("Hello! What can I do for you?").is_some());
    h.get_by_label("Hello! What can I do for you?").click();
    until(&mut h, "the old reply", |h| !h.state().copied.is_empty());
    assert_eq!(h.state().copied.last().unwrap(), "Hello! What can I do for you?");
    assert!(toasts(&h).contains(&"Copied Claude's reply (1 line)".to_string()), "{:?}", toasts(&h));
    engine.shutdown();
}

#[test]
fn ctrl_shift_c_copies_the_last_reply() {
    let (mut h, engine, id, _root, _folder) = claude_app();
    // As a key (no terminal focused, or another platform's copy)…
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::C);
    until(&mut h, "the copy", |h| !h.state().copied.is_empty());
    assert!(h.state().copied[0].starts_with("Run this in PowerShell:"));
    assert!(h.state().app.submitted().iter().any(|c| c.id == "claude_reply_get" && c.params == json!({"session_id": id})));
    assert!(toasts(&h).iter().any(|t| t.starts_with("Copied Claude's last reply (")), "{:?}", toasts(&h));
    // …and as the copy egui sends for it, with nothing focused.
    h.state_mut().copied.clear();
    let ctx = h.ctx.clone();
    ctx.memory_mut(|m| {
        if let Some(f) = m.focused() {
            m.surrender_focus(f);
        }
    });
    h.event_modifiers(Event::Copy, Modifiers::COMMAND | Modifiers::SHIFT);
    until(&mut h, "the second copy", |h| !h.state().copied.is_empty());
    assert!(h.state().copied[0].starts_with("Run this in PowerShell:"));
    engine.shutdown();
}

#[test]
fn a_wsl_or_ssh_claude_session_says_it_is_not_available() {
    let (mut h, engine, _id, _root, folder) = claude_app();
    let ssh = engine
        .call(Caller::Ui, "session_new", json!({"folder": folder.path().to_string_lossy(), "command": "claude", "kind": "ssh", "ssh": {"host": "box"}, "focus": false}))
        .unwrap();
    let ssh = ssh.data["id"].as_str().unwrap().to_owned();
    let ctx = h.ctx.clone();
    h.state_mut().app.run(&ctx, Action::CopyClaude("claude_reply_get", json!({"session_id": ssh})));
    until(&mut h, "the answer", |h| toasts(h).iter().any(|t| t.contains("not available for this session")));
    assert!(h.state().copied.is_empty());
    engine.shutdown();
}

// ── The terminal view: a selection in a Claude pane ─────────────────────────────────────────

struct View {
    source: StaticTerminal,
    view: TerminalView,
    renderer: TerminalRenderer,
    opts: ViewOptions,
    copied: Vec<String>,
    copy_reply: bool,
    rect: egui::Rect,
}

/// What Claude Code shows for a reply with a long PowerShell command, at 80 columns.
const SCREEN: &str = include_str!("../src/terminal/claude_screens/powershell_80.txt");

/// A view exactly 80 columns wide over [`SCREEN`], with `opts`.
fn view(opts: ViewOptions) -> (Harness<'static, View>, egui::Rect, f32, f32) {
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), 13.0, true);
    let mut renderer = TerminalRenderer::new(fonts);
    let m = renderer.fonts.metrics(1.0);
    let (cw, ch) = (m.cell_w, m.cell_h);
    let pad = opts.padding;
    let size = vec2(80.5 * cw + 2.0 * pad.x, 10.5 * ch + 2.0 * pad.y);
    let bytes = SCREEN.replace('\n', "\r\n");
    let source = StaticTerminal::new(GridSize::new(80, 10), bytes.as_bytes());
    let state = View { source, view: TerminalView::new(Id::new("claude-view")), renderer, opts, copied: Vec::new(), copy_reply: false, rect: egui::Rect::from_min_size(egui::Pos2::ZERO, size) };
    let mut h = Harness::builder().with_size(size + vec2(40.0, 40.0)).with_pixels_per_point(1.0).build_ui_state(
        |ui, s: &mut View| {
            let rect = s.rect;
            let r = s.view.show(ui, &s.source, &mut s.renderer, rect, &s.opts);
            s.copy_reply |= r.copy_reply;
            s.copied.extend(copies(ui.ctx()));
        },
        state,
    );
    h.step();
    h.step();
    assert_eq!(h.state().view.grid().map(|g| g.cols), Some(80), "the view is 80 columns wide");
    let origin = pos2(pad.x, pad.y);
    (h, egui::Rect::from_min_size(origin, vec2(80.0 * cw, 6.0 * ch)), cw, ch)
}

/// Drags over the first six rows (the whole reply) and copies with `mods`.
fn select_all_and_copy(h: &mut Harness<'static, View>, cells: egui::Rect, cw: f32, ch: f32, mods: Modifiers) {
    let from = cells.min + vec2(cw * 0.2, ch * 0.5);
    let to = pos2(cells.max.x - cw * 0.2, cells.max.y - ch * 0.5);
    h.hover_at(from);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.hover_at(pos2(from.x + cw * 5.0, from.y + ch));
    h.hover_at(to);
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    h.event_modifiers(Event::Copy, mods);
    h.step();
    h.step();
}

#[test]
fn a_selection_in_a_claude_pane_is_copied_clean_and_with_alt_as_shown() {
    let opts = ViewOptions { request_focus: true, claude: true, clean_copy: true, ..ViewOptions::default() };
    let (mut h, cells, cw, ch) = view(opts);
    select_all_and_copy(&mut h, cells, cw, ch, Modifiers::COMMAND);
    let got = h.state().copied.last().cloned().unwrap_or_default();
    assert_eq!(got, format!("Here is the command:\n\n{PS}\n\nRun it from the repository root."));

    // Ctrl+Alt+C: the text as the screen shows it.
    h.state_mut().copied.clear();
    select_all_and_copy(&mut h, cells, cw, ch, Modifiers::COMMAND | Modifiers::ALT);
    let raw = h.state().copied.last().cloned().unwrap_or_default();
    assert!(raw.starts_with("⏺ Here is the command:"), "{raw}");
    assert!(raw.contains("-gt 1MB\n  } | Sort-Object"), "the wrap is kept: {raw}");
}

#[test]
fn clean_copy_off_or_another_program_copies_as_shown() {
    let opts = ViewOptions { request_focus: true, claude: true, clean_copy: false, ..ViewOptions::default() };
    let (mut h, cells, cw, ch) = view(opts);
    select_all_and_copy(&mut h, cells, cw, ch, Modifiers::COMMAND);
    let raw = h.state().copied.last().cloned().unwrap_or_default();
    assert!(raw.starts_with("⏺ Here is the command:"), "{raw}");
}

#[test]
fn ctrl_shift_c_with_nothing_selected_asks_for_the_reply_and_ctrl_c_does_not() {
    let opts = ViewOptions { request_focus: true, claude: true, clean_copy: true, ..ViewOptions::default() };
    let (mut h, _, _, _) = view(opts);
    h.event_modifiers(Event::Copy, Modifiers::COMMAND);
    h.step();
    assert!(!h.state().copy_reply, "Ctrl+C with nothing selected interrupts, as before");
    h.event_modifiers(Event::Copy, Modifiers::COMMAND | Modifiers::SHIFT);
    h.step();
    assert!(h.state().copy_reply);
    assert!(h.state().copied.is_empty());

    // Not a Claude pane: Ctrl+Shift+C is Ctrl+C.
    let (mut h, _, _, _) = view(ViewOptions { request_focus: true, ..ViewOptions::default() });
    h.event_modifiers(Event::Copy, Modifiers::COMMAND | Modifiers::SHIFT);
    h.step();
    assert!(!h.state().copy_reply);
}
