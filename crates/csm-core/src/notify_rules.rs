//! Which session changes become OS notifications, and what the tray says (C# `ToastHelper` and
//! its callers in `MainViewModel`, with `SessionViewModel`'s per-session rate limit).
//!
//! The WPF app shows a balloon when a session raises an alert (the heuristic) or enters an
//! attention state through OSC 7501 (blocked / done / error), only with
//! `ShowToastNotifications` on, at most one per session per [`MIN_INTERVAL_MS`]. Here the
//! owner feeds every session's current attention state once a frame ([`Notifier::observe`]); the
//! notifier remembers the previous one and answers with the notifications to send:
//!
//! - entering *needs you* (approval or question, or switching between the two) or *error*;
//! - entering *done* (a turn finished, not seen yet) only while the window is not in front;
//! - never for the session the person is looking at (the focused session of a focused window);
//! - never with the setting off (the state is still tracked, so turning it on does not replay
//!   what already happened);
//! - never on the first observation (start-up and restore are not news), but a session that
//!   appears later already needing you is;
//! - at most one per session per [`MIN_INTERVAL_MS`]; one suppressed by the limit is dropped,
//!   not delayed (C# parity).
//!
//! Names and details come from terminals (titles, OSC payloads): they are cleaned of control
//! characters and capped before they reach the OS.

use std::collections::HashMap;

/// Minimum gap between two notifications for one session (C# `ProgramStatusToastInterval`).
pub const MIN_INTERVAL_MS: i64 = 5_000;
/// Longest title or body handed to the OS, in characters.
pub const MAX_TEXT_CHARS: usize = 200;
/// The app's name, the tray tooltip's and every notification title's prefix.
pub const APP_NAME: &str = "CodeShellManager";

/// What a session wants from the person, as far as notifications care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Attention {
    /// Working, idle, starting, asleep: nothing to say.
    #[default]
    Quiet,
    /// Needs you: a tool approval.
    Approval,
    /// Needs you: a question.
    Question,
    /// A turn finished and nobody has looked yet.
    Done,
    Error,
}

impl Attention {
    /// Approval or question.
    pub fn needs_you(self) -> bool {
        matches!(self, Attention::Approval | Attention::Question)
    }

    fn fallback_body(self) -> &'static str {
        match self {
            Attention::Approval => "Needs you: waiting for your approval.",
            Attention::Question => "Needs you: has a question for you.",
            Attention::Done => "Finished. Not looked at yet.",
            Attention::Error => "Stopped with an error.",
            Attention::Quiet => "",
        }
    }
}

/// One session as the notifier sees it this frame.
#[derive(Debug, Clone, Copy)]
pub struct Observation<'a> {
    pub session_id: &'a str,
    pub name: &'a str,
    pub attention: Attention,
    /// The status detail (the alert's or the program's message); may be empty.
    pub detail: &'a str,
}

/// What surrounds this frame's observation.
#[derive(Debug, Clone, Copy, Default)]
pub struct NotifyContext<'a> {
    /// `ShowToastNotifications`.
    pub enabled: bool,
    /// `ShowNotificationSound`.
    pub sound: bool,
    /// One of the app's windows has the keyboard focus and the main window is not minimized.
    pub window_focused: bool,
    /// The session the person is looking at.
    pub focused_session: Option<&'a str>,
    /// Milliseconds since the Unix epoch.
    pub now_ms: i64,
}

/// A notification to send.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub session_id: String,
    pub kind: Attention,
    pub title: String,
    pub body: String,
    pub sound: bool,
}

/// See the module docs. Plain `&mut self`; time is injected.
#[derive(Debug, Default)]
pub struct Notifier {
    last: HashMap<String, Attention>,
    sent_ms: HashMap<String, i64>,
    primed: bool,
}

impl Notifier {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds every current session (any order; ones missing are forgotten) and returns what to
    /// send, in the order given.
    pub fn observe(&mut self, sessions: &[Observation<'_>], cx: &NotifyContext<'_>) -> Vec<Notification> {
        let mut out = Vec::new();
        let primed = self.primed;
        self.primed = true;
        let mut next: HashMap<String, Attention> = HashMap::with_capacity(sessions.len());
        for o in sessions {
            let prev = match self.last.get(o.session_id) {
                Some(p) => *p,
                // Start-up: whatever state the sessions are in is not news.
                None if !primed => o.attention,
                None => Attention::Quiet,
            };
            next.insert(o.session_id.to_owned(), o.attention);
            if o.attention == prev || !self.worth_telling(o, cx) {
                continue;
            }
            if let Some(&t) = self.sent_ms.get(o.session_id)
                && (0..MIN_INTERVAL_MS).contains(&(cx.now_ms.saturating_sub(t)))
            {
                continue;
            }
            self.sent_ms.insert(o.session_id.to_owned(), cx.now_ms);
            out.push(Notification {
                session_id: o.session_id.to_owned(),
                kind: o.attention,
                title: title_for(o.name),
                body: body_for(o.attention, o.detail),
                sound: cx.sound,
            });
        }
        self.sent_ms.retain(|id, _| next.contains_key(id));
        self.last = next;
        out
    }

    fn worth_telling(&self, o: &Observation<'_>, cx: &NotifyContext<'_>) -> bool {
        if !cx.enabled {
            return false;
        }
        if cx.window_focused && cx.focused_session == Some(o.session_id) {
            return false;
        }
        match o.attention {
            Attention::Quiet => false,
            Attention::Approval | Attention::Question | Attention::Error => true,
            Attention::Done => !cx.window_focused,
        }
    }
}

/// "CodeShellManager — <name>" (the WPF balloon's title).
pub fn title_for(name: &str) -> String {
    let name = clean(name);
    if name.is_empty() { APP_NAME.to_owned() } else { format!("{APP_NAME} — {name}") }
}

fn body_for(kind: Attention, detail: &str) -> String {
    let d = clean(detail);
    if d.is_empty() { kind.fallback_body().to_owned() } else { d }
}

/// Control characters become spaces, runs of whitespace one space; capped at
/// [`MAX_TEXT_CHARS`] with an ellipsis.
pub fn clean(s: &str) -> String {
    let mut out = String::new();
    let mut n = 0usize;
    let mut space = false;
    for c in s.chars() {
        if c.is_control() || c.is_whitespace() {
            space = !out.is_empty();
            continue;
        }
        if space {
            out.push(' ');
            n += 1;
            space = false;
        }
        if n >= MAX_TEXT_CHARS {
            out.push('…');
            return out;
        }
        out.push(c);
        n += 1;
    }
    out
}

/// The tray icon's tooltip: "CodeShellManager", or "CodeShellManager — 2 need you".
pub fn tray_tooltip(needs_you: usize) -> String {
    match needs_you {
        0 => APP_NAME.to_owned(),
        1 => format!("{APP_NAME} — 1 needs you"),
        n => format!("{APP_NAME} — {n} need you"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(id: &str, a: Attention) -> Observation<'_> {
        Observation { session_id: id, name: id, attention: a, detail: "" }
    }

    fn cx(now_ms: i64) -> NotifyContext<'static> {
        NotifyContext { enabled: true, sound: false, window_focused: false, focused_session: None, now_ms }
    }

    /// A notifier past its first observation, with `a` in `id`'s current state.
    fn primed(id: &str, a: Attention) -> Notifier {
        let mut n = Notifier::new();
        assert!(n.observe(&[obs(id, a)], &cx(0)).is_empty());
        n
    }

    #[test]
    fn the_first_observation_is_not_news() {
        let mut n = Notifier::new();
        let out = n.observe(&[obs("a", Attention::Approval), obs("b", Attention::Error)], &cx(0));
        assert!(out.is_empty());
        // ...and staying there is not news either.
        assert!(n.observe(&[obs("a", Attention::Approval), obs("b", Attention::Error)], &cx(60_000)).is_empty());
    }

    #[test]
    fn entering_needs_you_or_error_notifies() {
        for a in [Attention::Approval, Attention::Question, Attention::Error, Attention::Done] {
            let mut n = primed("a", Attention::Quiet);
            let out = n.observe(&[obs("a", a)], &cx(10_000));
            assert_eq!(out.len(), 1, "{a:?}");
            assert_eq!(out[0].kind, a);
            assert_eq!(out[0].session_id, "a");
            assert_eq!(out[0].title, "CodeShellManager — a");
            assert_eq!(out[0].body, a.fallback_body());
        }
    }

    #[test]
    fn leaving_an_attention_state_is_not_news() {
        let mut n = primed("a", Attention::Approval);
        assert!(n.observe(&[obs("a", Attention::Quiet)], &cx(10_000)).is_empty());
    }

    #[test]
    fn approval_then_question_is_a_new_prompt() {
        let mut n = primed("a", Attention::Approval);
        assert_eq!(n.observe(&[obs("a", Attention::Question)], &cx(10_000)).len(), 1);
    }

    #[test]
    fn a_session_that_appears_later_already_needing_you_notifies() {
        let mut n = primed("a", Attention::Quiet);
        let out = n.observe(&[obs("a", Attention::Quiet), obs("b", Attention::Question)], &cx(10_000));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].session_id, "b");
    }

    #[test]
    fn off_means_off_and_turning_it_on_does_not_replay() {
        let mut n = primed("a", Attention::Quiet);
        let off = NotifyContext { enabled: false, ..cx(10_000) };
        assert!(n.observe(&[obs("a", Attention::Approval)], &off).is_empty());
        assert!(n.observe(&[obs("a", Attention::Approval)], &cx(20_000)).is_empty());
    }

    #[test]
    fn the_sound_follows_the_setting() {
        let mut n = primed("a", Attention::Quiet);
        let out = n.observe(&[obs("a", Attention::Error)], &NotifyContext { sound: true, ..cx(10_000) });
        assert!(out[0].sound);
    }

    #[test]
    fn never_for_the_session_in_front_of_you() {
        let mut n = primed("a", Attention::Quiet);
        let front = NotifyContext { window_focused: true, focused_session: Some("a"), ..cx(10_000) };
        assert!(n.observe(&[obs("a", Attention::Approval)], &front).is_empty());
        // The same session with the window in the background does notify.
        let mut n = primed("a", Attention::Quiet);
        let back = NotifyContext { window_focused: false, focused_session: Some("a"), ..cx(10_000) };
        assert_eq!(n.observe(&[obs("a", Attention::Approval)], &back).len(), 1);
        // Another session does, even with the window focused.
        let mut n = primed("b", Attention::Quiet);
        assert_eq!(n.observe(&[obs("b", Attention::Approval)], &front).len(), 1);
    }

    #[test]
    fn a_finished_turn_only_notifies_while_the_window_is_away() {
        let mut n = primed("a", Attention::Quiet);
        let focused = NotifyContext { window_focused: true, focused_session: Some("other"), ..cx(10_000) };
        assert!(n.observe(&[obs("a", Attention::Done)], &focused).is_empty());
        let mut n = primed("a", Attention::Quiet);
        assert_eq!(n.observe(&[obs("a", Attention::Done)], &cx(10_000)).len(), 1);
    }

    #[test]
    fn rate_limited_per_session() {
        let mut n = primed("a", Attention::Quiet);
        let _ = n.observe(&[obs("b", Attention::Quiet), obs("a", Attention::Quiet)], &cx(0));
        assert_eq!(n.observe(&[obs("a", Attention::Approval), obs("b", Attention::Quiet)], &cx(10_000)).len(), 1);
        // A flip back and forth within 5 s: dropped, not delayed.
        assert!(n.observe(&[obs("a", Attention::Quiet), obs("b", Attention::Quiet)], &cx(11_000)).is_empty());
        assert!(n.observe(&[obs("a", Attention::Question), obs("b", Attention::Quiet)], &cx(12_000)).is_empty());
        // Another session is not limited by the first.
        assert_eq!(n.observe(&[obs("a", Attention::Question), obs("b", Attention::Error)], &cx(12_500)).len(), 1);
        assert!(n.observe(&[obs("a", Attention::Question), obs("b", Attention::Error)], &cx(14_000)).is_empty());
        // 5 s after the last one, a new entry notifies again.
        assert!(n.observe(&[obs("a", Attention::Quiet), obs("b", Attention::Error)], &cx(14_500)).is_empty());
        assert_eq!(n.observe(&[obs("a", Attention::Approval), obs("b", Attention::Error)], &cx(15_000)).len(), 1);
    }

    #[test]
    fn a_clock_that_went_back_does_not_silence_a_session_forever() {
        let mut n = primed("a", Attention::Quiet);
        assert_eq!(n.observe(&[obs("a", Attention::Approval)], &cx(100_000)).len(), 1);
        let _ = n.observe(&[obs("a", Attention::Quiet)], &cx(100_100));
        assert_eq!(n.observe(&[obs("a", Attention::Error)], &cx(50_000)).len(), 1);
    }

    #[test]
    fn removed_sessions_are_forgotten() {
        let mut n = primed("a", Attention::Approval);
        let _ = n.observe(&[], &cx(10_000));
        // Back with the same id (a restart): a fresh entry.
        assert_eq!(n.observe(&[obs("a", Attention::Approval)], &cx(20_000)).len(), 1);
    }

    #[test]
    fn hostile_text_is_cleaned_and_capped() {
        let mut n = primed("a", Attention::Quiet);
        let long = "x".repeat(1000);
        let o = Observation { session_id: "a", name: "evil\x1b]0;name\x07\r\n  tab\there", attention: Attention::Approval, detail: &long };
        let out = n.observe(&[o], &cx(10_000));
        assert_eq!(out[0].title, "CodeShellManager — evil ]0;name tab here");
        assert_eq!(out[0].body.chars().count(), MAX_TEXT_CHARS + 1);
        assert!(out[0].body.ends_with('…'));
        assert_eq!(title_for(" \x07 "), "CodeShellManager");
        assert_eq!(clean("  a  b "), "a b");
    }

    #[test]
    fn the_detail_wins_over_the_fallback() {
        let mut n = primed("a", Attention::Quiet);
        let o = Observation { session_id: "a", name: "A", attention: Attention::Approval, detail: "Run `dotnet ef database update`?" };
        assert_eq!(n.observe(&[o], &cx(10_000))[0].body, "Run `dotnet ef database update`?");
    }

    #[test]
    fn tooltips() {
        assert_eq!(tray_tooltip(0), "CodeShellManager");
        assert_eq!(tray_tooltip(1), "CodeShellManager — 1 needs you");
        assert_eq!(tray_tooltip(4), "CodeShellManager — 4 need you");
    }
}
