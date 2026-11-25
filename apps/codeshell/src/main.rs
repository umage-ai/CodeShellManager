//! CodeShellManager desktop app.
//!
//! By default it is the real app: the state is loaded (honouring `--config-dir`, then
//! `CSM_CONFIG_DIR`, and `--clean`), the engine starts with it, `csm` / `csm mcp` reach it over
//! the IPC bridge, and the window draws the engine's snapshot. Closing the window follows
//! `CloseAction` (`csm_ui::background`): ask, hide to the tray, or quit through `app_quit` (the
//! shutdown board), after which the window closes on `QuitReady`; the tray's Quit quits. Without
//! a tray it always quits.
//!
//! On Windows the main window has no OS title bar: CSM's own title bar is the only one, with
//! minimize, maximize and close (`csm_ui::screens::titlebar`). macOS and Linux keep the OS frame.
//!
//! `--demo` runs the mock-up's data instead (screenshots, design review): see `csm_ui::demo`.
//!
//! ```text
//! codeshell [--clean] [--config-dir DIR] [--autostart] [--latte] [--font PATH] [--font-size PT] [--inbox]
//! codeshell --demo [--static] [--restore] [--latte] [--font PATH] [--font-size PT] [--inbox]
//! codeshell --autostart-refresh | --autostart-remove     (the installer's hooks; exit at once)
//! codeshell --remote <host-id> [--config-dir DIR] [--latte]   (a window onto a paired office)
//! ```
//!
//! `--remote` starts no engine: the window runs over `csm_remote::RemoteBackend` to the paired host
//! (its own view state is kept beside the laptop's pairing files). Closing it, or its Quit, only
//! disconnects; the office keeps running.
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
use csm_ui::start_window::{StartUp, StartWindow};
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, default_paths, embedded_mono, load_face};
use csm_ui::theme::{self, ThemeKind};
use csm_ui::{EngineBackend, EngineOptions};
use winit::application::ApplicationHandler;
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::platform::run_on_demand::EventLoopExtRunOnDemand as _;

const USAGE: &str = "codeshell [--clean] [--config-dir DIR] [--autostart] [--latte] [--font PATH] [--font-size PT] [--inbox]
codeshell --demo [--static] [--restore] [...]
codeshell --remote <host-id> [--config-dir DIR] [--latte]

  --clean           start without sessions and never write the state file (safe testing)
  --config-dir DIR  keep state.json in DIR (default: CSM_CONFIG_DIR, else the user data folder)
  --autostart       started by the OS at sign-in (the window opens as Start minimized says)
  --demo            the mock-up's sessions instead of yours (screenshots, review)
  --static          (demo) no live shells: every session is a static transcript
  --restore         (demo) play the restore rail on start
  --latte           start in the light theme (Catppuccin Latte)
  --font PATH       terminal font (default: embedded JetBrains Mono)
  --font-size PT    terminal font size (default 12)
  --inbox           prefer the inbox ConPTY over the bundled one
  --autostart-refresh  (installer) point the sign-in entry, if any, at this exe; then exit
  --autostart-remove   (installer) remove the sign-in entry if it starts this exe; then exit";

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
    /// Started by the OS's sign-in entry (logged; the window follows `StartMinimized` either way).
    autostart: bool,
    /// An installer hook: do it and exit.
    installer: Option<InstallerHook>,
    /// A remote window onto this paired host (`--remote <host-id>`): no local engine.
    remote: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InstallerHook {
    /// After an install or upgrade: an existing sign-in entry starts this exe from now on.
    Refresh,
    /// On uninstall: the sign-in entry goes, if it starts this exe (or nothing that exists).
    Remove,
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
            "--autostart" => a.autostart = true,
            "--autostart-refresh" => a.installer = Some(InstallerHook::Refresh),
            "--autostart-remove" => a.installer = Some(InstallerHook::Remove),
            "--remote" => a.remote = Some(value("--remote").to_string_lossy().into_owned()),
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
    /// The remote window's connection and what it keeps (`None` for the laptop's own window).
    remote: Option<RemoteWindow>,
}

/// What a remote window keeps beside its pairing: its own view state for the host (layout, focus,
/// rail, theme). The file is written on a worker thread, once a second at most, when it changed.
struct RemoteWindow {
    handle: csm_remote::RemoteHandle,
    /// `None` when the host id is not a plain name (then nothing is kept).
    view_file: Option<PathBuf>,
    saved: Option<csm_ui::view_overlay::ViewOverlay>,
    checked_at: Option<Instant>,
    /// The link has been stopped for a disconnect (it is stopped once).
    stopped: bool,
}

impl RemoteWindow {
    /// Once a frame: keeps the event loop ticking (the link's state arrives on the worker thread,
    /// not as an event), saves the view state when it changed, and closes the window on a disconnect.
    /// A disconnect stops the link and sends the close; the close is never cancelled (see
    /// `csm_ui::background`), so the window goes.
    fn step(&mut self, ctx: &egui::Context, disconnect: bool) {
        ctx.request_repaint_after(REMOTE_TICK);
        if disconnect {
            if !self.stopped {
                self.stopped = true;
                self.save_view();
                self.handle.stop();
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        let now = Instant::now();
        if self.checked_at.is_some_and(|t| now.duration_since(t) < Duration::from_secs(1)) {
            return;
        }
        self.checked_at = Some(now);
        self.save_view();
    }

    /// Writes the view state on a worker thread, when it differs from what was last written.
    fn save_view(&mut self) {
        let Some(path) = self.view_file.clone() else { return };
        let current = self.handle.overlay();
        if self.saved.as_ref() == Some(&current) {
            return;
        }
        self.saved = Some(current.clone());
        let worker = std::thread::Builder::new().name("csm-remote-view".into()).spawn(move || {
            if let Err(e) = csm_remote::connect::save_view(&path, &current) {
                log::warn!("remote window: the view state was not saved: {e}");
            }
        });
        if let Err(e) = worker {
            log::warn!("remote window: no thread to save the view state: {e}");
        }
    }
}

/// How often a remote window looks at its link when nothing else wakes it (the link runs on its own
/// thread and does not wake the UI).
const REMOTE_TICK: Duration = Duration::from_millis(100);

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
        if let Some(r) = self.remote.as_mut() {
            r.step(ctx, self.app.disconnect);
        }
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

/// First start after the MSI replaced the WPF app (or any first start beside it): bring over the
/// WPF app's sessions and groups (`state_import_wpf`: they arrive asleep), the settings that map
/// onto ours (theme, commands, font size…; its Open with list waits on a consent card,
/// `open_with_pending`), and its project notes (`notes_import_wpf`), through the same tools the
/// empty screen's button and the palette use. Never with `--clean`. Off the UI thread; the WPF
/// files are only read. Failures are logged, never shown as errors: the empty screen still offers
/// the import by hand.
fn import_wpf_on_first_run(engine: &Engine, state_json: PathBuf) {
    let engine = engine.clone();
    let worker = move || {
        log::info!("first run: importing the WPF app's state from {}", state_json.display());
        match engine.call(csm_engine::Caller::Ui, "state_import_wpf", json!({"path": state_json.display().to_string()})) {
            Ok(out) => log::info!("first run: {}", out.message),
            Err(e) => log::warn!("first run: the WPF state was not imported: {e}"),
        }
        if let Some(db) = csm_engine::wpf_output_db().filter(|p| p.is_file()) {
            match engine.call(csm_engine::Caller::Ui, "notes_import_wpf", json!({"path": db.display().to_string()})) {
                Ok(out) => log::info!("first run: {}", out.message),
                Err(e) => log::warn!("first run: the WPF notes were not imported: {e}"),
            }
        }
    };
    if let Err(e) = std::thread::Builder::new().name("csm-wpf-import".into()).spawn(worker) {
        log::warn!("first run: could not start the WPF import: {e}");
    }
}

/// The agent-app registrations `csm setup` wrote name `csm.exe` by full path. When the install
/// folder moved (2.0 installs under `C:\Program Files\umage.ai\`), entries that still name the old
/// folder are pointed at the `csm.exe` beside this exe: only entries setup added, comments kept,
/// a backup made (`csm_setup::repair_moved`). On a worker thread, Windows only, once per start.
fn repair_moved_registrations() {
    if !cfg!(windows) {
        return;
    }
    let worker = || {
        // Only the installed app (`...\umage.ai\<name>\codeshell.exe`): a development build must
        // not take over the installed app's registrations.
        let Some(csm) = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(std::path::Path::to_path_buf))
            .filter(|d| d.parent().and_then(|p| p.file_name()).is_some_and(|n| n.eq_ignore_ascii_case("umage.ai")))
            .map(|d| d.join("csm.exe"))
            .filter(|p| p.is_file())
        else {
            return;
        };
        let command = vec![csm.to_string_lossy().into_owned(), "mcp".to_owned()];
        // Not while a Claude process may be rewriting ~/.claude.json (a rewrite between our read
        // and our rename would be lost): wait for a quiet moment, a few minutes at most; the next
        // start repairs what is left.
        let results = csm_setup::repair_moved_when_idle(&csm_setup::Host::current(), &command, &claude_process_running, 10, std::time::Duration::from_secs(60));
        for r in results {
            log::info!("agent registration: {} {:?} (backup {:?})", r.config.display(), r.outcome, r.backup);
        }
    };
    if let Err(e) = std::thread::Builder::new().name("csm-repair-mcp".into()).spawn(worker) {
        log::warn!("agent registration: could not start the repair: {e}");
    }
}

/// Whether a Claude Code process runs on this machine: `claude.exe`, or `node.exe` with `claude`
/// in its command line. A process scan, so only from a worker thread. When the scan cannot be
/// done the answer is "yes", which only postpones the repair.
fn claude_process_running() -> bool {
    let script = "(Get-CimInstance Win32_Process | Where-Object { $_.Name -ieq 'claude.exe' -or ($_.Name -ieq 'node.exe' -and $_.CommandLine -match 'claude') } | Measure-Object).Count";
    let args = ["-NoProfile", "-NonInteractive", "-Command", script];
    match csm_engine::proc::capture_ok("powershell.exe", args, std::time::Duration::from_secs(20)) {
        Some(out) => out.trim().parse::<u32>().map_or(true, |n| n > 0),
        None => true,
    }
}

/// What the laptop's window does to start a remote window (`remote_window_open`): this exe, by argv,
/// `--remote <host-id>`, with the same state folder. The child starts on a thread of its own, never
/// on the UI thread; the call returns once that thread is started.
fn remote_window_launcher(config_dir: Option<PathBuf>) -> csm_engine::RemoteWindowLauncher {
    Arc::new(move |host_id: &str| {
        let exe = std::env::current_exe().map_err(|e| format!("this program's path is not known: {e}"))?;
        let mut argv = vec!["--remote".to_owned(), host_id.to_owned()];
        if let Some(dir) = &config_dir {
            argv.push("--config-dir".to_owned());
            argv.push(dir.display().to_string());
        }
        // `proc::spawn_detached` owns the process rules (no console, reaped on its own thread); this
        // thread only keeps the start off the UI thread.
        std::thread::Builder::new()
            .name("csm-remote-spawn".into())
            .spawn(move || {
                if let Err(e) = csm_engine::proc::spawn_detached(&exe, &argv, None) {
                    log::warn!("remote window: could not start it: {e}");
                }
            })
            .map(|_| ())
            .map_err(|e| format!("no thread to start the window on: {e}"))
    })
}

/// `--remote <host-id>`: a window onto a paired office. It starts no engine: the window's calls and
/// its state are the office's, over `csm-remote`, and its own view state is kept beside the
/// pairing files. A window that cannot connect (no key, a host that is not paired) says so on stderr
/// and ends; the laptop's own window shows the pairing.
fn run_remote_window(args: &Args, host_id: String, fonts: FontSet) {
    let theme_kind = if args.latte { ThemeKind::Latte } else { ThemeKind::Mocha };
    // The laptop's client folder: `peer-client/` beside the state file, where the pairing was made.
    let client_dir = persist::state_path(args.config_dir.as_deref()).as_deref().and_then(std::path::Path::parent).map(|d| d.join("peer-client"));
    let Some(client_dir) = client_dir else {
        eprintln!("A remote window needs the laptop's state folder: pass --config-dir.");
        std::process::exit(1);
    };
    let (params, host) = match csm_remote::window_params(&client_dir, &host_id, theme_kind, Vec::new()) {
        Ok(p) => p,
        Err(e) => {
            log::error!("remote window: {e}");
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let view_file = csm_remote::connect::view_path(&client_dir, &host_id);
    let saved = view_file.as_deref().and_then(csm_remote::connect::load_view);
    let mut backend = match csm_remote::RemoteBackend::connect(params) {
        Ok(b) => b,
        Err(e) => {
            log::error!("remote window: {e}");
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    backend.describe(&host.name, &host.address);

    let start = StartWindow::Normal;
    let viewport = egui::ViewportBuilder::default()
        .with_title(format!("{} · CodeShellManager", host.name))
        .with_inner_size(theme::size::WINDOW)
        .with_min_inner_size(theme::size::WINDOW_MIN)
        .with_icon(std::sync::Arc::new(csm_ui::app::app_icon(64)));
    #[cfg(windows)]
    let viewport = viewport.with_decorations(false);
    let options = eframe::NativeOptions { viewport: start.viewport(viewport, start.offscreen()), ..Default::default() };
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
            // The window's two faces share one backend, made on the UI thread.
            let (port, view, handle) = csm_remote::shared(backend);
            let mut app = csm_ui::CsmApp::new(Box::new(port), Box::new(view), TerminalRenderer::new(fonts));
            app.monitors = app_monitors;
            app.window_controls = cfg!(windows);
            let wake_ctx = cc.egui_ctx.clone();
            let dialogs = craft_dialogs::Dialogs::new(Arc::new(move || wake_ctx.request_repaint()));
            let remote = RemoteWindow { handle, view_file, saved, checked_at: None, stopped: false };
            Ok(Box::new(App { app, dialogs, tray: None, notifier: None, clicked: None, remote: Some(remote) }))
        }),
        &event_loop,
    );
    let mut handler = WithMonitors { inner: app, monitors, read_at: None };
    let mut event_loop = event_loop;
    if let Err(e) = event_loop.run_app_on_demand(&mut handler) {
        eprintln!("event loop: {e}");
        std::process::exit(1);
    }
    if !created.load(Ordering::Relaxed) {
        eprintln!("eframe: the window could not be created");
        std::process::exit(1);
    }
}

fn start_failed(e: impl std::fmt::Display) -> ! {
    eprintln!("could not start the engine: {e}");
    std::process::exit(1);
}

fn main() {
    let args = parse_args();
    // First: a panic on any thread leaves `logs/crash-<UTC>.log` (release builds have no console).
    let logs = csm_engine::diag::logs_dir(args.config_dir.as_deref());
    csm_engine::diag::install_panic_hook(logs.clone(), "codeshell");
    if let Some(hook) = args.installer {
        // Run by the MSI as the installing user (`installer/CodeShellManager2.wxs`): no window.
        let a = csm_engine::Autostart::system();
        let what = match hook {
            InstallerHook::Refresh => a.refresh(false),
            InstallerHook::Remove => a.remove_if_ours(),
        };
        if let Some(w) = what {
            log::info!("installer: {w}");
        }
        return;
    }
    if args.autostart {
        log::info!("started at sign-in (--autostart)");
    }
    // One app per state file: launched again (the Start menu, a second sign-in entry), ask the
    // running one to show its window and leave. Not for `--clean`, which never shares a state.
    // A remote window is a window of its own: it never shows the laptop's running app instead.
    if !args.demo
        && !args.clean
        && args.remote.is_none()
        && let Some(path) = persist::state_path(args.config_dir.as_deref())
    {
        match csm_mcp::activate_running(csm_mcp::RemoteOptions::default(), &path) {
            Ok(true) => {
                log::info!("CodeShellManager already runs on {}: showed its window", path.display());
                return;
            }
            Ok(false) => {}
            Err(e) => log::warn!("could not reach a running CodeShellManager ({e}); starting another"),
        }
    }

    // The file log and the run marker: the real app only. `--clean`, `--demo` and `--remote` run
    // beside the real app on the same folders, so they leave its log and marker alone.
    let diag_dir = logs.filter(|_| !args.demo && !args.clean && args.remote.is_none());
    let mut crash_notice = None;
    if let Some(dir) = &diag_dir {
        csm_engine::diag::init_file_logger(dir);
        log::info!("CodeShellManager {} ({} channel) starting", env!("CARGO_PKG_VERSION"), csm_core::channel::CURRENT.id());
        crash_notice = csm_engine::diag::begin_run(dir).notice();
        if let Some(n) = &crash_notice {
            log::warn!("{n}");
        }
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
    let mut fonts = FontSet::new(primary, fallbacks, size, true);
    // A font the user picked is drawn as designed; the default one gets a slashed zero.
    fonts.slashed_zero = args.font.is_none();

    // A window onto a paired office: no engine here, only the connection to the office.
    if let Some(host_id) = args.remote.clone() {
        return run_remote_window(&args, host_id, fonts);
    }

    let (engine, mode, theme_kind, start) = if args.demo {
        let shell: Vec<OsString> = match find_on_path("pwsh.exe").or_else(|| find_on_path("pwsh")) {
            Some(_) => vec!["pwsh".into(), "-NoLogo".into()],
            None if cfg!(windows) => vec!["cmd".into()],
            None => vec![std::env::var_os("SHELL").unwrap_or_else(|| "sh".into())],
        };
        let engine = csm_ui::demo_engine().unwrap_or_else(|e| start_failed(e));
        let theme_kind = if args.latte { ThemeKind::Latte } else { ThemeKind::Mocha };
        (engine, Mode::Demo { live: args.live, restore: args.restore, shell }, theme_kind, StartWindow::Normal)
    } else {
        // The data folders, once, before anything reads them: a stable build moves the pre-v2
        // folder (CodeShellManager-rs -> CodeShellManager2), a beta build copies the stable app's
        // folders on its first start. Nothing for --clean. Start-up, before the UI exists, so the
        // disk is fine here.
        for line in persist::prepare_data_dirs(args.config_dir.as_deref(), args.clean) {
            log::info!("data folder: {line}");
        }
        // Remote access (another CSM connecting to this one): a server whose files live beside the
        // state, off until the person turns it on in the app. Not for `--clean`, which writes nothing,
        // and not for an isolated run with no state folder: they get no host, so nothing can listen.
        let save_path = persist::state_path(args.config_dir.as_deref());
        let peer_server = save_path.as_deref().and_then(std::path::Path::parent).filter(|_| !args.clean).map(|d| csm_peer::PeerServer::new(d.join("peer")));
        if let Some(p) = &peer_server {
            // The relay secrets live in the same keychain as the add-ons' secrets.
            p.use_secrets(std::sync::Arc::new(csm_services::secrets::KeyringStore));
        }
        let peer_host: Arc<dyn csm_engine::PeerHost> = match &peer_server {
            Some(p) => Arc::new(p.clone()),
            None => Arc::new(csm_engine::NoPeerHost),
        };
        // The other side of the same link: pairing with another CSM and its host list. Its files live in
        // `peer-client` beside the state; it dials out only for a pairing the person starts in the app.
        let peer_client: Arc<dyn csm_engine::PeerClientHost> = match save_path.as_deref().and_then(std::path::Path::parent).filter(|_| !args.clean) {
            Some(d) => Arc::new(csm_peer::ClientPeer::new(d.join("peer-client"))),
            None => Arc::new(csm_engine::NoPeerClient),
        };
        let remote_window_launcher = remote_window_launcher(args.config_dir.clone());
        let options = StateOptions { save_path, clean: args.clean, peer_host, peer_client, remote_window_launcher, ..StateOptions::production() };
        let options_autostart = options.autostart.clone();
        let budget = options.shutdown_budget.as_secs_f32();
        let (state, report) = State::load(options);
        // The sign-in entry follows the app when it moved (an upgrade into a new folder). Not for a
        // test run (`--clean`, another config folder: the entry starts neither), and a development
        // build only takes over an entry whose program is gone.
        if !args.clean
            && args.config_dir.is_none()
            && std::env::var_os("CSM_CONFIG_DIR").is_none()
            && let Some(line) = options_autostart.refresh(cfg!(debug_assertions))
        {
            log::info!("autostart: {line}");
        }
        if !args.clean && args.config_dir.is_none() && std::env::var_os("CSM_CONFIG_DIR").is_none() {
            repair_moved_registrations();
        }
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
        // Minimized or in the tray on every start (`--autostart` alone changes nothing): see
        // `csm_ui::start_window`. A tray only where the tray module makes one.
        let start = StartWindow::choose(settings.start_minimized, settings.start_minimized_to_tray, tray::POSSIBLE);
        let first_run_import = persist::first_run_wpf_import(args.config_dir.as_deref(), args.clean, report.source);
        let beta_offer = persist::beta_import_offer(args.config_dir.as_deref(), args.clean, report.source);
        let engine = Engine::start(state, csm_ui::app_tools()).unwrap_or_else(|e| start_failed(e));
        if let Some(p) = &peer_server {
            // Gives it the engine its devices' calls run on, and replays the one thing the person
            // may have chosen before: remote access on, at the address they picked. A fresh install,
            // an upgrade or an import has no such choice, so this starts nothing.
            p.attach(engine.clone());
            p.resume();
        }
        if let Some(path) = first_run_import {
            import_wpf_on_first_run(&engine, path);
        }
        if let Some(path) = beta_offer {
            // A tester's beta data beside a stable build with none of its own: offered, never imported silently.
            engine.offer_beta_import(path);
        }
        (engine, Mode::Real { opts }, theme_kind, start)
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

    // The title stays set without the OS title bar: the taskbar and Alt+Tab show it.
    let viewport = egui::ViewportBuilder::default()
        .with_title("CodeShellManager")
        .with_inner_size(theme::size::WINDOW)
        .with_min_inner_size(theme::size::WINDOW_MIN)
        .with_icon(std::sync::Arc::new(csm_ui::app::app_icon(64)));
    // Windows: one title bar, CSM's own (winit keeps the snap styles and the drop shadow; the
    // title bar draws the caption buttons and the edges resize). Elsewhere the OS frame stays.
    #[cfg(windows)]
    let viewport = viewport.with_decorations(false);
    let options = eframe::NativeOptions { viewport: start.viewport(viewport, start.offscreen()), ..Default::default() };
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
            let mut app = csm_ui::CsmApp::new(Box::new(csm_ui::EnginePort::new(engine)), backend, TerminalRenderer::new(fonts));
            app.monitors = app_monitors;
            app.window_controls = cfg!(windows);
            if let Some(n) = crash_notice.take() {
                app.toast(&cc.egui_ctx, n);
            }
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
            // A start in the tray needs one too: without it, minimized instead.
            let mode = if tray.is_some() { start } else { start.without_tray() };
            app.start = StartUp::new(mode, start.offscreen());
            Ok(Box::new(App { app, dialogs, tray, notifier, clicked, remote: None }))
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
    if let Some(dir) = &diag_dir {
        log::info!("CodeShellManager quit");
        csm_engine::diag::end_run(dir);
    }
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
