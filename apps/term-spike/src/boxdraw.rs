//! Box-drawing (U+2500–U+257F) and block elements (U+2580–U+259F) drawn as rectangles, so lines
//! join across cells exactly whatever the font's line height. Rounded corners and dashes are
//! approximated by square corners and solid lines.

use egui::{Pos2, Rect};

/// Arm weights (up, right, down, left): 0 none, 1 light, 2 heavy, 3 double.
fn arms(c: char) -> Option<[u8; 4]> {
    const T: [[u8; 4]; 0x80] = {
        let mut t = [[0u8; 4]; 0x80];
        let h1 = [0, 1, 0, 1];
        let h2 = [0, 2, 0, 2];
        let v1 = [1, 0, 1, 0];
        let v2 = [2, 0, 2, 0];
        t[0x00] = h1; t[0x01] = h2; t[0x02] = v1; t[0x03] = v2;
        t[0x04] = h1; t[0x05] = h2; t[0x06] = v1; t[0x07] = v2;
        t[0x08] = h1; t[0x09] = h2; t[0x0A] = v1; t[0x0B] = v2;
        t[0x0C] = [0, 1, 1, 0]; t[0x0D] = [0, 2, 1, 0]; t[0x0E] = [0, 1, 2, 0]; t[0x0F] = [0, 2, 2, 0];
        t[0x10] = [0, 0, 1, 1]; t[0x11] = [0, 0, 1, 2]; t[0x12] = [0, 0, 2, 1]; t[0x13] = [0, 0, 2, 2];
        t[0x14] = [1, 1, 0, 0]; t[0x15] = [1, 2, 0, 0]; t[0x16] = [2, 1, 0, 0]; t[0x17] = [2, 2, 0, 0];
        t[0x18] = [1, 0, 0, 1]; t[0x19] = [1, 0, 0, 2]; t[0x1A] = [2, 0, 0, 1]; t[0x1B] = [2, 0, 0, 2];
        t[0x1C] = [1, 1, 1, 0]; t[0x1D] = [1, 2, 1, 0]; t[0x1E] = [2, 1, 1, 0]; t[0x1F] = [1, 1, 2, 0];
        t[0x20] = [2, 1, 2, 0]; t[0x21] = [2, 2, 1, 0]; t[0x22] = [1, 2, 2, 0]; t[0x23] = [2, 2, 2, 0];
        t[0x24] = [1, 0, 1, 1]; t[0x25] = [1, 0, 1, 2]; t[0x26] = [2, 0, 1, 1]; t[0x27] = [1, 0, 2, 1];
        t[0x28] = [2, 0, 2, 1]; t[0x29] = [2, 0, 1, 2]; t[0x2A] = [1, 0, 2, 2]; t[0x2B] = [2, 0, 2, 2];
        t[0x2C] = [0, 1, 1, 1]; t[0x2D] = [0, 1, 1, 2]; t[0x2E] = [0, 2, 1, 1]; t[0x2F] = [0, 2, 1, 2];
        t[0x30] = [0, 1, 2, 1]; t[0x31] = [0, 1, 2, 2]; t[0x32] = [0, 2, 2, 1]; t[0x33] = [0, 2, 2, 2];
        t[0x34] = [1, 1, 0, 1]; t[0x35] = [1, 1, 0, 2]; t[0x36] = [1, 2, 0, 1]; t[0x37] = [1, 2, 0, 2];
        t[0x38] = [2, 1, 0, 1]; t[0x39] = [2, 1, 0, 2]; t[0x3A] = [2, 2, 0, 1]; t[0x3B] = [2, 2, 0, 2];
        t[0x3C] = [1, 1, 1, 1]; t[0x3D] = [1, 1, 1, 2]; t[0x3E] = [1, 2, 1, 1]; t[0x3F] = [1, 2, 1, 2];
        t[0x40] = [2, 1, 1, 1]; t[0x41] = [1, 1, 2, 1]; t[0x42] = [2, 1, 2, 1]; t[0x43] = [2, 1, 1, 2];
        t[0x44] = [2, 2, 1, 1]; t[0x45] = [1, 1, 2, 2]; t[0x46] = [1, 2, 2, 1]; t[0x47] = [2, 2, 1, 2];
        t[0x48] = [1, 2, 2, 2]; t[0x49] = [2, 1, 2, 2]; t[0x4A] = [2, 2, 2, 1]; t[0x4B] = [2, 2, 2, 2];
        t[0x4C] = h1; t[0x4D] = h2; t[0x4E] = v1; t[0x4F] = v2;
        t[0x50] = [0, 3, 0, 3]; t[0x51] = [3, 0, 3, 0]; t[0x52] = [0, 3, 1, 0]; t[0x53] = [0, 1, 3, 0];
        t[0x54] = [0, 3, 3, 0]; t[0x55] = [0, 0, 1, 3]; t[0x56] = [0, 0, 3, 1]; t[0x57] = [0, 0, 3, 3];
        t[0x58] = [1, 3, 0, 0]; t[0x59] = [3, 1, 0, 0]; t[0x5A] = [3, 3, 0, 0]; t[0x5B] = [1, 0, 0, 3];
        t[0x5C] = [3, 0, 0, 1]; t[0x5D] = [3, 0, 0, 3]; t[0x5E] = [1, 3, 1, 0]; t[0x5F] = [3, 1, 3, 0];
        t[0x60] = [3, 3, 3, 0]; t[0x61] = [1, 0, 1, 3]; t[0x62] = [3, 0, 3, 1]; t[0x63] = [3, 0, 3, 3];
        t[0x64] = [0, 3, 1, 3]; t[0x65] = [0, 1, 3, 1]; t[0x66] = [0, 3, 3, 3]; t[0x67] = [1, 3, 0, 3];
        t[0x68] = [3, 1, 0, 1]; t[0x69] = [3, 3, 0, 3]; t[0x6A] = [1, 3, 1, 3]; t[0x6B] = [3, 1, 3, 1];
        t[0x6C] = [3, 3, 3, 3];
        t[0x6D] = [0, 1, 1, 0]; t[0x6E] = [0, 0, 1, 1]; t[0x6F] = [1, 0, 0, 1]; t[0x70] = [1, 1, 0, 0];
        t[0x74] = [0, 0, 0, 1]; t[0x75] = [1, 0, 0, 0]; t[0x76] = [0, 1, 0, 0]; t[0x77] = [0, 0, 1, 0];
        t[0x78] = [0, 0, 0, 2]; t[0x79] = [2, 0, 0, 0]; t[0x7A] = [0, 2, 0, 0]; t[0x7B] = [0, 0, 2, 0];
        t[0x7C] = [0, 2, 0, 1]; t[0x7D] = [1, 0, 2, 0]; t[0x7E] = [0, 1, 0, 2]; t[0x7F] = [2, 0, 1, 0];
        t
    };
    let cp = c as u32;
    if !(0x2500..0x2580).contains(&cp) {
        return None;
    }
    let a = *T.get((cp - 0x2500) as usize)?;
    (a != [0; 4]).then_some(a)
}

fn r(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::from_min_max(Pos2::new(x0, y0), Pos2::new(x1, y1))
}

/// Rectangles (relative to the cell's top-left, in pixels) and their coverage, or `None` when
/// `c` is not drawn here. `w`/`h` are the cell size, `stroke` the light line thickness.
pub fn shapes(c: char, w: f32, h: f32, stroke: f32) -> Option<Vec<(Rect, f32)>> {
    let cp = c as u32;
    if (0x2580..0x25A0).contains(&cp) {
        return blocks(cp, w, h);
    }
    let a = arms(c)?;
    let light = stroke.max(1.0);
    let heavy = (light * 2.0).max(2.0);
    let thick = |k: u8| match k {
        1 => light,
        2 => heavy,
        3 => light * 3.0,
        _ => 0.0,
    };
    let cx = (w / 2.0).floor();
    let cy = (h / 2.0).floor();
    let max_h = thick(a[1]).max(thick(a[3]));
    let max_v = thick(a[0]).max(thick(a[2]));
    let mut out = Vec::new();
    let mut hline = |x0: f32, x1: f32, k: u8| {
        let t = thick(k);
        if k == 3 {
            out.push((r(x0, cy - light * 1.5, x1, cy - light * 0.5), 1.0));
            out.push((r(x0, cy + light * 0.5, x1, cy + light * 1.5), 1.0));
        } else if k > 0 {
            out.push((r(x0, cy - (t / 2.0).floor(), x1, cy - (t / 2.0).floor() + t), 1.0));
        }
    };
    hline(0.0, cx + (max_v / 2.0).ceil(), a[3]);
    hline(cx - (max_v / 2.0).floor(), w, a[1]);
    let mut vline = |y0: f32, y1: f32, k: u8| {
        let t = thick(k);
        if k == 3 {
            out.push((r(cx - light * 1.5, y0, cx - light * 0.5, y1), 1.0));
            out.push((r(cx + light * 0.5, y0, cx + light * 1.5, y1), 1.0));
        } else if k > 0 {
            out.push((r(cx - (t / 2.0).floor(), y0, cx - (t / 2.0).floor() + t, y1), 1.0));
        }
    };
    vline(0.0, cy + (max_h / 2.0).ceil(), a[0]);
    vline(cy - (max_h / 2.0).floor(), h, a[2]);
    Some(out)
}

fn blocks(cp: u32, w: f32, h: f32) -> Option<Vec<(Rect, f32)>> {
    let eighth_h = |n: f32| (h * n / 8.0).round();
    let eighth_w = |n: f32| (w * n / 8.0).round();
    let (hw, hh) = ((w / 2.0).round(), (h / 2.0).round());
    let full = r(0.0, 0.0, w, h);
    let ul = r(0.0, 0.0, hw, hh);
    let ur = r(hw, 0.0, w, hh);
    let ll = r(0.0, hh, hw, h);
    let lr = r(hw, hh, w, h);
    let v = match cp {
        0x2580 => vec![(r(0.0, 0.0, w, hh), 1.0)],
        0x2581..=0x2587 => vec![(r(0.0, h - eighth_h((cp - 0x2580) as f32), w, h), 1.0)],
        0x2588 => vec![(full, 1.0)],
        0x2589..=0x258F => vec![(r(0.0, 0.0, eighth_w((0x2590 - cp) as f32), h), 1.0)],
        0x2590 => vec![(r(hw, 0.0, w, h), 1.0)],
        0x2591 => vec![(full, 0.25)],
        0x2592 => vec![(full, 0.5)],
        0x2593 => vec![(full, 0.75)],
        0x2594 => vec![(r(0.0, 0.0, w, eighth_h(1.0)), 1.0)],
        0x2595 => vec![(r(w - eighth_w(1.0), 0.0, w, h), 1.0)],
        0x2596 => vec![(ll, 1.0)],
        0x2597 => vec![(lr, 1.0)],
        0x2598 => vec![(ul, 1.0)],
        0x2599 => vec![(ul, 1.0), (ll, 1.0), (lr, 1.0)],
        0x259A => vec![(ul, 1.0), (lr, 1.0)],
        0x259B => vec![(ul, 1.0), (ur, 1.0), (ll, 1.0)],
        0x259C => vec![(ul, 1.0), (ur, 1.0), (lr, 1.0)],
        0x259D => vec![(ur, 1.0)],
        0x259E => vec![(ur, 1.0), (ll, 1.0)],
        0x259F => vec![(ur, 1.0), (ll, 1.0), (lr, 1.0)],
        _ => return None,
    };
    Some(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn horizontal_line_spans_the_cell_and_corners_join() {
        let s = shapes('─', 10.0, 20.0, 1.0).unwrap();
        let min_x = s.iter().map(|(r, _)| r.min.x).fold(f32::MAX, f32::min);
        let max_x = s.iter().map(|(r, _)| r.max.x).fold(f32::MIN, f32::max);
        assert_eq!((min_x, max_x), (0.0, 10.0));
        // ╭ is drawn (as a square corner) reaching the right and bottom edges.
        let c = shapes('╭', 10.0, 20.0, 1.0).unwrap();
        assert!(c.iter().any(|(r, _)| r.max.x == 10.0));
        assert!(c.iter().any(|(r, _)| r.max.y == 20.0));
        assert!(shapes('a', 10.0, 20.0, 1.0).is_none());
        assert!(shapes('\u{2571}', 10.0, 20.0, 1.0).is_none()); // diagonal: font draws it
    }

    #[test]
    fn blocks() {
        assert_eq!(shapes('█', 10.0, 20.0, 1.0).unwrap().len(), 1);
        assert_eq!(shapes('▄', 8.0, 16.0, 1.0).unwrap()[0].0, r(0.0, 8.0, 8.0, 16.0));
    }
}
