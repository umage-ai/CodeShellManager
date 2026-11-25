//! Regressions for the UI hardening review: paste (SEC-28), mouse release latching (COR-12), the
//! glyph atlas overflow (COR-13), link clicks (COR-16), and malformed theme fonts (SEC-31).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::AtomicU32;
use std::sync::{Arc, Mutex};

use csm_term::alacritty_terminal::term::TermMode;
use csm_term::{GridSize, ReplySink, TermConfig, Terminal, load_mode};
use csm_ui::terminal::fonts::{FontSet, Style, embedded_mono, face_from_bytes, validate_face};
use csm_ui::terminal::view::{PASTE_MAX_BYTES, PasteRequest, plan_paste};
use csm_ui::terminal::{SourceState, TerminalRenderer, TerminalSource, TerminalView, ViewOptions};
use egui::{Event, Id, Modifiers, PointerButton, pos2, vec2};
use egui_kittest::Harness;

struct Discard;
impl ReplySink for Discard {
    fn send(&self, _: &[u8]) {}
}

/// A terminal source that records what the view sends it.
struct Rec {
    term: Arc<Mutex<Terminal>>,
    mode: Arc<AtomicU32>,
    mouse: Mutex<Vec<Vec<u8>>>,
    pasted: Mutex<Vec<String>>,
}

impl Rec {
    fn new(grid: GridSize, feed: &[u8]) -> Self {
        let mut t = Terminal::new(grid, TermConfig::default(), Arc::new(Discard));
        t.advance(feed);
        let mode = t.shared_mode();
        Self { term: Arc::new(Mutex::new(t)), mode, mouse: Mutex::new(Vec::new()), pasted: Mutex::new(Vec::new()) }
    }
}

impl TerminalSource for Rec {
    fn terminal(&self) -> Option<Arc<Mutex<Terminal>>> {
        Some(self.term.clone())
    }
    fn mode(&self) -> TermMode {
        load_mode(&self.mode)
    }
    fn set_grid(&self, grid: GridSize) {
        let mut t = self.term.lock().unwrap();
        if t.size() != grid {
            t.resize(grid);
        }
    }
    fn write(&self, _: &[u8]) {}
    fn send_mouse(&self, bytes: &[u8]) {
        self.mouse.lock().unwrap().push(bytes.to_vec());
    }
    fn paste(&self, text: &str) {
        self.pasted.lock().unwrap().push(text.to_owned());
    }
    fn state(&self) -> SourceState {
        SourceState::Running
    }
}

struct Ui {
    source: Rec,
    view: TerminalView,
    renderer: TerminalRenderer,
    rect: egui::Rect,
    open_links: Vec<String>,
    pastes: Vec<PasteRequest>,
}

fn ui_for(source: Rec, pt: f32, cols: f32, rows: f32) -> (Harness<'static, Ui>, f32, f32) {
    let mut renderer = TerminalRenderer::new(FontSet::new(embedded_mono().unwrap(), Vec::new(), pt, true));
    let m = renderer.fonts.metrics(1.0);
    let pad = ViewOptions::default().padding;
    let size = vec2((cols + 0.5) * m.cell_w + 2.0 * pad.x, (rows + 0.5) * m.cell_h + 2.0 * pad.y);
    let state = Ui {
        source,
        view: TerminalView::new(Id::new("harden")),
        renderer,
        rect: egui::Rect::from_min_size(egui::Pos2::ZERO, size),
        open_links: Vec::new(),
        pastes: Vec::new(),
    };
    let mut h = Harness::builder().with_size(size + vec2(10.0, 10.0)).with_pixels_per_point(1.0).build_ui_state(
        |ui, s: &mut Ui| {
            let opts = ViewOptions { request_focus: true, ..ViewOptions::default() };
            let rect = s.rect;
            let r = s.view.show(ui, &s.source, &mut s.renderer, rect, &opts);
            s.open_links.extend(r.open_link);
            s.pastes.extend(r.paste);
        },
        state,
    );
    h.step();
    h.step();
    (h, m.cell_w, m.cell_h)
}

fn small(feed: &[u8]) -> (Harness<'static, Ui>, f32, f32) {
    ui_for(Rec::new(GridSize::new(40, 6), feed), 13.0, 40.0, 6.0)
}

fn cell_pos(cw: f32, ch: f32, col: f32, row: f32) -> egui::Pos2 {
    let pad = ViewOptions::default().padding;
    pos2(pad.x + (col + 0.5) * cw, pad.y + (row + 0.5) * ch)
}

// ── SEC-28 ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn plan_paste_asks_for_several_lines_without_bracketed_paste() {
    let off = TermMode::default();
    let on = TermMode::BRACKETED_PASTE;
    // One line (a trailing newline is not a second line), or bracketed: sent at once.
    assert_eq!(plan_paste("ls -la".into(), off), Ok("ls -la".into()));
    assert_eq!(plan_paste("ls -la\n".into(), off), Ok("ls -la\n".into()));
    assert_eq!(plan_paste("a\nb\nc".into(), on), Ok("a\nb\nc".into()));
    // Several lines into a program that did not ask for bracketed paste: ask.
    for text in ["a\nb", "a\r\nb", "a\rb\r"] {
        let r = plan_paste(text.into(), off).unwrap_err();
        assert!(r.confirm && !r.truncated, "{text:?}");
        assert_eq!(r.text, text);
    }
}

#[test]
fn plan_paste_caps_the_size_on_a_char_boundary_and_says_so() {
    let big = "é".repeat(PASTE_MAX_BYTES); // 2 bytes each: twice the cap
    let r = plan_paste(big, TermMode::BRACKETED_PASTE).unwrap_err();
    assert!(r.truncated && !r.confirm);
    assert!(r.text.len() <= PASTE_MAX_BYTES && r.text.len() >= PASTE_MAX_BYTES - 1);
    assert!(r.text.chars().all(|c| c == 'é'));
    // A big paste that fits is still not cleaned in the frame.
    let r = plan_paste("x".repeat(1 << 20), TermMode::BRACKETED_PASTE).unwrap_err();
    assert!(!r.truncated && !r.confirm);
}

#[test]
fn the_view_holds_a_multi_line_paste_back_for_a_yes() {
    let (mut h, _, _) = small(b"$ ");
    h.event(Event::Paste("rm -rf a\nrm -rf b\n".into()));
    h.step();
    h.step();
    assert!(h.state().source.pasted.lock().unwrap().is_empty(), "nothing reached the program");
    let req = h.state().pastes.last().cloned().expect("the view asked");
    assert!(req.confirm);

    // With bracketed paste on, the same text goes straight through.
    let (mut h, _, _) = small(b"\x1b[?2004h$ ");
    h.event(Event::Paste("rm -rf a\nrm -rf b\n".into()));
    h.step();
    h.step();
    assert_eq!(h.state().source.pasted.lock().unwrap().len(), 1);
    assert!(h.state().pastes.is_empty());
}

// ── COR-12 ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn a_release_follows_its_press_even_if_shift_changed_mid_drag() {
    // Mouse reporting on (X10 click + SGR coordinates).
    let (mut h, cw, ch) = small(b"\x1b[?1000h\x1b[?1006h hello");
    let p = cell_pos(cw, ch, 3.0, 1.0);
    h.hover_at(p);
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    let q = cell_pos(cw, ch, 8.0, 1.0);
    h.hover_at(q);
    // Shift is down at the release: it still belongs to the program that got the press.
    h.event(Event::PointerButton { pos: q, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::SHIFT });
    h.step();
    let sent = h.state().source.mouse.lock().unwrap().clone();
    let texts: Vec<String> = sent.iter().map(|b| String::from_utf8_lossy(b).into_owned()).collect();
    assert!(texts.iter().any(|t| t.ends_with('M')), "press reported: {texts:?}");
    assert!(texts.iter().any(|t| t.ends_with('m')), "release reported: {texts:?}");
}

#[test]
fn a_shift_press_selects_locally_and_its_release_is_not_reported() {
    let (mut h, cw, ch) = small(b"\x1b[?1000h\x1b[?1006h hello");
    let p = cell_pos(cw, ch, 3.0, 1.0);
    h.hover_at(p);
    h.event(Event::PointerButton { pos: p, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::SHIFT });
    h.step();
    let q = cell_pos(cw, ch, 8.0, 1.0);
    h.hover_at(q);
    // Shift let go before the release: still a local selection, nothing for the program.
    h.event(Event::PointerButton { pos: q, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    let sent = h.state().source.mouse.lock().unwrap().clone();
    assert!(sent.iter().all(|b| !b.ends_with(b"m") && !b.ends_with(b"M")), "no click reports: {sent:?}");
}

// ── COR-16 ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn ctrl_click_opens_the_link_under_the_click_not_the_one_hovered_last_frame() {
    let (mut h, cw, ch) = small(b"see https://example.com/a here");
    let on_link = cell_pos(cw, ch, 10.0, 0.0);
    h.hover_at(on_link);
    h.step();
    h.step();
    // The screen changes without a frame in between: the hover cache still holds the link.
    h.state_mut().source.term.lock().unwrap().advance(b"\x1b[2J\x1b[Hnothing to click here");
    h.event(Event::PointerButton { pos: on_link, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::COMMAND });
    h.step();
    assert!(h.state().open_links.is_empty(), "opened a stale link: {:?}", h.state().open_links);

    // And a link that is there is opened.
    h.event(Event::PointerButton { pos: on_link, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::COMMAND });
    h.step();
    h.state_mut().source.term.lock().unwrap().advance(b"\x1b[2J\x1b[Hsee https://example.com/b here");
    h.event(Event::PointerButton { pos: on_link, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::COMMAND });
    h.step();
    assert_eq!(h.state().open_links, vec!["https://example.com/b".to_owned()]);
}

// ── COR-13 ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn an_overflowing_atlas_is_cleared_once_and_does_not_keep_rebuilding() {
    // More distinct glyphs, at a size where each is big, than one atlas holds.
    let mut fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), 110.0, false);
    let chars: Vec<char> = ('\u{20}'..='\u{2fff}').filter(|c| !c.is_control() && fonts.lookup(*c).is_some()).take(1800).collect();
    assert!(chars.len() >= 1200, "the embedded font has {} usable chars", chars.len());
    let (cols, rows) = (50usize, chars.len().div_ceil(50));
    let mut feed = String::new();
    for row in chars.chunks(cols) {
        feed.extend(row);
        feed.push_str("\r\n");
    }
    let source = Rec::new(GridSize::new(cols as u16, rows as u16), feed.as_bytes());
    let (mut h, _, _) = ui_for(source, 110.0, cols as f32, rows as f32);
    for _ in 0..4 {
        h.step();
    }
    let epoch = |h: &Harness<'static, Ui>| h.state().renderer.atlas.as_ref().map(|a| (a.epoch, a.uploads));
    let first = epoch(&h).unwrap();
    assert!(first.0 >= 1, "the test text should overflow the atlas: {first:?}");
    for _ in 0..4 {
        h.step();
    }
    assert_eq!(epoch(&h).unwrap(), first, "no further clears or uploads once settled");
    assert!(!h.state().renderer.atlas.as_ref().unwrap().full);
}

#[test]
fn repeated_overflows_are_throttled() {
    let ctx = egui::Context::default();
    let mut atlas = csm_ui::terminal::atlas::GlyphAtlas::new(&ctx);
    let t0 = std::time::Instant::now();
    assert_eq!(atlas.overflow_repaint_delay(t0), std::time::Duration::ZERO);
    assert!(atlas.overflow_repaint_delay(t0 + std::time::Duration::from_millis(10)) >= std::time::Duration::from_millis(100));
    assert_eq!(atlas.overflow_repaint_delay(t0 + std::time::Duration::from_secs(30)), std::time::Duration::ZERO);
}

// ── SEC-31 ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn the_embedded_font_passes_validation_and_garbage_does_not() {
    let face = embedded_mono().unwrap();
    assert!(validate_face(face).is_ok());
    assert!(face_from_bytes(vec![0u8; 64], 0, "zeros".into()).is_none());
}

/// Corrupts bytes of a real font and runs the whole check: it must come back (Ok or Err) and
/// never take the process down, which is what a panic in the rasteriser did.
#[test]
fn corrupted_fonts_never_panic_the_validator_or_the_runtime_rasteriser() {
    let original = csm_ui::terminal::fonts::JETBRAINS_MONO;
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let (mut rejected, mut accepted) = (0, 0);
    for round in 0..6 {
        let mut bytes = original.to_vec();
        // Hit the table directory and the body; more bytes each round.
        for _ in 0..(40 << round) {
            let at = (next() as usize) % bytes.len();
            bytes[at] = next() as u8;
        }
        let Some(face) = face_from_bytes(bytes.clone(), 0, "fuzz".into()) else { continue };
        match validate_face(face) {
            Ok(_) => accepted += 1,
            Err(_) => rejected += 1,
        }
        // The runtime path survives the same bytes, whether or not the validator liked them.
        if let Some(face) = face_from_bytes(bytes, 0, "fuzz".into()) {
            let mut fonts = FontSet::new(face, Vec::new(), 13.0, true);
            for gid in (0..2000u16).step_by(7) {
                let _ = fonts.rasterize(0, gid, Style::default(), 20.0);
            }
            let _ = fonts.shape_run("fn main() { => }", Style::default(), 20.0);
        }
    }
    eprintln!("fuzzed fonts: {accepted} accepted, {rejected} rejected");
}
