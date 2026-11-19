//! Glyph atlas: one RGBA texture, shelf-packed, glyphs uploaded with partial texture updates.
//! A 2×2 white block at the origin serves solid rectangles from the same texture, so a whole
//! pane is ONE mesh with one texture.

use std::collections::HashMap;

use egui::{Color32, ColorImage, Context, Pos2, Rect, TextureHandle, TextureOptions};

use crate::terminal::fonts::{FontSet, Style};

pub const ATLAS_SIZE: usize = 2048;
const PAD: usize = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub face: u8,
    pub gid: u16,
    pub style: Style,
    pub px_bits: u32,
}

#[derive(Debug, Clone, Copy)]
pub struct AtlasGlyph {
    pub uv: Rect,
    pub left: f32,
    pub top: f32,
    pub w: f32,
    pub h: f32,
    pub color: bool,
}

struct Shelf {
    y: usize,
    h: usize,
    x: usize,
}

pub struct GlyphAtlas {
    tex: TextureHandle,
    shelves: Vec<Shelf>,
    next_y: usize,
    map: HashMap<GlyphKey, Option<AtlasGlyph>>,
    /// Bumped when the atlas is cleared; cached meshes built against an older epoch are stale.
    pub epoch: u64,
    /// Set when an insert failed for lack of space; the owner clears and rebuilds.
    pub full: bool,
    pub uploads: u64,
}

impl GlyphAtlas {
    pub fn new(ctx: &Context) -> Self {
        let mut img = ColorImage::filled([ATLAS_SIZE, ATLAS_SIZE], Color32::TRANSPARENT);
        for y in 0..2 {
            for x in 0..2 {
                img[(x, y)] = Color32::WHITE;
            }
        }
        let tex = ctx.load_texture("csm-glyph-atlas", img, TextureOptions::NEAREST);
        Self { tex, shelves: Vec::new(), next_y: 2 + PAD, map: HashMap::new(), epoch: 0, full: false, uploads: 0 }
    }

    pub fn texture_id(&self) -> egui::TextureId {
        self.tex.id()
    }

    /// UV of the white texel block (for solid quads).
    pub fn white_uv(&self) -> Rect {
        let c = 1.0 / ATLAS_SIZE as f32;
        Rect::from_min_max(Pos2::new(0.5 * c, 0.5 * c), Pos2::new(1.5 * c, 1.5 * c))
    }

    pub fn clear(&mut self) {
        self.shelves.clear();
        self.next_y = 2 + PAD;
        self.map.clear();
        self.epoch += 1;
        self.full = false;
    }

    fn alloc(&mut self, w: usize, h: usize) -> Option<(usize, usize)> {
        let (w, h) = (w + PAD, h + PAD);
        for s in &mut self.shelves {
            if h <= s.h && s.h <= h + h / 3 + 2 && s.x + w <= ATLAS_SIZE {
                let pos = (s.x, s.y);
                s.x += w;
                return Some(pos);
            }
        }
        if self.next_y + h > ATLAS_SIZE || w > ATLAS_SIZE {
            return None;
        }
        let shelf = Shelf { y: self.next_y, h, x: w };
        self.next_y += h;
        let pos = (0, shelf.y);
        self.shelves.push(shelf);
        Some(pos)
    }

    /// Returns the glyph, rasterizing and uploading it on first use. `None` for blank glyphs.
    pub fn get(&mut self, fonts: &mut FontSet, key: GlyphKey) -> Option<AtlasGlyph> {
        if let Some(hit) = self.map.get(&key) {
            return *hit;
        }
        let Some(raster) = fonts.rasterize(key.face, key.gid, key.style, f32::from_bits(key.px_bits)) else {
            self.map.insert(key, None);
            return None;
        };
        let Some((x, y)) = self.alloc(raster.width, raster.height) else {
            self.full = true;
            return None;
        };
        let image = ColorImage::from_rgba_premultiplied([raster.width, raster.height], &raster.rgba);
        self.tex.set_partial([x, y], image, TextureOptions::NEAREST);
        self.uploads += 1;
        let s = ATLAS_SIZE as f32;
        let uv = Rect::from_min_max(Pos2::new(x as f32 / s, y as f32 / s), Pos2::new((x + raster.width) as f32 / s, (y + raster.height) as f32 / s));
        let g =
            AtlasGlyph { uv, left: raster.left as f32, top: raster.top as f32, w: raster.width as f32, h: raster.height as f32, color: raster.color };
        self.map.insert(key, Some(g));
        Some(g)
    }
}
