//! CodeShellManager desktop app.
//!
//! Today it runs on the demo backend (`csm_ui::demo`): the mock-up's sessions, Claude sessions as
//! transcripts and shells as live terminals, with every action going through the engine's tool
//! registry. See `crates/csm-ui/src/backend.rs` for what wiring the real engine changes.
//!
//! ```text
//! codeshell [--static] [--latte] [--restore] [--font PATH] [--font-size PT] [--inbox] [--log-level LEVEL]
//! ```
#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;

use csm_ui::demo::{DemoBackend, DemoOptions};
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, default_paths, embedded_mono, load_face};
use csm_ui::theme::{self, ThemeKind};

const USAGE: &str = "codeshell [--static] [--latte] [--restore] [--font PATH] [--font-size PT] [--inbox]

  --static        no live shells: every session is a static transcript
  --latte         start in the light theme (Catppuccin Latte)
  --restore       play the restore rail on start
  --font PATH     terminal font (default: embedded JetBrains Mono)
  --font-size PT  terminal font size (default 12)
  --inbox         prefer the inbox ConPTY over the bundled one";

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join(name)).find(|p| p.is_file())
}

struct App {
    app: csm_ui::CsmApp,
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.frame(ui);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 1.0]
    }
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mut live = true;
    let mut theme_kind = ThemeKind::Mocha;
    let mut restore = false;
    let mut font: Option<PathBuf> = None;
    let mut font_size = theme::text::TERMINAL;
    while let Some(a) = args.next() {
        let s = a.to_string_lossy().into_owned();
        let mut value = || args.next().map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
        match s.as_str() {
            "--static" => live = false,
            "--latte" => theme_kind = ThemeKind::Latte,
            "--restore" => restore = true,
            "--font" => font = Some(PathBuf::from(value())),
            "--font-size" => font_size = value().parse().unwrap_or(font_size),
            "--inbox" => csm_pty::set_prefer_bundled(false),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => {
                eprintln!("unknown argument {other}\n{USAGE}");
                std::process::exit(2);
            }
        }
    }
    if std::env::var_os("RUST_LOG").is_none() {
        log::set_max_level(log::LevelFilter::Info);
    }

    // Everything that touches the disk happens here, before the UI exists.
    let shell: Vec<OsString> = match find_on_path("pwsh.exe").or_else(|| find_on_path("pwsh")) {
        Some(_) => vec!["pwsh".into(), "-NoLogo".into()],
        None if cfg!(windows) => vec!["cmd".into()],
        None => vec![std::env::var_os("SHELL").unwrap_or_else(|| "sh".into())],
    };
    let cwd = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    let primary = font.and_then(|p| load_face(&p, 0).map_err(|e| eprintln!("font: {e}")).ok()).or_else(embedded_mono);
    let Some(primary) = primary else {
        eprintln!("no usable terminal font");
        std::process::exit(1);
    };
    let (_, fallback_paths) = default_paths();
    let fallbacks = fallback_paths.iter().filter_map(|(p, i)| load_face(p, *i).ok()).collect();
    let fonts = FontSet::new(primary, fallbacks, font_size, true);

    let engine = match csm_ui::demo_engine() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("could not start the engine: {e}");
            std::process::exit(1);
        }
    };
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("CodeShellManager")
            .with_inner_size(theme::size::WINDOW)
            .with_min_inner_size(theme::size::WINDOW_MIN),
        ..Default::default()
    };
    let shutdown = engine.clone();
    let result = eframe::run_native(
        "codeshell",
        options,
        Box::new(move |cc| {
            csm_ui::app::setup(&cc.egui_ctx, theme_kind);
            let ctx = cc.egui_ctx.clone();
            let repaint: csm_ui::terminal::Repaint = Arc::new(move || ctx.request_repaint());
            let opts = DemoOptions { live, shell, cwd, now_ms: None, repaint: Some(repaint), theme: theme_kind };
            let mut backend = DemoBackend::new(opts);
            if restore {
                backend.start_restore();
            }
            let app = csm_ui::CsmApp::new(engine, Box::new(backend), TerminalRenderer::new(fonts));
            Ok(Box::new(App { app }))
        }),
    );
    shutdown.shutdown();
    if let Err(e) = result {
        eprintln!("eframe: {e}");
        std::process::exit(1);
    }
}
