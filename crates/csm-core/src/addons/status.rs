//! The `csm/status` notification: one short line an add-on sets to show its state in CSM (the
//! add-on's row in Settings and a dot in the title bar). Pure: parsing and cleaning. The engine
//! rate-limits it and holds it in memory (`csm-engine::plugins`); the UI draws it as plain text.
//!
//! See `docs/addons.md` ("csm/status") and `docs/design/phone-remote.md`, section 8.3.

use serde_json::Value;

use super::screen::is_invisible;

/// The longest status text, in characters, after cleaning.
pub const MAX_STATUS_CHARS: usize = 80;

/// The tone of a status. `Idle` means "nothing to say": the title-bar dot hides for it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tone {
    Idle,
    Ok,
    Warn,
    Error,
}

impl Tone {
    /// The tone named on the wire, or `None` for any other word.
    pub fn parse(s: &str) -> Option<Tone> {
        match s {
            "idle" => Some(Tone::Idle),
            "ok" => Some(Tone::Ok),
            "warn" => Some(Tone::Warn),
            "error" => Some(Tone::Error),
            _ => None,
        }
    }
}

/// One add-on's status: cleaned text and a tone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddonStatus {
    pub text: String,
    pub tone: Tone,
}

/// Why a `csm/status` was dropped. Its words are fixed, so they can go in a log line as they are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusError {
    NotAnObject,
    NoText,
    TextEmpty,
    BadTone,
}

impl std::fmt::Display for StatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            StatusError::NotAnObject => "csm/status params are not an object",
            StatusError::NoText => "csm/status has no `text`",
            StatusError::TextEmpty => "csm/status text is empty after cleaning",
            StatusError::BadTone => "csm/status `tone` is not ok, warn, error or idle",
        })
    }
}

/// The text as CSM shows it: control characters (line breaks and tabs included) and invisible or
/// bidi-control characters removed, runs of whitespace made one space, trimmed, and at most
/// [`MAX_STATUS_CHARS`] characters. Never empty unless the input had nothing visible in it.
pub fn clean_status_text(s: &str) -> String {
    let mut out = String::new();
    let mut pending_space = false;
    for c in s.chars() {
        if c.is_whitespace() || c.is_control() {
            // A control character between two words is a space, not a join.
            pending_space = true;
            continue;
        }
        if is_invisible(c) {
            continue;
        }
        if pending_space && !out.is_empty() {
            out.push(' ');
        }
        pending_space = false;
        out.push(c);
    }
    out.chars().take(MAX_STATUS_CHARS).collect::<String>().trim_end().to_owned()
}

/// Reads the params of a `csm/status` notification: `{"text": "...", "tone": "ok"}`. The tone
/// must be one of the four words; text cleaning is [`clean_status_text`].
pub fn parse(params: &Value) -> Result<AddonStatus, StatusError> {
    let obj = params.as_object().ok_or(StatusError::NotAnObject)?;
    let raw = obj.get("text").and_then(Value::as_str).ok_or(StatusError::NoText)?;
    let tone = obj.get("tone").and_then(Value::as_str).and_then(Tone::parse).ok_or(StatusError::BadTone)?;
    let text = clean_status_text(raw);
    if text.is_empty() {
        return Err(StatusError::TextEmpty);
    }
    Ok(AddonStatus { text, tone })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_good_status_parses_with_its_tone() {
        let s = parse(&json!({"text": "Connected to relay, 2 phones", "tone": "ok"})).unwrap();
        assert_eq!(s, AddonStatus { text: "Connected to relay, 2 phones".into(), tone: Tone::Ok });
        for (word, tone) in [("warn", Tone::Warn), ("error", Tone::Error), ("idle", Tone::Idle)] {
            assert_eq!(parse(&json!({"text": "x", "tone": word})).unwrap().tone, tone);
        }
    }

    #[test]
    fn a_bad_tone_or_shape_is_refused_with_a_reason() {
        assert_eq!(parse(&json!({"text": "x", "tone": "loud"})), Err(StatusError::BadTone));
        assert_eq!(parse(&json!({"text": "x"})), Err(StatusError::BadTone));
        assert_eq!(parse(&json!({"text": 5, "tone": "ok"})), Err(StatusError::NoText));
        assert_eq!(parse(&json!({"tone": "ok"})), Err(StatusError::NoText));
        assert_eq!(parse(&json!(["x"])), Err(StatusError::NotAnObject));
        assert_eq!(parse(&json!("text")), Err(StatusError::NotAnObject));
    }

    #[test]
    fn hostile_text_is_cleaned_not_obeyed() {
        // Bidi override, zero width space, a BOM, a NUL and a line break inside the text.
        let raw = "ok\u{202E}evil\u{200B}\u{FEFF} line\nbreak\u{0}\tend";
        assert_eq!(clean_status_text(raw), "okevil line break end");
        // Nothing visible left: refused, not shown as an empty line.
        assert_eq!(parse(&json!({"text": "\u{202E}\u{200B}\n\t", "tone": "ok"})), Err(StatusError::TextEmpty));
        assert_eq!(parse(&json!({"text": "", "tone": "ok"})), Err(StatusError::TextEmpty));
    }

    #[test]
    fn the_text_is_capped_at_eighty_characters() {
        let long = "é".repeat(200);
        let s = parse(&json!({"text": long, "tone": "ok"})).unwrap();
        assert_eq!(s.text.chars().count(), MAX_STATUS_CHARS, "a cap in characters, not bytes");
        let spaced = format!("{} {}", "a".repeat(79), "b");
        assert_eq!(clean_status_text(&spaced), "a".repeat(79), "a cut never leaves a trailing space");
    }

    #[test]
    fn an_oversize_frame_text_does_not_panic() {
        let huge = "x".repeat(1_000_000);
        assert_eq!(clean_status_text(&huge).len(), MAX_STATUS_CHARS);
    }
}
