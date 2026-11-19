//! The wording of the "Restart all..." confirmation: how long the queue will take, and whether
//! the user is told that it is a long one (C# `BulkRestartEstimate`).
//!
//! A restart queue is strictly sequential and a Claude session waits for its old process to
//! exit, so "restart all" on a large fleet is a minute-scale operation; the point of this text
//! is that the user learns that BEFORE clicking Yes rather than four minutes in.

/// Target count at which the confirmation stops merely quoting a duration and starts
/// explaining that the restarts are sequential.
pub const SCALE_WARNING_THRESHOLD: i64 = 10;

// Per-session cost in seconds, as a low-high range. A Claude session dominates: the restart
// waits for the outgoing process to exit (busy shutdowns measured at 2.3-4.7s), then a flat
// stagger of up to 1s, then the relaunch (~1.4s). A plain shell is just the relaunch. A range
// rather than a point because the spread is real.
const CLAUDE_LOW_SECONDS: i64 = 4;
const CLAUDE_HIGH_SECONDS: i64 = 8;
const OTHER_LOW_SECONDS: i64 = 1;
const OTHER_HIGH_SECONDS: i64 = 2;

// Switch to minutes only once the OPTIMISTIC bound passes a minute. Deciding on the
// pessimistic bound would report "roughly 1/2-2 minutes" for a queue that usually finishes in
// forty seconds.
const MINUTES_THRESHOLD_SECONDS: i64 = 60;

/// A human range for how long restarting `total_targets` sessions will take, of which
/// `claude_targets` are Claude sessions. Counts are `i64` because the C# API takes `int` and
/// is tested with negatives.
pub fn describe_duration(total_targets: i64, claude_targets: i64) -> String {
    // Clamped at both ends: an uncapped claude count invented duration out of nothing
    // ((3 targets, 10 Claude) quoted 40-80 seconds for three sessions).
    let total = total_targets.max(0);
    let claude = claude_targets.max(0).min(total);
    let others = total - claude;

    let low = CLAUDE_LOW_SECONDS
        .saturating_mul(claude)
        .saturating_add(OTHER_LOW_SECONDS.saturating_mul(others));
    let high = CLAUDE_HIGH_SECONDS
        .saturating_mul(claude)
        .saturating_add(OTHER_HIGH_SECONDS.saturating_mul(others));

    if low < MINUTES_THRESHOLD_SECONDS {
        return format!("about {low}\u{2013}{high} seconds");
    }
    format!(
        "roughly {}\u{2013}{} minutes",
        format_minutes(low as f64 / 60.0),
        format_minutes(high as f64 / 60.0)
    )
}

/// The body of the confirmation dialog. `headline` is the caller's scope-naming first line
/// ("Restart all 55 live sessions?"). Lines are separated by `\n` (C# used
/// `Environment.NewLine`, which is `\r\n` on Windows; a UI toolkit wants `\n`).
pub fn build_confirm_text(headline: &str, total_targets: i64, claude_targets: i64) -> String {
    let claude = claude_targets.max(0);
    let duration = describe_duration(total_targets, claude);

    let mut out = String::new();
    out.push_str(headline);
    out.push_str("\n\n");

    if total_targets >= SCALE_WARNING_THRESHOLD {
        // Naming the Claude count is what makes the estimate look earned: it is the reason the
        // queue is slow. Omitted when there are none, since blaming absent sessions is wrong.
        if claude > 0 {
            out.push_str(&format!(
                "{claude} of them are Claude sessions and restart one at a time \u{2014} expect {duration}."
            ));
        } else {
            out.push_str(&format!(
                "They restart one at a time \u{2014} expect {duration}."
            ));
        }
    } else {
        out.push_str(&format!("Expect {duration}."));
    }

    out.push_str("\n\n");
    out.push_str(
        "Each running process is terminated and relaunched in turn, and any running session commands are stopped.",
    );
    if claude > 0 {
        out.push_str(" Claude sessions resume their conversation.");
    }

    // Only when there is actually a queue to stop: the rail and pill are hidden at one target,
    // so at exactly one session this sentence would point at a control that never appears.
    if total_targets > 1 {
        out.push_str(" You can stop the queue from the toolbar once it starts.");
    }
    out
}

/// Minutes to the nearest half, with the half written as a glyph.
fn format_minutes(minutes: f64) -> String {
    // MidpointRounding.AwayFromZero == f64::round.
    let rounded = (minutes * 2.0).round() / 2.0;
    let whole = rounded.trunc() as i64;
    let has_half = (rounded - whole as f64).abs() > 0.01;

    if whole == 0 {
        "\u{bd}".to_owned()
    } else if has_half {
        format!("{whole}\u{bd}")
    } else {
        whole.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- Duration ----

    #[test]
    fn no_claude_sessions_stays_in_seconds() {
        // Plain shells skip the exit wait entirely; six of them is a few seconds, and quoting
        // minutes for that would train the user to ignore the estimate.
        let text = describe_duration(6, 0);
        assert!(text.contains("seconds"));
        assert!(!text.contains("minute"));
    }

    #[test]
    fn single_claude_session_stays_in_seconds() {
        let text = describe_duration(1, 1);
        assert!(text.contains("seconds"));
        assert!(!text.contains("minute"));
    }

    #[test]
    fn large_claude_fleet_is_reported_as_a_minute_range() {
        // 52 Claude at 4-8s each plus 3 plain shells at 1-2s: 211s to 422s.
        let text = describe_duration(55, 52);
        assert!(text.contains("3\u{bd}"));
        assert!(text.contains('7'));
        assert!(text.contains("minutes"));
    }

    #[test]
    fn whole_minute_range_omits_the_half_glyph() {
        // 30 Claude: 120s to 240s, i.e. exactly 2-4 minutes.
        let text = describe_duration(30, 30);
        assert!(!text.contains('\u{bd}'));
        assert!(text.contains("minutes"));
        assert_eq!(text, "roughly 2\u{2013}4 minutes");
    }

    #[test]
    fn duration_grows_with_the_number_of_claude_sessions() {
        // Guards the direction of the model: swapping the Claude and non-Claude costs would
        // still satisfy every assertion above. Same fleet size, only the Claude share moves.
        let small = describe_duration(40, 0);
        let large = describe_duration(40, 40);
        assert_ne!(small, large);
        assert!(!small.contains("minute"));
        assert!(large.contains("minutes"));
    }

    // ---- Confirmation text ----

    #[test]
    fn confirm_text_opens_with_the_headline_on_its_own_line() {
        let text = build_confirm_text("Restart all 55 live sessions?", 55, 52);
        assert_eq!(
            text.split('\n').next(),
            Some("Restart all 55 live sessions?")
        );
    }

    #[test]
    fn confirm_text_says_the_queue_can_be_stopped() {
        // Asserting on "stop the queue", not "stop": the body always ends "...any running
        // session commands are stopped", so the looser substring was satisfied by text that
        // says nothing about the affordance.
        let text = build_confirm_text("Restart all 55 live sessions?", 55, 52);
        assert!(text.contains("stop the queue"));
    }

    #[test]
    fn confirm_text_single_target_does_not_promise_a_stop_control() {
        let text = build_confirm_text("Restart all 1 live session(s)?", 1, 1);
        assert!(!text.contains("stop the queue"));
        assert!(!text.contains("toolbar"));
    }

    #[test]
    fn more_claude_targets_than_targets_is_clamped_not_compounded() {
        assert_eq!(describe_duration(3, 10), describe_duration(3, 3));
    }

    #[test]
    fn negative_targets_do_not_panic_or_invent_time() {
        let text = describe_duration(-5, -2);
        assert!(text.contains('0'));
        assert!(text.contains("seconds"));
    }

    #[test]
    fn confirm_text_says_run_commands_are_stopped() {
        assert!(build_confirm_text("Restart 3 sessions?", 3, 1).contains("session commands"));
    }

    #[test]
    fn confirm_text_below_the_threshold_has_no_scale_warning() {
        assert!(!build_confirm_text("Restart 3 sessions?", 3, 3).contains("one at a time"));
    }

    #[test]
    fn confirm_text_at_the_threshold_warns_about_scale() {
        let text = build_confirm_text(
            "Restart all 10 live sessions?",
            SCALE_WARNING_THRESHOLD,
            SCALE_WARNING_THRESHOLD,
        );
        assert!(text.contains("one at a time"));
    }

    #[test]
    fn confirm_text_scale_warning_names_how_many_are_claude() {
        // "52 of them are Claude sessions" is the line that explains WHY it is slow.
        assert!(build_confirm_text("Restart all 55 live sessions?", 55, 52).contains("52"));
    }

    #[test]
    fn confirm_text_scale_warning_with_no_claude_sessions_does_not_blame_claude() {
        let text = build_confirm_text("Restart all 20 live sessions?", 20, 0);
        assert!(text.contains("one at a time"));
        assert!(!text.contains("Claude session"));
    }

    #[test]
    fn extreme_counts_do_not_overflow() {
        let _ = describe_duration(i64::MAX, i64::MAX);
        let _ = build_confirm_text("x", i64::MAX, i64::MIN);
    }
}
