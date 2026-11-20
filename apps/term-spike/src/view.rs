//! The spike's pane: a header strip (name, backend, grid, 7501 state) over `csm-ui`'s terminal
//! view, plus the frame counters the bench reports.

use csm_ui::terminal::{TerminalRenderer, TerminalView, ViewOptions};
use egui::{Align2, FontId, Rect, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};

use crate::pane::{Pane, Placement};
use crate::theme;

#[derive(Default, Debug, Clone, Copy)]
pub struct FrameCounters {
    pub panes: u32,
    pub rebuilds: u32,
    pub lock_misses: u32,
    /// Time spent building meshes (ms).
    pub build_ms: f32,
    /// Time inside the torn-off viewports' immediate callbacks, which also render and present
    /// those windows (ms).
    pub torn_ms: f32,
    pub grid_ms: f32,
    pub float_ms: f32,
    /// eframe's CPU time for the PREVIOUS frame: ui() + tessellation + rendering, excluding
    /// vsync waiting (ms).
    pub prev_frame_cpu_ms: f32,
}

/// Render resources shared by every pane in every window.
pub struct Shared {
    pub renderer: TerminalRenderer,
    pub frame: FrameCounters,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaneAction {
    Float,
    TearOff,
    Dock,
}

/// Draws one pane (header + terminal) into `rect`. Returns a header action, if any.
pub fn pane_ui(ui: &mut Ui, pane: &mut Pane, shared: &mut Shared, rect: Rect) -> Option<PaneAction> {
    let header = Rect::from_min_size(rect.min, vec2(rect.width(), theme::PANE_HEADER));
    let body = Rect::from_min_max(pos2(rect.min.x, header.max.y), rect.max);
    shared.renderer.frame = Default::default();
    let opts = ViewOptions { padding: Vec2::splat(theme::PANE_PADDING), ..ViewOptions::default() };
    let r = pane.view.show(ui, pane.term.as_ref(), &mut shared.renderer, body, &opts);
    let f = shared.renderer.frame;
    shared.frame.panes += 1;
    shared.frame.rebuilds += f.rebuilds;
    shared.frame.lock_misses += f.lock_misses;
    shared.frame.build_ms += f.build_ms;
    let painter = ui.painter_at(rect);
    if let Some(text) = TerminalView::placeholder(pane.term.as_ref()) {
        painter.text(body.center(), Align2::CENTER_CENTER, text, FontId::proportional(13.0), theme::MUTED);
    }
    painter.rect_stroke(body, 0.0, Stroke::new(theme::RING, if r.focused { theme::ACCENT } else { theme::BORDER }), StrokeKind::Inside);
    header_ui(ui, pane, header)
}

fn header_ui(ui: &mut Ui, pane: &Pane, header: Rect) -> Option<PaneAction> {
    let painter = ui.painter_at(header);
    painter.rect_filled(header, 0.0, theme::TOOLBAR);
    let st = pane.term.status();
    let backend = pane.term.backend().map_or("…", |b| b.label());
    let grid = pane.view.grid().map_or_else(|| "…".to_string(), |g| format!("{}x{}", g.cols, g.rows));
    let mut text = format!("{}  ·  {}  ·  {}", pane.name, backend, grid);
    if let Some(ps) = &st.program_status {
        let state = ps.split(':').next().unwrap_or("");
        text.push_str(&format!("  ·  7501 {state}"));
    }
    if let Some(a) = st.probe_answered {
        text.push_str(if a { "  ·  probe answered" } else { "  ·  probe not answered" });
    }
    if let Some(code) = st.exit {
        text.push_str(&format!("  ·  exited {code:?}"));
    }
    if !st.title.is_empty() {
        text.push_str(&format!("  ·  {}", st.title));
    }
    painter.text(header.left_center() + Vec2::new(6.0, 0.0), Align2::LEFT_CENTER, text, FontId::proportional(theme::HEADER_TEXT), theme::FG);
    let mut action = None;
    let buttons: &[(PaneAction, &str)] = match pane.placement {
        Placement::Grid => &[(PaneAction::Float, "float"), (PaneAction::TearOff, "tear off")],
        _ => &[(PaneAction::Dock, "dock")],
    };
    let mut x = header.max.x - 4.0;
    for (a, label) in buttons.iter().rev() {
        let w = 58.0;
        let r = Rect::from_min_max(pos2(x - w, header.min.y + 1.0), pos2(x, header.max.y - 1.0));
        if ui.put(r, egui::Button::new(egui::RichText::new(*label).size(theme::HEADER_TEXT)).small()).clicked() {
            action = Some(*a);
        }
        x -= w + 4.0;
    }
    action
}
