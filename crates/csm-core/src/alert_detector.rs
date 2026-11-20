//! Pattern matching on raw terminal output that decides whether a CLI (Claude Code or any AI
//! tool) needs the user: tool approvals, yes/no questions, a waiting prompt (C#
//! `AlertDetector`).
//!
//! The C# class owns a `System.Threading.Timer` that fires 1.5 s after the last output. Here the
//! timer is a deadline and the clock is injected: [`AlertDetector::feed`] records the deadline,
//! and the owner calls [`AlertDetector::poll`] with the current time (from a UI tick or a
//! scheduled wake-up) to find out whether the idle period has elapsed. No real time is read.

use std::sync::LazyLock;

use regex::Regex;

use crate::models::{AlertEvent, AlertType};
use crate::text::{prefix_utf16, utf16_len};

/// Idle time after the last output before the last line is judged: long enough for Claude's
/// prompt redraw bursts to settle, short enough to feel responsive.
pub const IDLE_MS: i64 = 1_500;

/// Longest message carried by an alert, in UTF-16 code units, before an ellipsis is appended.
const MAX_MESSAGE_UNITS: usize = 100;

// OSC strings end in BEL (\x07) or ST (ESC \). Matching only BEL made an ST-terminated OSC
// either leak its payload into prompt matching or lazily swallow real output up to the next
// BEL. The `?` in the CSI class covers private-mode sequences (ESC[?25h). Same pattern as
// OutputIndexer.AnsiPattern in the C# app; keep the two in step.
static ANSI: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\x1B\[[?0-9;]*[mGKHFJABCDsuhlct]|\x1B\].*?(?:\x07|\x1B\\)|\x1B[=>]").ok());

// Claude's prompt (U+276F), generic y/n prompts, and "?" questions.
static PROMPT: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)(\[y/N\]|\[Y/n\]|\[yes/no\]|\(y/n\)|\(yes/no\)|>\s*$|\x{276f}\s*$|\?\s*\x{203a}|\?\s*$)").ok());

static APPROVAL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?i)(Do you want to|Allow|Approve|Bash command|Continue\?|Proceed\?|tool_use|permission)").ok());

/// Removes ANSI escape sequences (CSI, BEL- or ST-terminated OSC, and `ESC =` / `ESC >`).
pub fn strip_ansi(raw: &str) -> String {
    match ANSI.as_ref() {
        Some(re) => re.replace_all(raw, "").into_owned(),
        None => raw.to_owned(),
    }
}

fn matches(re: &LazyLock<Option<Regex>>, text: &str) -> bool {
    re.as_ref().is_some_and(|r| r.is_match(text))
}

/// Raised when [`AlertDetector::notify_user_interacted`] clears the alert state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlertCleared {
    pub session_id: String,
}

/// See the module docs.
#[derive(Debug)]
pub struct AlertDetector {
    session_id: String,
    session_name: String,
    last_line: String,
    /// When the idle timer fires; `None` when no timer is pending.
    deadline_ms: Option<i64>,
    alert_fired: bool,
    stood_down: bool,
}

impl AlertDetector {
    pub fn new(session_id: &str, session_name: &str) -> Self {
        Self {
            session_id: session_id.to_owned(),
            session_name: session_name.to_owned(),
            last_line: String::new(),
            deadline_ms: None,
            alert_fired: false,
            stood_down: false,
        }
    }

    /// True between [`AlertDetector::stand_down`] and [`AlertDetector::rearm`].
    pub fn is_stood_down(&self) -> bool {
        self.stood_down
    }

    /// When the pending idle timer will fire, if one is pending (for scheduling a wake-up).
    pub fn deadline_ms(&self) -> Option<i64> {
        self.deadline_ms
    }

    /// The program reports its own state through OSC 7501, so the screen heuristic stops
    /// guessing: its regexes would otherwise fight the reported state, e.g. raise "approval
    /// needed" off a line of output while the program says it is working. User-interaction
    /// notifications still fire: they are how a session acknowledges a done/error state.
    pub fn stand_down(&mut self) {
        self.stood_down = true;
        self.deadline_ms = None;
    }

    /// Undo [`AlertDetector::stand_down`]: the program no longer holds any OSC 7501 record, so
    /// the heuristic is the only source again. Starts from a clean slate: no stale last line,
    /// no fired flag. (C# parity: a timer already pending is not touched here, but `stand_down`
    /// cleared it and `feed` ignores output while stood down, so none is pending.)
    pub fn rearm(&mut self) {
        self.last_line.clear();
        self.alert_fired = false;
        self.stood_down = false;
    }

    /// Feeds raw PTY output at time `now_ms`. Remembers the last non-blank line and restarts
    /// the idle timer.
    pub fn feed(&mut self, raw_output: &str, now_ms: i64) {
        if self.stood_down {
            return;
        }
        let clean = strip_ansi(raw_output);
        for line in clean.split('\n').filter(|l| !l.is_empty()) {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                self.last_line = trimmed.to_owned();
            }
        }
        self.deadline_ms = Some(now_ms.saturating_add(IDLE_MS));
    }

    /// The user typed or focused the session: clears the fired flag and the pending timer, and
    /// reports that the alert was cleared.
    pub fn notify_user_interacted(&mut self) -> AlertCleared {
        self.alert_fired = false;
        self.deadline_ms = None;
        AlertCleared { session_id: self.session_id.clone() }
    }

    /// Advances the clock to `now_ms`. If the idle period has elapsed, judges the last line
    /// (C# `OnIdle`) and returns the alert it raises, if any. The timer is single-shot: it is
    /// spent whether or not an alert fires.
    pub fn poll(&mut self, now_ms: i64) -> Option<AlertEvent> {
        let due = self.deadline_ms.is_some_and(|d| now_ms >= d);
        if !due {
            return None;
        }
        self.deadline_ms = None;
        self.on_idle(now_ms)
    }

    /// Stops the pending timer (C# `Dispose`).
    pub fn dispose(&mut self) {
        self.deadline_ms = None;
    }

    fn on_idle(&mut self, now_ms: i64) -> Option<AlertEvent> {
        if self.alert_fired || self.stood_down || self.last_line.trim().is_empty() {
            return None;
        }

        // Approval is tested first, so a line matching both is a ToolApproval.
        let alert_type = if matches(&APPROVAL, &self.last_line) {
            AlertType::ToolApproval
        } else if matches(&PROMPT, &self.last_line) {
            AlertType::InputRequired
        } else {
            return None;
        };

        self.alert_fired = true;
        let message = if utf16_len(&self.last_line) > MAX_MESSAGE_UNITS {
            // C# cuts at 100 UTF-16 units and may leave half a surrogate pair; a Rust string
            // cannot, so a pair straddling the cut is dropped whole (99 units).
            format!("{}\u{2026}", prefix_utf16(&self.last_line, MAX_MESSAGE_UNITS))
        } else {
            self.last_line.clone()
        };
        Some(AlertEvent { session_id: self.session_id.clone(), session_name: self.session_name.clone(), message, alert_type, timestamp_ms: now_ms })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ESC: &str = "\x1b";
    const BEL: &str = "\x07";
    const ST: &str = "\x1b\\";

    /// Feeds `text` at t=0 and polls just after the idle period, like the C# tests waiting on
    /// the real timer.
    fn alert_for(text: &str) -> Option<AlertEvent> {
        let mut d = AlertDetector::new("s1", "session");
        d.feed(text, 0);
        d.poll(IDLE_MS)
    }

    // ---- Tool approval detection ----

    #[test]
    fn feed_claude_approval_phrasing_raises_tool_approval_alert() {
        for line in [
            "Do you want to run this command?",
            "Allow Claude to edit this file?",
            "Approve this tool use?",
            "Bash command: rm -rf /tmp/foo",
            "Continue?",
            "Proceed?",
            "tool_use requested",
            "Grant permission to read",
        ] {
            let e = alert_for(&format!("{line}\n")).unwrap_or_else(|| panic!("no alert for: {line}"));
            assert_eq!(e.alert_type, AlertType::ToolApproval, "{line}");
            assert_eq!(e.session_id, "s1");
            assert_eq!(e.session_name, "session");
        }
    }

    // ---- Input-required detection ----

    #[test]
    fn feed_input_required_phrasing_raises_input_required_alert() {
        for line in [
            "Delete file [y/N]",
            "Continue [Y/n]",
            "Proceed [yes/no]",
            "Are you sure (y/n)",
            "Confirm (yes/no)",
            "\u{276f}",                  // Claude's prompt alone (U+276F)
            "something \u{276f}",        // at end of line
            "Pick an option ? \u{203a}", // "? >" pattern (U+203A)
            "What now?",                 // trailing "?"
        ] {
            let e = alert_for(&format!("{line}\n")).unwrap_or_else(|| panic!("no alert for: {line}"));
            assert_eq!(e.alert_type, AlertType::InputRequired, "{line}");
        }
    }

    #[test]
    fn feed_trailing_greater_than_prompt_raises_input_required_alert() {
        // The prompt regex matches ">" at end of line.
        let e = alert_for("user@host:~$ >\n").unwrap();
        assert_eq!(e.alert_type, AlertType::InputRequired);
    }

    // ---- Negative cases ----

    #[test]
    fn feed_non_matching_line_does_not_raise_alert() {
        for line in ["just some regular log output", "Compilation succeeded.", "12 files indexed"] {
            assert_eq!(alert_for(&format!("{line}\n")), None, "{line}");
        }
    }

    #[test]
    fn feed_whitespace_only_does_not_raise_alert() {
        assert_eq!(alert_for("   \n\t\n"), None);
    }

    // ---- ANSI stripping ----

    #[test]
    fn feed_line_wrapped_in_ansi_escapes_still_fires_alert() {
        // Real Claude output is colourised: the detector strips ANSI before the regex match.
        let e = alert_for(&format!("{ESC}[1;33mDo you want to proceed?{ESC}[0m\n")).unwrap();
        assert_eq!(e.alert_type, AlertType::ToolApproval);
        assert!(e.message.contains("Do you want to proceed?"));
    }

    #[test]
    fn feed_prompt_wrapped_in_ansi_escapes_still_fires_input_required() {
        let e = alert_for(&format!("{ESC}[36m\u{276f}{ESC}[0m\n")).unwrap();
        assert_eq!(e.alert_type, AlertType::InputRequired);
    }

    // ---- NotifyUserInteracted ----

    #[test]
    fn notify_user_interacted_before_idle_timer_cancels_pending_alert() {
        // Feed a matching line, but interact before the 1500ms idle window expires: the
        // pending timer is cancelled, so no alert is ever raised.
        let mut d = AlertDetector::new("s1", "session");
        d.feed("Do you want to continue?\n", 0);
        d.notify_user_interacted();
        assert_eq!(d.poll(IDLE_MS), None);
        assert_eq!(d.poll(i64::MAX), None);
    }

    #[test]
    fn notify_user_interacted_raises_alert_cleared() {
        let mut d = AlertDetector::new("s1", "session");
        assert_eq!(d.notify_user_interacted(), AlertCleared { session_id: "s1".into() });
    }

    #[test]
    fn notify_user_interacted_resets_fired_flag_so_next_matching_feed_fires_again() {
        // Once an alert has fired, the fired flag latches. NotifyUserInteracted clears it so a
        // subsequent matching feed re-fires.
        let mut d = AlertDetector::new("s1", "session");
        d.feed("Do you want to proceed?\n", 0);
        assert!(d.poll(IDLE_MS).is_some(), "first alert should have fired");

        // Without interaction the latch holds, even for another matching line.
        d.feed("Allow this action?\n", 2_000);
        assert_eq!(d.poll(4_000), None);

        d.notify_user_interacted();
        d.feed("Allow this action?\n", 5_000);
        let second = d.poll(6_500).expect("second alert should have re-fired");
        assert_eq!(second.alert_type, AlertType::ToolApproval);
    }

    // ---- Idle-timer behaviour ----

    #[test]
    fn feed_last_line_wins_only_final_line_evaluated_for_alert_type() {
        // Feed processes all non-empty lines but only the LAST trimmed line is kept, so a
        // final tool-approval prompt wins even if an earlier line matched a different type.
        let e = alert_for("Continue [y/N]\nDo you want to run this?\n").unwrap();
        assert_eq!(e.alert_type, AlertType::ToolApproval);
    }

    #[test]
    fn feed_message_truncated_at_100_chars_when_line_is_longer() {
        let line = format!("{} Continue?", "x".repeat(130));
        assert!(line.len() > 100);
        let e = alert_for(&format!("{line}\n")).unwrap();
        assert!(e.message.ends_with('\u{2026}'));
        assert_eq!(utf16_len(&e.message), 101); // 100 chars + the ellipsis
    }

    // ---- Dispose ----

    #[test]
    fn dispose_after_feed_prevents_alert_from_firing() {
        let mut d = AlertDetector::new("s1", "session");
        d.feed("Do you want to continue?\n", 0);
        d.dispose();
        assert_eq!(d.poll(i64::MAX), None);
    }

    // ---- Timer semantics (the C# tests observed these through the real clock) ----

    #[test]
    fn nothing_fires_before_the_idle_period_and_it_fires_exactly_at_it() {
        let mut d = AlertDetector::new("s1", "session");
        d.feed("Do you want to continue?\n", 1_000);
        assert_eq!(d.deadline_ms(), Some(1_000 + IDLE_MS));
        assert_eq!(d.poll(1_000 + IDLE_MS - 1), None);
        assert!(d.poll(1_000 + IDLE_MS).is_some());
        // Single-shot: the timer is spent.
        assert_eq!(d.deadline_ms(), None);
    }

    #[test]
    fn more_output_restarts_the_idle_timer() {
        let mut d = AlertDetector::new("s1", "session");
        d.feed("Do you want to continue?\n", 0);
        d.feed("still working...\n", 1_000);
        assert_eq!(d.poll(1_500), None); // the first deadline was replaced
        // The last line is now "still working...", which matches nothing.
        assert_eq!(d.poll(2_500), None);
    }

    #[test]
    fn stand_down_silences_the_heuristic_until_rearmed() {
        let mut d = AlertDetector::new("s1", "session");
        d.feed("Do you want to continue?\n", 0);
        d.stand_down();
        assert!(d.is_stood_down());
        assert_eq!(d.poll(IDLE_MS), None);
        d.feed("Allow?\n", 2_000); // ignored while stood down
        assert_eq!(d.poll(10_000), None);

        d.rearm();
        assert!(!d.is_stood_down());
        d.feed("Allow?\n", 20_000);
        assert!(d.poll(21_500).is_some());
    }

    #[test]
    fn rearm_forgets_the_stale_last_line_and_the_fired_flag() {
        let mut d = AlertDetector::new("s1", "session");
        d.feed("Continue?\n", 0);
        assert!(d.poll(IDLE_MS).is_some());
        d.stand_down();
        d.rearm();
        // Output that matches nothing leaves nothing to judge; the old line is gone.
        d.feed("   \n", 5_000);
        assert_eq!(d.poll(7_000), None);
    }

    // ---- StripAnsi (AlertDetectorStripAnsiTests) ----

    #[test]
    fn strip_ansi_osc_terminated_with_bel_is_removed() {
        assert_eq!(strip_ansi(&format!("before {ESC}]0;window title{BEL}after")), "before after");
    }

    #[test]
    fn strip_ansi_osc_terminated_with_st_is_removed() {
        assert_eq!(strip_ansi(&format!("before {ESC}]9001;color=#a6e3a1;title=demo?{ST}after")), "before after");
    }

    #[test]
    fn strip_ansi_st_terminated_osc_does_not_swallow_following_output_up_to_a_later_bel() {
        // With a BEL-only regex the lazy match runs from the first OSC through the real text to
        // the BEL that ends the *second* OSC, deleting "Do you want to proceed?".
        let raw = format!("{ESC}]9001;git-branch=main{ST}Do you want to proceed?{ESC}]0;window title{BEL}");
        assert_eq!(strip_ansi(&raw), "Do you want to proceed?");
    }

    #[test]
    fn strip_ansi_csi_with_private_marker_is_removed() {
        assert_eq!(strip_ansi(&format!("{ESC}[?25hx")), "x");
    }

    #[test]
    fn strip_ansi_removes_the_bundled_conpty_opening_bytes() {
        // OpenConsole opens with ESC[1t ESC[c ESC[?1004h ESC[?9001h; `c` and `t` are in the
        // final-byte class so they do not reach the output.
        assert_eq!(strip_ansi("\x1b[1t\x1b[c\x1b[?1004h\x1b[?9001hhello"), "hello");
    }

    #[test]
    fn strip_ansi_lone_escapes_and_unterminated_osc_are_left_alone_not_fatal() {
        assert_eq!(strip_ansi("a\x1b"), "a\x1b");
        assert_eq!(strip_ansi("\x1b]0;never ends"), "\x1b]0;never ends");
        assert_eq!(strip_ansi("a\x1b=b\x1b>c"), "abc");
    }

    #[test]
    fn hostile_output_never_panics() {
        let mut d = AlertDetector::new("s", "n");
        for chunk in ["\0\0\0", "\u{1F600}".repeat(1000).as_str(), "\x1b[", "\x1b]", "\r\r\r\n\n", "\u{85}\u{2028}"] {
            d.feed(chunk, 0);
            let _ = d.poll(IDLE_MS);
        }
    }
}
