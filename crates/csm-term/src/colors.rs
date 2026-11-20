//! Default palette and colour resolution (named / indexed / true colour → RGB).

use alacritty_terminal::term::color::{COUNT, Colors};
use alacritty_terminal::vte::ansi::{Color, Rgb};

/// The terminal's default colours. The host passes its theme in; nothing here is a literal
/// the UI should use directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    /// ANSI 0–15.
    pub ansi: [Rgb; 16],
    pub foreground: Rgb,
    pub background: Rgb,
    pub cursor: Rgb,
}

const fn rgb(hex: u32) -> Rgb {
    Rgb { r: (hex >> 16) as u8, g: (hex >> 8) as u8, b: hex as u8 }
}

impl Default for Palette {
    /// Catppuccin Mocha, matching the WPF app's theme.
    fn default() -> Self {
        Self {
            ansi: [
                rgb(0x45475a),
                rgb(0xf38ba8),
                rgb(0xa6e3a1),
                rgb(0xf9e2af),
                rgb(0x89b4fa),
                rgb(0xf5c2e7),
                rgb(0x94e2d5),
                rgb(0xbac2de),
                rgb(0x585b70),
                rgb(0xf38ba8),
                rgb(0xa6e3a1),
                rgb(0xf9e2af),
                rgb(0x89b4fa),
                rgb(0xf5c2e7),
                rgb(0x94e2d5),
                rgb(0xa6adc8),
            ],
            foreground: rgb(0xcdd6f4),
            background: rgb(0x1e1e2e),
            cursor: rgb(0xf5e0dc),
        }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb { r: (c.r as u16 * 2 / 3) as u8, g: (c.g as u16 * 2 / 3) as u8, b: (c.b as u16 * 2 / 3) as u8 }
}

impl Palette {
    /// The default for colour slot `index` (0..269, alacritty's layout).
    pub fn default_slot(&self, index: usize) -> Rgb {
        match index {
            0..16 => self.ansi.get(index).copied().unwrap_or(self.foreground),
            16..232 => {
                let i = index - 16;
                let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
                Rgb { r: level(i / 36), g: level((i / 6) % 6), b: level(i % 6) }
            }
            232..256 => {
                let v = (8 + (index - 232) * 10) as u8;
                Rgb { r: v, g: v, b: v }
            }
            256 => self.foreground,
            257 => self.background,
            258 => self.cursor,
            259..267 => dim(self.ansi.get(index - 259).copied().unwrap_or(self.foreground)),
            267 => self.foreground,
            268 => dim(self.foreground),
            _ => self.foreground,
        }
    }

    /// Resolves a cell colour against the terminal's dynamic overrides (OSC 4/10/11).
    pub fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(i) => self.slot(i as usize, overrides),
            Color::Named(n) => self.slot(n as usize, overrides),
        }
    }

    pub fn slot(&self, index: usize, overrides: &Colors) -> Rgb {
        if index < COUNT
            && let Some(c) = overrides[index]
        {
            return c;
        }
        self.default_slot(index)
    }

    /// Snapshot of every slot with overrides applied (answers OSC colour queries in order,
    /// without the terminal lock).
    pub fn snapshot(&self, overrides: &Colors) -> Vec<Rgb> {
        (0..COUNT).map(|i| self.slot(i, overrides)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cube_and_gray_ramp() {
        let p = Palette::default();
        assert_eq!(p.default_slot(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(p.default_slot(231), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(p.default_slot(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(p.default_slot(255), Rgb { r: 238, g: 238, b: 238 });
        // Out of range never panics.
        let _ = p.default_slot(10_000);
    }
}
