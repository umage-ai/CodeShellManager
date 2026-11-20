//! CodeShellManager desktop app.
//!
//! By default it is the real app: the state is loaded (honouring `--config-dir`, then
//! `CSM_CONFIG_DIR`, and `--clean`), the engine starts with it, `csm` / `csm mcp` reach it over
//! the IPC bridge, and the window draws the engine's snapshot. Closing the window quits through
//! `app_quit` (the shutdown board) and the window closes on `QuitReady`.
//!
//! `--demo` runs the mock-up's data instead (screenshots, design review): see `csm_ui::demo`.
//!
//! ```text
//! codeshell [--clean] [--config-dir DIR] [--latte] [--font PATH] [--font-size PT] [--inbox]
//! codeshell --demo [--static] [--restore] [--latte] [--font PATH] [--font-size PT] [--inbox]
//! ```
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use csm_engine::{Engine, State, StateOptions, persist};
use csm_ui::app::DialogRequest;
use csm_ui::demo::{DemoBackend, DemoOptions};
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, default_paths, embedded_mono, load_face};
use csm_ui::theme::{self, ThemeKind};
use csm_ui::{EngineBackend, EngineOptions};

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
}

impl App {
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
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.frame(ui);
        let ctx = ui.ctx().clone();
        self.file_dialogs(&ctx, frame);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
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
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
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
    let result = eframe::run_native(
        "codeshell",
        options,
        Box::new(move |cc| {
            csm_ui::app::setup(&cc.egui_ctx, theme_kind);
            let ctx = cc.egui_ctx.clone();
            let _ = window.set(ctx.clone());
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
            let app = csm_ui::CsmApp::new(engine, backend, TerminalRenderer::new(fonts));
            let wake_ctx = cc.egui_ctx.clone();
            let dialogs = craft_dialogs::Dialogs::new(Arc::new(move || wake_ctx.request_repaint()));
            Ok(Box::new(App { app, dialogs }))
        }),
    );
    if let Some(h) = ipc
        && let Err(e) = h.shutdown()
    {
        log::warn!("ipc: {e}");
    }
    shutdown.shutdown();
    if let Err(e) = result {
        eprintln!("eframe: {e}");
        std::process::exit(1);
    }
}
