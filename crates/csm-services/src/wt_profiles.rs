//! Windows Terminal profile import (C# `WindowsTerminalProfileService`, `SchemeMapper`,
//! `BuiltInTerminalSchemes`, `CursorShapeMapper`, `PaddingParser`).
//!
//! Reads WT's `settings.json` (Stable, Preview and unpackaged installs), flattens each profile
//! with `profiles.defaults`, drops hidden ones, and maps the appearance fields to the formats
//! the session's `Profile*` fields store (the WPF app's xterm.js forms, see `csm_core::look`).
//!
//! The file is JSON with comments and trailing commas, written by another program and edited by
//! hand: hostile input. A file that does not parse yields no profiles; nothing here panics.
//!
//! [`parse_settings`] and everything it calls are pure. [`load_profiles`] and [`parse_file`] read
//! files and must run on a worker thread.

use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

/// A Windows Terminal profile flattened with its inherited defaults, appearance already mapped
/// (C# `Models/WindowsTerminalProfile.cs`).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct WtProfile {
    pub guid: String,
    pub name: String,
    /// `Stable`, `Preview` or `Unpackaged`.
    pub source: String,
    /// Environment variables expanded; `cmd.exe` when the profile has none.
    pub commandline: String,
    /// `~` and environment variables expanded; empty when the profile has none.
    pub starting_directory: String,
    /// CSS family list (`'Cascadia Code', monospace`).
    pub font_family: Option<String>,
    pub font_size: Option<i32>,
    pub font_weight: Option<String>,
    pub font_ligatures: Option<bool>,
    /// `block` | `underline` | `bar`.
    pub cursor_shape: Option<String>,
    pub cursor_blink: Option<bool>,
    /// CSS shorthand (`8px 8px 8px 8px`).
    pub padding: Option<String>,
    /// 0–1, set when the profile is translucent (acrylic or an opacity below 1).
    pub background_opacity: Option<f64>,
    pub retro_effect: Option<bool>,
    /// xterm.js theme JSON (`magenta`, not WT's `purple`).
    pub color_scheme_json: Option<String>,
}

/// Where Windows Terminal keeps `settings.json`, under `%LOCALAPPDATA%`, with each install's name.
pub fn known_paths(local_app_data: &Path) -> Vec<(&'static str, PathBuf)> {
    let pkg = |p: &str| local_app_data.join("Packages").join(p).join("LocalState").join("settings.json");
    vec![
        ("Stable", pkg("Microsoft.WindowsTerminal_8wekyb3d8bbwe")),
        ("Preview", pkg("Microsoft.WindowsTerminalPreview_8wekyb3d8bbwe")),
        ("Unpackaged", local_app_data.join("Microsoft").join("Windows Terminal").join("settings.json")),
    ]
}

/// Every install's profiles (blocking: reads up to three files). Empty off Windows, or when
/// `%LOCALAPPDATA%` is unset.
pub fn load_profiles() -> Vec<WtProfile> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let Some(lad) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) else { return Vec::new() };
    let files: Vec<(String, PathBuf)> = known_paths(Path::new(&lad)).into_iter().map(|(s, p)| (s.to_string(), p)).collect();
    load_from(&files)
}

/// The profiles of these `(source, settings.json)` files, names disambiguated across files
/// (blocking).
pub fn load_from(files: &[(String, PathBuf)]) -> Vec<WtProfile> {
    let all: Vec<WtProfile> = files.iter().flat_map(|(source, path)| parse_file(path, source)).collect();
    disambiguate(all)
}

/// One file's profiles; empty when it is missing, too large or malformed (blocking).
pub fn parse_file(path: &Path, source: &str) -> Vec<WtProfile> {
    /// A settings file is a few KiB; anything this large is not one.
    const MAX_BYTES: u64 = 8 << 20;
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_BYTES => {}
        _ => return Vec::new(),
    }
    match std::fs::read(path) {
        Ok(bytes) => parse_settings(&String::from_utf8_lossy(&bytes), source, &|k| std::env::var(k).ok()),
        Err(e) => {
            log::debug!("windows terminal settings {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// When two files have a profile of the same name, both get ` (<source>)` appended.
pub fn disambiguate(all: Vec<WtProfile>) -> Vec<WtProfile> {
    let count = |name: &str| all.iter().filter(|p| p.name == name).count();
    let dup: Vec<bool> = all.iter().map(|p| count(&p.name) > 1).collect();
    all.into_iter()
        .zip(dup)
        .map(|(mut p, d)| {
            if d {
                p.name = format!("{} ({})", p.name, p.source);
            }
            p
        })
        .collect()
}

/// Parses `settings.json` text. `env` looks up environment variables (for `%VAR%` expansion).
pub fn parse_settings(text: &str, source: &str, env: &dyn Fn(&str) -> Option<String>) -> Vec<WtProfile> {
    let Ok(root) = serde_json::from_str::<Value>(&strip_jsonc(text)) else { return Vec::new() };
    let Some(root) = root.as_object() else { return Vec::new() };
    // Current files: {"profiles": {"defaults": {...}, "list": [...]}}; old ones: {"profiles": [...]}.
    let (defaults, list) = match root.get("profiles") {
        Some(Value::Object(p)) => (p.get("defaults").and_then(Value::as_object), p.get("list").and_then(Value::as_array)),
        Some(Value::Array(a)) => (None, Some(a)),
        _ => return Vec::new(),
    };
    let Some(list) = list else { return Vec::new() };
    let schemes: Vec<&Map<String, Value>> =
        root.get("schemes").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_object).collect()).unwrap_or_default();
    list.iter()
        .filter_map(Value::as_object)
        .filter(|p| p.get("hidden").and_then(Value::as_bool) != Some(true))
        .filter_map(|p| build_profile(p, defaults, &schemes, source, env))
        .collect()
}

fn build_profile(
    p: &Map<String, Value>,
    defaults: Option<&Map<String, Value>>,
    schemes: &[&Map<String, Value>],
    source: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<WtProfile> {
    let name = clean(p.get("name").and_then(Value::as_str)?);
    if name.is_empty() {
        return None;
    }
    let get = |k: &str| p.get(k).or_else(|| defaults.and_then(|d| d.get(k)));
    let get_str = |k: &str| get(k).and_then(Value::as_str);
    let get_bool = |k: &str| p.get(k).and_then(Value::as_bool).or_else(|| defaults.and_then(|d| d.get(k)).and_then(Value::as_bool));
    let get_f64 = |k: &str| p.get(k).and_then(Value::as_f64).or_else(|| defaults.and_then(|d| d.get(k)).and_then(Value::as_f64));

    // Win32 CreateProcess does not expand %SystemRoot%: an unexpanded path is "file not found".
    let commandline = clean(&expand_env(get_str("commandline").unwrap_or("cmd.exe"), env));
    let starting_directory = clean(&expand_starting_directory(get_str("startingDirectory").unwrap_or(""), env));
    let (cursor_shape, cursor_blink) = map_cursor_shape(get_str("cursorShape"));
    let padding = get_str("padding").and_then(parse_padding);

    // WT's opacity is a percentage today (0–100) and was a fraction in older files.
    let opacity = get_f64("opacity").filter(|o| o.is_finite()).map(|o| if o > 1.0 { o / 100.0 } else { o }).map(|o| o.clamp(0.0, 1.0));
    let acrylic = get_bool("useAcrylic").unwrap_or(false);
    let effective = opacity.unwrap_or(1.0);
    let background_opacity = (acrylic || effective < 1.0).then_some(effective);
    let retro_effect = get_bool("experimental.retroTerminalEffect");

    let color_scheme_json = get_str("colorScheme").filter(|n| !n.is_empty()).and_then(|n| {
        let found = schemes.iter().find(|s| s.get("name").and_then(Value::as_str).is_some_and(|x| x.eq_ignore_ascii_case(n)));
        match found {
            Some(s) => scheme_to_xterm_json(s, effective),
            None => builtin_scheme(n).and_then(|s| s.as_object().and_then(|o| scheme_to_xterm_json(o, effective))),
        }
    });

    let font = resolve_font(p, defaults);
    Some(WtProfile {
        guid: clean(p.get("guid").and_then(Value::as_str).unwrap_or("")),
        name,
        source: source.to_string(),
        commandline,
        starting_directory,
        font_family: font.face,
        font_size: font.size,
        font_weight: font.weight,
        font_ligatures: font.ligatures,
        cursor_shape: cursor_shape.map(str::to_string),
        cursor_blink,
        padding,
        background_opacity,
        retro_effect,
        color_scheme_json,
    })
}

/// Control characters removed (a name or path from a file must not carry escapes into the UI or
/// a command line), capped.
fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(1024).collect::<String>().trim().to_string()
}

struct Font {
    face: Option<String>,
    size: Option<i32>,
    weight: Option<String>,
    ligatures: Option<bool>,
}

/// `font.face/size/weight/features.calt`, profile first then defaults; the legacy
/// `fontFace`/`fontSize`/`fontWeight` keys when the object is absent.
fn resolve_font(p: &Map<String, Value>, defaults: Option<&Map<String, Value>>) -> Font {
    fn from<'a>(m: &'a Map<String, Value>, child: &str, legacy: &str) -> Option<&'a Value> {
        m.get("font").and_then(Value::as_object).and_then(|f| f.get(child)).or_else(|| m.get(legacy))
    }
    let pick = |child: &str, legacy: &str| -> Option<&Value> { from(p, child, legacy).or_else(|| defaults.and_then(|d| from(d, child, legacy))) };
    let face = pick("face", "fontFace").and_then(Value::as_str).and_then(quote_font_face);
    let size = pick("size", "fontSize").and_then(Value::as_f64).filter(|s| s.is_finite() && *s >= 1.0 && *s <= 200.0).map(|s| s.round() as i32);
    let weight = match pick("weight", "fontWeight") {
        Some(Value::String(w)) => Some(clean(w)).filter(|w| !w.is_empty()),
        Some(Value::Number(n)) => n.as_i64().map(|n| n.to_string()),
        _ => None,
    };
    let ligatures = pick("features", "").and_then(Value::as_object).and_then(|f| f.get("calt")).and_then(Value::as_i64).map(|c| c != 0);
    Font { face, size, weight, ligatures }
}

/// WT stores the bare face; the session field is the WPF app's CSS family list.
fn quote_font_face(face: &str) -> Option<String> {
    let face = clean(face);
    if face.is_empty() {
        return None;
    }
    if face.starts_with(['\'', '"']) {
        return Some(face);
    }
    Some(format!("'{face}', monospace"))
}

/// `~` → `%USERPROFILE%`, then environment variables.
fn expand_starting_directory(dir: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    if dir.is_empty() {
        return String::new();
    }
    let dir = match dir.strip_prefix('~') {
        Some(rest) => format!("%USERPROFILE%{rest}"),
        None => dir.to_string(),
    };
    expand_env(&dir, env)
}

/// .NET `Environment.ExpandEnvironmentVariables`: each `%NAME%` that names a variable is
/// replaced; unknown names and a lone `%` stay as written.
pub fn expand_env(s: &str, env: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        let (before, after) = rest.split_at(start);
        out.push_str(before);
        let tail = after.get(1..).unwrap_or("");
        match tail.find('%') {
            Some(end) => {
                let name = tail.get(..end).unwrap_or("");
                match (!name.is_empty()).then(|| env(name)).flatten() {
                    Some(v) => {
                        out.push_str(&v);
                        rest = tail.get(end + 1..).unwrap_or("");
                    }
                    None => {
                        // Not a variable: keep the first `%`, and let the second start the next try.
                        out.push('%');
                        out.push_str(name);
                        rest = tail.get(end..).unwrap_or("");
                    }
                }
            }
            None => {
                out.push_str(after);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// WT `cursorShape` → the session's shape (`block` | `underline` | `bar`), plus a forced blink
/// where no exact equivalent exists (C# `CursorShapeMapper`).
pub fn map_cursor_shape(wt: Option<&str>) -> (Option<&'static str>, Option<bool>) {
    match wt {
        Some("bar") => (Some("bar"), None),
        Some("filledBox" | "vintage") => (Some("block"), None),
        // The closest visual approximation.
        Some("emptyBox") => (Some("block"), Some(false)),
        Some("underscore" | "doubleUnderscore") => (Some("underline"), None),
        _ => (None, None),
    }
}

/// WT `padding` (1, 2 or 4 comma-separated whole numbers) → CSS shorthand (C# `PaddingParser`).
pub fn parse_padding(input: &str) -> Option<String> {
    if input.trim().is_empty() {
        return None;
    }
    let parts: Vec<&str> = input.split(',').collect();
    if !matches!(parts.len(), 1 | 2 | 4) {
        return None;
    }
    let nums: Option<Vec<u32>> = parts.iter().map(|p| p.trim().parse::<u32>().ok()).collect();
    Some(nums?.iter().map(|n| format!("{n}px")).collect::<Vec<_>>().join(" "))
}

/// WT scheme key → xterm.js theme key (C# `SchemeMapper`).
const KEY_MAP: [(&str, &str); 20] = [
    ("background", "background"),
    ("foreground", "foreground"),
    ("cursorColor", "cursor"),
    ("selectionBackground", "selectionBackground"),
    ("black", "black"),
    ("red", "red"),
    ("green", "green"),
    ("yellow", "yellow"),
    ("blue", "blue"),
    ("purple", "magenta"),
    ("cyan", "cyan"),
    ("white", "white"),
    ("brightBlack", "brightBlack"),
    ("brightRed", "brightRed"),
    ("brightGreen", "brightGreen"),
    ("brightYellow", "brightYellow"),
    ("brightBlue", "brightBlue"),
    ("brightPurple", "brightMagenta"),
    ("brightCyan", "brightCyan"),
    ("brightWhite", "brightWhite"),
];

/// A WT scheme object → xterm.js theme JSON text; the background as `rgba(…)` when `opacity`
/// is below 1. Non-string values are skipped.
pub fn scheme_to_xterm_json(scheme: &Map<String, Value>, opacity: f64) -> Option<String> {
    let mut theme = Map::new();
    for (wt, xterm) in KEY_MAP {
        if let Some(v) = scheme.get(wt).and_then(Value::as_str) {
            theme.insert(xterm.to_string(), Value::String(clean(v)));
        }
    }
    if opacity < 1.0
        && let Some(bg) = theme.get("background").and_then(Value::as_str).and_then(|bg| hex_to_rgba(bg, opacity))
    {
        theme.insert("background".into(), Value::String(bg));
    }
    Some(Value::Object(theme).to_string())
}

fn hex_to_rgba(hex: &str, opacity: f64) -> Option<String> {
    let h = hex.strip_prefix('#').filter(|h| h.len() == 6 && h.is_ascii())?;
    let b = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    Some(format!("rgba({}, {}, {}, {})", b(0)?, b(2)?, b(4)?, opacity))
}

/// The colour schemes that ship with Windows Terminal (and are usually not repeated in the
/// user's file), from the Microsoft.Terminal defaults (C# `BuiltInTerminalSchemes`).
const BUILT_IN: [(&str, [&str; 20]); 9] = [
    // name: background, foreground, cursorColor, selectionBackground, then the 16 in KEY_MAP order.
    (
        "Campbell",
        [
            "#0C0C0C", "#CCCCCC", "#FFFFFF", "#FFFFFF", "#0C0C0C", "#C50F1F", "#13A10E", "#C19C00", "#0037DA", "#881798", "#3A96DD", "#CCCCCC",
            "#767676", "#E74856", "#16C60C", "#F9F1A5", "#3B78FF", "#B4009E", "#61D6D6", "#F2F2F2",
        ],
    ),
    (
        "Campbell Powershell",
        [
            "#012456", "#CCCCCC", "#FFFFFF", "#FFFFFF", "#0C0C0C", "#C50F1F", "#13A10E", "#C19C00", "#0037DA", "#881798", "#3A96DD", "#CCCCCC",
            "#767676", "#E74856", "#16C60C", "#F9F1A5", "#3B78FF", "#B4009E", "#61D6D6", "#F2F2F2",
        ],
    ),
    (
        "Vintage",
        [
            "#000000", "#C0C0C0", "#FFFFFF", "#FFFFFF", "#000000", "#800000", "#008000", "#808000", "#000080", "#800080", "#008080", "#C0C0C0",
            "#808080", "#FF0000", "#00FF00", "#FFFF00", "#0000FF", "#FF00FF", "#00FFFF", "#FFFFFF",
        ],
    ),
    (
        "One Half Dark",
        [
            "#282C34", "#DCDFE4", "#FFFFFF", "#FFFFFF", "#282C34", "#E06C75", "#98C379", "#E5C07B", "#61AFEF", "#C678DD", "#56B6C2", "#DCDFE4",
            "#5A6374", "#E06C75", "#98C379", "#E5C07B", "#61AFEF", "#C678DD", "#56B6C2", "#DCDFE4",
        ],
    ),
    (
        "One Half Light",
        [
            "#FAFAFA", "#383A42", "#4F525D", "#FFFFFF", "#383A42", "#E45649", "#50A14F", "#C18301", "#0184BC", "#A626A4", "#0997B3", "#FAFAFA",
            "#4F525D", "#DF6C75", "#98C379", "#E4C07A", "#61AFEF", "#C577DD", "#56B5C1", "#FFFFFF",
        ],
    ),
    (
        "Solarized Dark",
        [
            "#002B36", "#839496", "#FFFFFF", "#FFFFFF", "#002B36", "#DC322F", "#859900", "#B58900", "#268BD2", "#D33682", "#2AA198", "#EEE8D5",
            "#073642", "#CB4B16", "#586E75", "#657B83", "#839496", "#6C71C4", "#93A1A1", "#FDF6E3",
        ],
    ),
    (
        "Solarized Light",
        [
            "#FDF6E3", "#657B83", "#002B36", "#FFFFFF", "#002B36", "#DC322F", "#859900", "#B58900", "#268BD2", "#D33682", "#2AA198", "#EEE8D5",
            "#073642", "#CB4B16", "#586E75", "#657B83", "#839496", "#6C71C4", "#93A1A1", "#FDF6E3",
        ],
    ),
    (
        "Tango Dark",
        [
            "#000000", "#D3D7CF", "#FFFFFF", "#FFFFFF", "#000000", "#CC0000", "#4E9A06", "#C4A000", "#3465A4", "#75507B", "#06989A", "#D3D7CF",
            "#555753", "#EF2929", "#8AE234", "#FCE94F", "#729FCF", "#AD7FA8", "#34E2E2", "#EEEEEC",
        ],
    ),
    (
        "Tango Light",
        [
            "#FFFFFF", "#555753", "#000000", "#FFFFFF", "#000000", "#CC0000", "#4E9A06", "#C4A000", "#3465A4", "#75507B", "#06989A", "#D3D7CF",
            "#555753", "#EF2929", "#8AE234", "#FCE94F", "#729FCF", "#AD7FA8", "#34E2E2", "#EEEEEC",
        ],
    ),
];

/// A built-in scheme as a WT scheme object (WT key names, `purple` included), by name
/// (case-insensitive).
pub fn builtin_scheme(name: &str) -> Option<Value> {
    let (n, colors) = BUILT_IN.iter().find(|(n, _)| n.eq_ignore_ascii_case(name.trim()))?;
    let mut m = Map::new();
    m.insert("name".into(), Value::String((*n).to_string()));
    for ((wt, _), c) in KEY_MAP.iter().zip(colors) {
        m.insert((*wt).to_string(), Value::String((*c).to_string()));
    }
    Some(Value::Object(m))
}

/// The names of the built-in schemes.
pub fn builtin_scheme_names() -> impl Iterator<Item = &'static str> {
    BUILT_IN.iter().map(|(n, _)| *n)
}

/// JSON with comments (`//`, `/* */`) and trailing commas → strict JSON. Strings (with their
/// escapes) are copied untouched; an unterminated comment or string just ends the text, and the
/// JSON parser then rejects it.
pub fn strip_jsonc(text: &str) -> String {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push('"');
                let mut escaped = false;
                for s in chars.by_ref() {
                    out.push(s);
                    if escaped {
                        escaped = false;
                    } else if s == '\\' {
                        escaped = true;
                    } else if s == '"' {
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                for s in chars.by_ref() {
                    if s == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = '\0';
                for s in chars.by_ref() {
                    if prev == '*' && s == '/' {
                        break;
                    }
                    prev = s;
                }
                out.push(' ');
            }
            _ => out.push(c),
        }
    }
    remove_trailing_commas(&out)
}

/// Drops a `,` whose next non-space character is `}` or `]` (outside strings).
fn remove_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_str = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        if c == '"' {
            in_str = true;
        }
        if c == ',' {
            let next = chars.iter().skip(i + 1).find(|n| !n.is_whitespace());
            if matches!(next, Some('}' | ']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(k: &str) -> Option<String> {
        match k.to_ascii_uppercase().as_str() {
            "USERPROFILE" => Some(r"C:\Users\alice".into()),
            "SYSTEMROOT" => Some(r"C:\Windows".into()),
            _ => None,
        }
    }

    fn fixture(name: &str) -> String {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("wt").join(name);
        std::fs::read_to_string(p).unwrap()
    }

    fn parse(name: &str) -> Vec<WtProfile> {
        parse_settings(&fixture(name), "Stable", &env)
    }

    fn json(s: &Option<String>) -> Value {
        serde_json::from_str(s.as_deref().unwrap()).unwrap()
    }

    // ---- WindowsTerminalProfileServiceTests ----

    #[test]
    fn parse_happy_path_returns_profiles_with_mapped_fields() {
        let profiles = parse("happy.json");
        assert_eq!(profiles.len(), 2);
        let ps = profiles.iter().find(|p| p.name == "PowerShell").unwrap();
        assert_eq!(ps.commandline, "pwsh.exe -NoLogo");
        assert_eq!(ps.starting_directory, r"C:\Users\alice");
        assert_eq!(ps.font_family.as_deref(), Some("'Cascadia Code', monospace"));
        assert_eq!(ps.font_size, Some(12));
        assert_eq!(ps.font_weight.as_deref(), Some("normal"));
        assert_eq!(ps.cursor_shape.as_deref(), Some("bar"));
        assert_eq!(ps.padding.as_deref(), Some("8px 8px 8px 8px"));
        assert_eq!(ps.retro_effect, None);
        assert_eq!(ps.background_opacity, None);
        assert_eq!(json(&ps.color_scheme_json)["background"], "#0C0C0C");

        let ubuntu = profiles.iter().find(|p| p.name == "Ubuntu").unwrap();
        assert_eq!(ubuntu.commandline, "wsl.exe -d Ubuntu");
        assert_eq!(ubuntu.starting_directory, r"C:\Users\alice");
        assert_eq!(ubuntu.cursor_shape.as_deref(), Some("underline"));
        assert_eq!(ubuntu.retro_effect, Some(true));
        // "One Half Dark" is not in the file: the built-in table supplies it.
        assert_eq!(json(&ubuntu.color_scheme_json)["background"], "#282C34");
    }

    #[test]
    fn parse_hidden_profile_is_excluded() {
        let profiles = parse("hidden.json");
        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "Visible");
    }

    #[test]
    fn parse_defaults_inheritance_flattens_fields() {
        let profiles = parse("inheritance.json");
        let inherits = profiles.iter().find(|p| p.name == "Inherits").unwrap();
        assert_eq!(inherits.commandline, "pwsh.exe");
        assert_eq!(inherits.font_family.as_deref(), Some("'Cascadia Mono', monospace"));
        assert_eq!(inherits.font_size, Some(11));
        assert_eq!(inherits.padding.as_deref(), Some("4px"));
        let overrides = profiles.iter().find(|p| p.name == "Overrides").unwrap();
        assert_eq!(overrides.commandline, "cmd.exe");
        assert_eq!(overrides.font_family.as_deref(), Some("'Cascadia Mono', monospace"));
        assert_eq!(overrides.padding.as_deref(), Some("12px"));
    }

    #[test]
    fn parse_malformed_returns_empty() {
        assert!(parse("malformed.json").is_empty());
    }

    #[test]
    fn parse_missing_file_returns_empty() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("wt").join("does-not-exist.json");
        assert!(parse_file(&p, "Stable").is_empty());
    }

    #[test]
    fn parse_file_reads_a_real_file() {
        let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join("wt").join("hidden.json");
        assert_eq!(parse_file(&p, "Preview").first().map(|p| p.source.as_str()), Some("Preview"));
    }

    #[test]
    fn jsonc_comments_trailing_commas_and_legacy_keys() {
        let profiles = parse("jsonc.json");
        assert_eq!(profiles.len(), 2, "{profiles:?}");
        let dev = &profiles[0];
        assert_eq!(dev.name, "Dev // shell");
        assert_eq!(dev.commandline, r#"C:\Windows\System32\cmd.exe /k "echo hi""#);
        assert_eq!(dev.starting_directory, r"C:\Users\alice\src");
        assert_eq!(dev.padding.as_deref(), Some("4px 8px"));
        // Inherited from defaults: weight as a number, calt 0 → no ligatures, emptyBox.
        assert_eq!(dev.font_weight.as_deref(), Some("600"));
        assert_eq!(dev.font_ligatures, Some(false));
        assert_eq!(dev.cursor_shape.as_deref(), Some("block"));
        assert_eq!(dev.cursor_blink, Some(false));
        assert_eq!(dev.background_opacity, Some(0.75));
        // Built-in scheme by a case-insensitive name; translucent background as rgba.
        assert_eq!(json(&dev.color_scheme_json)["background"], "rgba(12, 12, 12, 0.75)");
        let legacy = &profiles[1];
        assert_eq!(legacy.font_family.as_deref(), Some("'Consolas', monospace"));
        assert_eq!(legacy.font_size, Some(14));
        // The defaults' opacity is a percentage.
        assert_eq!(legacy.background_opacity, Some(0.85));
        assert_eq!(legacy.color_scheme_json, None);
        assert_eq!(legacy.padding, None);
    }

    #[test]
    fn legacy_array_profiles_and_name_clashes_across_installs() {
        let text = r#"{"profiles": [{"name": "cmd", "commandline": "cmd.exe"}]}"#;
        let a = parse_settings(text, "Stable", &env);
        let b = parse_settings(text, "Preview", &env);
        assert_eq!(a.len(), 1);
        let all = disambiguate(a.into_iter().chain(b).collect());
        let names: Vec<&str> = all.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["cmd (Stable)", "cmd (Preview)"]);
    }

    #[test]
    fn hostile_input_never_panics() {
        let deep = "[".repeat(10_000);
        for text in [
            "",
            "null",
            "[]",
            "{\"profiles\": 3}",
            "{\"profiles\": {\"list\": {}}}",
            "{\"profiles\": {\"list\": [1, null, {\"name\": 5}, {\"name\": \"\"}]}}",
            "{\"profiles\": {\"list\": [{\"name\": \"x\", \"font\": 3, \"padding\": 9, \"opacity\": \"a\", \"colorScheme\": 4}]}, \"schemes\": 7}",
            "/* unterminated",
            "{\"a\": \"unterminated",
            deep.as_str(),
            "{\"profiles\": {\"list\": [{\"name\": \"\\u001b[31mred\", \"commandline\": \"%%%\", \"font\": {\"size\": 1e308}}]}}",
        ] {
            let _ = parse_settings(text, "Stable", &env);
        }
        let p = parse_settings(r#"{"profiles": {"list": [{"name": "\u001b[31mred", "commandline": "%%%"}]}}"#, "Stable", &env);
        assert_eq!(p[0].name, "[31mred");
        assert_eq!(p[0].commandline, "%%%");
    }

    // ---- PaddingParserTests ----

    #[test]
    fn padding_valid_shorthand() {
        for (input, want) in [
            ("8", "8px"),
            ("0", "0px"),
            ("8, 12", "8px 12px"),
            ("4, 8, 4, 8", "4px 8px 4px 8px"),
            ("  6  ,7  ", "6px 7px"),
            ("8,8,8,8", "8px 8px 8px 8px"),
        ] {
            assert_eq!(parse_padding(input).as_deref(), Some(want), "{input}");
        }
    }

    #[test]
    fn padding_invalid_or_unsupported() {
        for input in ["", "   ", "abc", "8, 8, 8", "8, 8, 8, 8, 8", "-1"] {
            assert_eq!(parse_padding(input), None, "{input}");
        }
    }

    // ---- CursorShapeMapperTests ----

    #[test]
    fn cursor_known_shapes() {
        assert_eq!(map_cursor_shape(Some("bar")), (Some("bar"), None));
        assert_eq!(map_cursor_shape(Some("filledBox")), (Some("block"), None));
        assert_eq!(map_cursor_shape(Some("vintage")), (Some("block"), None));
        assert_eq!(map_cursor_shape(Some("emptyBox")), (Some("block"), Some(false)));
        assert_eq!(map_cursor_shape(Some("underscore")), (Some("underline"), None));
        assert_eq!(map_cursor_shape(Some("doubleUnderscore")), (Some("underline"), None));
    }

    #[test]
    fn cursor_unknown_or_empty() {
        for s in [Some(""), None, Some("nonsense")] {
            assert_eq!(map_cursor_shape(s), (None, None));
        }
    }

    // ---- BuiltInTerminalSchemesTests ----

    #[test]
    fn builtin_names_resolve() {
        for n in [
            "Campbell",
            "Campbell Powershell",
            "Vintage",
            "One Half Dark",
            "One Half Light",
            "Solarized Dark",
            "Solarized Light",
            "Tango Dark",
            "Tango Light",
        ] {
            let s = builtin_scheme(n).unwrap();
            assert!(s["background"].as_str().unwrap().starts_with('#'));
        }
        assert_eq!(builtin_scheme_names().count(), 9);
        assert!(builtin_scheme("nonsense").is_none());
        assert!(builtin_scheme("").is_none());
    }

    // ---- SchemeMapperTests ----

    #[test]
    fn scheme_maps_to_xterm_keys() {
        let s: Value = serde_json::from_str(
            r##"{"name":"Demo","background":"#0C0C0C","foreground":"#CCCCCC","cursorColor":"#FFFFFF","selectionBackground":"#264F78",
                "purple":"#881798","brightPurple":"#B4009E"}"##,
        )
        .unwrap();
        let out = json(&scheme_to_xterm_json(s.as_object().unwrap(), 1.0));
        assert_eq!(out["background"], "#0C0C0C");
        assert_eq!(out["foreground"], "#CCCCCC");
        assert_eq!(out["cursor"], "#FFFFFF");
        assert_eq!(out["selectionBackground"], "#264F78");
        assert_eq!(out["magenta"], "#881798");
        assert_eq!(out["brightMagenta"], "#B4009E");
        assert!(out.get("purple").is_none());
        assert!(out.get("brightPurple").is_none());
    }

    #[test]
    fn scheme_missing_cursor_omits_key_and_opacity_rewrites_background() {
        let s: Value = serde_json::from_str(r##"{"name":"x","background":"#0C0C0C","foreground":"#FFFFFF"}"##).unwrap();
        let out = json(&scheme_to_xterm_json(s.as_object().unwrap(), 1.0));
        assert!(out.get("cursor").is_none());
        let out = json(&scheme_to_xterm_json(s.as_object().unwrap(), 0.8));
        assert_eq!(out["background"], "rgba(12, 12, 12, 0.8)");
    }

    // ---- helpers ----

    #[test]
    fn env_expansion_matches_dotnet() {
        assert_eq!(expand_env("%SystemRoot%\\x", &env), r"C:\Windows\x");
        assert_eq!(expand_env("%NOPE%\\x", &env), r"%NOPE%\x");
        assert_eq!(expand_env("100%", &env), "100%");
        assert_eq!(expand_env("%NOPE%%USERPROFILE%", &env), r"%NOPE%C:\Users\alice");
        assert_eq!(expand_env("%%", &env), "%%");
    }

    #[test]
    fn known_paths_are_the_three_installs() {
        let p = known_paths(Path::new("L"));
        let names: Vec<&str> = p.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["Stable", "Preview", "Unpackaged"]);
        assert!(p[0].1.ends_with("Packages/Microsoft.WindowsTerminal_8wekyb3d8bbwe/LocalState/settings.json"));
        assert!(p[2].1.ends_with("Microsoft/Windows Terminal/settings.json"));
    }

    #[test]
    fn jsonc_stripping_keeps_strings() {
        let s = strip_jsonc("{\"a\": \"// not a comment, /* nor this */\", /* x */ \"b\": [1, 2,], // end\n}");
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["a"], "// not a comment, /* nor this */");
        assert_eq!(v["b"], serde_json::json!([1, 2]));
        let esc = strip_jsonc(r#"{"a": "q\"//x", "b": 1,}"#);
        let v: Value = serde_json::from_str(&esc).unwrap();
        assert_eq!(v["a"], "q\"//x");
    }
}
