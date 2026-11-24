//! Is the terminal's `0` distinguishable from `8` at small sizes? Renders glyph masks with the
//! real rasteriser and compares them; writes a contact sheet to `$ZERO_OUT` when set.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use csm_ui::terminal::fonts::{Coverage, FontSet, Style, embedded_mono, face_from_bytes, load_face};
use image::{Rgba, RgbaImage};

const LINE: &str = "0808 8080 1010 O0o 8B3 0x80 2048 10.0.0.8";

fn faces() -> Vec<(String, csm_ui::terminal::fonts::Face)> {
    let mut v = vec![("JetBrainsMono".to_string(), embedded_mono().unwrap())];
    let fonts = std::path::PathBuf::from(std::env::var("WINDIR").unwrap_or_default()).join("Fonts");
    for n in ["CascadiaCode.ttf", "CascadiaMono.ttf", "consola.ttf"] {
        if let Ok(f) = load_face(&fonts.join(n), 0) {
            v.push((n.to_string(), f));
        }
    }
    v
}

/// Renders `text` into an RGBA image, one glyph per cell, light on dark.
fn render_line(fonts: &mut FontSet, text: &str, ppp: f32) -> RgbaImage {
    let m = fonts.metrics(ppp);
    let w = (m.cell_w as u32) * text.chars().count() as u32 + 4;
    let h = m.cell_h as u32 + 4;
    let mut img = RgbaImage::from_pixel(w, h, Rgba([30, 30, 46, 255]));
    for (i, c) in text.chars().enumerate() {
        let Some((face, gid)) = fonts.lookup(c) else { continue };
        let Some(g) = fonts.rasterize(face, gid, Style::default(), m.px) else { continue };
        let ox = 2 + (i as f32 * m.cell_w) as i32 + g.left;
        let oy = 2 + m.baseline as i32 - g.top;
        for y in 0..g.height {
            for x in 0..g.width {
                let a = u32::from(g.rgba[(y * g.width + x) * 4 + 3]);
                let (px, py) = (ox + x as i32, oy + y as i32);
                if px < 0 || py < 0 || px as u32 >= w || py as u32 >= h {
                    continue;
                }
                let p = img.get_pixel_mut(px as u32, py as u32);
                let fg = [205u32, 214, 244];
                for (k, f) in fg.iter().enumerate() {
                    p.0[k] = ((u32::from(p.0[k]) * (255 - a) + f * a) / 255) as u8;
                }
            }
        }
    }
    img
}

fn paste(sheet: &mut RgbaImage, img: &RgbaImage, x0: u32, y0: u32, scale: u32) {
    for y in 0..img.height() * scale {
        for x in 0..img.width() * scale {
            if x0 + x < sheet.width() && y0 + y < sheet.height() {
                sheet.put_pixel(x0 + x, y0 + y, *img.get_pixel(x / scale, y / scale));
            }
        }
    }
}

#[test]
fn contact_sheet() {
    let Ok(out) = std::env::var("ZERO_OUT") else { return };
    let hint = std::env::var("ZERO_HINT").is_ok();
    let plain = std::env::var("ZERO_PLAIN").is_ok();
    let mut sheet = RgbaImage::from_pixel(1700, 4200, Rgba([0, 0, 0, 255]));
    let mut y = 0;
    for (name, face) in faces() {
        let mut fs = FontSet::new(face, Vec::new(), 12.0, false);
        fs.slashed_zero = !plain;
        if hint {
            fs.coverage = Coverage { hint: true, ..fs.coverage };
        }
        eprintln!("{name}");
        for ppp in [1.0f32, 1.25] {
            for pt in [9.0f32, 10.0, 11.0, 12.0] {
                fs.size_pt = pt;
                let img = render_line(&mut fs, LINE, ppp);
                paste(&mut sheet, &img, 0, y, 4);
                y += img.height() * 4 + 4;
            }
        }
        y += 20;
    }
    image::imageops::crop_imm(&sheet, 0, 0, 1700, y.min(4200)).to_image().save(out).unwrap();
    let _ = face_from_bytes;
}

/// Cell-aligned coverage of `c` (the glyph placed in a cell-sized buffer).
fn cell_mask(fonts: &mut FontSet, c: char, ppp: f32) -> Vec<u8> {
    let m = fonts.metrics(ppp);
    let (w, h) = (m.cell_w as i32 + 4, m.cell_h as i32 + 4);
    let mut buf = vec![0u8; (w * h) as usize];
    let (face, gid) = fonts.lookup(c).unwrap();
    let g = fonts.rasterize(face, gid, Style::default(), m.px).unwrap();
    for y in 0..g.height as i32 {
        for x in 0..g.width as i32 {
            let (px, py) = (2 + g.left + x, 2 + m.baseline as i32 - g.top + y);
            if (0..w).contains(&px) && (0..h).contains(&py) {
                buf[(py * w + px) as usize] = g.rgba[(y as usize * g.width + x as usize) * 4 + 3];
            }
        }
    }
    buf
}

/// How different two masks are: summed absolute difference over summed ink of the heavier one.
fn distance(a: &[u8], b: &[u8]) -> f64 {
    let diff: u64 = a.iter().zip(b).map(|(x, y)| u64::from(x.abs_diff(*y))).sum();
    let ink = a.iter().map(|&x| u64::from(x)).sum::<u64>().max(b.iter().map(|&x| u64::from(x)).sum::<u64>());
    diff as f64 / ink.max(1) as f64
}

#[test]
fn default_zero_differs_from_eight_at_small_sizes() {
    for (ppp, pt) in [(1.0f32, 9.0f32), (1.0, 10.0), (1.0, 11.0), (1.0, 12.0), (1.25, 9.0), (1.25, 10.0), (1.25, 12.0)] {
        let mut dist = [0.0f64; 2];
        for (i, slashed) in [false, true].into_iter().enumerate() {
            let mut fs = FontSet::new(embedded_mono().unwrap(), Vec::new(), pt, false);
            fs.slashed_zero = slashed;
            dist[i] = distance(&cell_mask(&mut fs, '0', ppp), &cell_mask(&mut fs, '8', ppp));
        }
        eprintln!("{pt}pt@{ppp}: dotted {:.3} slashed {:.3}", dist[0], dist[1]);
        assert!(dist[1] > 0.30, "0 vs 8 only {:.3} apart at {pt}pt@{ppp}", dist[1]);
        assert!(dist[1] > dist[0] * 1.25, "slashed zero must be clearly further from 8 than the plain one");
    }
}
