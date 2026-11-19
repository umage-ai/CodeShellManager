//! Design tokens for the spike. Screen code takes colours and sizes from here, never literals.

use egui::Color32;

pub const BG: Color32 = Color32::from_rgb(0x1e, 0x1e, 0x2e);
pub const TOOLBAR: Color32 = Color32::from_rgb(0x18, 0x18, 0x25);
pub const PANEL: Color32 = Color32::from_rgb(0x11, 0x11, 0x1b);
pub const FG: Color32 = Color32::from_rgb(0xcd, 0xd6, 0xf4);
pub const MUTED: Color32 = Color32::from_rgb(0x6c, 0x70, 0x86);
pub const BORDER: Color32 = Color32::from_rgb(0x31, 0x32, 0x44);
pub const ACCENT: Color32 = Color32::from_rgb(0x89, 0xb4, 0xfa);
pub const GREEN: Color32 = Color32::from_rgb(0xa6, 0xe3, 0xa1);
pub const PEACH: Color32 = Color32::from_rgb(0xfa, 0xb3, 0x87);
pub const ALERT: Color32 = Color32::from_rgb(0xf3, 0x8b, 0xa8);
pub const SELECTION: Color32 = Color32::from_rgb(0x58, 0x5b, 0x70);

/// Gap between panes and around the grid, in points.
pub const PANE_GAP: f32 = 4.0;
/// Height of a pane's header strip, in points.
pub const PANE_HEADER: f32 = 18.0;
/// Inner padding between a pane's border and its cells, in points.
pub const PANE_PADDING: f32 = 3.0;
/// Active-pane ring thickness, in points.
pub const RING: f32 = 1.5;
pub const DEFAULT_FONT_SIZE: f32 = 13.0;
pub const HEADER_TEXT: f32 = 11.5;

pub fn rgb(c: csm_term::alacritty_terminal::vte::ansi::Rgb) -> Color32 {
    Color32::from_rgb(c.r, c.g, c.b)
}
