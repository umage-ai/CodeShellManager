//! Builds one textured mesh for a pane from the emulator's grid: backgrounds, glyphs (with
//! ligature shaping and font fallback), box drawing, decorations, selection and cursor.
//! Called only when the pane is dirty; the result is cached as an `Arc<Mesh>`.

use egui::{Color32, Mesh, Pos2, Rect, Vec2};

use csm_term::Terminal;
use csm_term::alacritty_terminal::grid::Dimensions;
use csm_term::alacritty_terminal::term::cell::Flags;
use csm_term::alacritty_terminal::vte::ansi::{CursorShape, Rgb};

use crate::terminal::atlas::{GlyphAtlas, GlyphKey};
use crate::terminal::boxdraw;
use crate::terminal::fonts::{CellMetrics, FontSet, Style};
use csm_term::Palette;

/// How a terminal is coloured on screen: the app theme's terminal palette (so a theme switch
/// recolours running terminals; OSC colour overrides from the program still win) and the
/// selection colour.
#[derive(Debug, Clone, PartialEq)]
pub struct TermStyle {
    pub palette: Palette,
    pub selection: Color32,
}

impl Default for TermStyle {
    fn default() -> Self {
        let palette = Palette::default();
        let s = palette.ansi[8];
        Self { selection: Color32::from_rgb(s.r, s.g, s.b), palette }
    }
}

impl TermStyle {
    /// The terminal background as an egui colour (a pane's placeholder fills with it).
    pub fn background(&self) -> Color32 {
        c32(self.palette.background)
    }

    /// This style with a session's own colours (its look) over it; a colour the look leaves out
    /// stays the theme's.
    pub fn with_colors(mut self, c: &csm_core::look::ResolvedColors) -> Self {
        let rgb = |[r, g, b]: [u8; 3]| Rgb { r, g, b };
        for (slot, c) in self.palette.ansi.iter_mut().zip(c.ansi) {
            if let Some(c) = c {
                *slot = rgb(c);
            }
        }
        if let Some(v) = c.foreground {
            self.palette.foreground = rgb(v);
        }
        if let Some(v) = c.background {
            self.palette.background = rgb(v);
        }
        if let Some(v) = c.cursor {
            self.palette.cursor = rgb(v);
        }
        if let Some([r, g, b]) = c.selection {
            // WT draws its selection colour translucent over the text (the built-in schemes say
            // #FFFFFF); drawn opaque the selected text would vanish. 35% over the background.
            let bg = self.palette.background;
            let mix = |s: u8, b: u8| ((u16::from(s) * 35 + u16::from(b) * 65) / 100) as u8;
            self.selection = Color32::from_rgb(mix(r, bg.r), mix(g, bg.g), mix(b, bg.b));
        }
        self
    }
}

pub struct Built {
    pub mesh: Mesh,
    /// The cursor cell in points (for IME placement).
    pub cursor_rect: Rect,
}

#[derive(Clone)]
struct CellInfo {
    c: char,
    fg: Color32,
    bg: Color32,
    flags: Flags,
    zerowidth: Vec<char>,
    underline: Option<Color32>,
}

struct Painter<'a> {
    mesh: Mesh,
    origin: Pos2,
    ppp: f32,
    white: Rect,
    _p: std::marker::PhantomData<&'a ()>,
}

impl Painter<'_> {
    /// A quad in physical pixels relative to the pane origin.
    fn quad(&mut self, x: f32, y: f32, w: f32, h: f32, uv: Rect, color: Color32) {
        let min = self.origin + Vec2::new(x / self.ppp, y / self.ppp);
        let rect = Rect::from_min_size(min, Vec2::new(w / self.ppp, h / self.ppp));
        self.mesh.add_rect_with_uv(rect, uv, color);
    }

    fn fill(&mut self, x: f32, y: f32, w: f32, h: f32, color: Color32) {
        let uv = self.white;
        self.quad(x, y, w, h, uv, color);
    }
}

fn c32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}

/// Faint (SGR 2): the colour moved 40% of the way to the background, so it reads as quieter on
/// a light theme too (scaling toward black made light-theme text darker, not fainter).
fn dim(c: Color32, bg: Color32) -> Color32 {
    let mix = |a: u8, b: u8| ((u16::from(a) * 3 + u16::from(b) * 2) / 5) as u8;
    Color32::from_rgb(mix(c.r(), bg.r()), mix(c.g(), bg.g()), mix(c.b(), bg.b()))
}

#[allow(clippy::too_many_arguments)]
/// Builds the pane mesh. `origin` is the top-left of the cell grid in points, already snapped to
/// a physical pixel.
pub fn build(
    terminal: &Terminal,
    fonts: &mut FontSet,
    atlas: &mut GlyphAtlas,
    origin: Pos2,
    ppp: f32,
    m: CellMetrics,
    focused: bool,
    style: &TermStyle,
) -> Built {
    let term = terminal.term();
    let palette = &style.palette;
    let content = term.renderable_content();
    let colors = content.colors;
    let default_bg = c32(palette.slot(257, colors));
    let default_fg = c32(palette.slot(256, colors));
    let cursor_color = c32(palette.slot(258, colors));
    let display_offset = content.display_offset as i32;
    let cursor = content.cursor;
    let selection = content.selection;
    let rows = term.grid().screen_lines();
    let cols = term.grid().columns();

    let blank = CellInfo { c: ' ', fg: default_fg, bg: default_bg, flags: Flags::empty(), zerowidth: Vec::new(), underline: None };
    let mut grid: Vec<Vec<CellInfo>> = vec![vec![blank; cols]; rows];
    let cursor_vrow = cursor.point.line.0 + display_offset;
    let block_cursor = focused && cursor.shape == CursorShape::Block;

    for indexed in content.display_iter {
        let vrow = indexed.point.line.0 + display_offset;
        let Ok(vrow) = usize::try_from(vrow) else { continue };
        let col = indexed.point.column.0;
        let Some(slot) = grid.get_mut(vrow).and_then(|r| r.get_mut(col)) else { continue };
        let cell = indexed.cell;
        let mut fg = c32(palette.resolve(cell.fg, colors));
        let mut bg = c32(palette.resolve(cell.bg, colors));
        if cell.flags.contains(Flags::DIM) {
            fg = dim(fg, default_bg);
        }
        if cell.flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        if cell.flags.contains(Flags::HIDDEN) {
            fg = bg;
        }
        if selection.is_some_and(|s| s.contains_cell(&indexed, cursor.point, cursor.shape)) {
            bg = style.selection;
        }
        if block_cursor && indexed.point == cursor.point {
            bg = cursor_color;
            fg = default_bg;
        }
        *slot = CellInfo {
            c: cell.c,
            fg,
            bg,
            flags: cell.flags,
            zerowidth: cell.zerowidth().map(<[char]>::to_vec).unwrap_or_default(),
            underline: cell.underline_color().map(|c| c32(palette.resolve(c, colors))),
        };
    }

    let mut p = Painter { mesh: Mesh::with_texture(atlas.texture_id()), origin, ppp, white: atlas.white_uv(), _p: std::marker::PhantomData };
    let (cw, ch) = (m.cell_w, m.cell_h);
    let px_bits = m.px.to_bits();

    for (row, cells) in grid.iter().enumerate() {
        let y = row as f32 * ch;
        // Backgrounds, merged into runs.
        let mut col = 0;
        while col < cells.len() {
            let bg = cells.get(col).map_or(default_bg, |c| c.bg);
            let start = col;
            while col < cells.len() && cells.get(col).is_some_and(|c| c.bg == bg) {
                col += 1;
            }
            if bg != default_bg {
                p.fill(start as f32 * cw, y, (col - start) as f32 * cw, ch, bg);
            }
        }

        // Glyphs.
        let mut col = 0;
        while col < cells.len() {
            let Some(cell) = cells.get(col) else { break };
            let x = col as f32 * cw;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
                col += 1;
                continue;
            }
            let style = Style { bold: cell.flags.contains(Flags::BOLD), italic: cell.flags.contains(Flags::ITALIC) };
            if let Some(rects) = boxdraw::shapes(cell.c, cw, ch, m.stroke) {
                for (r, alpha) in rects {
                    let color = cell.fg.gamma_multiply(alpha);
                    p.fill(x + r.min.x, y + r.min.y, r.width(), r.height(), color);
                }
                col += 1;
                continue;
            }
            // Ligature runs: printable ASCII in one style, shaped together.
            if fonts.ligatures && cell.c.is_ascii_graphic() {
                let start = col;
                let mut run = String::new();
                while let Some(c) = cells.get(col) {
                    let same_style = c.flags.contains(Flags::BOLD) == style.bold && c.flags.contains(Flags::ITALIC) == style.italic;
                    if !(c.c == ' ' || c.c.is_ascii_graphic()) || !same_style || !c.zerowidth.is_empty() {
                        break;
                    }
                    run.push(c.c);
                    col += 1;
                }
                // Trailing spaces add nothing to shaping.
                let trimmed = run.trim_end_matches(' ');
                if trimmed.chars().count() > 1 {
                    let shaped = fonts.shape_run(trimmed, style, m.px);
                    for g in shaped.iter() {
                        let ci = start + usize::from(g.char_index);
                        let Some(src) = cells.get(ci) else { continue };
                        if src.c == ' ' || g.gid == 0 {
                            if g.gid == 0 {
                                draw_char(&mut p, fonts, atlas, src, ci as f32 * cw, y, style, m, px_bits);
                            }
                            continue;
                        }
                        let key = GlyphKey { face: 0, gid: g.gid, style, px_bits };
                        if let Some(glyph) = atlas.get(fonts, key) {
                            let gx = (ci as f32 * cw + g.x + glyph.left).round();
                            let gy = (y + m.baseline - glyph.top - g.y).round();
                            let color = if glyph.color { Color32::WHITE } else { src.fg };
                            p.quad(gx, gy, glyph.w, glyph.h, glyph.uv, color);
                        }
                    }
                    continue;
                }
                col = start;
            }
            if cell.c != ' ' {
                draw_char(&mut p, fonts, atlas, cell, x, y, style, m, px_bits);
            }
            for &z in &cell.zerowidth {
                let zc = CellInfo { c: z, zerowidth: Vec::new(), ..cell.clone() };
                draw_char(&mut p, fonts, atlas, &zc, x, y, style, m, px_bits);
            }
            col += 1;
        }

        // Decorations.
        for (col, cell) in cells.iter().enumerate() {
            let x = col as f32 * cw;
            let w = if cell.flags.contains(Flags::WIDE_CHAR) { cw * 2.0 } else { cw };
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                let color = cell.underline.unwrap_or(cell.fg);
                p.fill(x, y + m.underline, w, m.stroke, color);
                if cell.flags.contains(Flags::DOUBLE_UNDERLINE) {
                    p.fill(x, y + m.underline - 2.0 * m.stroke, w, m.stroke, color);
                }
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                p.fill(x, y + (ch / 2.0).round(), w, m.stroke, cell.fg);
            }
        }
    }

    // Non-block cursors (and the hollow block of an unfocused pane).
    let cursor_rect = {
        let x = cursor.point.column.0 as f32 * cw;
        let y = cursor_vrow as f32 * ch;
        Rect::from_min_size(origin + Vec2::new(x / ppp, y / ppp), Vec2::new(cw / ppp, ch / ppp))
    };
    if (0..rows as i32).contains(&cursor_vrow) {
        let x = cursor.point.column.0 as f32 * cw;
        let y = cursor_vrow as f32 * ch;
        let t = m.stroke.max(1.0);
        match cursor.shape {
            CursorShape::Block if focused => {}
            CursorShape::Block | CursorShape::HollowBlock => {
                p.fill(x, y, cw, t, cursor_color);
                p.fill(x, y + ch - t, cw, t, cursor_color);
                p.fill(x, y, t, ch, cursor_color);
                p.fill(x + cw - t, y, t, ch, cursor_color);
            }
            CursorShape::Beam => p.fill(x, y, (t * 2.0).max(2.0), ch, cursor_color),
            CursorShape::Underline => p.fill(x, y + ch - 2.0 * t, cw, 2.0 * t, cursor_color),
            CursorShape::Hidden => {}
        }
    }

    Built { mesh: p.mesh, cursor_rect }
}

#[allow(clippy::too_many_arguments)]
fn draw_char(
    p: &mut Painter<'_>,
    fonts: &mut FontSet,
    atlas: &mut GlyphAtlas,
    cell: &CellInfo,
    x: f32,
    y: f32,
    style: Style,
    m: CellMetrics,
    px_bits: u32,
) {
    let Some((face, gid)) = fonts.lookup(cell.c) else { return };
    let key = GlyphKey { face, gid, style, px_bits };
    let Some(g) = atlas.get(fonts, key) else { return };
    let span = if cell.flags.contains(Flags::WIDE_CHAR) { 2.0 * m.cell_w } else { m.cell_w };
    // Fallback glyphs (CJK, emoji) are centred in their span rather than left-aligned.
    let gx = if face == 0 { x + g.left } else { x + ((span - g.w) / 2.0).max(g.left.min(0.0)) };
    let gy = y + m.baseline - g.top;
    let color = if g.color { Color32::WHITE } else { cell.fg };
    p.quad(gx.round(), gy.round(), g.w, g.h, g.uv, color);
}
