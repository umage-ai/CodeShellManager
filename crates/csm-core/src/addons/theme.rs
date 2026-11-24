//! `[[theme]]` entries of an add-on pack (docs/addons.md, "Themes"): a colour palette, no code.
//!
//! A theme names the Catppuccin palette slots the app's design tokens are derived from
//! ([`PALETTE_KEYS`]) and, optionally, the terminal's colours ([`TERMINAL_KEYS`]). Every key is
//! optional: a missing one is the chosen base theme's (Mocha for `dark`, Latte for `light`). A
//! colour that does not parse is skipped and reported; low contrast is reported as a warning and
//! the theme still loads. Pure, hostile input: nothing here panics.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use toml::{Table, Value};

use super::manifest::{MAX_DESCRIPTION, MAX_NAME, Problem, clean_text, valid_id};

/// The palette slots, in Catppuccin's order (accents first, then text and surfaces).
pub const PALETTE_KEYS: [&str; 26] = [
    "rosewater", "flamingo", "pink", "mauve", "red", "maroon", "peach", "yellow", "green", "teal", "sky", "sapphire", "blue", "lavender",
    "text", "subtext1", "subtext0", "overlay2", "overlay1", "overlay0", "surface2", "surface1", "surface0", "base", "mantle", "crust",
];

/// The terminal's colours: the 16 ANSI slots (as `crate::look::ANSI_NAMES` names them), then
/// the default foreground and background, the cursor and the selection's background.
pub const TERMINAL_KEYS: [&str; 20] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white", "bright_black", "bright_red", "bright_green", "bright_yellow",
    "bright_blue", "bright_magenta", "bright_cyan", "bright_white", "foreground", "background", "cursor", "selection",
];

/// Catppuccin Mocha (MIT, github.com/catppuccin/palette), in [`PALETTE_KEYS`] order.
pub const MOCHA: [u32; 26] = [
    0xf5e0dc, 0xf2cdcd, 0xf5c2e7, 0xcba6f7, 0xf38ba8, 0xeba0ac, 0xfab387, 0xf9e2af, 0xa6e3a1, 0x94e2d5, 0x89dceb, 0x74c7ec, 0x89b4fa, 0xb4befe,
    0xcdd6f4, 0xbac2de, 0xa6adc8, 0x9399b2, 0x7f849c, 0x6c7086, 0x585b70, 0x45475a, 0x313244, 0x1e1e2e, 0x181825, 0x11111b,
];

/// Catppuccin Latte, in [`PALETTE_KEYS`] order.
pub const LATTE: [u32; 26] = [
    0xdc8a78, 0xdd7878, 0xea76cb, 0x8839ef, 0xd20f39, 0xe64553, 0xfe640b, 0xdf8e1d, 0x40a02b, 0x179299, 0x04a5e5, 0x209fb5, 0x1e66f5, 0x7287fd,
    0x4c4f69, 0x5c5f77, 0x6c6f85, 0x7c7f93, 0x8c8fa1, 0x9ca0b0, 0xacb0be, 0xbcc0cc, 0xccd0da, 0xeff1f5, 0xe6e9ef, 0xdce0e8,
];

/// The built-in themes: `(id, name, base)`. Add-on themes are `addon-id.theme-id`.
pub const BUILTIN_THEMES: [(&str, &str, ThemeBase); 2] = [("mocha", "Catppuccin Mocha", ThemeBase::Dark), ("latte", "Catppuccin Latte", ThemeBase::Light)];

/// Body text must reach this contrast on the surfaces it sits on (WCAG AA).
pub const MIN_TEXT_CONTRAST: f64 = 4.5;
/// Secondary text (`overlay2`, drawn for captions and meta text).
pub const MIN_MUTED_CONTRAST: f64 = 3.0;

/// Whether a theme is drawn as a dark or a light one: decides the fallback palette, egui's own
/// visuals, and which of the two theme choices (Settings › Appearance) it is offered for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThemeBase {
    Dark,
    Light,
}

impl ThemeBase {
    pub fn id(self) -> &'static str {
        match self {
            ThemeBase::Dark => "dark",
            ThemeBase::Light => "light",
        }
    }

    pub fn parse(s: &str) -> Option<ThemeBase> {
        match s {
            "dark" => Some(ThemeBase::Dark),
            "light" => Some(ThemeBase::Light),
            _ => None,
        }
    }

    /// The base theme's palette, in [`PALETTE_KEYS`] order.
    pub fn palette(self) -> [u32; 26] {
        match self {
            ThemeBase::Dark => MOCHA,
            ThemeBase::Light => LATTE,
        }
    }

    /// The built-in theme a missing key falls back to (`mocha` / `latte`).
    pub fn builtin_id(self) -> &'static str {
        match self {
            ThemeBase::Dark => "mocha",
            ThemeBase::Light => "latte",
        }
    }
}

/// One `[[theme]]`, validated. Colours are kept as normalised hex text (`#rrggbb` or
/// `#rrggbbaa`, lowercase), only the keys that were given and parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeSpec {
    /// The id inside the add-on (`frappe`); the app's id is `addon-id.frappe`.
    pub id: String,
    pub name: String,
    pub base: ThemeBase,
    #[serde(default)]
    pub description: String,
    /// Palette slot ([`PALETTE_KEYS`]) → colour.
    #[serde(default)]
    pub colors: BTreeMap<String, String>,
    /// Terminal colour ([`TERMINAL_KEYS`]) → colour.
    #[serde(default)]
    pub terminal: BTreeMap<String, String>,
    /// `[theme.style]`: corners, borders, title bar colours and fonts. Defaults draw today's look.
    #[serde(default)]
    pub style: ThemeStyle,
}

/// Corner roundness (`corners`): every corner radius token is scaled by it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Corners {
    /// The app's own radii (4 to 10 px). The default.
    #[default]
    Rounded,
    /// No rounding at all.
    Square,
    /// One and a half times the usual radii.
    Soft,
}

impl Corners {
    pub fn parse(s: &str) -> Option<Corners> {
        match s {
            "rounded" => Some(Corners::Rounded),
            "square" => Some(Corners::Square),
            "soft" => Some(Corners::Soft),
            _ => None,
        }
    }
}

/// Edge drawing (`borders`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Borders {
    /// One hairline. The default.
    #[default]
    Flat,
    /// The classic 3D look: raised buttons and panels, sunken fields, terminal and lists.
    Bevel,
}

impl Borders {
    pub fn parse(s: &str) -> Option<Borders> {
        match s {
            "flat" => Some(Borders::Flat),
            "bevel" => Some(Borders::Bevel),
            _ => None,
        }
    }
}

/// A font a theme asks for (`ui_font`, `terminal_font`).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FontChoice {
    /// The app's own: Figtree for the UI, JetBrains Mono for the terminal. The default.
    #[default]
    Default,
    /// The bundled monospace font (JetBrains Mono); for the terminal that is the default too.
    Mono,
    /// A font file inside the add-on's folder (a path with `/`, ending in .ttf or .otf).
    File(String),
}

/// The title bar's colours, as normalised hex. Missing: the theme's own surface and text.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TitleStyle {
    pub background: Option<String>,
    pub text: Option<String>,
    pub inactive_background: Option<String>,
    pub inactive_text: Option<String>,
}

/// Largest font file a theme may bring, in bytes.
pub const MAX_FONT_BYTES: u64 = 4 * 1024 * 1024;
/// The `ui_font_scale` range, in percent.
pub const FONT_SCALE_PCT: std::ops::RangeInclusive<u16> = 50..=200;

/// `[theme.style]`: how the theme draws beyond its colours. Every field has a default that is
/// today's look, so a theme without the table is unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThemeStyle {
    #[serde(default)]
    pub corners: Corners,
    #[serde(default)]
    pub borders: Borders,
    #[serde(default)]
    pub titlebar: TitleStyle,
    #[serde(default)]
    pub ui_font: FontChoice,
    /// The UI font's size, in percent of the app's (a pixel font wants its native size).
    #[serde(default = "hundred")]
    pub ui_font_scale: u16,
    #[serde(default)]
    pub terminal_font: FontChoice,
}

fn hundred() -> u16 {
    100
}

impl Default for ThemeStyle {
    fn default() -> Self {
        ThemeStyle {
            corners: Corners::default(),
            borders: Borders::default(),
            titlebar: TitleStyle::default(),
            ui_font: FontChoice::default(),
            ui_font_scale: 100,
            terminal_font: FontChoice::default(),
        }
    }
}

impl ThemeStyle {
    /// The font files this style names (relative paths), UI font first.
    pub fn font_files(&self) -> Vec<&str> {
        [&self.ui_font, &self.terminal_font]
            .into_iter()
            .filter_map(|f| match f {
                FontChoice::File(p) => Some(p.as_str()),
                _ => None,
            })
            .collect()
    }
}

/// Whether `rel` is a font path a theme may name: relative, with `/` separators, no `..`, no drive
/// or root, ending in `.ttf` or `.otf`. Checked again, against the disk, where the file is read.
pub fn check_font_path(rel: &str) -> Result<(), String> {
    if rel.is_empty() || rel.len() > 200 {
        return Err("the path must be 1 to 200 characters".into());
    }
    if rel.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')) {
        return Err("the path may use only `/` as a separator (no `\\`, drive letters or wildcards)".into());
    }
    if rel.starts_with('/') {
        return Err("the path must be relative to the add-on's folder".into());
    }
    if rel.split('/').any(|c| c.is_empty() || c == "." || c == "..") {
        return Err("the path may not contain `..`, `.` or empty parts".into());
    }
    let lower = rel.to_ascii_lowercase();
    if !(lower.ends_with(".ttf") || lower.ends_with(".otf")) {
        return Err("the font must be a .ttf or .otf file".into());
    }
    Ok(())
}

impl ThemeSpec {
    /// The full palette as RGBA, in [`PALETTE_KEYS`] order: the theme's colours over its base's.
    pub fn resolved(&self) -> [[u8; 4]; 26] {
        let base = self.base.palette();
        let mut out = [[0, 0, 0, 255]; 26];
        for ((slot, key), fallback) in out.iter_mut().zip(PALETTE_KEYS).zip(base) {
            *slot = self.colors.get(key).and_then(|c| parse_hex(c)).unwrap_or_else(|| rgba(fallback));
        }
        out
    }

    /// One resolved palette colour by key (`None` for a key that is not a palette slot).
    pub fn color(&self, key: &str) -> Option<[u8; 4]> {
        let i = PALETTE_KEYS.iter().position(|k| *k == key)?;
        self.resolved().get(i).copied()
    }
}

/// An opaque RGBA from `0xrrggbb`.
pub const fn rgba(rgb: u32) -> [u8; 4] {
    [(rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8, 255]
}

/// `#rrggbb` or `#rrggbbaa` (either case). Anything else is `None`.
pub fn parse_hex(s: &str) -> Option<[u8; 4]> {
    let h = s.trim().strip_prefix('#')?;
    if !(h.len() == 6 || h.len() == 8) || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    Some([byte(0)?, byte(2)?, byte(4)?, if h.len() == 8 { byte(6)? } else { 255 }])
}

/// The normalised text form of a colour (alpha only when it is not opaque).
pub fn to_hex(c: [u8; 4]) -> String {
    let [r, g, b, a] = c;
    if a == 255 { format!("#{r:02x}{g:02x}{b:02x}") } else { format!("#{r:02x}{g:02x}{b:02x}{a:02x}") }
}

/// WCAG 2 contrast ratio of two colours (alpha ignored: the check is about the opaque colours).
pub fn contrast(a: [u8; 4], b: [u8; 4]) -> f64 {
    fn lum(c: [u8; 4]) -> f64 {
        let ch = |v: u8| {
            let v = f64::from(v) / 255.0;
            if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        let [r, g, b, _] = c;
        0.2126 * ch(r) + 0.7152 * ch(g) + 0.0722 * ch(b)
    }
    let (la, lb) = (lum(a), lum(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Parses one `[[theme]]` table: the spec, plus problems that did not stop it loading (a bad or
/// unknown colour key, low contrast). `Err` when the entry cannot be a theme at all.
pub(crate) fn parse_theme(t: &Table) -> Result<(ThemeSpec, Vec<Problem>), String> {
    let id = t.get("id").and_then(Value::as_str).ok_or("`id` must be a string")?;
    if !valid_id(id) {
        return Err(format!("theme id `{}` must be 1 to 64 characters: a-z, 0-9 and '-'", clean_text(id, 40)));
    }
    let scope = format!("theme `{id}`");
    let name = clean_text(t.get("name").and_then(Value::as_str).ok_or_else(|| format!("{scope}: `name` must be a string"))?, MAX_NAME);
    if name.is_empty() {
        return Err(format!("{scope}: `name` is empty"));
    }
    let base_text = t.get("base").and_then(Value::as_str).ok_or_else(|| format!("{scope}: `base` must be \"dark\" or \"light\""))?;
    let base = ThemeBase::parse(base_text).ok_or_else(|| format!("{scope}: base `{}` is not \"dark\" or \"light\"", clean_text(base_text, 40)))?;
    let description = t.get("description").and_then(Value::as_str).map(|d| clean_text(d, MAX_DESCRIPTION)).unwrap_or_default();
    let mut problems = Vec::new();
    for key in t.keys() {
        if !matches!(key.as_str(), "id" | "name" | "base" | "description" | "colors" | "terminal" | "style") {
            problems.push(Problem { scope: scope.clone(), message: format!("unknown key `{}`; ignored", clean_text(key, 40)) });
        }
    }
    let colors = color_table(t, "colors", &PALETTE_KEYS, &scope, &mut problems);
    let terminal = color_table(t, "terminal", &TERMINAL_KEYS, &scope, &mut problems);
    let style = parse_style(t, &scope, &mut problems);
    let spec = ThemeSpec { id: id.to_owned(), name, base, description, colors, terminal, style };
    problems.extend(contrast_warnings(&spec).into_iter().map(|message| Problem { scope: scope.clone(), message }));
    Ok((spec, problems))
}

/// `[theme.style]`: each key checked on its own; a bad one is reported and keeps its default.
fn parse_style(t: &Table, scope: &str, problems: &mut Vec<Problem>) -> ThemeStyle {
    let mut style = ThemeStyle::default();
    let items = match t.get("style") {
        None => return style,
        Some(Value::Table(items)) => items,
        Some(_) => {
            problems.push(Problem { scope: scope.to_owned(), message: "`style` must be a table; ignored".into() });
            return style;
        }
    };
    let mut bad = |key: &str, message: String| problems.push(Problem { scope: scope.to_owned(), message: format!("`style.{key}` {message}") });
    for (key, value) in items {
        let shown = clean_text(key, 40);
        match key.as_str() {
            "corners" => match value.as_str().and_then(Corners::parse) {
                Some(c) => style.corners = c,
                None => bad(&shown, "must be \"rounded\", \"square\" or \"soft\"; rounded is used".into()),
            },
            "borders" => match value.as_str().and_then(Borders::parse) {
                Some(b) => style.borders = b,
                None => bad(&shown, "must be \"flat\" or \"bevel\"; flat is used".into()),
            },
            "ui_font" | "terminal_font" => {
                let choice = match value.as_str() {
                    Some("default") => Ok(FontChoice::Default),
                    Some("mono") => Ok(FontChoice::Mono),
                    Some(path) => check_font_path(path).map(|()| FontChoice::File(path.to_owned())),
                    None => Err("is not text".to_owned()),
                };
                match choice {
                    Ok(c) if key == "ui_font" => style.ui_font = c,
                    Ok(c) => style.terminal_font = if c == FontChoice::Mono { FontChoice::Default } else { c },
                    Err(why) => bad(&shown, format!("cannot be used ({why}); use \"default\", \"mono\" or a font file in the add-on's folder; the default font is used")),
                }
            }
            "ui_font_scale" => {
                let v = value.as_float().or_else(|| value.as_integer().map(|n| n as f64));
                match v.filter(|v| v.is_finite()).map(|v| (v * 100.0).round()).filter(|p| (0.0..=f64::from(u16::MAX)).contains(p)) {
                    Some(p) if FONT_SCALE_PCT.contains(&(p as u16)) => style.ui_font_scale = p as u16,
                    _ => bad(&shown, "must be a number from 0.5 to 2.0; 1.0 is used".into()),
                }
            }
            "titlebar" => match value {
                Value::Table(colors) => {
                    for (ck, cv) in colors {
                        let cshown = clean_text(ck, 40);
                        let slot = match ck.as_str() {
                            "background" => &mut style.titlebar.background,
                            "text" => &mut style.titlebar.text,
                            "inactive_background" => &mut style.titlebar.inactive_background,
                            "inactive_text" => &mut style.titlebar.inactive_text,
                            _ => {
                                bad(&format!("titlebar.{cshown}"), "is not a title bar key; ignored".into());
                                continue;
                            }
                        };
                        match cv.as_str().and_then(parse_hex) {
                            Some(c) => *slot = Some(to_hex(c)),
                            None => bad(&format!("titlebar.{cshown}"), "must be \"#rrggbb\" or \"#rrggbbaa\"; the theme's own colour is used".into()),
                        }
                    }
                }
                _ => bad(&shown, "must be a table of colours; ignored".into()),
            },
            _ => bad(&shown, "is not a style key; ignored".into()),
        }
    }
    style
}

/// A `[theme.<table>]` of colours: known keys with valid hex only; the rest reported.
fn color_table(t: &Table, table: &str, keys: &[&str], scope: &str, problems: &mut Vec<Problem>) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let items = match t.get(table) {
        None => return out,
        Some(Value::Table(items)) => items,
        Some(_) => {
            problems.push(Problem { scope: scope.to_owned(), message: format!("`{table}` must be a table of colours; ignored") });
            return out;
        }
    };
    for (key, value) in items {
        let shown = clean_text(key, 40);
        if !keys.contains(&key.as_str()) {
            problems.push(Problem { scope: scope.to_owned(), message: format!("`{table}.{shown}` is not a colour key; ignored") });
            continue;
        }
        match value.as_str().and_then(parse_hex) {
            Some(c) => {
                out.insert(key.clone(), to_hex(c));
            }
            None => {
                let text = value.as_str().map_or_else(|| "a non-string value".to_owned(), |s| format!("`{}`", clean_text(s, 40)));
                problems.push(Problem {
                    scope: scope.to_owned(),
                    message: format!("`{table}.{shown}` is {text}, not \"#rrggbb\" or \"#rrggbbaa\"; the base theme's colour is used"),
                });
            }
        }
    }
    out
}

/// A contrast check: foreground name and colour, background name and colour, the ratio needed.
type Pair = (String, [u8; 4], String, [u8; 4], f64);

/// Text-on-background pairs below WCAG's ratios, as warnings (the theme still loads).
pub fn contrast_warnings(spec: &ThemeSpec) -> Vec<String> {
    let p = |k: &str| spec.color(k).unwrap_or([0, 0, 0, 255]);
    let mut pairs: Vec<Pair> = Vec::new();
    for bg in ["base", "mantle", "crust", "surface0"] {
        pairs.push(("text".into(), p("text"), bg.into(), p(bg), MIN_TEXT_CONTRAST));
    }
    pairs.push(("subtext1".into(), p("subtext1"), "base".into(), p("base"), MIN_TEXT_CONTRAST));
    pairs.push(("overlay2".into(), p("overlay2"), "base".into(), p("base"), MIN_MUTED_CONTRAST));
    let term = |k: &str, fallback: &str| spec.terminal.get(k).and_then(|c| parse_hex(c)).unwrap_or_else(|| p(fallback));
    pairs.push(("terminal.foreground".into(), term("foreground", "text"), "terminal.background".into(), term("background", "base"), MIN_TEXT_CONTRAST));
    pairs
        .into_iter()
        .filter_map(|(fg, a, bg, b, need)| {
            let ratio = contrast(a, b);
            (ratio < need).then(|| format!("warning: {fg} on {bg} has a contrast of {ratio:.1}:1 (at least {need:.1}:1 reads well); the theme still loads"))
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn hex_parses_both_forms_and_refuses_the_rest() {
        assert_eq!(parse_hex("#1E1e2e"), Some([0x1e, 0x1e, 0x2e, 255]));
        assert_eq!(parse_hex("#11223380"), Some([0x11, 0x22, 0x33, 0x80]));
        for bad in ["1e1e2e", "#1e1e2", "#1e1e2e8", "#gggggg", "", "#", "#１２３４５６", "#1e1e2e80ff"] {
            assert_eq!(parse_hex(bad), None, "{bad}");
        }
        assert_eq!(to_hex([1, 2, 3, 255]), "#010203");
        assert_eq!(to_hex([1, 2, 3, 4]), "#01020304");
    }

    #[test]
    fn the_built_in_palettes_pass_their_own_contrast_checks() {
        for base in [ThemeBase::Dark, ThemeBase::Light] {
            let spec = ThemeSpec { id: "x".into(), name: "x".into(), base, description: String::new(), colors: BTreeMap::new(), terminal: BTreeMap::new(), style: ThemeStyle::default() };
            assert!(contrast_warnings(&spec).is_empty(), "{base:?}: {:?}", contrast_warnings(&spec));
        }
    }
    fn parse(toml_text: &str) -> (ThemeSpec, Vec<Problem>) {
        let t: Table = toml_text.parse().unwrap();
        parse_theme(&t).unwrap()
    }

    const HEAD: &str = "id = \"x\"\nname = \"X\"\nbase = \"light\"\n";

    #[test]
    fn a_theme_without_style_has_the_default_style() {
        let (spec, problems) = parse(HEAD);
        assert_eq!(spec.style, ThemeStyle::default());
        assert!(problems.is_empty(), "{problems:?}");
    }

    #[test]
    fn a_valid_style_table_is_read() {
        let (spec, problems) = parse(&format!(
            "{HEAD}[style]\ncorners = \"square\"\nborders = \"bevel\"\nui_font = \"fonts/W95FA.otf\"\nui_font_scale = 1.25\nterminal_font = \"mono\"\n[style.titlebar]\nbackground = \"#000080\"\ntext = \"#FFFFFF\"\ninactive_background = \"#808080\"\n"
        ));
        assert!(problems.is_empty(), "{problems:?}");
        let s = spec.style;
        assert_eq!((s.corners, s.borders), (Corners::Square, Borders::Bevel));
        assert_eq!(s.ui_font, FontChoice::File("fonts/W95FA.otf".into()));
        assert_eq!(s.terminal_font, FontChoice::Default, "mono is the terminal's own font");
        assert_eq!(s.ui_font_scale, 125);
        assert_eq!(s.titlebar.background.as_deref(), Some("#000080"));
        assert_eq!(s.titlebar.text.as_deref(), Some("#ffffff"));
        assert_eq!(s.titlebar.inactive_text, None);
        assert_eq!(s.font_files(), vec!["fonts/W95FA.otf"]);
    }

    #[test]
    fn bad_style_values_and_unknown_keys_are_problems_not_failures() {
        let (spec, problems) = parse(&format!(
            "{HEAD}[style]\ncorners = \"pointy\"\nborders = 3\nui_font = \"../x.ttf\"\nterminal_font = \"C:/Windows/Fonts/a.ttf\"\nui_font_scale = 9\nsparkle = true\ntitlebar = {{ background = \"navy\", glow = \"#fff\" }}\n"
        ));
        assert_eq!(spec.style, ThemeStyle::default());
        let text: Vec<&str> = problems.iter().map(|p| p.message.as_str()).collect();
        for key in ["style.corners", "style.borders", "style.ui_font`", "style.terminal_font", "style.ui_font_scale", "style.sparkle", "style.titlebar.background", "style.titlebar.glow"] {
            assert!(text.iter().any(|m| m.contains(key)), "{key} in {text:?}");
        }
        let (spec, problems) = parse(&format!("{HEAD}style = 3\n"));
        assert_eq!(spec.style, ThemeStyle::default());
        assert!(problems.iter().any(|p| p.message.contains("`style` must be a table")));
    }

    #[test]
    fn font_paths_must_stay_inside_the_add_on() {
        for ok in ["fonts/a.ttf", "A.OTF", "a/b/c.otf"] {
            assert!(check_font_path(ok).is_ok(), "{ok}");
        }
        for bad in ["", "/a.ttf", "../a.ttf", "a/../b.ttf", "a/./b.ttf", "a//b.ttf", r"a\b.ttf", "C:/a.ttf", "a.woff", "a.ttf.exe", "fonts/", "a\0.ttf"] {
            assert!(check_font_path(bad).is_err(), "{bad}");
        }
    }
}
