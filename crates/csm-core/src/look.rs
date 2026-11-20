//! A session's terminal look: colours, font size, cursor and padding, stamped onto a session
//! from a Windows Terminal profile (or set directly) and kept with it.
//!
//! Persisted in the WPF app's own `Profile*` fields of `state.json` (C# `ShellSession`), in the
//! formats that app wrote, so a state file round-trips between the two apps unchanged:
//!
//! * `ProfileColorSchemeJson`: an xterm.js theme object as JSON text (`background`, `foreground`,
//!   `cursor`, `selectionBackground`, `black` … `brightWhite`, with `magenta` for WT's `purple`).
//!   With an opacity below 1 the WPF app wrote the background as `rgba(r, g, b, a)`.
//! * `ProfilePadding`: CSS shorthand (`8px`, `8px 12px`, `4px 8px 4px 8px`).
//! * `ProfileCursorShape`: `block` | `underline` | `bar`.
//! * `ProfileFontFamily`: a CSS family list (`'Cascadia Code', monospace`).
//!
//! [`Look`] is the plain wire form the tools take and return. Everything read from a file is
//! hostile input: malformed values are dropped, never fatal.

use serde::{Deserialize, Serialize};

use crate::models::ShellSession;

/// An sRGB colour.
pub type Rgb = [u8; 3];

/// The cursor shapes a look can ask for (WT's six map onto these three; see
/// `csm_services::wt_profiles::map_cursor_shape`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CursorShape {
    Block,
    Underline,
    Bar,
}

impl CursorShape {
    pub fn id(self) -> &'static str {
        match self {
            CursorShape::Block => "block",
            CursorShape::Underline => "underline",
            CursorShape::Bar => "bar",
        }
    }

    pub fn from_id(s: &str) -> Option<CursorShape> {
        match s.trim().to_ascii_lowercase().as_str() {
            "block" => Some(CursorShape::Block),
            "underline" => Some(CursorShape::Underline),
            "bar" => Some(CursorShape::Bar),
            _ => None,
        }
    }
}

/// The 16 ANSI slot names, in slot order, as the wire form and the xterm theme name them.
pub const ANSI_NAMES: [(&str, &str); 16] = [
    ("black", "black"),
    ("red", "red"),
    ("green", "green"),
    ("yellow", "yellow"),
    ("blue", "blue"),
    ("magenta", "magenta"),
    ("cyan", "cyan"),
    ("white", "white"),
    ("bright_black", "brightBlack"),
    ("bright_red", "brightRed"),
    ("bright_green", "brightGreen"),
    ("bright_yellow", "brightYellow"),
    ("bright_blue", "brightBlue"),
    ("bright_magenta", "brightMagenta"),
    ("bright_cyan", "brightCyan"),
    ("bright_white", "brightWhite"),
];

/// A look's colours as `#rrggbb` text. Each is optional: a missing one is the app theme's.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct LookColors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub foreground: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    /// The selection's background.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub black: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub red: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub green: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yellow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub magenta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cyan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub white: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright_black: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright_red: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright_green: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright_yellow: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright_blue: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright_magenta: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright_cyan: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bright_white: Option<String>,
}

/// [`LookColors`] parsed: what a renderer uses. `None` = the theme's colour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ResolvedColors {
    pub background: Option<Rgb>,
    pub foreground: Option<Rgb>,
    pub cursor: Option<Rgb>,
    pub selection: Option<Rgb>,
    pub ansi: [Option<Rgb>; 16],
}

impl LookColors {
    fn ansi(&self) -> [&Option<String>; 16] {
        [
            &self.black,
            &self.red,
            &self.green,
            &self.yellow,
            &self.blue,
            &self.magenta,
            &self.cyan,
            &self.white,
            &self.bright_black,
            &self.bright_red,
            &self.bright_green,
            &self.bright_yellow,
            &self.bright_blue,
            &self.bright_magenta,
            &self.bright_cyan,
            &self.bright_white,
        ]
    }

    fn ansi_mut(&mut self) -> [&mut Option<String>; 16] {
        [
            &mut self.black,
            &mut self.red,
            &mut self.green,
            &mut self.yellow,
            &mut self.blue,
            &mut self.magenta,
            &mut self.cyan,
            &mut self.white,
            &mut self.bright_black,
            &mut self.bright_red,
            &mut self.bright_green,
            &mut self.bright_yellow,
            &mut self.bright_blue,
            &mut self.bright_magenta,
            &mut self.bright_cyan,
            &mut self.bright_white,
        ]
    }

    /// Every colour with its wire name.
    fn named(&self) -> Vec<(&'static str, &Option<String>)> {
        let mut v =
            vec![("background", &self.background), ("foreground", &self.foreground), ("cursor", &self.cursor), ("selection", &self.selection)];
        v.extend(ANSI_NAMES.iter().map(|(n, _)| *n).zip(self.ansi()));
        v
    }

    pub fn is_empty(&self) -> bool {
        self.named().iter().all(|(_, c)| c.is_none())
    }

    /// The first colour that does not parse, by name.
    pub fn invalid(&self) -> Option<(&'static str, String)> {
        self.named().into_iter().find_map(|(n, c)| c.as_ref().filter(|c| parse_css_color(c).is_none()).map(|c| (n, c.clone())))
    }

    pub fn resolve(&self) -> ResolvedColors {
        let p = |c: &Option<String>| c.as_deref().and_then(parse_css_color);
        let mut ansi = [None; 16];
        for (slot, c) in ansi.iter_mut().zip(self.ansi()) {
            *slot = p(c);
        }
        ResolvedColors {
            background: p(&self.background),
            foreground: p(&self.foreground),
            cursor: p(&self.cursor),
            selection: p(&self.selection),
            ansi,
        }
    }

    /// From an xterm.js theme object (the persisted form). Unknown keys and colours that do not
    /// parse are dropped; `None` when the text is not a JSON object.
    pub fn from_xterm_json(text: &str) -> Option<LookColors> {
        let v: serde_json::Value = serde_json::from_str(text).ok()?;
        let o = v.as_object()?;
        let get = |k: &str| o.get(k).and_then(serde_json::Value::as_str).and_then(parse_css_color).map(hex);
        let mut c = LookColors {
            background: get("background"),
            foreground: get("foreground"),
            cursor: get("cursor"),
            selection: get("selectionBackground"),
            ..Default::default()
        };
        for (slot, (_, xterm)) in c.ansi_mut().into_iter().zip(ANSI_NAMES) {
            *slot = get(xterm);
        }
        Some(c)
    }

    /// To an xterm.js theme object, the WPF app's format: the background as `rgba(…)` when
    /// `opacity` is below 1. Colours that do not parse are left out.
    pub fn to_xterm_json(&self, opacity: Option<f64>) -> String {
        let mut m = serde_json::Map::new();
        let mut put = |k: &str, c: &Option<String>| {
            if let Some(rgb) = c.as_deref().and_then(parse_css_color) {
                m.insert(k.to_string(), serde_json::Value::String(hex(rgb).to_ascii_uppercase()));
            }
        };
        put("background", &self.background);
        put("foreground", &self.foreground);
        put("cursor", &self.cursor);
        put("selectionBackground", &self.selection);
        for ((_, xterm), c) in ANSI_NAMES.iter().zip(self.ansi()) {
            put(xterm, c);
        }
        if let (Some(o), Some([r, g, b])) = (opacity.filter(|o| o.is_finite() && *o < 1.0), self.background.as_deref().and_then(parse_css_color)) {
            m.insert("background".into(), serde_json::Value::String(format!("rgba({r}, {g}, {b}, {})", o.clamp(0.0, 1.0))));
        }
        serde_json::Value::Object(m).to_string()
    }
}

/// A session's look, as tools take and return it. Every field is optional; an empty look is
/// "follow the app's settings".
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Look {
    /// The colour scheme.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub colors: Option<LookColors>,
    /// The font face (stored; the app draws every terminal with its own font).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_family: Option<String>,
    /// Points, 4–72.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_size: Option<i32>,
    /// Stored, not drawn (`normal`, `bold`, `600`, …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub font_weight: Option<String>,
    /// Stored, not drawn per session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ligatures: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_shape: Option<CursorShape>,
    /// Stored; the app's cursor does not blink.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor_blink: Option<bool>,
    /// Points between the pane's edge and the text, CSS order: 1 value (all sides), 2 (vertical,
    /// horizontal) or 4 (top, right, bottom, left). Each 0–100.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub padding: Option<Vec<u16>>,
    /// Background opacity 0–1 (stored; panes are drawn opaque).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f64>,
    /// WT's retro terminal effect (stored; not drawn).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retro: Option<bool>,
}

/// Largest padding a look may ask for, in points.
pub const MAX_PADDING: u16 = 100;

impl Look {
    pub fn is_empty(&self) -> bool {
        self.colors.as_ref().is_none_or(LookColors::is_empty)
            && self.font_family.is_none()
            && self.font_size.is_none()
            && self.font_weight.is_none()
            && self.ligatures.is_none()
            && self.cursor_shape.is_none()
            && self.cursor_blink.is_none()
            && self.padding.is_none()
            && self.opacity.is_none()
            && self.retro.is_none()
    }

    /// A plain-English reason the look cannot be stored, or `None`.
    pub fn validate(&self) -> Option<String> {
        if let Some((name, value)) = self.colors.as_ref().and_then(LookColors::invalid) {
            return Some(format!("colour `{name}` is not a #rrggbb colour: {value:?}"));
        }
        if let Some(s) = self.font_size
            && !(4..=72).contains(&s)
        {
            return Some(format!("font_size {s} is not between 4 and 72"));
        }
        if let Some(p) = &self.padding {
            if !matches!(p.len(), 1 | 2 | 4) {
                return Some("padding takes 1, 2 or 4 values".into());
            }
            if p.iter().any(|v| *v > MAX_PADDING) {
                return Some(format!("padding values are at most {MAX_PADDING}"));
            }
        }
        if let Some(o) = self.opacity
            && !(o.is_finite() && (0.0..=1.0).contains(&o))
        {
            return Some(format!("opacity {o} is not between 0 and 1"));
        }
        for (field, v) in [("font_family", &self.font_family), ("font_weight", &self.font_weight)] {
            if let Some(v) = v
                && (v.chars().any(char::is_control) || v.chars().count() > 200)
            {
                return Some(format!("`{field}` contains control characters or is too long"));
            }
        }
        None
    }

    /// The padding as (top, right, bottom, left).
    pub fn padding_edges(&self) -> Option<[u16; 4]> {
        edges(self.padding.as_deref()?)
    }

    /// The look stored on `s` (empty when it has none). Malformed stored values are dropped.
    pub fn from_session(s: &ShellSession) -> Look {
        Look {
            colors: s.profile_color_scheme_json.as_deref().and_then(LookColors::from_xterm_json).filter(|c| !c.is_empty()),
            font_family: s.profile_font_family.as_deref().and_then(unquote_font_family),
            font_size: s.profile_font_size.filter(|n| (4..=72).contains(n)),
            font_weight: s.profile_font_weight.clone().filter(|w| !w.trim().is_empty()),
            ligatures: s.profile_font_ligatures,
            cursor_shape: s.profile_cursor_shape.as_deref().and_then(CursorShape::from_id),
            cursor_blink: s.profile_cursor_blink,
            padding: s.profile_padding.as_deref().and_then(parse_css_padding).map(|p| p.to_vec()),
            opacity: s.profile_background_opacity.filter(|o| o.is_finite()).map(|o| o.clamp(0.0, 1.0)),
            retro: s.profile_retro_effect,
        }
    }

    /// Stamps this look onto `s`, replacing whatever look it had (a field this look leaves out is
    /// cleared). The values are written in the WPF app's formats.
    pub fn apply_to(&self, s: &mut ShellSession) {
        s.profile_color_scheme_json = self.colors.as_ref().filter(|c| !c.is_empty()).map(|c| c.to_xterm_json(self.opacity));
        s.profile_font_family = self.font_family.as_deref().and_then(quote_font_family);
        s.profile_font_size = self.font_size;
        s.profile_font_weight = self.font_weight.clone();
        s.profile_font_ligatures = self.ligatures;
        s.profile_cursor_shape = self.cursor_shape.map(|c| c.id().to_string());
        s.profile_cursor_blink = self.cursor_blink;
        s.profile_padding = self.padding.as_deref().and_then(edges).map(css_padding);
        s.profile_background_opacity = self.opacity;
        s.profile_retro_effect = self.retro;
    }
}

fn edges(p: &[u16]) -> Option<[u16; 4]> {
    match *p {
        [a] => Some([a, a, a, a]),
        [v, h] => Some([v, h, v, h]),
        [t, r, b, l] => Some([t, r, b, l]),
        _ => None,
    }
}

/// `#rrggbb`, lower case.
pub fn hex([r, g, b]: Rgb) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa` (alpha ignored), `rgb(r, g, b)` or `rgba(r, g, b, a)`.
pub fn parse_css_color(s: &str) -> Option<Rgb> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix('#') {
        if !h.is_ascii() || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
        return match h.len() {
            3 => {
                let n = |i: usize| h.get(i..=i).and_then(|x| u8::from_str_radix(x, 16).ok()).map(|v| v * 17);
                Some([n(0)?, n(1)?, n(2)?])
            }
            6 | 8 => Some([byte(0)?, byte(2)?, byte(4)?]),
            _ => None,
        };
    }
    let lower = s.to_ascii_lowercase();
    let inner = lower.strip_prefix("rgba(").or_else(|| lower.strip_prefix("rgb("))?.strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if !matches!(parts.len(), 3 | 4) {
        return None;
    }
    let ch = |i: usize| parts.get(i).and_then(|p| p.parse::<u8>().ok());
    Some([ch(0)?, ch(1)?, ch(2)?])
}

/// CSS padding shorthand (`8px`, `8px 12px`, `4px 8px 4px 8px`; `px` optional) as
/// (top, right, bottom, left). Three values, negatives and other units are refused.
pub fn parse_css_padding(s: &str) -> Option<[u16; 4]> {
    let vals: Option<Vec<u16>> =
        s.split_whitespace().map(|p| p.strip_suffix("px").unwrap_or(p).parse::<u16>().ok().filter(|v| *v <= MAX_PADDING)).collect();
    edges(&vals?)
}

/// The WPF form of a padding: four `Npx` values.
pub fn css_padding([t, r, b, l]: [u16; 4]) -> String {
    if t == r && r == b && b == l {
        format!("{t}px")
    } else if t == b && r == l {
        format!("{t}px {r}px")
    } else {
        format!("{t}px {r}px {b}px {l}px")
    }
}

/// `'Cascadia Code', monospace` → `Cascadia Code` (the first family, unquoted).
pub fn unquote_font_family(s: &str) -> Option<String> {
    let first = s.split(',').next()?.trim().trim_matches(['\'', '"']).trim();
    (!first.is_empty() && !first.chars().any(char::is_control)).then(|| first.to_string())
}

/// `Cascadia Code` → `'Cascadia Code', monospace` (the WPF app's CSS form, so the field still
/// works there). Already-quoted text is kept.
pub fn quote_font_family(face: &str) -> Option<String> {
    let face = face.trim();
    if face.is_empty() {
        return None;
    }
    if face.starts_with(['\'', '"']) {
        return Some(face.to_string());
    }
    Some(format!("'{face}', monospace"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn css_colours_parse_and_junk_does_not() {
        assert_eq!(parse_css_color("#0C0C0C"), Some([12, 12, 12]));
        assert_eq!(parse_css_color("#fff"), Some([255, 255, 255]));
        assert_eq!(parse_css_color("#11223344"), Some([0x11, 0x22, 0x33]));
        assert_eq!(parse_css_color("rgba(12, 12, 12, 0.8)"), Some([12, 12, 12]));
        assert_eq!(parse_css_color("rgb(1,2,3)"), Some([1, 2, 3]));
        for bad in ["", "#", "#12", "#gggggg", "#ééé", "rgba(300, 0, 0, 1)", "rgb(1,2)", "red", "#1234567"] {
            assert_eq!(parse_css_color(bad), None, "{bad}");
        }
    }

    #[test]
    fn padding_shorthand_round_trips() {
        assert_eq!(parse_css_padding("8px"), Some([8, 8, 8, 8]));
        assert_eq!(parse_css_padding("8px 12px"), Some([8, 12, 8, 12]));
        assert_eq!(parse_css_padding("4px 8px 4px 8px"), Some([4, 8, 4, 8]));
        assert_eq!(parse_css_padding("1 2 3 4"), Some([1, 2, 3, 4]));
        for bad in ["", "8px 8px 8px", "-1px", "abc", "8em", "1000px"] {
            assert_eq!(parse_css_padding(bad), None, "{bad}");
        }
        for p in [[8, 8, 8, 8], [8, 12, 8, 12], [1, 2, 3, 4]] {
            assert_eq!(parse_css_padding(&css_padding(p)), Some(p));
        }
    }

    #[test]
    fn font_family_quotes_like_the_wpf_app() {
        assert_eq!(quote_font_family("Cascadia Code").as_deref(), Some("'Cascadia Code', monospace"));
        assert_eq!(unquote_font_family("'Cascadia Code', monospace").as_deref(), Some("Cascadia Code"));
        assert_eq!(unquote_font_family("Consolas").as_deref(), Some("Consolas"));
        assert_eq!(quote_font_family("  "), None);
    }

    #[test]
    fn a_look_round_trips_through_the_session_fields() {
        let look = Look {
            colors: Some(LookColors { background: Some("#0c0c0c".into()), magenta: Some("#881798".into()), ..Default::default() }),
            font_family: Some("Cascadia Code".into()),
            font_size: Some(12),
            cursor_shape: Some(CursorShape::Bar),
            cursor_blink: Some(false),
            padding: Some(vec![8, 12]),
            opacity: Some(0.8),
            retro: Some(true),
            ..Default::default()
        };
        assert!(look.validate().is_none());
        let mut s = ShellSession::default();
        look.apply_to(&mut s);
        assert_eq!(s.profile_cursor_shape.as_deref(), Some("bar"));
        assert_eq!(s.profile_padding.as_deref(), Some("8px 12px"));
        assert_eq!(s.profile_font_family.as_deref(), Some("'Cascadia Code', monospace"));
        let json: serde_json::Value = serde_json::from_str(s.profile_color_scheme_json.as_deref().unwrap()).unwrap();
        // The WPF format: magenta (not purple), rgba background under an opacity.
        assert_eq!(json["magenta"], "#881798");
        assert_eq!(json["background"], "rgba(12, 12, 12, 0.8)");
        let back = Look::from_session(&s);
        assert_eq!(back.colors.as_ref().unwrap().background.as_deref(), Some("#0c0c0c"));
        assert_eq!(back.padding, Some(vec![8, 12, 8, 12]));
        assert_eq!(back.font_family.as_deref(), Some("Cascadia Code"));
        assert_eq!(back.cursor_shape, Some(CursorShape::Bar));
        assert_eq!(back.opacity, Some(0.8));
        // Clearing.
        Look::default().apply_to(&mut s);
        assert!(Look::from_session(&s).is_empty());
        assert!(s.profile_color_scheme_json.is_none());
    }

    #[test]
    fn hostile_stored_values_are_dropped_not_fatal() {
        let s = ShellSession {
            profile_color_scheme_json: Some("{not json".into()),
            profile_padding: Some("lots".into()),
            profile_cursor_shape: Some("triangle".into()),
            profile_font_size: Some(-3),
            profile_background_opacity: Some(f64::NAN),
            ..Default::default()
        };
        assert!(Look::from_session(&s).is_empty());
        let arr = ShellSession { profile_color_scheme_json: Some("[1,2]".into()), ..Default::default() };
        assert!(Look::from_session(&arr).colors.is_none());
    }

    #[test]
    fn validation_says_what_is_wrong() {
        let bad = |l: Look| l.validate().unwrap();
        assert!(bad(Look { font_size: Some(1), ..Default::default() }).contains("font_size"));
        assert!(bad(Look { padding: Some(vec![1, 2, 3]), ..Default::default() }).contains("padding"));
        assert!(bad(Look { opacity: Some(2.0), ..Default::default() }).contains("opacity"));
        let colors = LookColors { red: Some("nope".into()), ..Default::default() };
        assert!(bad(Look { colors: Some(colors), ..Default::default() }).contains("red"));
    }

    #[test]
    fn resolve_maps_every_slot() {
        let c = LookColors::from_xterm_json(r##"{"background":"#010203","brightWhite":"#F2F2F2","cursor":"#fff","purple":"#123456"}"##).unwrap();
        let r = c.resolve();
        assert_eq!(r.background, Some([1, 2, 3]));
        assert_eq!(r.ansi[15], Some([0xf2, 0xf2, 0xf2]));
        assert_eq!(r.cursor, Some([255, 255, 255]));
        // `purple` is WT's name; the xterm theme only knows magenta.
        assert_eq!(r.ansi[5], None);
    }
}
