//! The "Recent" toolbar menu (Ctrl+Shift+E): the sessions used most recently, newest first (C#
//! `RecentSessions`).
//!
//! "Used" is `ShellSession::last_activity_at`, stamped whenever a session becomes the active
//! one and persisted, so the list survives a restart. The restore loop does not stamp.

use crate::models::ShellSession;

/// How many sessions the menu lists.
pub const MAX_ITEMS: usize = 10;

/// Live and sleeping sessions by most recent use, without the active one (it is already on
/// screen; leaving it out makes the first item "the session I was in before", so
/// Ctrl+Shift+E, Enter flips between the two). Stable on ties: sessions never used (same
/// backfilled stamp) keep sidebar order.
pub fn pick<'a>(sessions: &'a [ShellSession], active_id: Option<&str>, max: usize) -> Vec<&'a ShellSession> {
    pick_by(sessions, |s| s.id.as_str(), |s| s.last_activity_at.epoch_ms(), active_id, max)
}

/// [`pick`] over any list that knows each item's id and last-used stamp (epoch ms; `None` is
/// "never"), so a view model orders its rows exactly like the engine's `session_recent`.
pub fn pick_by<'a, T>(
    items: &'a [T],
    id: impl Fn(&T) -> &str,
    last_used_ms: impl Fn(&T) -> Option<i64>,
    active_id: Option<&str>,
    max: usize,
) -> Vec<&'a T> {
    let mut candidates: Vec<(usize, &T)> = items.iter().filter(|s| Some(id(s)) != active_id).enumerate().collect();
    // Newest first; the original index breaks ties. An unparseable stamp sorts as "never",
    // like C#'s `default(DateTime)`.
    candidates.sort_by_key(|(i, s)| (std::cmp::Reverse(last_used_ms(s).unwrap_or(i64::MIN)), *i));
    candidates.into_iter().take(max).map(|(_, s)| s).collect()
}

/// [`pick`] with the default [`MAX_ITEMS`].
pub fn pick_default<'a>(sessions: &'a [ShellSession], active_id: Option<&str>) -> Vec<&'a ShellSession> {
    pick(sessions, active_id, MAX_ITEMS)
}

const MINUTE_MS: i64 = 60_000;
const HOUR_MS: i64 = 60 * MINUTE_MS;
const DAY_MS: i64 = 24 * HOUR_MS;

/// Compact "how long ago": now, 5m, 3h, 2d, 6w. Blank for "never" (`then_ms` is `None`).
/// Times are epoch milliseconds.
pub fn ago(then_ms: Option<i64>, now_ms: i64) -> String {
    let Some(then) = then_ms else {
        return String::new();
    };
    let d = now_ms.saturating_sub(then);
    if d < MINUTE_MS {
        "now".to_owned()
    } else if d < HOUR_MS {
        format!("{}m", d / MINUTE_MS)
    } else if d < DAY_MS {
        format!("{}h", d / HOUR_MS)
    } else if d < 14 * DAY_MS {
        format!("{}d", d / DAY_MS)
    } else {
        format!("{}w", d / (7 * DAY_MS))
    }
}

/// [`ago`] for a stored timestamp: blank when it is the "never" sentinel (C# `thenUtc == default`).
pub fn ago_for(then: &crate::time::Timestamp, now_ms: i64) -> String {
    if then.is_never() { String::new() } else { ago(then.epoch_ms(), now_ms) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Timestamp;

    const NOW: i64 = 1_793_966_400_000; // 2026-11-06T12:00:00Z

    fn s(id: &str, minutes_ago: f64, dormant: bool) -> ShellSession {
        ShellSession {
            id: id.into(),
            name: id.into(),
            last_activity_at: Timestamp::from_epoch_ms(NOW - (minutes_ago * 60_000.0) as i64),
            is_dormant: dormant,
            ..Default::default()
        }
    }

    fn ids(v: &[&ShellSession]) -> Vec<String> {
        v.iter().map(|s| s.id.clone()).collect()
    }

    #[test]
    fn newest_first_active_excluded() {
        let list = [s("a", 30.0, false), s("b", 1.0, false), s("c", 10.0, false), s("d", 0.0, false)];
        assert_eq!(ids(&pick_default(&list, Some("d"))), ["b", "c", "a"]);
    }

    #[test]
    fn sleeping_sessions_are_included() {
        let list = [s("live", 20.0, false), s("asleep", 5.0, true)];
        assert_eq!(pick_default(&list, None)[0].id, "asleep");
    }

    #[test]
    fn capped_at_max() {
        let list: Vec<ShellSession> = (0..25).map(|i| s(&format!("s{i}"), f64::from(i), false)).collect();
        let picked = pick_default(&list, None);
        assert_eq!(picked.len(), MAX_ITEMS);
        assert_eq!(picked[0].id, "s0");
    }

    #[test]
    fn ties_keep_sidebar_order() {
        let list = [s("x", 60.0, false), s("y", 60.0, false), s("z", 60.0, false)];
        assert_eq!(ids(&pick_default(&list, None)), ["x", "y", "z"]);
    }

    #[test]
    fn no_active_all_candidates() {
        assert_eq!(pick_default(&[s("a", 1.0, false), s("b", 2.0, false)], None).len(), 2);
    }

    #[test]
    fn ago_compact() {
        for (minutes_ago, expected) in
            [(0.2, "now"), (5.0, "5m"), (59.0, "59m"), (60.0, "1h"), (60.0 * 23.0, "23h"), (60.0 * 24.0 * 3.0, "3d"), (60.0 * 24.0 * 30.0, "4w")]
        {
            let then = NOW - (minutes_ago * 60_000.0) as i64;
            assert_eq!(ago(Some(then), NOW), expected, "{minutes_ago}");
        }
    }

    #[test]
    fn ago_never_is_blank() {
        assert_eq!(ago(None, NOW), "");
        // The sentinel stamp maps to "never" for the caller via Timestamp::is_never.
        assert!(Timestamp::never().is_never());
    }

    #[test]
    fn ago_with_a_future_stamp_is_now_and_extremes_do_not_overflow() {
        assert_eq!(ago(Some(NOW + 10_000), NOW), "now");
        let _ = ago(Some(i64::MIN), i64::MAX);
    }

    #[test]
    fn max_zero_returns_nothing_and_unparseable_stamps_sort_last() {
        let mut bad = s("bad", 0.0, false);
        bad.last_activity_at = Timestamp::from_text("not a date");
        let list = [bad, s("ok", 100.0, false)];
        assert!(pick(&list, None, 0).is_empty());
        assert_eq!(ids(&pick_default(&list, None)), ["ok", "bad"]);
    }
}
