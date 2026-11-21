//! CodeShellManager desktop app.
//!
//! By default it is the real app: the state is loaded (honouring `--config-dir`, then
//! `CSM_CONFIG_DIR`, and `--clean`), the engine starts with it, `csm` / `csm mcp` reach it over
//! the IPC bridge, and the window draws the engine's snapshot. Closing the window quits through
//! `app_quit` (the shutdown board) and the window closes on `QuitReady`; with `CloseToTray` on it
//! hides to the tray instead (`csm_ui::background`), and the tray's Quit quits.
//!
//! `--demo` runs the mock-up's data instead (screenshots, design review): see `csm_ui::demo`.
//!
//! ```text
//! codeshell [--clean] [--config-dir DIR] [--latte] [--font PATH] [--font-size PT] [--inbox]
//! codeshell --demo [--static] [--restore] [--latte] [--font PATH] [--font-size PT] [--inbox]
//! ```
// A GUI program on Windows, so no console window opens with it. Release builds only: a debug
// build (`cargo run`) keeps its console for the log. The `csm` CLI stays a console program.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
#![forbid(unsafe_code)]

mod os_notify;
mod tray;

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use csm_ui::Action;
use serde_json::json;

use csm_core::window_placement::{MonitorArea, PixelRect};
use csm_engine::{Engine, State, StateOptions, persist};
use csm_ui::app::DialogRequest;
use csm_ui::demo::{DemoBackend, DemoOptions};
use csm_ui::placement::Monitors;
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, default_paths, embedded_mono, load_face};
use csm_ui::theme::{self, ThemeKind};
use csm_ui::{EngineBackend, EngineOptions};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;

const USAGE: &str = "codeshell [--clean] [--config-dir DIR] [--latte] [--font PATH] [--font-size PT] [--inbox]
codeshell --demo [--static] [--restore] [...]

  --clean           start without sessions and never write the state file (safe testing)
  --config-dir DIR  keep state.json in DIR (default: CSM_CONFIG_DIR, else the user data folder)
  --demo            the mock-up's sessions instead of yours (screenshots, review)
  --static          (demo) no live shells: every session is a static transcript
  --restore         (demo) play the restore rail on start
  --latte           start in the light theme (Catppuccin Latte)
  --font PATH       terminal font (default: embedded JetBrains Mono)
  --font-size PT    terminal font size (default 12)
  --inbox           prefer the inbox ConPTY over the bundled one";

#[derive(Default)]
struct Args {
    demo: bool,
    live: bool,
    restore: bool,
    clean: bool,
    config_dir: Option<PathBuf>,
    latte: bool,
    font: Option<PathBuf>,
    font_size: Option<f32>,
}

fn parse_args() -> Args {
    let mut args = std::env::args_os().skip(1);
    let mut a = Args { live: true, ..Args::default() };
    while let Some(x) = args.next() {
        let s = x.to_string_lossy().into_owned();
        let mut value = |name: &str| match args.next() {
            Some(v) => v,
            None => {
                eprintln!("{name} needs a value\n{USAGE}");
                std::process::exit(2);
            }
        };
        match s.as_str() {
            "--demo" => a.demo = true,
            "--static" => a.live = false,
            "--restore" => a.restore = true,
            "--clean" => a.clean = true,
            "--config-dir" => a.config_dir = Some(PathBuf::from(value("--config-dir"))),
            "--latte" => a.latte = true,
            "--font" => a.font = Some(PathBuf::from(value("--font"))),
            "--font-size" => a.font_size = value("--font-size").to_string_lossy().parse().ok(),
            "--inbox" => csm_pty::set_prefer_bundled(false),
            "-h" | "--help" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    a
}

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join(name)).find(|p| p.is_file())
}

fn home() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from)
}

struct App {
    app: csm_ui::CsmApp,
    /// Native dialogs, shown after the frame with this window as their parent; answers arrive on a
    /// later frame, so the event loop never blocks.
    dialogs: craft_dialogs::Dialogs<DialogRequest>,
    /// The real app's tray icon, OS notifications, and the sessions whose notification was
    /// clicked. `None` in the demo (its sessions aren't real) and where there is no tray.
    tray: Option<tray::TrayHost>,
    notifier: Option<os_notify::OsNotifier>,
    clicked: Option<Receiver<String>>,
}

impl App {
    /// Sends what the UI decided to notify, follows the "needs you" count on the tray, and turns
    /// tray picks and notification clicks into tools.
    fn tray_and_notifications(&mut self, ctx: &egui::Context) {
        let out = self.app.take_notifications();
        if let Some(n) = &self.notifier {
            for x in out {
                n.send(x);
            }
        }
        let clicked: Vec<String> = self.clicked.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        for id in clicked {
            // The test notification and "still running in the tray" have no session.
            if !id.is_empty() {
                self.app.run(ctx, Action::Tool("session_focus", json!({"session_id": id})));
            }
            self.app.run(ctx, Action::Tool("window_show", json!({})));
        }
        let Some(t) = &mut self.tray else { return };
        t.set_needs_you(self.app.needs_you_count());
        for c in t.commands() {
            match c {
                tray::TrayCommand::Show => self.app.run(ctx, Action::Tool("window_show", json!({}))),
                tray::TrayCommand::NextNeedsYou => {
                    self.app.run(ctx, Action::Tool("inbox_next", json!({})));
                    self.app.run(ctx, Action::Tool("window_show", json!({})));
                }
                // As closing the window: the busy warning, then `app_quit` and the shutdown board.
                tray::TrayCommand::Quit => {
                    self.app.run(ctx, Action::Tool("window_show", json!({})));
                    self.app.run(ctx, Action::AskQuit);
                }
            }
        }
    }

    /// The file dialogs the UI asks for (the quick-open's Browse…, Settings' Export… and
    /// Import…): shown with this window as their parent, the answer handed back on a later frame.
    fn file_dialogs(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if let Some(req) = self.app.take_dialog_request() {
            const BACKUP: (&str, &[&str]) = ("CodeShellManager backup", &["csm"]);
            let d = match &req {
                DialogRequest::Folder { start } => {
                    let d = craft_dialogs::FileDialog::folder().title("Folder to start the session in");
                    if start.is_empty() { d } else { d.directory(start) }
                }
                DialogRequest::SaveBackup { file_name } => {
                    craft_dialogs::FileDialog::save().title("Export CodeShellManager setup").filter(BACKUP.0, BACKUP.1).file_name(file_name)
                }
                DialogRequest::OpenBackup => craft_dialogs::FileDialog::open().title("Import CodeShellManager setup").filter(BACKUP.0, BACKUP.1),
            };
            self.dialogs.ask(d, req);
        }
        self.dialogs.show_pending(Some(frame));
        for (req, answer) in self.dialogs.take_answers() {
            match answer {
                craft_dialogs::Answer::Paths(p) => self.app.dialog_answered(req, p.first().map(|p| p.display().to_string())),
                craft_dialogs::Answer::Unavailable(why) => {
                    log::warn!("file dialog: {why}");
                    self.app.toast(ctx, format!("The file dialog couldn't open: {why}"));
                    self.app.dialog_answered(req, None);
                }
                _ => self.app.dialog_answered(req, None),
            }
        }
    }
}

impl eframe::App for App {
    /// Before every UI pass, and also while the window is hidden to the tray (where eframe runs
    /// no UI pass): engine replies and events, notifications, the tray, and the close on
    /// `QuitReady`. See `csm_ui::background`.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.app.logic(ctx);
        self.tray_and_notifications(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.frame(ui);
        let ctx = ui.ctx().clone();
        self.file_dialogs(&ctx, frame);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

/// The monitors as winit reports them, in physical pixels. winit has no work area, so the full
/// bounds stand in for it (a caption under a taskbar counts as reachable).
fn monitors_of(event_loop: &ActiveEventLoop) -> Vec<MonitorArea> {
    let primary = event_loop.primary_monitor();
    event_loop
        .available_monitors()
        .map(|m| {
            let (p, s) = (m.position(), m.size());
            let bounds = PixelRect::new(i64::from(p.x), i64::from(p.y), i64::from(s.width), i64::from(s.height));
            let dpi = (m.scale_factor() * 96.0).round();
            let dpi = if dpi.is_finite() && dpi >= 1.0 { dpi as i64 } else { 96 };
            MonitorArea { bounds, work: bounds, dpi, is_primary: primary.as_ref() == Some(&m) }
        })
        .filter(|m| m.bounds.width > 0 && m.bounds.height > 0)
        .collect()
}

/// How often a window move may re-read the monitor list.
const MONITOR_REREAD: Duration = Duration::from_millis(250);

/// eframe's application with the monitor list kept current: read when the app resumes (before
/// eframe creates the window, so the first frame has it) and again, at most every
/// [`MONITOR_REREAD`], when a window moves or changes scale. This is event-loop code, not frame
/// code; the frames only copy the list.
struct WithMonitors<'a> {
    inner: eframe::EframeWinitApplication<'a>,
    monitors: Monitors,
    read_at: Option<Instant>,
}

impl WithMonitors<'_> {
    fn read(&mut self, event_loop: &ActiveEventLoop, force: bool) {
        if !force && self.read_at.is_some_and(|t| t.elapsed() < MONITOR_REREAD) {
            return;
        }
        self.read_at = Some(Instant::now());
        let list = monitors_of(event_loop);
        *self.monitors.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = list;
    }
}

impl ApplicationHandler<eframe::UserEvent> for WithMonitors<'_> {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.read(event_loop, true);
        self.inner.resumed(event_loop);
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: winit::window::WindowId, event: winit::event::WindowEvent) {
        if matches!(event, winit::event::WindowEvent::Moved(_) | winit::event::WindowEvent::ScaleFactorChanged { .. }) {
            self.read(event_loop, false);
        }
        self.inner.window_event(event_loop, window_id, event);
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: winit::event::StartCause) {
        self.inner.new_events(event_loop, cause);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: eframe::UserEvent) {
        self.inner.user_event(event_loop, event);
    }

    fn device_event(&mut self, event_loop: &ActiveEventLoop, device_id: winit::event::DeviceId, event: winit::event::DeviceEvent) {
        self.inner.device_event(event_loop, device_id, event);
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.about_to_wait(event_loop);
    }

    fn suspended(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.suspended(event_loop);
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.exiting(event_loop);
    }

    fn memory_warning(&mut self, event_loop: &ActiveEventLoop) {
        self.inner.memory_warning(event_loop);
    }
}

/// What a run needs that differs between the real app and the demo.
enum Mode {
    Real { opts: EngineOptions },
    Demo { live: bool, restore: bool, shell: Vec<OsString> },
}

fn start_failed(e: impl std::fmt::Display) -> ! {
    eprintln!("could not start the engine: {e}");
    std::process::exit(1);
}

fn main() {
    let args = parse_args();
    if std::env::var_os("RUST_LOG").is_none() {
        log::set_max_level(log::LevelFilter::Info);
    }

    // Everything that touches the disk happens here, before the UI exists.
    let size = args.font_size.unwrap_or(theme::text::TERMINAL);
    let primary = args.font.as_ref().and_then(|p| load_face(p, 0).map_err(|e| eprintln!("font: {e}")).ok()).or_else(embedded_mono);
    let Some(primary) = primary else {
        eprintln!("no usable terminal font");
        std::process::exit(1);
    };
    let (_, fallback_paths) = default_paths();
    let fallbacks = fallback_paths.iter().filter_map(|(p, i)| load_face(p, *i).ok()).collect();
    let fonts = FontSet::new(primary, fallbacks, size, true);

    let (engine, mode, theme_kind) = if args.demo {
        let shell: Vec<OsString> = match find_on_path("pwsh.exe").or_else(|| find_on_path("pwsh")) {
            Some(_) => vec!["pwsh".into(), "-NoLogo".into()],
            None if cfg!(windows) => vec!["cmd".into()],
            None => vec![std::env::var_os("SHELL").unwrap_or_else(|| "sh".into())],
        };
        let engine = csm_ui::demo_engine().unwrap_or_else(|e| start_failed(e));
        let theme_kind = if args.latte { ThemeKind::Latte } else { ThemeKind::Mocha };
        (engine, Mode::Demo { live: args.live, restore: args.restore, shell }, theme_kind)
    } else {
        // The data folder was renamed for v2 (CodeShellManager-rs -> CodeShellManager2): move it
        // once, before anything reads it. Start-up, before the UI exists, so the disk is fine here.
        if !args.clean {
            for line in persist::migrate_legacy_data_dirs(args.config_dir.as_deref()) {
                log::info!("data folder: {line}");
            }
        }
        let options = StateOptions { save_path: persist::state_path(args.config_dir.as_deref()), clean: args.clean, ..StateOptions::production() };
        let budget = options.shutdown_budget.as_secs_f32();
        let (state, report) = State::load(options);
        match (&report.path, args.clean) {
            (_, true) => log::info!("state: --clean, starting empty and saving nothing"),
            (Some(p), _) => log::info!("state: {} ({:?})", p.display(), report.source),
            (None, _) => log::info!("state: isolated (CSM_CONFIG_DIR is empty); nothing is saved"),
        }
        let settings = &state.app.settings;
        let theme_kind = if args.latte { ThemeKind::Latte } else { csm_ui::engine_backend::theme_from_setting(&settings.theme) };
        let wpf_import = persist::wpf_state_path().filter(|p| p.is_file()).map(|p| p.display().to_string());
        let folders = [home(), std::env::current_dir().ok()].into_iter().flatten().map(|p| p.display().to_string()).collect();
        let opts =
            EngineOptions { theme: theme_kind, launch_commands: settings.launch_commands.clone(), folders, wpf_import, shutdown_budget_s: budget };
        let engine = Engine::start(state, csm_ui::app_tools()).unwrap_or_else(|e| start_failed(e));
        (engine, Mode::Real { opts }, theme_kind)
    };

    // The IPC bridge (`csm`, `csm mcp`): the real app only; the demo's sessions aren't real.
    let window: Arc<OnceLock<egui::Context>> = Arc::new(OnceLock::new());
    let ipc = match &mode {
        Mode::Real { .. } => {
            let raise = window.clone();
            let on_activate: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
                if let Some(ctx) = raise.get() {
                    // Shown too: the window may be hidden to the tray.
                    csm_ui::notify::show_window(ctx);
                }
            });
            match csm_mcp::serve_ipc_with(engine.clone(), csm_mcp::IpcOptions { on_activate: Some(on_activate), ..csm_mcp::IpcOptions::default() }) {
                Ok(h) => Some(h),
                Err(e) => {
                    log::warn!("ipc: `csm` and `csm mcp` cannot reach this window: {e}");
                    None
                }
            }
        }
        Mode::Demo { .. } => None,
    };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CodeShellManager")
            .with_inner_size(theme::size::WINDOW)
            .with_min_inner_size(theme::size::WINDOW_MIN)
            .with_icon(std::sync::Arc::new(csm_ui::app::app_icon(64))),
        ..Default::default()
    };
    let shutdown = engine.clone();
    // The window opens where it was: the app's first frame restores the saved bounds against
    // these monitors (`csm_ui::placement`), which the event loop below keeps current.
    let monitors = Monitors::default();
    let created = Arc::new(AtomicBool::new(false));
    let (app_monitors, app_created) = (monitors.clone(), created.clone());
    let event_loop = match EventLoop::<eframe::UserEvent>::with_user_event().build() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("event loop: {e}");
            std::process::exit(1);
        }
    };
    let app = eframe::create_native(
        "codeshell",
        options,
        Box::new(move |cc| {
            app_created.store(true, Ordering::Relaxed);
            csm_ui::app::setup(&cc.egui_ctx, theme_kind);
            let ctx = cc.egui_ctx.clone();
            let _ = window.set(ctx.clone());
            let real = matches!(mode, Mode::Real { .. });
            let backend: Box<dyn csm_ui::Backend> = match mode {
                Mode::Real { opts } => Box::new(EngineBackend::new(engine.clone(), opts)),
                Mode::Demo { live, restore, shell } => {
                    let repaint: csm_ui::terminal::Repaint = Arc::new(move || ctx.request_repaint());
                    let opts = DemoOptions { live, shell, cwd: home(), now_ms: None, repaint: Some(repaint), theme: theme_kind };
                    let mut b = DemoBackend::new(opts);
                    if restore {
                        b.start_restore();
                    }
                    Box::new(b)
                }
            };
            let mut app = csm_ui::CsmApp::new(engine, backend, TerminalRenderer::new(fonts));
            app.monitors = app_monitors;
            let wake_ctx = cc.egui_ctx.clone();
            let dialogs = craft_dialogs::Dialogs::new(Arc::new(move || wake_ctx.request_repaint()));
            // On the main thread, with the event loop running: where the tray must be made.
            let (tray, notifier, clicked) = if real {
                let (click_tx, click_rx) = mpsc::channel();
                let wake_ctx = cc.egui_ctx.clone();
                let notifier = os_notify::OsNotifier::start(click_tx, Arc::new(move || wake_ctx.request_repaint()));
                (tray::TrayHost::start(&cc.egui_ctx), Some(notifier), Some(click_rx))
            } else {
                (None, None, None)
            };
            // Close to tray needs a tray icon to come back from.
            app.background.tray_available = tray.is_some();
            Ok(Box::new(App { app, dialogs, tray, notifier, clicked }))
        }),
        &event_loop,
    );
    let mut handler = WithMonitors { inner: app, monitors, read_at: None };
    let mut event_loop = event_loop;
    let result = event_loop.run_app_on_demand(&mut handler);
    if let Some(h) = ipc
        && let Err(e) = h.shutdown()
    {
        log::warn!("ipc: {e}");
    }
    shutdown.shutdown();
    if let Err(e) = result {
        eprintln!("event loop: {e}");
        std::process::exit(1);
    }
    if !created.load(Ordering::Relaxed) {
        // eframe logged why (no graphics adapter, no window).
        eprintln!("eframe: the window could not be created");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod icon_file {
    /// The exe's icon (embedded by build.rs) and the installer's shortcut icon are
    /// `assets/codeshell.ico`, generated from the window icon's drawing. Run with
    /// `CSM_WRITE_ICON=1` to regenerate it after changing `csm_ui::app::app_icon`.
    #[test]
    fn the_committed_ico_matches_the_window_icon() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("codeshell.ico");
        let fresh = csm_ui::app::app_icon_ico();
        if std::env::var_os("CSM_WRITE_ICON").is_some() {
            std::fs::write(&path, &fresh).unwrap();
        }
        let committed = std::fs::read(&path).unwrap_or_default();
        assert!(committed == fresh, "{} is stale: run `CSM_WRITE_ICON=1 cargo test -p codeshell icon_file`", path.display());
    }
}
