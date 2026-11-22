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
}
