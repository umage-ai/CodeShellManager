//! Parser and validator for the OSC 7501 Program Status Protocol (C# `ProgramStatusPayload`;
//! <https://www.superlogical.com/rex/docs/build/program-status>).
//!
//! Wire form: `ESC ] 7501 ; key=value:key=value ST`. Everything here is untrusted terminal
//! output, so a report that breaks a limit, carries invalid base64 or decodes to a control
//! character is dropped WHOLE, as the spec requires; nothing from it is applied.

use std::collections::HashMap;

use base64::Engine;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// The `state` of an OSC 7501 report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProgramState {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
    /// An action, not a state: removes the addressed record and its children.
    Clear,
}

/// The `kind` of a [`ProgramState::Blocked`] report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BlockedKind {
    /// Not given, given with a state other than blocked, or unrecognised.
    #[default]
    None,
    Permission,
    Question,
    Auth,
}

/// One validated OSC 7501 report. `id` is `None` for the root record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatusReport {
    pub state: ProgramState,
    pub kind: BlockedKind,
    pub progress: Option<u8>,
    pub app: Option<String>,
    pub id: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
}

impl ProgramStatusReport {
    /// A report carrying only a state (everything else absent).
    pub fn new(state: ProgramState) -> Self {
        Self { state, kind: BlockedKind::None, progress: None, app: None, id: None, title: None, message: None }
    }
}

// ---- Limits, all from the spec ----
/// The whole escape sequence, introducer and terminator included.
pub const MAX_SEQUENCE_BYTES: usize = 4096;
pub const MAX_KEY_BYTES: usize = 16;
pub const MAX_MSG_ENCODED_BYTES: usize = 2732;
pub const MAX_MSG_DECODED_BYTES: usize = 2048;
pub const MAX_TITLE_ENCODED_BYTES: usize = 256;
pub const MAX_TITLE_DECODED_BYTES: usize = 192;
pub const MAX_APP_BYTES: usize = 32;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_ID_SEGMENT_BYTES: usize = 32;
pub const MAX_ID_DEPTH: usize = 8;

/// `ESC ] 7501 ;` (7 bytes) + the longest ST, `ESC \` (2 bytes). The page hands over the
/// payload only, so the envelope is added back before comparing to [`MAX_SEQUENCE_BYTES`].
const ENVELOPE_BYTES: usize = 9;

/// The terminal's answer to the feature query `OSC 7501 ; ? ST`. Fixed bytes authored here;
/// nothing from the query is echoed. Omitting `states=`/`kinds=` means "the baseline sets".
pub const PROBE_REPLY: &str = "\x1b]7501;?\x1b\\";

/// [`PROBE_REPLY`] as it must be WRITTEN to a Windows ConPTY: each character as a
/// win32-input-mode key-down event (`ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _`).
///
/// The inbox conhost silently drops every OSC written to the pseudo console's input; encoded
/// as key events the same bytes reach the program intact. Only ever applied to fixed,
/// app-authored bytes.
pub fn probe_reply_for_conpty() -> String {
    encode_as_win32_input_mode(PROBE_REPLY)
}

/// One win32-input-mode key-down event per UTF-16 unit, which conhost turns back into the same
/// characters for the program.
pub fn encode_as_win32_input_mode(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 16);
    for unit in text.encode_utf16() {
        out.push_str("\x1b[0;0;");
        out.push_str(&unit.to_string());
        out.push_str(";1;0;1_");
    }
    out
}

/// True when the OSC payload is the feature query (`?`) rather than a report. Only an exact
/// `?` (after trimming) counts; the query carries no pairs.
pub fn is_query(payload: &str) -> bool {
    payload.trim() == "?"
}

/// Parses and validates one report payload (the text after `7501;`). `None` when the report
/// must be ignored: missing or unknown state, an invalid id, a limit exceeded, bad base64 or a
/// control character in decoded text.
pub fn try_parse(payload: &str) -> Option<ProgramStatusReport> {
    if payload.is_empty() || payload.len().saturating_add(ENVELOPE_BYTES) > MAX_SEQUENCE_BYTES {
        return None;
    }

    let mut fields: HashMap<&str, &str> = HashMap::new();
    for raw_pair in payload.split(':') {
        let pair = raw_pair.trim();
        let Some(eq) = pair.find('=') else { continue };
        if eq == 0 {
            continue; // malformed -> skip
        }
        let key = pair.get(..eq)?.trim();
        let value = pair.get(eq + 1..)?.trim();
        if !is_key_shaped(key) {
            continue; // malformed -> skip
        }
        if key.len() > MAX_KEY_BYTES {
            return None; // a LIMIT -> drop the whole report
        }
        if !is_valid_value(value) {
            continue; // malformed -> skip
        }
        fields.insert(key, value); // repeated -> last wins
    }

    let state = match *fields.get("state")? {
        "idle" => ProgramState::Idle,
        "working" => ProgramState::Working,
        "done" => ProgramState::Done,
        "blocked" => ProgramState::Blocked,
        "error" => ProgramState::Error,
        "clear" => ProgramState::Clear,
        _ => return None,
    };

    // An invalid id ignores the report rather than falling back to the root: writing a
    // malformed child report over the root would be worse than dropping it.
    let id = match fields.get("id") {
        Some(text) if is_valid_id(text) => Some((*text).to_owned()),
        Some(_) => return None,
        None => None,
    };

    // app: 1-32 of [A-Za-z0-9_.+-]; anything else is treated as absent (only a grouping label).
    let app = fields.get("app").filter(|t| is_identifier_segment(t, MAX_APP_BYTES)).map(|t| (*t).to_owned());

    let title = match fields.get("title") {
        Some(b64) => {
            if b64.len() > MAX_TITLE_ENCODED_BYTES {
                return None;
            }
            Some(try_decode_text(b64, MAX_TITLE_DECODED_BYTES)?)
        }
        None => None,
    };

    let message = match fields.get("msg") {
        Some(b64) => {
            if b64.len() > MAX_MSG_ENCODED_BYTES {
                return None;
            }
            Some(try_decode_text(b64, MAX_MSG_DECODED_BYTES)?)
        }
        None => None,
    };

    let kind = match (state, fields.get("kind")) {
        (ProgramState::Blocked, Some(&"permission")) => BlockedKind::Permission,
        (ProgramState::Blocked, Some(&"question")) => BlockedKind::Question,
        (ProgramState::Blocked, Some(&"auth")) => BlockedKind::Auth,
        _ => BlockedKind::None,
    };

    // Out-of-range or non-integer progress is treated as absent (indeterminate): it is not a
    // size limit, so it does not justify dropping an otherwise valid report.
    let progress = if matches!(state, ProgramState::Working | ProgramState::Blocked) {
        fields
            .get("progress")
            .filter(|t| (1..=3).contains(&t.len()) && t.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|t| t.parse::<u8>().ok())
            .filter(|p| *p <= 100)
    } else {
        None
    };

    Some(ProgramStatusReport { state, kind, progress, app, id, title: title.filter(|t| !t.is_empty()), message: message.filter(|t| !t.is_empty()) })
}

/// `[a-z]+`, any length; the 16-byte cap is a limit, checked separately.
pub(crate) fn is_key_shaped(key: &str) -> bool {
    !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase())
}

fn is_identifier_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'+' | b'-')
}

/// `[A-Za-z0-9_.+-]{1,max}`: one id segment, or an `app` value.
pub(crate) fn is_identifier_segment(s: &str, max_len: usize) -> bool {
    !s.is_empty() && s.len() <= max_len && s.bytes().all(is_identifier_byte)
}

/// `[A-Za-z0-9_.,+/=-]*`
pub(crate) fn is_valid_value(value: &str) -> bool {
    value.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b',' | b'+' | b'/' | b'=' | b'-'))
}

/// Slash path: 1-8 segments of `[A-Za-z0-9_.+-]{1,32}`, at most 128 bytes in all.
pub(crate) fn is_valid_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID_BYTES {
        return false;
    }
    let mut depth = 0usize;
    for segment in id.split('/') {
        depth += 1;
        if depth > MAX_ID_DEPTH || !is_identifier_segment(segment, MAX_ID_SEGMENT_BYTES) {
            return false;
        }
    }
    true
}

/// Is `c` in Unicode general category Cf (Format)? A fixed table (Unicode 15.1) because std has
/// no general-category API; C# asks `Rune.GetUnicodeCategory`.
fn is_format_char(c: char) -> bool {
    matches!(u32::from(c),
        0x00AD
        | 0x0600..=0x0605
        | 0x061C
        | 0x06DD
        | 0x070F
        | 0x0890..=0x0891
        | 0x08E2
        | 0x180E
        | 0x200B..=0x200F
        | 0x202A..=0x202E
        | 0x2060..=0x2064
        | 0x2066..=0x206F
        | 0xFEFF
        | 0xFFF9..=0xFFFB
        | 0x110BD
        | 0x110CD
        | 0x13430..=0x1343F
        | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A
        | 0xE0001
        | 0xE0020..=0xE007F)
}

/// Standard base64 with optional padding, then strict UTF-8, single line, no control
/// characters (C0, DEL, C1). Unicode format characters (bidi overrides, zero-width joiners and
/// the like) are removed rather than rejected: the spec asks terminals to neutralise them and
/// they are invisible, so stripping loses nothing a user could see.
pub(crate) fn try_decode_text(b64: &str, max_decoded_bytes: usize) -> Option<String> {
    if b64.is_empty() {
        return Some(String::new());
    }
    if b64.bytes().any(|b| matches!(b, b'-' | b'_' | b',' | b'.')) {
        return None; // not standard base64
    }

    let body = b64.trim_end_matches('=');
    let pad_count = b64.len() - body.len();
    if pad_count > 2 || body.contains('=') || body.len() % 4 == 1 {
        return None;
    }
    // Padding is optional, but if present it must be exactly the right amount.
    let needed_pad = (4 - body.len() % 4) % 4;
    if pad_count > 0 && pad_count != needed_pad {
        return None;
    }

    // Padding was validated above, so `body` is decoded unpadded. Like .NET's
    // Convert.TryFromBase64String (checked against .NET 11), non-zero unused trailing bits are
    // rejected, which is the base64 crate's default.
    const STRICT: GeneralPurpose =
        GeneralPurpose::new(&alphabet::STANDARD, GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent));
    let bytes = STRICT.decode(body).ok()?;
    if bytes.len() > max_decoded_bytes {
        return None;
    }
    let decoded = std::str::from_utf8(&bytes).ok()?; // strict UTF-8

    // By scalar value, not by UTF-16 unit: format characters outside the BMP (the tag block,
    // U+E0001 and U+E0020-E007F) are invisible all the same.
    let mut out = String::with_capacity(decoded.len());
    for c in decoded.chars() {
        if c.is_control() {
            return None; // C0, DEL, C1, including CR/LF/NEL
        }
        // U+2028 / U+2029: "single line" means no line break of any kind.
        if matches!(c, '\u{2028}' | '\u{2029}') {
            return None;
        }
        if is_format_char(c) {
            continue;
        }
        out.push(c);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
    }

    fn parse(p: &str) -> Option<ProgramStatusReport> {
        try_parse(p)
    }

    // ---- query ----

    #[test]
    fn is_query_recognises_the_feature_query() {
        for p in ["?", " ? "] {
            assert!(is_query(p), "{p:?}");
        }
    }

    #[test]
    fn is_query_rejects_reports() {
        // only an exact "?" is a query
        for p in ["?:states=idle", "??", "?x", "state=idle", ""] {
            assert!(!is_query(p), "{p:?}");
        }
    }

    #[test]
    fn probe_reply_is_osc_7501_question_mark_with_st() {
        assert_eq!(PROBE_REPLY, "\x1b]7501;?\x1b\\");
        assert_eq!(parse("?"), None); // a query is never a report
    }

    #[test]
    fn conpty_reply_is_the_probe_reply_as_win32_input_mode_key_events() {
        // Conhost drops plain OSC on the ConPTY input side; one key-down per character
        // (ESC [ Vk ; Sc ; Uc ; Kd ; Cs ; Rc _) is delivered to the program as those chars.
        let encoded = probe_reply_for_conpty();
        let events: Vec<&str> = encoded.split('_').filter(|e| !e.is_empty()).collect();
        assert_eq!(events.len(), PROBE_REPLY.encode_utf16().count());
        assert_eq!(events[0], "\x1b[0;0;27;1;0;1"); // ESC
        assert_eq!(events[1], "\x1b[0;0;93;1;0;1"); // ]
        let decoded: String = events.iter().map(|e| char::from_u32(e.split(';').nth(2).unwrap().parse().unwrap()).unwrap()).collect();
        assert_eq!(decoded, PROBE_REPLY);
        assert!(!encoded.contains("\x1b]")); // no raw OSC left to drop
    }

    // ---- states ----

    #[test]
    fn every_baseline_state_parses() {
        for (text, expected) in [
            ("idle", ProgramState::Idle),
            ("working", ProgramState::Working),
            ("done", ProgramState::Done),
            ("blocked", ProgramState::Blocked),
            ("error", ProgramState::Error),
            ("clear", ProgramState::Clear),
        ] {
            assert_eq!(parse(&format!("state={text}")).unwrap().state, expected, "{text}");
        }
    }

    #[test]
    fn missing_or_unknown_state_ignores_the_report() {
        for p in ["", "app=x", "state=sleeping", "state=Working" /* case-sensitive */, "state="] {
            assert_eq!(parse(p), None, "{p:?}");
        }
    }

    #[test]
    fn claude_code_shaped_report_parses() {
        // Exactly the field order Claude Code 2.1.319 emits.
        let r = parse(&format!("state=blocked:app=claude-code:kind=permission:progress=40:msg={}", b64("Run npm test?"))).unwrap();
        assert_eq!(r.state, ProgramState::Blocked);
        assert_eq!(r.kind, BlockedKind::Permission);
        assert_eq!(r.progress, Some(40));
        assert_eq!(r.app.as_deref(), Some("claude-code"));
        assert_eq!(r.message.as_deref(), Some("Run npm test?"));
        assert_eq!(r.id, None);
    }

    // ---- pair syntax ----

    #[test]
    fn whitespace_around_pairs_is_trimmed() {
        assert_eq!(parse("  state = working :  progress=5 ").unwrap().state, ProgramState::Working);
    }

    #[test]
    fn repeated_key_last_wins() {
        assert_eq!(parse("state=working:state=done").unwrap().state, ProgramState::Done);
    }

    #[test]
    fn malformed_pairs_are_skipped_not_fatal() {
        for p in [
            "state=working:garbage", // no '='
            "state=working:=x",      // empty key
            "state=working:Bad=1",   // uppercase key
            "state=working:k3y=1",   // digit in key
            "state=working:app=a b", // space inside a value
            "state=working:app=a;b", // ';' not in alphabet
        ] {
            let r = parse(p).unwrap_or_else(|| panic!("{p}"));
            assert_eq!(r.state, ProgramState::Working, "{p}");
            assert_eq!(r.app, None, "{p}");
        }
    }

    #[test]
    fn key_over_16_bytes_is_a_limit_and_drops_the_whole_report() {
        assert!(parse("state=working:abcdefghijklmnop=1").is_some()); // 16
        assert!(parse("state=working:abcdefghijklmnopq=1").is_none()); // 17
    }

    #[test]
    fn malformed_state_pair_means_no_state() {
        assert_eq!(parse("state=work ing"), None);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        assert!(parse("state=idle:future=1:icon=x").is_some());
    }

    // ---- kind / progress ----

    #[test]
    fn kind_parses_for_blocked() {
        for (kind, expected) in
            [("permission", BlockedKind::Permission), ("question", BlockedKind::Question), ("auth", BlockedKind::Auth), ("other", BlockedKind::None)]
        {
            assert_eq!(parse(&format!("state=blocked:kind={kind}")).unwrap().kind, expected, "{kind}");
        }
    }

    #[test]
    fn kind_is_ignored_outside_blocked() {
        assert_eq!(parse("state=working:kind=permission").unwrap().kind, BlockedKind::None);
    }

    #[test]
    fn progress_accepts_0_to_100() {
        for (text, expected) in [("0", 0), ("100", 100), ("040", 40)] {
            assert_eq!(parse(&format!("state=working:progress={text}")).unwrap().progress, Some(expected), "{text}");
        }
    }

    #[test]
    fn invalid_progress_is_indeterminate_not_fatal() {
        for text in ["101", "-1", "4.5", "1000", "", "+5"] {
            let r = parse(&format!("state=working:progress={text}")).unwrap_or_else(|| panic!("{text}"));
            assert_eq!(r.progress, None, "{text:?}");
        }
    }

    #[test]
    fn progress_is_ignored_outside_working_and_blocked() {
        for state in ["idle", "done", "error"] {
            assert_eq!(parse(&format!("state={state}:progress=50")).unwrap().progress, None, "{state}");
        }
    }

    #[test]
    fn blocked_carries_progress() {
        assert_eq!(parse("state=blocked:progress=70").unwrap().progress, Some(70));
    }

    // ---- id / app ----

    #[test]
    fn valid_ids_parse() {
        for id in ["build", "build/test/unit", "a/b/c/d/e/f/g/h" /* 8 levels */] {
            assert_eq!(parse(&format!("state=working:id={id}")).unwrap().id.as_deref(), Some(id));
        }
    }

    #[test]
    fn invalid_id_ignores_the_report_rather_than_writing_root() {
        for id in [
            "a/b/c/d/e/f/g/h/i", // 9 levels
            "/build",            // empty segment
            "build/",
            "a//b",
            "",
            "a,b", // ',' is a value char, not an id char
            "a=b",
            "build/x=1",
        ] {
            assert_eq!(parse(&format!("state=working:id={id}")), None, "{id:?}");
        }
    }

    #[test]
    fn id_segment_over_32_bytes_ignores_the_report() {
        assert!(parse(&format!("state=idle:id={}", "a".repeat(32))).is_some());
        assert!(parse(&format!("state=idle:id={}", "a".repeat(33))).is_none());
    }

    #[test]
    fn id_over_128_bytes_ignores_the_report() {
        let ok = vec!["a".repeat(31); 4].join("/"); // 127
        let too_long = format!("{ok}aa"); // 129, last seg 33
        let too_long2 = format!("{}/a", vec!["a".repeat(32); 4].join("/")); // 134
        assert!(parse(&format!("state=idle:id={ok}")).is_some());
        assert!(parse(&format!("state=idle:id={too_long}")).is_none());
        assert!(parse(&format!("state=idle:id={too_long2}")).is_none());
    }

    #[test]
    fn app_allows_32_identifier_bytes() {
        let long = "x".repeat(32);
        assert_eq!(parse(&format!("state=idle:app={long}")).unwrap().app.as_deref(), Some(long.as_str()));
        assert_eq!(parse("state=idle:app=my_app-1.2+x").unwrap().app.as_deref(), Some("my_app-1.2+x"));
    }

    #[test]
    fn invalid_app_is_treated_as_absent_not_fatal() {
        for app in ["xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx" /* 33 */, "a/b", "a,b", "a=b", ""] {
            let r = parse(&format!("state=working:app={app}")).unwrap_or_else(|| panic!("{app}"));
            assert_eq!(r.app, None, "{app:?}");
            assert_eq!(r.state, ProgramState::Working);
        }
    }

    // ---- base64 text ----

    #[test]
    fn msg_and_title_decode_utf8() {
        let r = parse(&format!("state=done:title={}:msg={}", b64("Café ✓"), b64("Tests passed – 42"))).unwrap();
        assert_eq!(r.title.as_deref(), Some("Café ✓"));
        assert_eq!(r.message.as_deref(), Some("Tests passed – 42"));
    }

    #[test]
    fn base64_padding_is_optional() {
        for b in ["SGk" /* "Hi" unpadded */, "SGk=" /* padded */] {
            assert_eq!(parse(&format!("state=done:msg={b}")).unwrap().message.as_deref(), Some("Hi"), "{b}");
        }
    }

    #[test]
    fn invalid_base64_drops_the_report() {
        for b in [
            "S",     // length % 4 == 1 can never be base64
            "SGk==", // too much padding
            "S=Gk",  // padding in the middle
            "SG-k",  // url-safe alphabet is not standard base64
            "SG_k",
        ] {
            assert_eq!(parse(&format!("state=done:msg={b}")), None, "{b}");
        }
    }

    #[test]
    fn control_characters_in_decoded_text_drop_the_report() {
        for text in ["line1\nline2", "carriage\rreturn", "esc \u{1b}[31m red", "bell\u{7}", "del\u{7f}", "c1\u{9b}", "nul\0"] {
            assert_eq!(parse(&format!("state=done:msg={}", b64(text))), None, "{text:?}");
            assert_eq!(parse(&format!("state=done:title={}", b64(text))), None, "{text:?}");
        }
    }

    #[test]
    fn invalid_utf8_drops_the_report() {
        let b = base64::engine::general_purpose::STANDARD.encode([0x48u8, 0xff, 0xfe]);
        assert_eq!(parse(&format!("state=done:msg={b}")), None);
    }

    #[test]
    fn bidi_and_invisible_format_characters_are_neutralised() {
        // U+202E RIGHT-TO-LEFT OVERRIDE, U+200B ZERO WIDTH SPACE, U+2066 LRI.
        let r = parse(&format!("state=done:msg={}", b64("ok\u{202e}txt.exe\u{200b}\u{2066}"))).unwrap();
        assert_eq!(r.message.as_deref(), Some("oktxt.exe"));
    }

    #[test]
    fn supplementary_plane_format_characters_are_stripped_by_scalar() {
        // U+E0001 LANGUAGE TAG and U+E0041 TAG LATIN CAPITAL A: invisible, category Cf.
        let tagged = "ok\u{E0001}\u{E0041}!";
        assert_eq!(parse(&format!("state=done:msg={}", b64(tagged))).unwrap().message.as_deref(), Some("ok!"));
    }

    #[test]
    fn visible_supplementary_characters_survive() {
        let emoji = "done \u{1F389}";
        assert_eq!(parse(&format!("state=done:msg={}", b64(emoji))).unwrap().message.as_deref(), Some(emoji));
    }

    #[test]
    fn unicode_line_breaks_drop_the_report() {
        for cp in [0x2028u32, 0x2029, 0x0085] {
            let text = format!("a{}b", char::from_u32(cp).unwrap());
            assert_eq!(parse(&format!("state=done:msg={}", b64(&text))), None, "{cp:x}");
            assert_eq!(parse(&format!("state=done:title={}", b64(&text))), None, "{cp:x}");
        }
    }

    #[test]
    fn title_decoded_limit_is_192_bytes() {
        assert!(parse(&format!("state=idle:title={}", b64(&"t".repeat(192)))).is_some());
        assert!(parse(&format!("state=idle:title={}", b64(&"t".repeat(193)))).is_none());
    }

    #[test]
    fn title_decoded_limit_counts_bytes_not_chars() {
        // 97 x "é" = 194 UTF-8 bytes, only 97 chars.
        assert!(parse(&format!("state=idle:title={}", b64(&"é".repeat(97)))).is_none());
    }

    #[test]
    fn msg_decoded_limit_is_2048_bytes() {
        assert!(parse(&format!("state=idle:msg={}", b64(&"m".repeat(2048)))).is_some());
        assert!(parse(&format!("state=idle:msg={}", b64(&"m".repeat(2049)))).is_none());
    }

    #[test]
    fn encoded_limits_are_enforced_before_decoding() {
        // 260 chars of valid-looking base64 for title (cap 256 encoded).
        assert!(parse(&format!("state=idle:title={}", "A".repeat(260))).is_none());
        assert!(parse(&format!("state=idle:msg={}", "A".repeat(2736))).is_none());
    }

    #[test]
    fn whole_sequence_over_4096_bytes_is_dropped() {
        // Padding with unknown keys keeps every individual field legal, so only the overall
        // cap can reject this.
        let filler = ":pad=xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".repeat(100);
        let payload = format!("state=working{filler}");
        assert!(payload.len() > 4096);
        assert_eq!(parse(&payload), None);

        let cut = 4096 - 9 - "state=working".len();
        let fits = format!("state=working{}", &filler[..cut]);
        assert!(parse(fits.trim_end_matches(':')).is_some());
    }

    #[test]
    fn a_bad_field_poisons_the_whole_report_even_with_a_valid_state() {
        // Nothing from a discarded report is applied, not even its state.
        assert_eq!(parse(&format!("state=blocked:kind=permission:msg={}", b64("a\nb"))), None);
    }

    // ---- additions: Rust-specific hostile-input checks ----

    #[test]
    fn non_ascii_and_multibyte_payloads_never_panic() {
        for p in ["state=working:é=1", "é=1:state=idle", "state=idle:app=é", "=", ":", "::=::", "state=idle:\u{1F389}=x"] {
            let _ = parse(p);
        }
    }

    /// The Cf (Format) table against .NET 11's `Rune.GetUnicodeCategory`: ranges generated from
    /// the real runtime (`tests/fixtures/dotnet_format_chars.txt`). Also pins the two other
    /// classes the decoder relies on: C0/DEL/C1 controls and the line/paragraph separators.
    #[test]
    fn format_char_table_matches_dotnet_exactly() {
        let mut dotnet = Vec::new();
        for line in include_str!("../tests/fixtures/dotnet_format_chars.txt").lines() {
            let (a, b) = line.trim().split_once('-').unwrap();
            let (a, b) = (u32::from_str_radix(a, 16).unwrap(), u32::from_str_radix(b, 16).unwrap());
            dotnet.extend(a..=b);
        }
        let ours: Vec<u32> = (0..=0x10FFFFu32).filter(|cp| char::from_u32(*cp).is_some_and(is_format_char)).collect();
        assert_eq!(ours, dotnet);
        // 65 controls in .NET (Rune.IsControl): U+0000-1F, U+007F-9F.
        assert_eq!((0..=0x10FFFFu32).filter(|cp| char::from_u32(*cp).is_some_and(char::is_control)).count(), 65);
    }

    #[test]
    fn non_canonical_trailing_bits_drop_the_report_like_dotnet() {
        // Verified against .NET: Convert.TryFromBase64String("SGl=") fails (the last symbol
        // carries stray bits), while "SGk=" decodes to "Hi".
        assert_eq!(parse("state=done:msg=SGl"), None);
        assert_eq!(parse("state=done:msg=SGk").unwrap().message.as_deref(), Some("Hi"));
    }
}
