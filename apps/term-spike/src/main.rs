//! term-spike: Phase 0 gate harness. Usage in docs/phase0-spike.md.

#![forbid(unsafe_code)]

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use term_spike::app::{Config, SpikeApp};
use term_spike::fonts::{FontSet, default_paths, load_face};
use term_spike::view::{FrameCounters, Shared};
use term_spike::{logging, theme};

const USAGE: &str = "term-spike [--panes N] [--stress] [--rate LINES_PER_SEC] [--no-extra] [--bench SECS]
           [--font PATH] [--font-size PT] [--no-ligatures] [--inbox] [--cwd DIR] [--log PATH]
           [-- PROGRAM ARGS...]

  --panes N        grid panes (default 4)
  --stress         grid panes run spike-gen (streaming true-colour output)
  --rate N         spike-gen lines/second per pane (default 2000, 0 = unlimited)
  --no-extra       no floating / torn-off panes
  --bench SECS     run, log a frame-time summary of the second half, exit
  --inbox          prefer the inbox ConPTY over the bundled one
  --no-vsync       present without waiting for vsync (measures raw throughput)
  --spin           repaint continuously (control experiment for the presentation path)
  -- PROGRAM ...   what grid panes run (default: pwsh -NoLogo, else cmd)";

fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path).filter(|d| d.is_absolute()).map(|d| d.join(name)).find(|p| p.is_file())
}

fn main() {
    let mut args = std::env::args_os().skip(1);
    let mut panes = 4usize;
    let mut stress = false;
    let mut rate = 2000u32;
    let mut extra = true;
    let mut bench = None;
    let mut font: Option<PathBuf> = None;
    let mut font_size = theme::DEFAULT_FONT_SIZE;
    let mut ligatures = true;
    let mut vsync = true;
    let mut spin = false;
    let mut cwd = None;
    let mut log_path = PathBuf::from("term-spike.log");
    let mut program: Vec<OsString> = Vec::new();
    while let Some(a) = args.next() {
        let s = a.to_string_lossy().into_owned();
        let mut value = || args.next().map(|v| v.to_string_lossy().into_owned()).unwrap_or_default();
        match s.as_str() {
            "--panes" => panes = value().parse().unwrap_or(panes).clamp(0, 64),
            "--stress" => stress = true,
            "--rate" => rate = value().parse().unwrap_or(rate),
            "--no-extra" => extra = false,
            "--bench" => bench = value().parse::<f32>().ok().map(Duration::from_secs_f32),
            "--font" => font = Some(PathBuf::from(value())),
            "--font-size" => font_size = value().parse().unwrap_or(font_size),
            "--no-ligatures" => ligatures = false,
            "--no-vsync" => vsync = false,
            "--spin" => spin = true,
            "--inbox" => csm_pty::set_prefer_bundled(false),
            "--cwd" => cwd = Some(PathBuf::from(value())),
            "--log" => log_path = PathBuf::from(value()),
            "--" => {
                program.extend(args.by_ref());
                break;
            }
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
    logging::init(log_path, log::LevelFilter::Info);

    // Everything below that touches the disk happens here, before the UI exists.
    let shell_argv: Vec<OsString> = match find_on_path("pwsh.exe") {
        Some(_) => vec!["pwsh".into(), "-NoLogo".into()],
        None => vec!["cmd".into()],
    };
    let grid_argv = if stress {
        let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(PathBuf::from)).unwrap_or_default();
        let gen_name = if cfg!(windows) { "spike-gen.exe" } else { "spike-gen" };
        vec![exe_dir.join(gen_name).into_os_string(), "--rate".into(), rate.to_string().into()]
    } else if !program.is_empty() {
        program
    } else {
        shell_argv.clone()
    };

    let (primary_paths, fallback_paths) = default_paths();
    let primary_candidates: Vec<PathBuf> = font.into_iter().chain(primary_paths).collect();
    let Some(primary) = primary_candidates.iter().find_map(|p| match load_face(p, 0) {
        Ok(f) => Some(f),
        Err(e) => {
            log::warn!("font: {e}");
            None
        }
    }) else {
        eprintln!("no usable monospace font found (tried {primary_candidates:?}); pass --font PATH");
        std::process::exit(1);
    };
    let fallbacks = fallback_paths.iter().filter_map(|(p, i)| load_face(p, *i).ok()).collect();
    let fonts = FontSet::new(primary, fallbacks, font_size, ligatures);
    log::info!("font: {}", fonts.describe());

    let conpty = match csm_pty::conpty_selection() {
        Some((backend, reason)) => format!("{} — {reason}", backend.label()),
        None => "openpty".to_string(),
    };
    log::info!("ConPTY selection: {conpty}");

    let cfg = Config { panes, grid_argv, shell_argv, extra, bench, cwd, conpty, spin };
    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("CSM term-spike").with_inner_size([1500.0, 950.0]),
        ..Default::default()
    };
    if !vsync {
        options.wgpu_options.surface.present_mode = eframe::wgpu::PresentMode::AutoNoVsync;
    }
    let result = eframe::run_native(
        "csm-term-spike",
        options,
        Box::new(move |cc| {
            cc.egui_ctx.set_visuals(egui::Visuals::dark());
            let shared = Shared { fonts, atlas: None, frame: FrameCounters::default() };
            Ok(Box::new(SpikeApp::new(cfg, shared)))
        }),
    );
    if let Err(e) = result {
        log::error!("eframe: {e}");
        eprintln!("eframe: {e}");
        std::process::exit(1);
    }
}
