//! Glyph weight (the spike's known gap 3: "text looks heavier than egui's own text").
//!
//! Renders the same line with egui's text renderer and with the terminal renderer, in the same
//! font (embedded JetBrains Mono), size and colours, offscreen with the real wgpu renderer, and
//! compares the ink each puts on screen: the summed luminance distance from the background. The
//! terminal's coverage settings are kept within 10% of egui's text. Prints the spike's setting
//! (hinted, linear) next to the current one, so the before/after is visible in the test output.
//! Output images: <target>/text-weight-*.png.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csm_term::GridSize;
use csm_ui::terminal::fonts::{Coverage, FontSet, embedded_mono};
use csm_ui::terminal::{StaticTerminal, TerminalRenderer, TerminalView, ViewOptions};
use csm_ui::theme::{self, ThemeKind, Tokens};
use egui::{Align2, Id, Rect, pos2, vec2};
use egui_kittest::Harness;

const LINE: &str = "The quick brown fox jumps over the lazy dog; 0123456789 (){}[] ABCDEFGHIJKLMNOP";

struct State {
    source: StaticTerminal,
    view: TerminalView,
    renderer: TerminalRenderer,
    kind: ThemeKind,
    ready: bool,
}

/// Ink in the top half (egui) and the bottom half (terminal), and the image.
fn measure(kind: ThemeKind, coverage: Coverage, ppp: f32) -> (f64, f64) {
    let mut fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), theme::text::TERMINAL, false);
    fonts.coverage = coverage;
    // The cursor hidden: only glyphs count.
    let source = StaticTerminal::new(GridSize::new(80, 2), format!("\x1b[?25l{LINE}").as_bytes());
    let state = State { source, view: TerminalView::new(Id::new("tw")), renderer: TerminalRenderer::new(fonts), kind, ready: false };
    let mut h = Harness::builder().with_size(vec2(640.0, 80.0)).with_pixels_per_point(ppp).wgpu().build_ui_state(
        |ui, s: &mut State| {
            if !s.ready {
                csm_ui::app::setup(ui.ctx(), s.kind);
                s.ready = true;
                return;
            }
            let t = Tokens::get(ui.ctx());
            let style = t.terminal_style();
            let full = ui.ctx().content_rect();
            ui.painter().rect_filled(full, 0.0, t.term_bg);
            // egui's text at the terminal's padding and size.
            ui.painter().text(pos2(8.0, 6.0), Align2::LEFT_TOP, LINE, theme::mono(theme::text::TERMINAL), t.term_fg);
            let rect = Rect::from_min_max(pos2(0.0, 40.0), full.max);
            let opts = ViewOptions { interactive: false, style, ..ViewOptions::default() };
            s.view.show(ui, &s.source, &mut s.renderer, rect, &opts);
        },
        state,
    );
    for _ in 0..4 {
        h.step();
    }
    let img = h.render().unwrap();
    let name = format!("text-weight-{}-{}-{}.png", kind.id(), if coverage.hint { "hinted" } else { "unhinted" }, ppp);
    let out = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join(name);
    img.save(&out).unwrap();
    let t = Tokens::for_kind(kind);
    let lum = |p: [u8; 4]| 0.2126 * f64::from(p[0]) + 0.7152 * f64::from(p[1]) + 0.0722 * f64::from(p[2]);
    let bg = lum([t.term_bg.r(), t.term_bg.g(), t.term_bg.b(), 255]);
    let split = (40.0 * ppp) as u32;
    let (mut top, mut bottom) = (0.0, 0.0);
    for (_, y, p) in img.enumerate_pixels() {
        let d = (lum(p.0) - bg).abs();
        if y < split { top += d } else { bottom += d }
    }
    (top, bottom)
}

#[test]
fn terminal_text_weighs_what_egui_text_weighs() {
    for kind in ThemeKind::ALL {
        for ppp in [1.0, 1.5] {
            let (egui_ink, spike) = measure(kind, Coverage::SPIKE, ppp);
            let current = Coverage::for_dark_theme(kind == ThemeKind::Mocha);
            let (_, now) = measure(kind, current, ppp);
            let (r0, r1) = (spike / egui_ink, now / egui_ink);
            eprintln!("{kind:?} @{ppp}: ink vs egui text — spike (hinted, linear) {r0:.3}, now {current:?} {r1:.3}");
            assert!((r1 - 1.0).abs() <= 0.10, "{kind:?} @{ppp}: terminal ink is {r1:.3}× egui's");
        }
    }
}
