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

    /// Dark (Mocha) or light (Latte): the base an add-on theme is drawn over. `Tokens::kind` is
    /// the base of whatever theme is drawn, so `Mocha` there means "a dark theme".
    pub fn base(self) -> csm_core::addons::ThemeBase {
        match self {
            ThemeKind::Mocha => csm_core::addons::ThemeBase::Dark,
            ThemeKind::Latte => csm_core::addons::ThemeBase::Light,
        }
    }

    pub fn of_base(base: csm_core::addons::ThemeBase) -> ThemeKind {
        match base {
            csm_core::addons::ThemeBase::Dark => ThemeKind::Mocha,
            csm_core::addons::ThemeBase::Light => ThemeKind::Latte,
        }
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
    /// umage.ai's brand gold (the ".ai" in the wordmark; the About page).
    pub brand: Color32,
    /// Text and marks on a filled accent (a primary button, a count badge).
    pub on_accent: Color32,
    /// The title bar's Close under the pointer, and its ✕ there: Windows' own caption red.
    pub close_hover: Color32,
    pub on_close_hover: Color32,
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
    /// The terminal's ANSI 0-15 and its cursor.
    pub term_ansi: [Color32; 16],
    pub term_cursor: Color32,
    pub scrim: Color32,
    pub shadow: Color32,
    // Code and diff in add-on screens: syntax classes (from `csm_services::highlight`), diff
    // backgrounds (translucent, drawn over the code background), the gutter and a line range.
    pub syn_keyword: Color32,
    pub syn_string: Color32,
    pub syn_comment: Color32,
    pub syn_number: Color32,
    pub syn_type: Color32,
    pub syn_function: Color32,
    pub syn_constant: Color32,
    pub syn_macro: Color32,
    pub syn_operator: Color32,
    pub syn_punct: Color32,
    pub syn_tag: Color32,
    pub syn_attribute: Color32,
    pub diff_add_bg: Color32,
    pub diff_del_bg: Color32,
    pub diff_hunk_bg: Color32,
    pub gutter_fg: Color32,
    pub range_sel_bg: Color32,
    pub thread_bar: Color32,
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
                brand: hex(0xb8966a),
                on_accent: hex(0x11111b),
                close_hover: hex(0xc42b1c),
                on_close_hover: hex(0xffffff),
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
                term_ansi: MOCHA_ANSI.map(hex),
                term_cursor: hex(0xf5e0dc),
                scrim: Color32::from_rgba_unmultiplied(8, 8, 14, 148),
                shadow: Color32::from_black_alpha(140),
                syn_keyword: hex(0xcba6f7),
                syn_string: hex(0xa6e3a1),
                syn_comment: hex(0x7f849c),
                syn_number: hex(0xfab387),
                syn_type: hex(0xf9e2af),
                syn_function: hex(0x89b4fa),
                syn_constant: hex(0xfab387),
                syn_macro: hex(0x94e2d5),
                syn_operator: hex(0x89dceb),
                syn_punct: hex(0x9399b2),
                syn_tag: hex(0x89b4fa),
                syn_attribute: hex(0xf2cdcd),
                diff_add_bg: Color32::from_rgba_unmultiplied(166, 227, 161, 38),
                diff_del_bg: Color32::from_rgba_unmultiplied(243, 139, 168, 38),
                diff_hunk_bg: Color32::from_rgba_unmultiplied(137, 180, 250, 26),
                gutter_fg: hex(0x7f849c),
                range_sel_bg: Color32::from_rgba_unmultiplied(137, 180, 250, 60),
                thread_bar: hex(0xcba6f7),
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
                brand: hex(0x8a6a3e),
                on_accent: hex(0xffffff),
                close_hover: hex(0xc42b1c),
                on_close_hover: hex(0xffffff),
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
                term_ansi: LATTE_ANSI.map(hex),
                term_cursor: hex(0xdc8a78),
                scrim: Color32::from_rgba_unmultiplied(76, 79, 105, 82),
                shadow: Color32::from_black_alpha(46),
                // The Latte accents that are too light for text on this background (green 2.9:1,
                // peach 2.6:1, yellow 2.5:1, sky 2.4:1) are darkened to about 4:1 for code.
                syn_keyword: hex(0x8839ef),
                syn_string: hex(0x2f7d1f),
                syn_comment: hex(0x7c7f93),
                syn_number: hex(0xc94f00),
                syn_type: hex(0xa96500),
                syn_function: hex(0x1e66f5),
                syn_constant: hex(0xc94f00),
                syn_macro: hex(0x0f7a80),
                syn_operator: hex(0x2a7fa8),
                syn_punct: hex(0x6c6f85),
                syn_tag: hex(0x1e66f5),
                syn_attribute: hex(0xb4505a),
                diff_add_bg: Color32::from_rgba_unmultiplied(64, 160, 43, 44),
                diff_del_bg: Color32::from_rgba_unmultiplied(210, 15, 57, 34),
                diff_hunk_bg: Color32::from_rgba_unmultiplied(30, 102, 245, 22),
                gutter_fg: hex(0x7c7f93),
                range_sel_bg: Color32::from_rgba_unmultiplied(30, 102, 245, 44),
                thread_bar: hex(0x8839ef),
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
        for (slot, c) in palette.ansi.iter_mut().zip(self.term_ansi) {
            *slot = rgb(c);
        }
        palette.cursor = rgb(self.term_cursor);
        palette.foreground = rgb(self.term_fg);
        palette.background = rgb(self.term_bg);
        crate::terminal::TermStyle { palette, selection: self.term_selection }
    }

    /// The tokens of any palette. Each token keeps the offset it has in the base theme from
    /// the palette slot it derives from: a token that *is* a slot in Mocha (`bg` is `base`)
    /// is that slot, and one a shade off a slot (`field` is `base` lifted by 5) stays that shade
    /// off the new slot. Alpha is kept from the base token. With the base's own palette every
    /// token is exactly the built-in one (tested). Terminal colours a theme names win over the
    /// derived ones.
    pub fn from_palette(p: &Palette) -> Tokens {
        let r = Tokens::for_kind(p.kind);
        let b = Palette::builtin(p.kind);
        let d = |token: Color32, key: &str| shift(token, b.c(key), p.c(key));
        let mut t = Tokens {
            kind: p.kind,
            crust: d(r.crust, "crust"),
            mantle: d(r.mantle, "mantle"),
            bg: d(r.bg, "base"),
            surface0: d(r.surface0, "surface0"),
            surface1: d(r.surface1, "surface1"),
            surface2: d(r.surface2, "surface2"),
            field: d(r.field, "base"),
            pop: d(r.pop, "base"),
            row_hover: d(r.row_hover, "base"),
            row_sel: d(r.row_sel, "surface0"),
            hover: d(r.hover, "surface1"),
            fg: d(r.fg, "text"),
            fg2: d(r.fg2, "subtext1"),
            muted: d(r.muted, "overlay2"),
            faint: d(r.faint, "overlay0"),
            blue: d(r.blue, "blue"),
            green: d(r.green, "green"),
            red: d(r.red, "red"),
            peach: d(r.peach, "peach"),
            yellow: d(r.yellow, "yellow"),
            mauve: d(r.mauve, "mauve"),
            teal: d(r.teal, "teal"),
            sky: d(r.sky, "sky"),
            sapphire: d(r.sapphire, "sapphire"),
            lavender: d(r.lavender, "lavender"),
            pink: d(r.pink, "pink"),
            maroon: d(r.maroon, "maroon"),
            flamingo: d(r.flamingo, "flamingo"),
            brand: r.brand,
            on_accent: d(r.on_accent, "crust"),
            st_working: d(r.st_working, "blue"),
            st_approval: d(r.st_approval, "peach"),
            st_question: d(r.st_question, "yellow"),
            st_done: d(r.st_done, "green"),
            st_error: d(r.st_error, "red"),
            st_idle: d(r.st_idle, "overlay1"),
            st_asleep: d(r.st_asleep, "overlay1"),
            st_starting: d(r.st_starting, "overlay1"),
            term_bg: d(r.term_bg, "base"),
            term_fg: d(r.term_fg, "text"),
            term_selection: d(r.term_selection, "surface2"),
            term_ansi: r.term_ansi,
            term_cursor: d(r.term_cursor, "rosewater"),
            scrim: r.scrim,
            shadow: r.shadow,
            // Windows' own caption-button red, the same in every theme.
            close_hover: r.close_hover,
            on_close_hover: r.on_close_hover,
            syn_keyword: d(r.syn_keyword, "mauve"),
            syn_string: d(r.syn_string, "green"),
            syn_comment: d(r.syn_comment, "overlay1"),
            syn_number: d(r.syn_number, "peach"),
            syn_type: d(r.syn_type, "yellow"),
            syn_function: d(r.syn_function, "blue"),
            syn_constant: d(r.syn_constant, "peach"),
            syn_macro: d(r.syn_macro, "teal"),
            syn_operator: d(r.syn_operator, "sky"),
            syn_punct: d(r.syn_punct, "overlay2"),
            syn_tag: d(r.syn_tag, "blue"),
            syn_attribute: d(r.syn_attribute, "flamingo"),
            diff_add_bg: d(r.diff_add_bg, "green"),
            diff_del_bg: d(r.diff_del_bg, "red"),
            diff_hunk_bg: d(r.diff_hunk_bg, "blue"),
            gutter_fg: d(r.gutter_fg, "overlay1"),
            range_sel_bg: d(r.range_sel_bg, "blue"),
            thread_bar: d(r.thread_bar, "mauve"),
        };
        let ansi_keys = if p.kind == ThemeKind::Mocha { MOCHA_ANSI_KEYS } else { LATTE_ANSI_KEYS };
        for ((slot, key), named) in t.term_ansi.iter_mut().zip(ansi_keys).zip(p.term.ansi) {
            *slot = named.unwrap_or_else(|| d(*slot, key));
        }
        t.term_fg = p.term.foreground.unwrap_or(t.term_fg);
        t.term_bg = p.term.background.unwrap_or(t.term_bg);
        t.term_cursor = p.term.cursor.unwrap_or(t.term_cursor);
        t.term_selection = p.term.selection.unwrap_or(t.term_selection);
        t
    }
}

/// Mocha's ANSI 0-15 (the emulator's defaults) and the palette slot each one is.
const MOCHA_ANSI: [u32; 16] =
    [0x45475a, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xbac2de, 0x585b70, 0xf38ba8, 0xa6e3a1, 0xf9e2af, 0x89b4fa, 0xf5c2e7, 0x94e2d5, 0xa6adc8];
const MOCHA_ANSI_KEYS: [&str; 16] =
    ["surface1", "red", "green", "yellow", "blue", "pink", "teal", "subtext1", "surface2", "red", "green", "yellow", "blue", "pink", "teal", "subtext0"];
/// Latte's ANSI 0-15 and their slots.
const LATTE_ANSI: [u32; 16] =
    [0x5c5f77, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xacb0be, 0x6c6f85, 0xd20f39, 0x40a02b, 0xdf8e1d, 0x1e66f5, 0xea76cb, 0x179299, 0xbcc0cc];
const LATTE_ANSI_KEYS: [&str; 16] =
    ["subtext1", "red", "green", "yellow", "blue", "pink", "teal", "surface2", "subtext0", "red", "green", "yellow", "blue", "pink", "teal", "surface1"];

/// `token` moved by the step from `from` to `to` (each channel, clamped), its alpha scaled by
/// `to`'s. Unchanged when the slot is unchanged, so a base theme's tokens are exact.
fn shift(token: Color32, from: Color32, to: Color32) -> Color32 {
    if from == to {
        return token;
    }
    let [r, g, b, a] = token.to_srgba_unmultiplied();
    let [fr, fg, fb, _] = from.to_srgba_unmultiplied();
    let [tr, tg, tb, ta] = to.to_srgba_unmultiplied();
    let ch = |c: u8, f: u8, t: u8| (i16::from(c) + i16::from(t) - i16::from(f)).clamp(0, 255) as u8;
    let alpha = (u16::from(a) * u16::from(ta) / 255) as u8;
    Color32::from_rgba_unmultiplied(ch(r, fr, tr), ch(g, fg, tg), ch(b, fb, tb), alpha)
}

/// Terminal colours a theme names (`[theme.terminal]`); `None` = derived from the palette.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct TermColors {
    pub ansi: [Option<Color32>; 16],
    pub foreground: Option<Color32>,
    pub background: Option<Color32>,
    pub cursor: Option<Color32>,
    pub selection: Option<Color32>,
}

/// A theme's colours: the 26 palette slots (`csm_core::addons::PALETTE_KEYS` order) and the
/// terminal colours it names, over a base (Mocha's tokens for a dark one, Latte's for a light).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub kind: ThemeKind,
    pub colors: [Color32; 26],
    pub term: TermColors,
}

fn color_of(c: [u8; 4]) -> Color32 {
    let [r, g, b, a] = c;
    Color32::from_rgba_unmultiplied(r, g, b, a)
}

impl Palette {
    /// Mocha's or Latte's palette.
    pub fn builtin(kind: ThemeKind) -> Palette {
        Palette { kind, colors: kind.base().palette().map(hex), term: TermColors::default() }
    }

    /// An add-on theme's palette: its colours over its base's (the manifest parser already
    /// dropped the ones that do not parse; any that still don't are the base's).
    pub fn from_spec(spec: &csm_core::addons::ThemeSpec) -> Palette {
        let kind = ThemeKind::of_base(spec.base);
        let colors = spec.resolved().map(color_of);
        let named = |k: &str| spec.terminal.get(k).and_then(|c| csm_core::addons::theme::parse_hex(c)).map(color_of);
        let mut term = TermColors {
            foreground: named("foreground"),
            background: named("background"),
            cursor: named("cursor"),
            selection: named("selection"),
            ..TermColors::default()
        };
        for (slot, (key, _)) in term.ansi.iter_mut().zip(csm_core::look::ANSI_NAMES) {
            *slot = named(key);
        }
        Palette { kind, colors, term }
    }

    /// One palette slot by name (`base`, `text`, `blue` …); black for a name that is not one.
    pub fn c(&self, key: &str) -> Color32 {
        csm_core::addons::PALETTE_KEYS.iter().position(|k| *k == key).and_then(|i| self.colors.get(i).copied()).unwrap_or(Color32::BLACK)
    }

    /// A few slots that show a theme at a glance (Settings › Appearance).
    pub fn swatches(&self) -> [Color32; 8] {
        ["base", "surface0", "text", "blue", "green", "yellow", "red", "mauve"].map(|k| self.c(k))
    }
}

/// One theme the person can choose: a built-in or an enabled add-on's.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeEntry {
    /// `mocha`, `latte`, or `addon-id.theme-id`.
    pub id: String,
    pub name: String,
    /// The add-on's name; `None` for a built-in.
    pub source: Option<String>,
    pub palette: Palette,
}

/// Every theme to choose from: the built-ins, then each enabled add-on's, in folder order.
pub fn registry(addons: &[csm_engine::addons::AddonSummary]) -> Vec<ThemeEntry> {
    let mut out: Vec<ThemeEntry> = csm_core::addons::BUILTIN_THEMES
        .iter()
        .map(|(id, name, base)| ThemeEntry { id: (*id).to_owned(), name: (*name).to_owned(), source: None, palette: Palette::builtin(ThemeKind::of_base(*base)) })
        .collect();
    for a in addons.iter().filter(|a| a.state == csm_engine::addons::AddonState::Loaded) {
        out.extend(a.themes.iter().map(|t| ThemeEntry { id: format!("{}.{}", a.id, t.id), name: t.name.clone(), source: Some(a.name.clone()), palette: Palette::from_spec(t) }));
    }
    out
}

/// The palette to draw for `kind` when the chosen theme is `id`: that theme, or the built-in
/// with the reason when it is not available (an add-on turned off or removed, an id that is
/// not a theme, a light theme chosen for dark mode).
pub fn resolve(kind: ThemeKind, id: &str, addons: &[csm_engine::addons::AddonSummary]) -> (Palette, Option<String>) {
    let id = id.trim();
    let builtin = kind.base().builtin_id();
    if id.is_empty() || id == builtin {
        return (Palette::builtin(kind), None);
    }
    let found = id.split_once('.').and_then(|(addon, theme)| {
        addons.iter().find(|a| a.id == addon && a.state == csm_engine::addons::AddonState::Loaded).and_then(|a| a.themes.iter().find(|t| t.id == theme))
    });
    match found {
        Some(t) if ThemeKind::of_base(t.base) == kind => (Palette::from_spec(t), None),
        Some(t) => (Palette::builtin(kind), Some(format!("“{}” is a {} theme, so {builtin} is drawn instead.", t.name, t.base.id()))),
        None => (Palette::builtin(kind), Some(format!("The theme `{id}` is not available (its add-on is off or gone), so {builtin} is drawn instead."))),
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
    /// The app name on the About page.
    pub const DISPLAY: f32 = 22.0;
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
    /// The logo on the About page.
    pub const ABOUT_LOGO: f32 = 56.0;
    pub const HAIRLINE: f32 = 1.0;
    pub const RING: f32 = 2.0;
    pub const STRIPE: f32 = 3.0;
    pub const TITLE_BAR: f32 = 40.0;
    /// Minimize, maximize and close at the title bar's right end: as wide as Windows' caption
    /// buttons, the bar's full height.
    pub const CAPTION_BUTTON_W: f32 = 46.0;
    /// The frameless window's resize zones: the band along each edge, and how far a corner's
    /// diagonal resize reaches along the edges.
    pub const WINDOW_RESIZE_EDGE: f32 = 5.0;
    pub const WINDOW_RESIZE_CORNER: f32 = 14.0;
    pub const GROUP_BAR: f32 = 44.0;
    /// The strip of views (`Sessions`, add-on screens) above the pane area.
    pub const VIEW_STRIP: f32 = 36.0;
    /// The widest a tab in that strip gets.
    pub const VIEW_TAB_MAX_W: f32 = 260.0;
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
    /// The Recent menu (name, folder, status, age and key per row).
    pub const RECENT_W: f32 = 400.0;
    pub const DIALOG_W: f32 = 500.0;
    pub const BOARD_W: f32 = 560.0;
    /// The Board layout: narrowest column, and one line of a card.
    pub const BOARD_COLUMN_MIN: f32 = 168.0;
    pub const BOARD_LINE: f32 = 18.0;
    pub const CONSENT_W: f32 = 380.0;
    pub const TOAST_MAX_W: f32 = 620.0;
    pub const INBOX_DEFAULT_H: f32 = 206.0;
    pub const INBOX_MIN_H: f32 = 64.0;
    pub const INBOX_MAX_H: f32 = 560.0;
    pub const SPLIT_GRIP: f32 = 10.0;
    pub const DIVIDER: f32 = 6.0;
    /// The mark column of a code or diff gutter (dots, counts, closed-thread badges).
    pub const GUTTER_MARK: f32 = 22.0;
    /// The strip right of the numbers where a thread's bar runs.
    pub const GUTTER_BAR: f32 = 8.0;
    /// A thread's bar.
    pub const THREAD_BAR: f32 = 3.0;
    /// A gutter mark's dot radius, its count's box and the step between marks.
    pub const MARK_DOT: f32 = 3.0;
    pub const MARK_COUNT: f32 = 14.0;
    pub const MARK_STEP: f32 = 10.0;
    /// A closed thread's badge in the gutter.
    pub const THREAD_BADGE_W: f32 = 18.0;
    pub const THREAD_BADGE_H: f32 = 15.0;
    /// The sideways scroll bar of a code view: its height, the gap under it and beside it, the shortest thumb.
    pub const HBAR_H: f32 = 6.0;
    pub const HBAR_GAP: f32 = 2.0;
    pub const HBAR_SIDE: f32 = 12.0;
    pub const HBAR_MIN: f32 = 24.0;
    pub const RESTORE_RAIL: f32 = 2.0;
    pub const BAR: f32 = 6.0;
    pub const GRID_TILE_W: f32 = 92.0;
    pub const GRID_GLYPH: [f32; 2] = [56.0, 36.0];
    pub const MIN_PANE: f32 = 120.0;
    pub const FLOAT_DEFAULT: [f32; 2] = [440.0, 300.0];
    /// The smallest a floating pane resizes to: room for its header's window bar and a few rows.
    pub const FLOAT_MIN: [f32; 2] = [MIN_PANE * 2.0, MIN_PANE];
    /// A floating pane's resize band, straddling each edge.
    pub const RESIZE_EDGE: f32 = 6.0;
    /// The "Drag to resize" grip in a floating pane's bottom-right corner.
    pub const RESIZE_GRIP: f32 = 14.0;
    /// The strip under the pane canvas holding the minimized panes' chips.
    pub const MINIMIZED_STRIP: f32 = 30.0;
    /// A minimized pane's chip: the widest it gets before its name elides.
    pub const MINIMIZED_CHIP_MAX_W: f32 = 180.0;
    pub const BRANCH_MAX_W: f32 = 68.0;
    /// A sidebar row narrower than this shows no usage chip (the name keeps the room).
    pub const USAGE_CHIP_MIN_ROW_W: f32 = 200.0;
    /// A sidebar row this wide also shows the session's cost in its chip.
    pub const USAGE_COST_MIN_ROW_W: f32 = 300.0;
    /// The Usage tab's context gauge.
    pub const GAUGE_H: f32 = 10.0;
    /// The Usage dashboard: its widest column, a figure tile, the daily chart's plot height,
    /// the room left of the plot for the axis labels and under it for the dates, a table row
    /// (two lines), a number column, and the share of a day's slot a bar fills.
    pub const USAGE_PAGE_MAX_W: f32 = 1180.0;
    pub const USAGE_KPI_H: f32 = 74.0;
    pub const USAGE_KPI_MIN_W: f32 = 150.0;
    pub const USAGE_CHART_H: f32 = 190.0;
    pub const USAGE_AXIS_W: f32 = 52.0;
    pub const USAGE_AXIS_H: f32 = 18.0;
    pub const USAGE_ROW_H: f32 = 40.0;
    pub const USAGE_NUM_W: f32 = 92.0;
    pub const USAGE_BAR_FILL: f32 = 0.72;
    /// A legend swatch.
    pub const SWATCH: f32 = 10.0;
    /// A date field of the custom range.
    pub const DATE_FIELD_W: f32 = 110.0;
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
    /// The pairing QR code: its side with the quiet zone, and the quiet zone in modules.
    pub const QR_SIDE: f32 = 208.0;
    pub const QR_QUIET: i32 = 4;
    /// The status dot of the remote-access indicator and the device rows.
    pub const STATUS_DOT: f32 = 8.0;
    /// The title bar indicator: the room it never takes more than.
    pub const REMOTE_PILL_MAX_W: f32 = 270.0;
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
    /// A sidebar row in the multi-selection (Ctrl/Shift-click).
    pub const PICKED: f32 = 0.2;
    /// The warning bands behind the Usage tab's context gauge.
    pub const GAUGE_BAND: f32 = 0.28;
    /// The Usage dashboard's bar under a row's cost (its share of the costliest row).
    pub const SHARE_BAR: f32 = 0.56;
    /// The empty half of a side-by-side row.
    pub const CODE_FILLER: f32 = 0.35;
    /// A gutter count's tint.
    pub const MARK: f32 = 0.28;
    /// A closed thread's badge, and under the pointer.
    pub const THREAD_BADGE: f32 = 0.25;
    pub const THREAD_BADGE_HOVER: f32 = 0.4;
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
    apply_tokens(ctx, Tokens::for_kind(kind));
}

/// Apply any theme's palette (a built-in's gives exactly [`apply`]).
pub fn apply_palette(ctx: &egui::Context, palette: &Palette) {
    apply_tokens(ctx, Tokens::from_palette(palette));
}

fn apply_tokens(ctx: &egui::Context, t: Tokens) {
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

    /// No visual change for the built-ins: their palettes give today's tokens exactly, the
    /// terminal colours included (Mocha's ANSI are the emulator's defaults).
    #[test]
    fn a_built_in_palette_gives_exactly_the_built_in_tokens() {
        for kind in ThemeKind::ALL {
            assert_eq!(Tokens::from_palette(&Palette::builtin(kind)), Tokens::for_kind(kind), "{kind:?}");
        }
        let mocha = Tokens::for_kind(ThemeKind::Mocha).terminal_style().palette;
        let default = csm_term::Palette::default();
        assert_eq!((mocha.ansi, mocha.cursor), (default.ansi, default.cursor));
        let latte = Palette::builtin(ThemeKind::Latte);
        assert_eq!(Tokens::for_kind(ThemeKind::Latte).term_ansi, LATTE_ANSI_KEYS.map(|k| latte.c(k)), "Latte's ANSI are its slots");
        let mocha_p = Palette::builtin(ThemeKind::Mocha);
        assert_eq!(Tokens::for_kind(ThemeKind::Mocha).term_ansi, MOCHA_ANSI_KEYS.map(|k| mocha_p.c(k)), "Mocha's ANSI are its slots");
    }

    fn spec(text: &str) -> csm_core::addons::ThemeSpec {
        let head = "[addon]\nid = \"t\"\nname = \"T\"\nversion = \"0.1.0\"\napi = 1\nkind = \"pack\"\n";
        csm_core::addons::parse(&format!("{head}{text}"), "t").unwrap().themes.remove(0)
    }

    #[test]
    fn a_palette_moves_the_tokens_derived_from_it_and_keeps_the_rest() {
        let s = spec("[[theme]]\nid = \"x\"\nname = \"X\"\nbase = \"dark\"\n[theme.colors]\nbase = \"#303446\"\ngreen = \"#a6d189\"\n");
        let t = Tokens::from_palette(&Palette::from_spec(&s));
        let m = Tokens::for_kind(ThemeKind::Mocha);
        assert_eq!(t.bg, hex(0x303446));
        assert_eq!(t.term_bg, hex(0x303446));
        // field is Mocha's base lifted by (5, 5, 6): the same lift over the new base.
        assert_eq!(t.field, hex(0x35394c));
        assert_eq!(t.green, hex(0xa6d189));
        // A translucent token is stored premultiplied, so its colour is only as exact as its
        // alpha allows (38/255 here): within a few steps.
        let [r, g, b, a] = t.diff_add_bg.to_srgba_unmultiplied();
        assert_eq!(a, m.diff_add_bg.a(), "the tint keeps its alpha");
        assert!([(r, 0xa6), (g, 0xd1), (b, 0x89)].iter().all(|(x, y)| x.abs_diff(*y) <= 4), "{r} {g} {b}");
        assert_eq!((t.fg, t.blue, t.mantle, t.brand), (m.fg, m.blue, m.mantle, m.brand), "slots not given stay Mocha's");
        assert_eq!(t.term_ansi[2], hex(0xa6d189), "ANSI green follows the palette");
    }

    #[test]
    fn terminal_colours_a_theme_names_win() {
        let s = spec("[[theme]]\nid = \"x\"\nname = \"X\"\nbase = \"light\"\n[theme.terminal]\nbackground = \"#ffffff\"\nbright_black = \"#101010\"\ncursor = \"#ff0000\"\nselection = \"#00ff0080\"\n");
        let t = Tokens::from_palette(&Palette::from_spec(&s));
        assert_eq!(t.kind, ThemeKind::Latte);
        assert_eq!(t.term_bg, hex(0xffffff));
        assert_eq!(t.term_ansi[8], hex(0x101010));
        assert_eq!(t.term_cursor, hex(0xff0000));
        assert_eq!(t.term_selection, Color32::from_rgba_unmultiplied(0, 255, 0, 128));
        assert_eq!(t.term_fg, Tokens::for_kind(ThemeKind::Latte).term_fg);
        let style = t.terminal_style();
        assert_eq!((style.palette.cursor.r, style.palette.background.g), (255, 255));
    }

    fn summary(state: csm_engine::addons::AddonState, themes: Vec<csm_core::addons::ThemeSpec>) -> csm_engine::addons::AddonSummary {
        csm_engine::addons::AddonSummary {
            id: "pack".into(),
            name: "Pack".into(),
            version: "0.1.0".into(),
            description: String::new(),
            kind: "pack",
            author: None,
            homepage: None,
            state,
            message: None,
            folder: "pack".into(),
            path: String::new(),
            digest: None,
            profiles: vec![],
            run_templates: vec![],
            slash: vec![],
            commands: vec![],
            process_effects: vec![],
            process_command_line: None,
            process_calls: vec![],
            process: None,
            themes,
            problems: vec![],
        }
    }

    #[test]
    fn an_unavailable_theme_falls_back_to_the_built_in_and_says_why() {
        use csm_engine::addons::AddonState;
        let dark = spec("[[theme]]\nid = \"night\"\nname = \"Night\"\nbase = \"dark\"\n[theme.colors]\nbase = \"#000000\"\n");
        let on = vec![summary(AddonState::Loaded, vec![dark.clone()])];
        let (p, why) = resolve(ThemeKind::Mocha, "pack.night", &on);
        assert_eq!((p, why), (Palette::from_spec(&dark), None));
        assert_eq!(registry(&on).iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), vec!["mocha", "latte", "pack.night"]);
        // Off, gone, not a theme, or the wrong mode: the built-in, with the reason.
        let off = vec![summary(AddonState::Disabled, vec![dark])];
        for (kind, id, addons, says) in [
            (ThemeKind::Mocha, "pack.night", &off, "not available"),
            (ThemeKind::Mocha, "pack.gone", &on, "not available"),
            (ThemeKind::Mocha, "plain", &on, "not available"),
            (ThemeKind::Latte, "pack.night", &on, "dark theme"),
            (ThemeKind::Mocha, "latte", &on, "not available"),
        ] {
            let (p, why) = resolve(kind, id, addons);
            assert_eq!(p, Palette::builtin(kind), "{id}");
            assert!(why.as_deref().is_some_and(|w| w.contains(says)), "{id}: {why:?}");
        }
        assert_eq!(registry(&off).len(), 2, "a disabled add-on offers nothing");
        assert_eq!(resolve(ThemeKind::Latte, "latte", &on), (Palette::builtin(ThemeKind::Latte), None));
    }

    /// The sample themes keep the app's own contrast rules (the ones the built-ins pass).
    #[test]
    fn the_sample_themes_keep_the_contrast_the_built_ins_keep() {
        let text = include_str!("../../../samples/addons/themes/addon.toml");
        for s in csm_core::addons::parse(text, "themes").unwrap().themes {
            let t = Tokens::from_palette(&Palette::from_spec(&s));
            for surface in [t.bg, t.mantle, t.pop, t.row_sel] {
                assert!(contrast(t.fg, surface) >= 4.5, "{}: body text {:.2}", s.id, contrast(t.fg, surface));
                assert!(contrast(t.muted, surface) >= 3.0, "{}: muted text {:.2}", s.id, contrast(t.muted, surface));
            }
            for st in [t.st_working, t.st_approval, t.st_question, t.st_done, t.st_error] {
                assert!(contrast(st, t.mantle) >= 3.0, "{}: status {st:?}", s.id);
            }
            assert!(contrast(t.term_fg, t.term_bg) >= 4.5, "{}: terminal text", s.id);
        }
    }

    #[test]
    fn theme_ids_round_trip() {
        for k in ThemeKind::ALL {
            assert_eq!(ThemeKind::from_id(k.id()), Some(k));
        }
    }
}
