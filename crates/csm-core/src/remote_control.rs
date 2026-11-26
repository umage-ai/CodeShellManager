//! Claude Code's Remote Control (`/remote-control`, `claude --remote-control`): whether a session
//! can be continued from claude.ai or the Claude app, read from what Claude writes to its
//! transcript. Pure.
//!
//! The signal (Claude Code 2.1.x; the transcript format is Claude's own and not a documented
//! contract, so everything here is matched conservatively and anything unexpected is ignored):
//!
//! - **On:** a `system` line with `subtype: "bridge_status"`, written when the Remote Control
//!   connection is up. It carries the session's link in `url`
//!   (`https://claude.ai/code/session_…`); its `content` reads
//!   `/remote-control is active · Continue here, on your phone, or at <url>`.
//! - **Off:** a `system` line whose text starts with `Remote Control disconnected` or
//!   `Remote Control stopped` (the `/remote-control` disconnect writes it as a `local_command`
//!   line, `<local-command-stdout>Remote Control disconnected.</local-command-stdout>`; a dropped
//!   connection words it as a warning). The process ending turns it off too (the engine's job).
//!
//! `Remote Control connecting…` is neither: the connection is not up yet.
//!
//! A resumed conversation reconnects Remote Control silently (no new `bridge_status` line), so the
//! transcript alone misses it. Claude also keeps a live per-process file,
//! `<config dir>/sessions/<pid>.json`, whose `bridgeSessionId` is present exactly while Remote
//! Control is on; [`parse_session_file`] reads it and [`resolve`] lets a fresh one decide.

/// Longest session link kept, in bytes.
const MAX_URL: usize = 512;

/// What one transcript line says about Remote Control.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteControlEvent {
    /// Connected; the session's link when the line carried a usable one.
    On { url: Option<String> },
    Off,
}

/// A [`RemoteControlEvent`] and when Claude wrote it (epoch ms, when the line had a timestamp).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteControlMark {
    pub event: RemoteControlEvent,
    pub ts_ms: Option<i64>,
}

/// A session's Remote Control state as the app shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RemoteControl {
    /// `https://claude.ai/code/session_…`, when known.
    pub url: Option<String>,
    /// When it was turned on (epoch ms), when known.
    pub since_ms: Option<i64>,
}

/// The link, if it is an absolute http(s) URL with nothing a shell or a browser could misread
/// (no whitespace, control characters or quotes) and of sane length.
pub fn session_url(raw: &str) -> Option<String> {
    let u = raw.trim();
    if u.is_empty() || u.len() > MAX_URL {
        return None;
    }
    let lower = u.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    if rest.is_empty() || rest.starts_with(['/', '.', '-', '@', ':']) {
        return None;
    }
    if u.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '"' | '\'' | '<' | '>' | '\\' | '^' | '`' | '{' | '}' | '|')) {
        return None;
    }
    Some(u.to_owned())
}

/// The `<local-command-stdout>` wrapper Claude puts around a slash command's output, removed.
fn unwrap_local(content: &str) -> &str {
    let c = content.trim();
    let c = c.strip_prefix("<local-command-stdout>").unwrap_or(c);
    let c = c.strip_suffix("</local-command-stdout>").unwrap_or(c);
    c.trim()
}

/// The link at the end of a `bridge_status` line's text (after the last ` at `).
fn url_in_content(content: &str) -> Option<String> {
    let (_, tail) = content.rsplit_once(" at ")?;
    session_url(tail.trim_end_matches(['.', ')']))
}

/// Classifies a `system` transcript line from its `subtype`, its `content` text and its `url`
/// field. `None` for anything that is not about Remote Control. Never fails.
pub fn classify(subtype: Option<&str>, content: Option<&str>, url: Option<&str>) -> Option<RemoteControlEvent> {
    if subtype == Some("bridge_status") {
        let url = url.and_then(session_url).or_else(|| content.and_then(url_in_content));
        return Some(RemoteControlEvent::On { url });
    }
    let text = unwrap_local(content?);
    if text.starts_with("Remote Control disconnected") || text.starts_with("Remote Control stopped") {
        return Some(RemoteControlEvent::Off);
    }
    None
}

/// Folds a mark into the current state, for a process that started at `started_ms` (epoch ms).
/// A mark written before this process started (an earlier run of the same conversation, read
/// again from its transcript) or without a timestamp does not turn it on; an off mark always
/// turns it off. Returns the new state.
pub fn apply(current: Option<RemoteControl>, mark: &RemoteControlMark, started_ms: i64) -> Option<RemoteControl> {
    match &mark.event {
        RemoteControlEvent::Off => None,
        RemoteControlEvent::On { url } => match mark.ts_ms {
            Some(ts) if ts >= started_ms => Some(RemoteControl { url: url.clone(), since_ms: Some(ts) }),
            _ => current,
        },
    }
}

/// Most bytes of a `sessions/<pid>.json` file that are looked at; a larger file is ignored.
pub const MAX_SESSION_FILE: usize = 64 * 1024;
/// Longest `bridgeSessionId` accepted.
const MAX_BRIDGE_ID: usize = 128;
/// A sessions file counts as written by this process when its `startedAt` is at least the
/// process start minus this much: Claude starts after CSM spawns the PTY, but clocks and
/// rounding differ a little.
pub const RECORD_SLACK_MS: i64 = 5_000;

/// What one `sessions/<pid>.json` file says (Claude Code 2.1.x; not a documented contract).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRecord {
    /// The Claude conversation id (the transcript's file stem).
    pub session_id: String,
    /// When that Claude process started (epoch ms).
    pub started_ms: i64,
    /// Its Remote Control link, present exactly while Remote Control is on.
    pub link: Option<String>,
    /// Whether `bridgeSessionId` was present (and valid): Remote Control is on.
    pub bridge: bool,
}

/// Parses the text of one sessions file. Lenient: anything malformed is `None`; an unusable
/// `bridgeSessionId` (not letters, digits and `_`, or of silly length) counts as absent.
pub fn parse_session_file(text: &str) -> Option<SessionRecord> {
    if text.len() > MAX_SESSION_FILE {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let session_id = v.get("sessionId")?.as_str()?.trim();
    if session_id.is_empty() || session_id.len() > 64 || !session_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        return None;
    }
    let started_ms = v.get("startedAt")?.as_i64().filter(|t| *t > 0)?;
    let bridge_id = v
        .get("bridgeSessionId")
        .and_then(|b| b.as_str())
        .map(str::trim)
        .filter(|b| !b.is_empty() && b.len() <= MAX_BRIDGE_ID && b.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_'));
    let link = bridge_id.and_then(|b| session_url(&format!("https://claude.ai/code/{b}")));
    Some(SessionRecord { session_id: session_id.to_owned(), started_ms, bridge: link.is_some(), link })
}

/// Whether a sessions file belongs to a process started at `process_started_ms` (not a stale file
/// of an earlier or crashed process of the same conversation).
pub fn record_is_fresh(record: &SessionRecord, process_started_ms: i64) -> bool {
    record.started_ms >= process_started_ms.saturating_sub(RECORD_SLACK_MS)
}

/// The state to show: a fresh sessions file is authoritative (`bridgeSessionId` present: on, with
/// its link, since the transcript's `bridge_status` time when `transcript` has one, else the
/// file's `startedAt`; absent: off). Without one, the transcript's state stands. `transcript` is
/// what the transcript rule ([`apply`]) says.
pub fn resolve(transcript: Option<RemoteControl>, record: Option<&SessionRecord>, process_started_ms: i64) -> Option<RemoteControl> {
    match record.filter(|r| record_is_fresh(r, process_started_ms)) {
        None => transcript,
        Some(r) if !r.bridge => None,
        Some(r) => Some(RemoteControl { url: r.link.clone(), since_ms: transcript.and_then(|t| t.since_ms).or(Some(r.started_ms)) }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://claude.ai/code/session_018joSyLkVsZFtGhjh64f31X";

    fn on(url: Option<&str>) -> Option<RemoteControlEvent> {
        Some(RemoteControlEvent::On { url: url.map(str::to_owned) })
    }

    #[test]
    fn bridge_status_turns_it_on_with_its_link() {
        let content = format!("/remote-control is active \u{b7} Continue here, on your phone, or at {URL}");
        assert_eq!(classify(Some("bridge_status"), Some(&content), Some(URL)), on(Some(URL)));
        // The link from the text when the field is missing or unusable.
        assert_eq!(classify(Some("bridge_status"), Some(&content), None), on(Some(URL)));
        assert_eq!(classify(Some("bridge_status"), Some(&content), Some("javascript:alert(1)")), on(Some(URL)));
        // On without a link when neither has one.
        assert_eq!(classify(Some("bridge_status"), None, None), on(None));
        assert_eq!(classify(Some("bridge_status"), Some("active"), Some("file:///C:/x")), on(None));
    }

    #[test]
    fn disconnects_turn_it_off() {
        let local = "<local-command-stdout>Remote Control disconnected.</local-command-stdout>";
        assert_eq!(classify(Some("local_command"), Some(local), None), Some(RemoteControlEvent::Off));
        let warn = "Remote Control disconnected \u{2014} network error \u{2014} run /remote-control to reconnect";
        assert_eq!(classify(Some("informational"), Some(warn), None), Some(RemoteControlEvent::Off));
        let stopped = "Remote Control stopped \u{2014} the app running this session is signed out of Claude.";
        assert_eq!(classify(None, Some(stopped), None), Some(RemoteControlEvent::Off));
    }

    #[test]
    fn other_lines_say_nothing() {
        assert_eq!(classify(Some("local_command"), Some("<local-command-stdout>Remote Control connecting\u{2026}</local-command-stdout>"), None), None);
        assert_eq!(classify(Some("informational"), Some("Remote Control is only available with claude.ai subscriptions."), None), None);
        assert_eq!(classify(Some("compact_boundary"), Some("Conversation compacted"), None), None);
        assert_eq!(classify(None, None, None), None);
        // Mentioned mid-sentence is not the message.
        assert_eq!(classify(Some("informational"), Some("Tip: Remote Control disconnected? Run /remote-control"), None), None);
    }

    #[test]
    fn garbage_never_panics_and_unsafe_links_are_dropped() {
        for junk in ["", " ", "\u{0}", "at", " at ", "at https://", "\u{1b}]8;;x\u{7}", "<local-command-stdout>", "</local-command-stdout>"] {
            let _ = classify(Some(junk), Some(junk), Some(junk));
            let _ = classify(Some("bridge_status"), Some(junk), Some(junk));
        }
        assert_eq!(session_url("https://claude.ai/code/x y"), None);
        assert_eq!(session_url("https://claude.ai/\"x"), None);
        assert_eq!(session_url("https://"), None);
        assert_eq!(session_url("https:///path"), None);
        assert_eq!(session_url("ftp://claude.ai/x"), None);
        assert_eq!(session_url(&format!("https://claude.ai/{}", "a".repeat(600))), None);
        assert_eq!(session_url(&format!(" {URL} ")).as_deref(), Some(URL));
        assert_eq!(session_url("http://localhost:8080/x").as_deref(), Some("http://localhost:8080/x"));
    }

    #[test]
    fn apply_ignores_marks_from_before_the_process_and_off_always_wins() {
        let started = 1_000;
        let mark = |event, ts| RemoteControlMark { event, ts_ms: ts };
        let on_ev = RemoteControlEvent::On { url: Some(URL.to_owned()) };
        // From an earlier run of the conversation: stays off.
        assert_eq!(apply(None, &mark(on_ev.clone(), Some(999)), started), None);
        // Without a timestamp: not trusted to turn it on.
        assert_eq!(apply(None, &mark(on_ev.clone(), None), started), None);
        // This run: on, with its link and time.
        let now_on = apply(None, &mark(on_ev.clone(), Some(1_500)), started);
        assert_eq!(now_on, Some(RemoteControl { url: Some(URL.to_owned()), since_ms: Some(1_500) }));
        // Off turns it off, whatever its time.
        assert_eq!(apply(now_on.clone(), &mark(RemoteControlEvent::Off, None), started), None);
        // A stale on mark leaves the current state alone.
        assert_eq!(apply(now_on.clone(), &mark(on_ev, Some(1)), started), now_on);
    }

    const LINE: &str = r#"{"pid":75612,"sessionId":"e5376029-51fe-49f7-8f33-b7042c8d643d","cwd":"C:\\x","startedAt":1795601554115,"version":"2.1.332","status":"busy","bridgeSessionId":"session_01W5FasiVSwTVCABQ9n9zXTw"}"#;

    #[test]
    fn a_sessions_file_with_a_bridge_id_is_on_with_its_link() {
        let r = parse_session_file(LINE).unwrap();
        assert_eq!(r.session_id, "e5376029-51fe-49f7-8f33-b7042c8d643d");
        assert_eq!(r.started_ms, 1_795_601_554_115);
        assert!(r.bridge);
        assert_eq!(r.link.as_deref(), Some("https://claude.ai/code/session_01W5FasiVSwTVCABQ9n9zXTw"));
    }

    #[test]
    fn a_sessions_file_without_one_is_off_and_junk_is_nothing() {
        let r = parse_session_file(r#"{"sessionId":"abc-1","startedAt":5}"#).unwrap();
        assert!(!r.bridge && r.link.is_none());
        let junk = [
            "",
            "{",
            "[]",
            "null",
            "42",
            r#"{"sessionId":"abc"}"#,
            r#"{"startedAt":5}"#,
            r#"{"sessionId":"a/b","startedAt":5}"#,
            r#"{"sessionId":"abc","startedAt":"5"}"#,
            r#"{"sessionId":"abc","startedAt":-1}"#,
            r#"{"sessionId":"","startedAt":5}"#,
            "\u{0}",
        ];
        for j in junk {
            assert_eq!(parse_session_file(j), None, "{j}");
        }
    }

    #[test]
    fn hostile_values_are_ignored_not_trusted() {
        for bad in ["a b", "x/../y", "a\"b", "<script>", "a?b=c", "", &"a".repeat(129)] {
            let t = format!(r#"{{"sessionId":"abc","startedAt":5,"bridgeSessionId":{}}}"#, serde_json::Value::String(bad.to_owned()));
            let r = parse_session_file(&t).unwrap();
            assert!(!r.bridge && r.link.is_none(), "{bad}");
        }
        assert!(!parse_session_file(r#"{"sessionId":"abc","startedAt":5,"bridgeSessionId":42}"#).unwrap().bridge);
        let huge = format!(r#"{{"sessionId":"abc","startedAt":5,"pad":"{}"}}"#, "x".repeat(MAX_SESSION_FILE));
        assert_eq!(parse_session_file(&huge), None);
        assert_eq!(parse_session_file(r#"{"sessionId":"abc","startedAt":1e400}"#), None);
        assert_eq!(parse_session_file(r#"{"sessionId":"abc","startedAt":99999999999999999999999}"#), None);
    }

    #[test]
    fn a_fresh_record_decides_and_a_stale_one_is_ignored() {
        let rec = |bridge: bool, started_ms| SessionRecord { session_id: "abc".into(), started_ms, link: bridge.then(|| URL.to_owned()), bridge };
        let from_transcript = Some(RemoteControl { url: Some("https://x.test/t".into()), since_ms: Some(9_000) });
        let got = resolve(None, Some(&rec(true, 10_000)), 10_000);
        assert_eq!(got, Some(RemoteControl { url: Some(URL.into()), since_ms: Some(10_000) }));
        let got = resolve(from_transcript.clone(), Some(&rec(true, 10_000)), 10_000);
        assert_eq!(got, Some(RemoteControl { url: Some(URL.into()), since_ms: Some(9_000) }));
        assert_eq!(resolve(from_transcript.clone(), Some(&rec(false, 10_000)), 10_000), None);
        assert!(resolve(None, Some(&rec(true, 10_000 - RECORD_SLACK_MS)), 10_000).is_some());
        assert_eq!(resolve(None, Some(&rec(true, 10_000 - RECORD_SLACK_MS - 1)), 10_000), None);
        assert_eq!(resolve(from_transcript.clone(), Some(&rec(false, 1)), 10_000), from_transcript);
        assert_eq!(resolve(from_transcript.clone(), None, 10_000), from_transcript);
    }
}
