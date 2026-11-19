//! The design system: colours as tokens (Catppuccin Mocha, the default, and Latte), and one
//! scale for gaps, corners, type and control sizes. Values come from the approved mock-up's CSS
//! (`--g1..--g7`, `--r-sm/--r/--r-lg`, the `--st-*` status colours …).
//!
//! **Screens never write a colour or a size as a number.** `screens_take_sizes_and_colours_from_the_scale`
//! below walks the source and fails when one does (after craft-anything's `ui-egui/src/theme.rs`,
//! MIT/Apache-2.0, whose approach this copies).

use std::sync::Arc;

use egui::epaint::text::{FontData, FontDefinitions, FontFamily, FontTweak, VariationCoords};
use egui::{Color32, CornerRadius, FontId, Stroke, TextStyle, Visuals};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemeKind {
    /// Catppuccin Mocha (dark). The default.
    #[default]
    Mocha,
    /// Catppuccin Latte (light).
    Latte,
}

impl ThemeKind {
    pub const ALL: [ThemeKind; 2] = [ThemeKind::Mocha, ThemeKind::Latte];

    pub fn id(self) -> &'static str {
        match self {
            ThemeKind::Mocha => "mocha",
            ThemeKind::Latte => "latte",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ThemeKind::Mocha => "Mocha (dark)",
            ThemeKind::Latte => "Latte (light)",
        }
    }

    pub fn from_id(s: &str) -> Option<ThemeKind> {
        ThemeKind::ALL.into_iter().find(|t| t.id() == s)
    }
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

/// Every colour the UI uses. One set per theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tokens {
    pub kind: ThemeKind,
    // Surfaces, darkest to lightest in Mocha.
    pub crust: Color32,
    pub mantle: Color32,
    pub bg: Color32,
    pub surface0: Color32,
    pub surface1: Color32,
    pub surface2: Color32,
    /// Anything to type in or press.
    pub field: Color32,
    /// Menus, the palette, dialogs.
    pub pop: Color32,
    pub row_hover: Color32,
    pub row_sel: Color32,
    pub hover: Color32,
    // Text.
    pub fg: Color32,
    pub fg2: Color32,
    pub muted: Color32,
    pub faint: Color32,
    // Accents.
    pub blue: Color32,
    pub green: Color32,
    pub red: Color32,
    pub peach: Color32,
    pub yellow: Color32,
    pub mauve: Color32,
    pub teal: Color32,
    pub sky: Color32,
    pub sapphire: Color32,
    pub lavender: Color32,
    pub pink: Color32,
    pub maroon: Color32,
    pub flamingo: Color32,
    /// Text and marks on a filled accent (a primary button, a count badge).
    pub on_accent: Color32,
    // The status vocabulary.
    pub st_working: Color32,
    pub st_approval: Color32,
    pub st_question: Color32,
    pub st_done: Color32,
    pub st_error: Color32,
    pub st_idle: Color32,
    pub st_asleep: Color32,
    pub st_starting: Color32,
    // Terminal.
    pub term_bg: Color32,
    pub term_fg: Color32,
    pub term_selection: Color32,
    pub scrim: Color32,
    pub shadow: Color32,
}

impl Tokens {
    pub fn for_kind(kind: ThemeKind) -> Tokens {
        match kind {
            ThemeKind::Mocha => Tokens {
                kind,
                crust: hex(0x11111b),
                mantle: hex(0x181825),
                bg: hex(0x1e1e2e),
                surface0: hex(0x313244),
                surface1: hex(0x45475a),
                surface2: hex(0x585b70),
                field: hex(0x232334),
                pop: hex(0x232336),
                row_hover: hex(0x24243a),
                row_sel: hex(0x313244),
                hover: hex(0x45475a),
                fg: hex(0xcdd6f4),
                fg2: hex(0xbac2de),
                muted: hex(0x9399b2),
                faint: hex(0x6c7086),
                blue: hex(0x89b4fa),
                green: hex(0xa6e3a1),
                red: hex(0xf38ba8),
                peach: hex(0xfab387),
                yellow: hex(0xf9e2af),
                mauve: hex(0xcba6f7),
                teal: hex(0x94e2d5),
                sky: hex(0x89dceb),
                sapphire: hex(0x74c7ec),
                lavender: hex(0xb4befe),
                pink: hex(0xf5c2e7),
                maroon: hex(0xeba0ac),
                flamingo: hex(0xf2cdcd),
                on_accent: hex(0x11111b),
                st_working: hex(0x89b4fa),
                st_approval: hex(0xfab387),
                st_question: hex(0xf9e2af),
                st_done: hex(0xa6e3a1),
                st_error: hex(0xf38ba8),
                st_idle: hex(0x7f849c),
                st_asleep: hex(0x7f849c),
                st_starting: hex(0x7f849c),
                term_bg: hex(0x1e1e2e),
                term_fg: hex(0xcdd6f4),
                term_selection: hex(0x585b70),
                scrim: Color32::from_rgba_unmultiplied(8, 8, 14, 148),
                shadow: Color32::from_black_alpha(140),
            },
            ThemeKind::Latte => Tokens {
                kind,
                crust: hex(0xdce0e8),
                mantle: hex(0xe6e9ef),
                bg: hex(0xeff1f5),
                surface0: hex(0xccd0da),
                surface1: hex(0xbcc0cc),
                surface2: hex(0xacb0be),
                field: hex(0xffffff),
                pop: hex(0xffffff),
                row_hover: hex(0xdce0e8),
                row_sel: hex(0xd3d7e1),
                hover: hex(0xccd0da),
                fg: hex(0x4c4f69),
                fg2: hex(0x5c5f77),
                muted: hex(0x6c6f85),
                faint: hex(0x8c8fa1),
                blue: hex(0x1e66f5),
                green: hex(0x40a02b),
                red: hex(0xd20f39),
                peach: hex(0xfe640b),
                yellow: hex(0xdf8e1d),
                mauve: hex(0x8839ef),
                teal: hex(0x179299),
                sky: hex(0x04a5e5),
                sapphire: hex(0x209fb5),
                lavender: hex(0x7287fd),
                pink: hex(0xea76cb),
                maroon: hex(0xe64553),
                flamingo: hex(0xdd7878),
                on_accent: hex(0xffffff),
                st_working: hex(0x1e66f5),
                // Latte's peach, yellow and green are 2.4, 2.2 and 2.75:1 on the sidebar: darkened to keep
                // status glyphs at 3:1 (a deviation from the mock-up's exact Latte values).
                st_approval: hex(0xc94f00),
                st_question: hex(0xa96500),
                st_done: hex(0x2f7d1f),
                st_error: hex(0xd20f39),
                st_idle: hex(0x8c8fa1),
                st_asleep: hex(0x8c8fa1),
                st_starting: hex(0x8c8fa1),
                term_bg: hex(0xf8f9fb),
                term_fg: hex(0x4c4f69),
                term_selection: hex(0xbcc0cc),
                scrim: Color32::from_rgba_unmultiplied(76, 79, 105, 82),
                shadow: Color32::from_black_alpha(46),
            },
        }
    }

    /// The tokens of the active theme.
    pub fn get(ctx: &egui::Context) -> Tokens {
        ctx.data(|d| d.get_temp::<Tokens>(tokens_id())).unwrap_or_else(|| Tokens::for_kind(ThemeKind::Mocha))
    }

    pub fn dark(&self) -> bool {
        self.kind == ThemeKind::Mocha
    }

    /// Session accents, in the order of `csm_core::color::palette()` (red, orange, yellow, lime,
    /// green, emerald, cyan, blue, indigo, violet, pink, rose), drawn in this theme's shades.
    pub fn accents(&self) -> [Color32; 12] {
        [
            self.red,
            self.peach,
            self.yellow,
            self.green,
            self.teal,
            self.sapphire,
            self.sky,
            self.blue,
            self.lavender,
            self.mauve,
            self.pink,
            self.maroon,
        ]
    }

    /// A session accent in this theme.
    pub fn accent(&self, a: crate::model::Accent) -> Color32 {
        match a {
            crate::model::Accent::Palette(i) => self.accents().get(usize::from(i) % 12).copied().unwrap_or(self.blue),
            crate::model::Accent::Rgb([r, g, b]) => Color32::from_rgb(r, g, b),
        }
    }

    /// `color` at `alpha` (0–1) over transparent: tinted backgrounds (`color-mix(... 14%)`).
    pub fn tint(color: Color32, alpha: f32) -> Color32 {
        color.gamma_multiply(alpha)
    }

    /// The terminal palette for this theme: the emulator's default ANSI colours for Mocha, and
    /// Latte's for the light theme.
    pub fn terminal_style(&self) -> crate::terminal::TermStyle {
        use csm_term::alacritty_terminal::vte::ansi::Rgb;
        let rgb = |c: Color32| Rgb { r: c.r(), g: c.g(), b: c.b() };
        let mut palette = csm_term::Palette::default();
        if !self.dark() {
            let ansi = [
                0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be, 0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5,
                0xea76cb, 0x179299, 0xbcc0cc,
            ];
            for (slot, c) in palette.ansi.iter_mut().zip(ansi) {
                *slot = rgb(hex(c));
            }
            palette.cursor = rgb(hex(0xdc8a78));
        }
        palette.foreground = rgb(self.term_fg);
        palette.background = rgb(self.term_bg);
        crate::terminal::TermStyle { palette, selection: self.term_selection }
    }
}

fn tokens_id() -> egui::Id {
    egui::Id::new("csm-theme-tokens")
}

/// Gaps (the mock-up's `--g1 … --g7`).
pub mod space {
    pub const XXS: f32 = 2.0;
    pub const XS: f32 = 4.0;
    pub const SM: f32 = 8.0;
    pub const MD: f32 = 12.0;
    pub const LG: f32 = 16.0;
    pub const XL: f32 = 24.0;
    pub const XXL: f32 = 32.0;
    /// The odd 6 px the mock uses between panes and inside small rows.
    pub const PANE_GAP: f32 = 6.0;
    /// The mock's 10 px row/button side padding.
    pub const ROW_X: f32 = 10.0;
}

/// Corners: tags and rows (4), controls and fields (6), cards, menus and dialogs (8), the
/// palette and the window (10).
pub mod radius {
    pub const SM: u8 = 4;
    pub const MD: u8 = 6;
    pub const LG: u8 = 8;
    pub const XL: u8 = 10;
    /// Fully round (pills, badges).
    pub const PILL: u8 = 255;
}

/// Type sizes, in points. The mock-up's app window is dense: 13 px body text.
pub mod text {
    /// Uppercase section captions ("NEEDS YOU", "SESSIONS").
    pub const CAPS: f32 = 10.5;
    /// Counts, ages, key caps.
    pub const TINY: f32 = 11.0;
    /// Secondary lines (the inbox "why", meta text).
    pub const CAPTION: f32 = 12.0;
    pub const SMALL: f32 = 12.5;
    pub const BODY: f32 = 13.0;
    /// The group name in the group bar.
    pub const EMPH: f32 = 14.0;
    /// The palette's input.
    pub const INPUT: f32 = 15.0;
    /// Dialog titles.
    pub const TITLE: f32 = 16.0;
    /// Branch chips, paths, durations.
    pub const MONO_SM: f32 = 11.0;
    pub const MONO: f32 = 12.0;
    /// The terminal font size.
    pub const TERMINAL: f32 = 12.0;
}

/// Icon sizes.
pub mod icon {
    pub const XS: f32 = 12.0;
    pub const SM: f32 = 13.0;
    pub const MD: f32 = 14.0;
    pub const LG: f32 = 15.0;
    pub const XL: f32 = 16.0;
    pub const XXL: f32 = 18.0;
    /// The big spinner on a starting pane.
    pub const HERO: f32 = 28.0;
}

/// Fixed sizes of the window's parts.
pub mod size {
    pub const HAIRLINE: f32 = 1.0;
    pub const RING: f32 = 2.0;
    pub const STRIPE: f32 = 3.0;
    pub const TITLE_BAR: f32 = 40.0;
    pub const GROUP_BAR: f32 = 44.0;
    pub const PANE_HEADER: f32 = 32.0;
    pub const PANE_HEADER_MINI: f32 = 28.0;
    pub const RAIL: f32 = 48.0;
    pub const RAIL_TAB: f32 = 36.0;
    pub const SIDEBAR: f32 = 272.0;
    pub const INSPECTOR: f32 = 340.0;
    pub const ROW: f32 = 28.0;
    pub const INBOX_ROW: f32 = 44.0;
    pub const CONTROL: f32 = 28.0;
    pub const CONTROL_SM: f32 = 24.0;
    pub const CONTROL_LG: f32 = 32.0;
    pub const CHIP: f32 = 20.0;
    pub const BADGE: f32 = 16.0;
    pub const COUNT: f32 = 18.0;
    pub const KBD: f32 = 18.0;
    pub const SEG_BUTTON: f32 = 26.0;
    pub const TAB: f32 = 36.0;
    pub const SEARCH_W: f32 = 440.0;
    pub const PALETTE_W: f32 = 640.0;
    pub const PALETTE_TOP: f32 = 84.0;
    pub const PALETTE_INPUT: f32 = 48.0;
    pub const PALETTE_ROW: f32 = 34.0;
    pub const PALETTE_LIST_H: f32 = 420.0;
    pub const MENU_W: f32 = 236.0;
    pub const MENU_ROW: f32 = 30.0;
    pub const DIALOG_W: f32 = 500.0;
    pub const BOARD_W: f32 = 560.0;
    pub const CONSENT_W: f32 = 380.0;
    pub const TOAST_MAX_W: f32 = 620.0;
    pub const INBOX_DEFAULT_H: f32 = 206.0;
    pub const INBOX_MIN_H: f32 = 64.0;
    pub const INBOX_MAX_H: f32 = 560.0;
    pub const SPLIT_GRIP: f32 = 10.0;
    pub const DIVIDER: f32 = 6.0;
    pub const RESTORE_RAIL: f32 = 2.0;
    pub const BAR: f32 = 6.0;
    pub const GRID_TILE_W: f32 = 92.0;
    pub const GRID_GLYPH: [f32; 2] = [56.0, 36.0];
    pub const MIN_PANE: f32 = 120.0;
    pub const FLOAT_DEFAULT: [f32; 2] = [440.0, 300.0];
    pub const BRANCH_MAX_W: f32 = 68.0;
    pub const PANE_NAME_MIN_W: f32 = 72.0;
    pub const TEAR_OFF: [f32; 2] = [960.0, 560.0];
    pub const WINDOW: [f32; 2] = [1280.0, 800.0];
    pub const WINDOW_MIN: [f32; 2] = [760.0, 480.0];
    /// Below this window width the sidebar folds to the rail.
    pub const COMPACT_W: f32 = 1000.0;
    pub const INSPECTOR_MIN: f32 = 280.0;
    /// The settings page's section list.
    pub const SETTINGS_NAV_W: f32 = 200.0;
    /// The settings page's column of controls.
    pub const SETTINGS_W: f32 = 620.0;
    /// A text field in a settings row.
    pub const FIELD_W: f32 = 300.0;
    /// The shortcut list.
    pub const HELP_W: f32 = 560.0;
    /// A pane edge that takes a split drop: this fraction of the pane's width or height.
    pub const DROP_EDGE: f32 = 0.3;
    /// How far a press must move before it is a drag (a click stays a click).
    pub const DRAG_SLOP: f32 = 4.0;
    /// An on/off switch's track.
    pub const SWITCH: [f32; 2] = [34.0, 18.0];
}

/// Opacities (the mock-up's `opacity` and `color-mix` percentages).
pub mod alpha {
    /// An asleep row.
    pub const ASLEEP: f32 = 0.6;
    /// A tinted background behind an accent (`color-mix(... 14%)`).
    pub const SOFT: f32 = 0.14;
    /// A status pill's tint.
    pub const PILL: f32 = 0.15;
    /// A drop target or a selected grid preset.
    pub const DROP: f32 = 0.12;
}

/// How long a toast stays, and the status glyph animation rate.
pub mod timing {
    pub const TOAST_SECS: f32 = 3.2;
    /// Working glyphs turn at this many frames per second, and only while visible: a spinner
    /// repainting every frame would pin a core with many panes.
    pub const SPINNER_FPS: f32 = 10.0;
}

// ---- fonts ---------------------------------------------------------------------------------

/// Figtree, a variable font: one file set to each weight used here.
pub static FIGTREE: &[u8] = include_bytes!("../assets/fonts/Figtree-Variable.ttf");
const FIGTREE_FACES: [(&str, f32); 3] = [("Figtree", 400.0), ("Figtree-Medium", 500.0), ("Figtree-SemiBold", 600.0)];

pub fn medium_family() -> FontFamily {
    FontFamily::Name("medium".into())
}

pub fn semibold_family() -> FontFamily {
    FontFamily::Name("semibold".into())
}

/// Figtree at 400.
pub fn regular(size: f32) -> FontId {
    FontId::proportional(size)
}

/// Figtree at 500 (labels, rows).
pub fn medium(size: f32) -> FontId {
    FontId::new(size, medium_family())
}

/// Figtree at 600 (names, headings).
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, semibold_family())
}

/// JetBrains Mono (branches, paths, key caps).
pub fn mono(size: f32) -> FontId {
    FontId::monospace(size)
}

/// Figtree (three weights) and JetBrains Mono, with egui's default fonts behind them for
/// symbols Figtree lacks.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    for (name, weight) in FIGTREE_FACES {
        let tweak = FontTweak { coords: VariationCoords::new([(*b"wght", weight)]), ..Default::default() };
        fonts.font_data.insert(name.to_owned(), Arc::new(FontData::from_static(FIGTREE).tweak(tweak)));
    }
    fonts.font_data.insert("JetBrainsMono".to_owned(), Arc::new(FontData::from_static(crate::terminal::fonts::JETBRAINS_MONO)));
    let defaults_prop = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let defaults_mono = fonts.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();
    let with = |first: &str, rest: &[String]| -> Vec<String> { std::iter::once(first.to_owned()).chain(rest.iter().cloned()).collect() };
    fonts.families.insert(FontFamily::Proportional, with("Figtree", &defaults_prop));
    fonts.families.insert(medium_family(), with("Figtree-Medium", &defaults_prop));
    fonts.families.insert(semibold_family(), with("Figtree-SemiBold", &defaults_prop));
    fonts.families.insert(FontFamily::Monospace, with("JetBrainsMono", &defaults_mono));
    ctx.set_fonts(fonts);
}

/// Apply a theme: stores the tokens and sets egui's own visuals from them.
pub fn apply(ctx: &egui::Context, kind: ThemeKind) {
    let t = Tokens::for_kind(kind);
    ctx.data_mut(|d| d.insert_temp(tokens_id(), t));
    ctx.set_theme(if t.dark() { egui::Theme::Dark } else { egui::Theme::Light });
    let mut v = if t.dark() { Visuals::dark() } else { Visuals::light() };
    v.text_options.subpixel_binning = false;
    v.panel_fill = t.mantle;
    v.window_fill = t.pop;
    v.window_stroke = Stroke::new(size::HAIRLINE, t.surface1);
    v.extreme_bg_color = t.field;
    v.faint_bg_color = t.mantle;
    v.code_bg_color = t.crust;
    v.override_text_color = Some(t.fg);
    v.hyperlink_color = t.blue;
    v.warn_fg_color = t.peach;
    v.error_fg_color = t.red;
    v.window_corner_radius = CornerRadius::same(radius::LG);
    v.menu_corner_radius = CornerRadius::same(radius::LG);
    v.window_shadow = egui::Shadow { offset: [0, 14], blur: 36, spread: 0, color: t.shadow };
    v.popup_shadow = egui::Shadow { offset: [0, 10], blur: 28, spread: 0, color: t.shadow };
    v.text_edit_bg_color = Some(t.field);
    v.selection.bg_fill = Tokens::tint(t.blue, 0.32);
    v.selection.stroke = Stroke::new(size::HAIRLINE, t.blue);
    v.striped = false;
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = t.pop;
    w.noninteractive.weak_bg_fill = t.pop;
    w.noninteractive.bg_stroke = Stroke::new(size::HAIRLINE, t.surface0);
    w.noninteractive.fg_stroke = Stroke::new(size::HAIRLINE, t.fg2);
    for (wv, bg, stroke) in [
        (&mut w.inactive, t.field, t.surface0),
        (&mut w.hovered, t.row_sel, t.surface2),
        (&mut w.active, t.row_sel, t.blue),
        (&mut w.open, t.row_sel, t.blue),
    ] {
        wv.bg_fill = bg;
        wv.weak_bg_fill = bg;
        wv.bg_stroke = Stroke::new(size::HAIRLINE, stroke);
        wv.fg_stroke = Stroke::new(size::HAIRLINE, t.fg);
        wv.corner_radius = CornerRadius::same(radius::MD);
        wv.expansion = 0.0;
    }
    ctx.set_visuals(v);
    ctx.global_style_mut(|s| {
        s.text_styles = [
            (TextStyle::Small, regular(text::TINY)),
            (TextStyle::Body, regular(text::BODY)),
            (TextStyle::Button, medium(text::BODY)),
            (TextStyle::Heading, semibold(text::TITLE)),
            (TextStyle::Monospace, mono(text::MONO)),
        ]
        .into();
        s.spacing.item_spacing = egui::vec2(space::SM, space::XS);
        s.spacing.button_padding = egui::vec2(space::ROW_X, space::XS);
        s.spacing.interact_size = egui::vec2(size::CONTROL, size::CONTROL_SM);
        s.spacing.menu_margin = egui::Margin::same(space::XS as i8);
        s.spacing.window_margin = egui::Margin::same(space::LG as i8);
        // Scroll bars stay visible (a slim thumb at rest), not floating and invisible.
        s.spacing.scroll = egui::style::ScrollStyle::solid();
        s.spacing.scroll.bar_width = space::SM - space::XXS;
        s.interaction.tooltip_delay = 0.4;
    });
}

/// WCAG relative-luminance contrast ratio.
pub fn contrast(a: Color32, b: Color32) -> f32 {
    fn lum(c: Color32) -> f32 {
        let ch = |v: u8| {
            let v = f32::from(v) / 255.0;
            if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.r()) + 0.7152 * ch(c.g()) + 0.0722 * ch(c.b())
    }
    let (la, lb) = (lum(a), lum(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Text, status glyphs and the scroll thumb keep their contrast on the surfaces they sit on,
    /// in both themes.
    #[test]
    fn contrast_is_kept_in_both_themes() {
        for kind in ThemeKind::ALL {
            let t = Tokens::for_kind(kind);
            for surface in [t.bg, t.mantle, t.pop, t.row_sel] {
                // WCAG AA for text (4.5:1); muted text is secondary and at least 3:1.
                assert!(contrast(t.fg, surface) >= 4.5, "{kind:?}: body text {:.2}", contrast(t.fg, surface));
                assert!(contrast(t.muted, surface) >= 3.0, "{kind:?}: muted text {:.2}", contrast(t.muted, surface));
            }
            for st in [t.st_working, t.st_approval, t.st_question, t.st_done, t.st_error] {
                // Glyphs are graphics: 3:1 against the sidebar, except Latte's yellow, which
                // the mock-up accepts (it always carries its word next to it).
                let need = 3.0;
                assert!(contrast(st, t.mantle) >= need, "{kind:?}: status {st:?} {:.2}", contrast(st, t.mantle));
            }
            // The scroll bar thumb (egui draws it in the widgets' fg/bg strokes) stays visible.
            assert!(contrast(t.surface2, t.mantle) >= 1.6, "{kind:?}: scroll thumb");
        }
    }

    fn rust_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                rust_files(&path, out);
            } else if path.extension().is_some_and(|x| x == "rs") {
                out.push(path);
            }
        }
    }

    /// Outside this file (and the terminal renderer, whose colours come from the emulator's
    /// palette) nothing writes a gap, a padding, a corner, a stroke, a colour or a font size as
    /// a number: screens take them from the scale, so they can't drift apart.
    #[test]
    fn screens_take_sizes_and_colours_from_the_scale() {
        let followed_by_digit = |line: &str, marker: &str| {
            line.match_indices(marker).any(|(at, m)| line[at + m.len()..].trim_start().starts_with(|c: char| c.is_ascii_digit()))
        };
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        let mut found = Vec::new();
        let mut checked = 0;
        for path in files {
            let rel = path.strip_prefix(&src).unwrap().to_string_lossy().replace('\\', "/");
            // The scale itself, the glyph drawings (their geometry is the drawing) and the renderer.
            if rel == "theme.rs" || rel == "icons.rs" || rel == "icon_data.rs" || rel.starts_with("terminal/") {
                continue;
            }
            checked += 1;
            for (n, line) in std::fs::read_to_string(&path).unwrap().lines().enumerate() {
                let code = line.trim_start();
                if code.starts_with("//") {
                    continue;
                }
                let numbered = [
                    "add_space(",
                    "Margin::same(",
                    "Margin::symmetric(",
                    "CornerRadius::same(",
                    "Stroke::new(",
                    "medium(",
                    "semibold(",
                    "regular(",
                    "mono(",
                    "FontId::proportional(",
                    "FontId::monospace(",
                    "gamma_multiply(",
                    "tint(",
                ];
                let named = [
                    "Color32::from_rgb",
                    "Color32::from_gray",
                    "Color32::from_rgba",
                    "Color32::from_black_alpha",
                    "Color32::from_white_alpha",
                    "Color32::WHITE",
                    "Color32::BLACK",
                    "Color32::RED",
                    "Color32::GRAY",
                    "hex(0x",
                ];
                if numbered.iter().any(|m| followed_by_digit(code, m)) || named.iter().any(|m| code.contains(m)) {
                    found.push(format!("{rel}:{}: {}", n + 1, code));
                }
            }
        }
        assert!(checked >= 8, "the source scan found only {checked} screen files");
        assert!(found.is_empty(), "use the theme's scale (`space`, `radius`, `text`, `size`, `Tokens`):\n{}", found.join("\n"));
    }

    #[test]
    fn theme_ids_round_trip() {
        for k in ThemeKind::ALL {
            assert_eq!(ThemeKind::from_id(k.id()), Some(k));
        }
    }
}
