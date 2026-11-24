//! Fonts: loading (startup only), cell metrics, glyph lookup with fallback, ligature shaping
//! and rasterization, all with swash (pure Rust).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use swash::scale::image::Content;
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::shape::ShapeContext;
use swash::zeno::{Angle, Format, Transform};
use swash::{CacheKey, FontRef};

pub struct Face {
    data: Arc<Vec<u8>>,
    offset: u32,
    key: CacheKey,
    pub name: String,
    has_wght: bool,
}

impl Face {
    fn font(&self) -> FontRef<'_> {
        FontRef { data: &self.data, offset: self.offset, key: self.key }
    }
}

/// Reads a font file. Disk I/O: call before the UI starts or off the UI thread.
pub fn load_face(path: &Path, index: usize) -> Result<Face, String> {
    let data = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    face_from_bytes(data, index, name).ok_or_else(|| format!("{}: not a font", path.display()))
}

/// A face from font bytes already in memory (an embedded font, or a file read earlier).
pub fn face_from_bytes(data: Vec<u8>, index: usize, name: String) -> Option<Face> {
    let font = FontRef::from_index(&data, index)?;
    let (offset, key) = (font.offset, font.key);
    let has_wght = font.variations().any(|v| v.tag() == swash::tag_from_bytes(b"wght"));
    Some(Face { data: Arc::new(data), offset, key, name, has_wght })
}

/// JetBrains Mono, embedded (OFL, `assets/fonts`): the terminal font of the design, and a primary
/// face that needs no disk access, so tests and first frames never wait on I/O.
pub static JETBRAINS_MONO: &[u8] = include_bytes!("../../assets/fonts/JetBrainsMono-Regular.ttf");

/// The embedded JetBrains Mono face. `None` only if the embedded bytes were corrupted.
pub fn embedded_mono() -> Option<Face> {
    face_from_bytes(JETBRAINS_MONO.to_vec(), 0, "JetBrainsMono-Regular.ttf".to_string())
}

/// Windows system fonts: monospace primaries (first that loads wins) and the fallback stack for
/// emoji, symbols and CJK. Paths only; loading them is disk I/O.
pub fn default_paths() -> (Vec<PathBuf>, Vec<(PathBuf, usize)>) {
    let fonts = std::env::var_os("WINDIR").map(|w| PathBuf::from(w).join("Fonts")).unwrap_or_default();
    let primary = vec![fonts.join("CascadiaCode.ttf"), fonts.join("CascadiaMono.ttf"), fonts.join("consola.ttf")];
    let fallbacks = vec![
        (fonts.join("seguiemj.ttf"), 0),
        (fonts.join("seguisym.ttf"), 0),
        (fonts.join("msyh.ttc"), 0),
        (fonts.join("YuGothM.ttc"), 0),
        (fonts.join("malgun.ttf"), 0),
    ];
    (primary, fallbacks)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CellMetrics {
    /// Font size in physical pixels.
    pub px: f32,
    pub cell_w: f32,
    pub cell_h: f32,
    /// Baseline offset from the cell top.
    pub baseline: f32,
    pub underline: f32,
    pub stroke: f32,
}

#[derive(Debug, Clone, Copy)]
pub struct Shaped {
    pub char_index: u16,
    pub gid: u16,
    pub x: f32,
    pub y: f32,
}

pub struct RasterGlyph {
    pub left: i32,
    pub top: i32,
    pub width: usize,
    pub height: usize,
    /// Premultiplied RGBA.
    pub rgba: Vec<u8>,
    pub color: bool,
}

pub struct FontSet {
    primary: Face,
    /// The face the app started with, while a theme's own is the primary.
    original: Option<Face>,
    fallbacks: Vec<Face>,
    pub size_pt: f32,
    pub ligatures: bool,
    /// How glyph coverage becomes alpha (hinting and the coverage curve). Changing it after glyphs
    /// were rasterised needs an atlas clear.
    pub coverage: Coverage,
    lookup: HashMap<char, Option<(u8, u16)>>,
    shape_ctx: ShapeContext,
    scale_ctx: ScaleContext,
    shape_cache: HashMap<u64, Arc<Vec<Shaped>>>,
    metrics: HashMap<u32, CellMetrics>,
    pub shaped_runs: u64,
}

const WGHT_BOLD: [(&[u8; 4], f32); 1] = [(b"wght", 700.0)];
const SHAPE_CACHE_MAX: usize = 50_000;

impl FontSet {
    pub fn new(primary: Face, fallbacks: Vec<Face>, size_pt: f32, ligatures: bool) -> Self {
        Self {
            primary,
            original: None,
            fallbacks,
            size_pt,
            ligatures,
            coverage: Coverage::default(),
            lookup: HashMap::new(),
            shape_ctx: ShapeContext::new(),
            scale_ctx: ScaleContext::new(),
            shape_cache: HashMap::new(),
            metrics: HashMap::new(),
            shaped_runs: 0,
        }
    }

    /// Makes a theme's face the primary one (`Some`), or brings back the one the app started
    /// with (`None`). Everything keyed by the old face is dropped; the caller clears the atlas.
    pub fn use_theme_face(&mut self, face: Option<Face>) {
        match face {
            Some(f) => {
                let old = std::mem::replace(&mut self.primary, f);
                self.original.get_or_insert(old);
            }
            None => match self.original.take() {
                Some(o) => self.primary = o,
                None => return,
            },
        }
        self.lookup.clear();
        self.shape_cache.clear();
        self.metrics.clear();
    }

    pub fn describe(&self) -> String {
        let fb: Vec<&str> = self.fallbacks.iter().map(|f| f.name.as_str()).collect();
        format!("{} {}pt (fallbacks: {})", self.primary.name, self.size_pt, fb.join(", "))
    }

    fn face(&self, idx: u8) -> &Face {
        if idx == 0 { &self.primary } else { self.fallbacks.get(usize::from(idx) - 1).unwrap_or(&self.primary) }
    }

    /// Cell metrics at a display scale (pixels per point). Cell sizes are whole pixels so the grid
    /// never drifts.
    pub fn metrics(&mut self, ppp: f32) -> CellMetrics {
        let px = (self.size_pt * ppp).max(4.0);
        if let Some(m) = self.metrics.get(&px.to_bits()) {
            return *m;
        }
        let font = self.primary.font();
        let m = font.metrics(&[]).scale(px);
        let gid = font.charmap().map('0');
        let adv = font.glyph_metrics(&[]).scale(px).advance_width(gid);
        let cell_w = adv.round().max(1.0);
        let cell_h = (m.ascent + m.descent + m.leading).ceil().max(1.0);
        let baseline = (m.ascent + m.leading / 2.0).round();
        let stroke = m.stroke_size.round().max(1.0);
        let underline = (baseline - m.underline_offset).round().min(cell_h - stroke);
        let cm = CellMetrics { px, cell_w, cell_h, baseline, underline, stroke };
        self.metrics.insert(px.to_bits(), cm);
        cm
    }

    /// Which face draws `c`, and its glyph id there. Primary first, then fallbacks in order.
    pub fn lookup(&mut self, c: char) -> Option<(u8, u16)> {
        if let Some(hit) = self.lookup.get(&c) {
            return *hit;
        }
        let mut found = None;
        let gid = self.primary.font().charmap().map(c);
        if gid != 0 {
            found = Some((0u8, gid));
        } else {
            for (i, f) in self.fallbacks.iter().enumerate() {
                let gid = f.font().charmap().map(c);
                if gid != 0 {
                    found = Some((u8::try_from(i + 1).unwrap_or(0), gid));
                    break;
                }
            }
        }
        self.lookup.insert(c, found);
        found
    }

    /// Shapes a run of primary-font text with ligatures (`calt`, `liga`). One entry per output
    /// glyph, positioned relative to the run start; `char_index` is the source character.
    pub fn shape_run(&mut self, text: &str, style: Style, px: f32) -> Arc<Vec<Shaped>> {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (text, style, px.to_bits()).hash(&mut h);
        let key = h.finish();
        if let Some(hit) = self.shape_cache.get(&key) {
            return hit.clone();
        }
        if self.shape_cache.len() > SHAPE_CACHE_MAX {
            self.shape_cache.clear();
        }
        let mut char_at_byte = vec![0u16; text.len() + 1];
        for (ci, (bi, _)) in text.char_indices().enumerate() {
            if let Some(slot) = char_at_byte.get_mut(bi) {
                *slot = u16::try_from(ci).unwrap_or(u16::MAX);
            }
        }
        let primary = &self.primary;
        let builder = self.shape_ctx.builder(primary.font()).size(px).features(&[(b"calt", 1u16), (b"liga", 1u16)]);
        let builder = if style.bold && primary.has_wght { builder.variations(&WGHT_BOLD) } else { builder };
        let mut shaper = builder.build();
        shaper.add_str(text);
        let mut out = Vec::new();
        let mut pen = 0.0f32;
        let mut last_cluster = usize::MAX;
        let mut cluster_pen = 0.0f32;
        shaper.shape_with(|cluster| {
            let start = cluster.source.start as usize;
            if start != last_cluster {
                last_cluster = start;
                cluster_pen = 0.0;
            }
            let char_index = char_at_byte.get(start).copied().unwrap_or(0);
            for g in cluster.glyphs {
                out.push(Shaped { char_index, gid: g.id, x: cluster_pen + g.x, y: g.y });
                cluster_pen += g.advance;
                pen += g.advance;
            }
        });
        self.shaped_runs += 1;
        let out = Arc::new(out);
        self.shape_cache.insert(key, out.clone());
        out
    }

    /// Renders one glyph. CPU only, no I/O.
    pub fn rasterize(&mut self, face_idx: u8, gid: u16, style: Style, px: f32) -> Option<RasterGlyph> {
        let face = if face_idx == 0 { &self.primary } else { self.fallbacks.get(usize::from(face_idx) - 1)? };
        let builder = self.scale_ctx.builder(face.font()).size(px).hint(self.coverage.hint);
        let real_bold = style.bold && face.has_wght;
        let builder = if real_bold { builder.variations(&WGHT_BOLD) } else { builder };
        let mut scaler = builder.build();
        let mut render = Render::new(&[Source::ColorOutline(0), Source::ColorBitmap(StrikeWith::BestFit), Source::Outline]);
        render.format(Format::Alpha);
        if style.italic {
            render.transform(Some(Transform::skew(Angle::from_degrees(12.0), Angle::ZERO)));
        }
        let img = render.render(&mut scaler, gid)?;
        let (mut w, h) = (img.placement.width as usize, img.placement.height as usize);
        if w == 0 || h == 0 {
            return None;
        }
        let (rgba, color) = match img.content {
            Content::Mask => {
                // A face without a weight axis gets a synthetic bold: the mask smeared right by
                // a pixel or two (swash's outline embolden distorts thin glyphs such as `i`).
                let smear = if style.bold && !real_bold { ((px / 20.0).round() as usize).max(1) } else { 0 };
                let src: Vec<u8> = img.data.iter().take(w * h).copied().collect();
                if src.len() != w * h {
                    return None;
                }
                let ow = w + smear;
                let mut v = Vec::with_capacity(ow * h * 4);
                for y in 0..h {
                    for x in 0..ow {
                        let a = (0..=smear)
                            .filter_map(|d| x.checked_sub(d).filter(|&sx| sx < w).and_then(|sx| src.get(y * w + sx)))
                            .copied()
                            .max()
                            .unwrap_or(0);
                        let a = self.coverage.apply(a);
                        v.extend_from_slice(&[a, a, a, a]);
                    }
                }
                w = ow;
                (v, false)
            }
            Content::Color => {
                let mut v = Vec::with_capacity(w * h * 4);
                for px in img.data.as_chunks::<4>().0.iter().take(w * h) {
                    let c = egui::Color32::from_rgba_unmultiplied(px[0], px[1], px[2], px[3]);
                    v.extend_from_slice(&c.to_array());
                }
                (v, true)
            }
            Content::SubpixelMask => return None,
        };
        if rgba.len() != w * h * 4 {
            return None;
        }
        Some(RasterGlyph { left: img.placement.left, top: img.placement.top, width: w, height: h, rgba, color })
    }

    pub fn face_name(&self, idx: u8) -> &str {
        &self.face(idx).name
    }
}

#[cfg(all(test, windows))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn coverage(fonts: &mut FontSet, bold: bool) -> u64 {
        let (face, gid) = fonts.lookup('m').unwrap();
        let g = fonts.rasterize(face, gid, Style { bold, italic: false }, 26.0).unwrap();
        g.rgba.as_chunks::<4>().0.iter().map(|p| u64::from(p[3])).sum()
    }

    #[test]
    fn bold_is_heavier_than_regular() {
        let (primary, _) = default_paths();
        let face = primary.iter().find_map(|p| load_face(p, 0).ok()).unwrap();
        let name = face.name.clone();
        let mut fonts = FontSet::new(face, Vec::new(), 13.0, true);
        let (r, b) = (coverage(&mut fonts, false), coverage(&mut fonts, true));
        eprintln!("{name}: regular {r} bold {b}");
        assert!(b as f64 > r as f64 * 1.15, "bold {b} not heavier than regular {r}");
    }
}

/// How a glyph's coverage becomes the alpha it is drawn with.
///
/// The renderer blends in gamma space, as egui does: alpha `a` puts `bg + a·(fg − bg)` on screen.
/// `hint` snaps outlines to the pixel grid (it thickens stems at small sizes); `curve` reshapes
/// coverage before blending.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub hint: bool,
    pub curve: Curve,
}

/// Coverage → alpha, as egui's `FontColorTransferFunction` does for its own text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Curve {
    /// `a = c`: dark text on a light background.
    Linear,
    /// `a = 2c − c²`: light text on a dark background (egui's dark-mode default).
    Lifted,
}

impl Default for Coverage {
    fn default() -> Self {
        Coverage::for_dark_theme(true)
    }
}

impl Coverage {
    /// The spike's setting: hinted, linear.
    pub const SPIKE: Coverage = Coverage { hint: true, curve: Curve::Linear };

    /// Measured against egui's own text in the same font, size and colours (the `text_weight`
    /// test): unhinted, with egui's curve for the theme, the terminal's ink matches the UI's text.
    pub fn for_dark_theme(dark: bool) -> Coverage {
        Coverage { hint: false, curve: if dark { Curve::Lifted } else { Curve::Linear } }
    }

    pub fn apply(self, a: u8) -> u8 {
        match self.curve {
            Curve::Linear => a,
            Curve::Lifted => {
                let c = f32::from(a) / 255.0;
                ((2.0 * c - c * c) * 255.0).round().clamp(0.0, 255.0) as u8
            }
        }
    }
}
