//! Renders a pane offscreen with the real wgpu renderer (kittest) and writes a PNG for a human
//! to look at: true colour, box drawing, wide chars, emoji, ligatures, attributes, cursor.
//! Output: $CSM_RENDER_OUT or <target>/term-spike-render.png.
#![cfg(windows)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};

use csm_term::{GridSize, ReplySink, TermConfig, Terminal};
use egui::vec2;
use egui_kittest::Harness;
use term_spike::fonts::{FontSet, default_paths, load_face};
use term_spike::pane::{Pane, Placement};
use term_spike::view::{FrameCounters, Shared, pane_ui};

struct NoReplies;
impl ReplySink for NoReplies {
    fn send(&self, _: &[u8]) {}
}

struct State {
    pane: Pane,
    shared: Shared,
}

const CONTENT: &str = concat!(
    "\x1b[1;38;2;137;180;250mCodeShellManager term-spike\x1b[0m  \x1b[2m(offscreen render)\x1b[0m\r\n",
    "ligatures: -> => != === <= >= :: |> <!-- www &&\r\n",
    "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[9mstrike\x1b[0m \x1b[7minverse\x1b[0m \x1b[2mdim\x1b[0m\r\n",
    "╭──────────┬──────────╮ ┏━━━━┓ ╔════╗ ▀▄█▌▐ ░▒▓\r\n",
    "│ box      │ drawing  │ ┃ hv ┃ ║ dd ║ ▖▗▘▙▚▛\r\n",
    "├──────────┼──────────┤ ┗━━━━┛ ╚════╝\r\n",
    "╰──────────┴──────────╯\r\n",
    "wide: 日本語 한국어 中文 | emoji: 🚀✅🦀🎉 | ascii after wide ok\r\n",
    "\x1b[31mred\x1b[32m green\x1b[33m yellow\x1b[34m blue\x1b[35m magenta\x1b[36m cyan\x1b[0m \x1b[48;5;208m 256 \x1b[0m\r\n",
);

#[test]
fn render_pane_offscreen() {
    let (primary, fallbacks) = default_paths();
    let face = primary.iter().find_map(|p| load_face(p, 0).ok()).expect("monospace font");
    let fallbacks = fallbacks.iter().filter_map(|(p, i)| load_face(p, *i).ok()).collect();
    let fonts = FontSet::new(face, fallbacks, 13.0, true);

    let mut pane = Pane::new(0, "offscreen".into(), Vec::new(), None, Placement::Grid);
    let grid = GridSize::new(70, 14);
    let mut term = Terminal::new(grid, TermConfig::default(), Arc::new(NoReplies));
    term.advance(CONTENT.as_bytes());
    for i in 0..64u32 {
        let t = i * 4;
        term.advance(format!("\x1b[48;2;{};{};{}m ", t, 255 - t, 128).as_bytes());
    }
    term.advance(b"\x1b[0m\r\n$ ");
    pane.mode = Some(term.shared_mode());
    pane.term = Some(Arc::new(Mutex::new(term)));
    // Pretend the size was measured already so the pane does not try to spawn.
    pane.grid = Some(grid);

    let state = State { pane, shared: Shared { fonts, atlas: None, frame: FrameCounters::default() } };
    let mut h = Harness::builder()
        .with_size(vec2(760.0, 330.0))
        .with_pixels_per_point(1.5)
        .wgpu()
        .build_ui_state(
            |ui, s: &mut State| {
                let rect = ui.max_rect();
                pane_ui(ui, &mut s.pane, &mut s.shared, rect);
            },
            state,
        );
    h.step();
    h.step();
    let img = h.render().expect("render");
    let out = std::env::var_os("CSM_RENDER_OUT").map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("term-spike-render.png")
    });
    img.save(&out).expect("save png");
    eprintln!("wrote {}", out.display());
    // Not blank: many distinct colours (text, gradient, box lines).
    let distinct: std::collections::HashSet<[u8; 4]> = img.pixels().map(|p| p.0).collect();
    assert!(distinct.len() > 200, "render looks blank ({} colours)", distinct.len());
    assert!(h.state().shared.atlas.as_ref().is_some_and(|a| a.uploads > 50));
}
