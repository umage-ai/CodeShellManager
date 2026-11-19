//! The spike window: N panes in a grid, one pane floating in an `egui::Window`, one torn off
//! into its own OS window (immediate viewport), a HUD with frame statistics, and a bench mode
//! that logs a summary and exits.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use egui::{CentralPanel, Frame, Id, Panel, Rect, Sense, ViewportBuilder, ViewportClass, ViewportCommand, ViewportId, pos2, vec2};

use crate::pane::{Pane, Placement};
use crate::perf::{Perf, Sample};
use crate::theme;
use crate::view::{FrameCounters, PaneAction, Shared, pane_ui};

pub struct Config {
    pub panes: usize,
    pub grid_argv: Vec<OsString>,
    pub shell_argv: Vec<OsString>,
    pub extra: bool,
    pub bench: Option<Duration>,
    pub cwd: Option<PathBuf>,
    pub conpty: String,
    pub spin: bool,
}

pub struct SpikeApp {
    panes: Vec<Pane>,
    shared: Shared,
    perf: Perf,
    started: Instant,
    last_log: Instant,
    bench: Option<Duration>,
    bench_done: bool,
    conpty: String,
    last_bytes: (Instant, u64),
    ingest_mb_s: f32,
    spin: bool,
}

impl SpikeApp {
    pub fn new(cfg: Config, shared: Shared, ctx: &egui::Context) -> Self {
        let mut panes = Vec::new();
        let grid_name = cfg.grid_argv.first().map(short).unwrap_or_default();
        for i in 0..cfg.panes {
            panes.push(Pane::new(i, format!("#{i} {grid_name}"), cfg.grid_argv.clone(), cfg.cwd.clone(), Placement::Grid, ctx));
        }
        if cfg.extra {
            let shell = cfg.shell_argv.first().map(short).unwrap_or_default();
            let n = panes.len();
            panes.push(Pane::new(n, format!("#{n} {shell} (floating)"), cfg.shell_argv.clone(), cfg.cwd.clone(), Placement::Floating, ctx));
            panes.push(Pane::new(n + 1, format!("#{} {shell} (torn off)", n + 1), cfg.shell_argv.clone(), cfg.cwd.clone(), Placement::TornOff, ctx));
        }
        let now = Instant::now();
        Self {
            panes,
            shared,
            perf: Perf::default(),
            started: now,
            last_log: now,
            bench: cfg.bench,
            bench_done: false,
            conpty: cfg.conpty,
            last_bytes: (now, 0),
            ingest_mb_s: 0.0,
            spin: cfg.spin,
        }
    }

    fn hud(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        let s = self.perf.summary(now, Duration::from_secs(2));
        let total: u64 = self.panes.iter().map(|p| p.term.bytes_read()).sum();
        let dt = now.duration_since(self.last_bytes.0).as_secs_f32();
        if dt >= 1.0 {
            self.ingest_mb_s = (total - self.last_bytes.1) as f32 / dt / 1_000_000.0;
            self.last_bytes = (now, total);
        }
        ui.horizontal(|ui| {
            ui.colored_label(theme::ACCENT, "term-spike");
            ui.colored_label(
                theme::FG,
                format!(
                    "{:.0} fps · ui() avg {:.2} ms p95 {:.2} ms max {:.2} ms · rebuilds/frame {:.1} · lock misses {} · ingest {:.1} MB/s · {} panes",
                    s.fps,
                    s.cpu_avg,
                    s.cpu_p95,
                    s.cpu_max,
                    s.rebuilds_per_frame,
                    s.lock_misses,
                    self.ingest_mb_s,
                    self.panes.len()
                ),
            );
        });
        ui.colored_label(theme::MUTED, format!("ConPTY: {} · font: {}", self.conpty, self.shared.renderer.fonts.describe()));
    }
}

fn short(a: &OsString) -> String {
    let p = std::path::Path::new(a);
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| a.to_string_lossy().into_owned())
}

fn grid_cells(area: Rect, n: usize) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }
    let cols = (n as f32).sqrt().ceil() as usize;
    let rows = n.div_ceil(cols);
    let gap = theme::PANE_GAP;
    let w = (area.width() - gap * (cols as f32 - 1.0)) / cols as f32;
    let h = (area.height() - gap * (rows as f32 - 1.0)) / rows as f32;
    (0..n)
        .map(|i| {
            let (c, r) = (i % cols, i / cols);
            Rect::from_min_size(pos2(area.min.x + c as f32 * (w + gap), area.min.y + r as f32 * (h + gap)), vec2(w, h))
        })
        .collect()
}

impl eframe::App for SpikeApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let t0 = Instant::now();
        self.shared.frame = FrameCounters { prev_frame_cpu_ms: frame.info().cpu_usage.unwrap_or(0.0) * 1000.0, ..Default::default() };
        let ctx = ui.ctx().clone();
        if std::env::var_os("CSM_LOG_REPAINT_CAUSES").is_some() {
            let causes = ctx.repaint_causes();
            let input_events = ctx.input(|i| i.events.len());
            log::info!("frame: repaint causes {causes:?}; {input_events} input events");
        }
        let mut actions: Vec<(usize, PaneAction)> = Vec::new();

        Panel::top("hud").frame(Frame::NONE.fill(theme::TOOLBAR).inner_margin(4.0)).show(ui, |ui| self.hud(ui));

        let tg = Instant::now();
        CentralPanel::default().frame(Frame::NONE.fill(theme::PANEL).inner_margin(theme::PANE_GAP)).show(ui, |ui| {
            let area = ui.max_rect();
            let grid: Vec<usize> = (0..self.panes.len()).filter(|&i| self.panes[i].placement == Placement::Grid).collect();
            for (rect, i) in grid_cells(area, grid.len()).into_iter().zip(grid) {
                if let Some(pane) = self.panes.get_mut(i)
                    && let Some(a) = pane_ui(ui, pane, &mut self.shared, rect)
                {
                    actions.push((i, a));
                }
            }
        });

        self.shared.frame.grid_ms = tg.elapsed().as_secs_f32() * 1000.0;
        let tf = Instant::now();
        // Floating panes: an egui window over the canvas (no airspace problem with a native renderer).
        for i in 0..self.panes.len() {
            if self.panes[i].placement != Placement::Floating {
                continue;
            }
            let title = self.panes[i].name.clone();
            let (panes, shared) = (&mut self.panes, &mut self.shared);
            egui::Window::new(title).id(Id::new(("floating", i))).default_size([640.0, 360.0]).min_size([200.0, 100.0]).resizable(true).show(
                &ctx,
                |ui| {
                    let (rect, _) = ui.allocate_exact_size(ui.available_size(), Sense::hover());
                    if let Some(pane) = panes.get_mut(i)
                        && let Some(a) = pane_ui(ui, pane, shared, rect)
                    {
                        actions.push((i, a));
                    }
                },
            );
        }

        self.shared.frame.float_ms = tf.elapsed().as_secs_f32() * 1000.0;
        let tt = Instant::now();
        // Torn-off panes: a second OS window each (multi-viewport, immediate mode).
        for i in 0..self.panes.len() {
            if self.panes[i].placement != Placement::TornOff {
                continue;
            }
            let title = self.panes[i].name.clone();
            let (panes, shared) = (&mut self.panes, &mut self.shared);
            let vid = ViewportId::from_hash_of(("torn", i));
            let builder = ViewportBuilder::default().with_title(title).with_inner_size([820.0, 520.0]);
            let (close, action) = ctx.show_viewport_immediate(vid, builder, |ui, class| {
                if class == ViewportClass::EmbeddedWindow {
                    log::warn!("multi-viewport unsupported by this backend: torn-off pane is embedded");
                }
                let mut action = None;
                CentralPanel::default().frame(Frame::NONE.fill(theme::PANEL).inner_margin(theme::PANE_GAP)).show(ui, |ui| {
                    let rect = ui.max_rect();
                    if let Some(pane) = panes.get_mut(i) {
                        action = pane_ui(ui, pane, shared, rect);
                    }
                });
                (ui.ctx().input(|inp| inp.viewport().close_requested()), action)
            });
            if let Some(a) = action {
                actions.push((i, a));
            }
            if close {
                actions.push((i, PaneAction::Dock));
            }
        }

        self.shared.frame.torn_ms = tt.elapsed().as_secs_f32() * 1000.0;
        for (i, a) in actions {
            if let Some(p) = self.panes.get_mut(i) {
                p.placement = match a {
                    PaneAction::Float => Placement::Floating,
                    PaneAction::TearOff => Placement::TornOff,
                    PaneAction::Dock => Placement::Grid,
                };
                p.view = csm_ui::terminal::TerminalView::new(egui::Id::new(("csm-term-pane", p.id)));
                log::info!("pane {i} → {:?}", p.placement);
            }
        }

        if self.spin {
            // Control experiment: repaint continuously with whatever is on screen.
            ctx.request_repaint();
        }
        let now = Instant::now();
        let cpu_ms = now.duration_since(t0).as_secs_f32() * 1000.0;
        self.perf.record(Sample { at: now, cpu_ms, counters: self.shared.frame });

        if now.duration_since(self.last_log) >= Duration::from_secs(5) {
            self.last_log = now;
            log::info!("perf (last 5s): {}", self.perf.summary(now, Duration::from_secs(5)));
        }

        if let Some(bench) = self.bench
            && !self.bench_done
        {
            let elapsed = now.duration_since(self.started);
            if elapsed >= bench {
                self.bench_done = true;
                // Measure the second half only: the first half covers startup and spawns.
                let window = bench / 2;
                let s = self.perf.summary(now, window);
                let total: u64 = self.panes.iter().map(|p| p.term.bytes_read()).sum();
                let msg = format!(
                    "BENCH panes={} {} | total PTY bytes {:.3} MB | atlas uploads {}",
                    self.panes.len(),
                    s,
                    total as f64 / 1e6,
                    self.shared.renderer.atlas.as_ref().map_or(0, |a| a.uploads)
                );
                log::info!("{msg}");
                println!("{msg}");
                for p in &self.panes {
                    let st = p.term.status();
                    log::info!(
                        "BENCH pane {} '{}' {:?} backend={:?} first_grid={:?} grid={:?} bytes={} probe_answered={:?} 7501={:?} exit={:?}",
                        p.id,
                        p.name,
                        p.placement,
                        p.term.backend(),
                        p.term.first_grid().map(|g| (g.cols, g.rows)),
                        p.view.grid().map(|g| (g.cols, g.rows)),
                        p.term.bytes_read(),
                        st.probe_answered,
                        st.program_status,
                        st.exit
                    );
                }
                ctx.send_viewport_cmd(ViewportCommand::Close);
            } else {
                // One wake-up at the end of the bench; idle panes otherwise cause no frames.
                ctx.request_repaint_after(bench - elapsed);
            }
        }
    }
}
