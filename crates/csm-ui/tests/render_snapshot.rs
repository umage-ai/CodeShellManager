//! Renders a terminal offscreen with the real wgpu renderer (kittest) and writes a PNG for a
//! human to look at: true colour, box drawing, wide chars, emoji, ligatures, attributes, cursor.
//! Output: $CSM_RENDER_OUT or <target>/csm-ui-terminal-render.png.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csm_term::GridSize;
use csm_ui::terminal::fonts::{FontSet, default_paths, embedded_mono, load_face};
use csm_ui::terminal::{StaticTerminal, TerminalRenderer, TerminalView, ViewOptions};
use egui::{Id, vec2};
use egui_kittest::Harness;

pub const CONTENT: &str = concat!(
    "\x1b[1;38;2;137;180;250mCodeShellManager terminal view\x1b[0m  \x1b[2m(offscreen render)\x1b[0m\r\n",
    "ligatures: -> => != === <= >= :: |> <!-- www &&\r\n",
    "\x1b[1mbold\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m \x1b[9mstrike\x1b[0m \x1b[7minverse\x1b[0m \x1b[2mdim\x1b[0m\r\n",
    "╭──────────┬──────────╮ ┏━━━━┓ ╔════╗ ▀▄█▌▐ ░▒▓\r\n",
    "│ box      │ drawing  │ ┃ hv ┃ ║ dd ║ ▖▗▘▙▚▛\r\n",
    "├──────────┼──────────┤ ┗━━━━┛ ╚════╝\r\n",
    "╰──────────┴──────────╯\r\n",
    "wide: 日本語 한국어 中文 | emoji: 🚀✅🦀🎉 | ascii after wide ok\r\n",
    "\x1b[31mred\x1b[32m green\x1b[33m yellow\x1b[34m blue\x1b[35m magenta\x1b[36m cyan\x1b[0m \x1b[48;5;208m 256 \x1b[0m\r\n",
);

struct State {
    source: StaticTerminal,
    view: TerminalView,
    renderer: TerminalRenderer,
}

#[test]
fn render_terminal_offscreen() {
    let face = embedded_mono().expect("embedded JetBrains Mono");
    // System fallbacks (emoji, CJK) when they exist; the test still runs without them.
    let (_, fallbacks) = default_paths();
    let fallbacks = fallbacks.iter().filter_map(|(p, i)| load_face(p, *i).ok()).collect();
    let fonts = FontSet::new(face, fallbacks, 13.0, true);

    let mut bytes = CONTENT.as_bytes().to_vec();
    for i in 0..64u32 {
        let t = i * 4;
        bytes.extend(format!("\x1b[48;2;{};{};{}m ", t, 255 - t, 128).as_bytes());
    }
    bytes.extend(b"\x1b[0m\r\n$ ");
    let source = StaticTerminal::new(GridSize::new(70, 14), &bytes);
    let state = State { source, view: TerminalView::new(Id::new("offscreen")), renderer: TerminalRenderer::new(fonts) };
    let mut h = Harness::builder().with_size(vec2(760.0, 330.0)).with_pixels_per_point(1.5).wgpu().build_ui_state(
        |ui, s: &mut State| {
            let rect = ui.max_rect();
            s.view.show(ui, &s.source, &mut s.renderer, rect, &ViewOptions::default());
        },
        state,
    );
    h.step();
    h.step();
    let img = h.render().expect("render");
    let out = std::env::var_os("CSM_RENDER_OUT").map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("csm-ui-terminal-render.png")
    });
    img.save(&out).expect("save png");
    eprintln!("wrote {}", out.display());
    // Not blank: many distinct colours (text, gradient, box lines).
    let distinct: std::collections::HashSet<[u8; 4]> = img.pixels().map(|p| p.0).collect();
    assert!(distinct.len() > 200, "render looks blank ({} colours)", distinct.len());
    assert!(h.state().renderer.atlas.as_ref().is_some_and(|a| a.uploads > 50));
}
