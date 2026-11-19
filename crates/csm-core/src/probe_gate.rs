//! Whether, and with which bytes, one session answers an OSC 7501 feature query (C#
//! `ProgramStatusProbeGate`).
//!
//! The C# gate is shared between the PTY read thread (`ObserveOutput`) and the UI thread
//! (`TryAcquire`) and locks internally. This one is plain `&mut self`: the owner decides how to
//! share it (a `Mutex`, or one thread). Time is injected as epoch milliseconds, never read.
//!
//! Two ConPTYs, two answers: on the **bundled** one (OpenConsole) the raw reply arrives in
//! order and is answered by default; on the **inbox** one (conhost) it always arrives late as
//! unsolicited input, so it is off by default and, when forced on, only sent in
//! win32-input-mode. Either way at most one reply per [`MIN_INTERVAL_MS`] and
//! [`MAX_REPLIES_PER_SESSION`] in all.

use crate::models::ProgramStatusProbeMode;
use crate::program_status_payload::{PROBE_REPLY, probe_reply_for_conpty};

/// Minimum spacing between two replies (5 s).
pub const MIN_INTERVAL_MS: i64 = 5_000;
/// Lifetime cap per session.
pub const MAX_REPLIES_PER_SESSION: u32 = 3;

const WIN32_INPUT_ON: &str = "\x1b[?9001h";
const WIN32_INPUT_OFF: &str = "\x1b[?9001l";

/// See the module docs.
#[derive(Debug, Default)]
pub struct ProgramStatusProbeGate {
    win32_input_mode: bool,
    replies: u32,
    last_reply_ms: Option<i64>,
}

impl ProgramStatusProbeGate {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn win32_input_mode(&self) -> bool {
        self.win32_input_mode
    }

    /// Whether answering is enabled for a session: Auto -> only on the bundled ConPTY;
    /// On -> always; Off -> never.
    pub fn is_enabled(mode: ProgramStatusProbeMode, bundled_conpty: bool) -> bool {
        match mode {
            ProgramStatusProbeMode::On => true,
            ProgramStatusProbeMode::Off => false,
            ProgramStatusProbeMode::Auto => bundled_conpty,
        }
    }

    /// The bytes to write: raw on the bundled ConPTY, key-event encoded on inbox.
    pub fn reply_for(bundled_conpty: bool) -> String {
        if bundled_conpty { PROBE_REPLY.to_owned() } else { probe_reply_for_conpty() }
    }

    /// Tracks win32-input-mode from PTY output; the last of `?9001h`/`?9001l` in a chunk wins.
    /// A sequence split across two chunks is missed: conhost emits it as the very first bytes
    /// of a session, in one write.
    pub fn observe_output(&mut self, chunk: &str) {
        if !chunk.contains("\x1b[?9001") {
            return;
        }
        let on = chunk.rfind(WIN32_INPUT_ON);
        let off = chunk.rfind(WIN32_INPUT_OFF);
        if on.is_none() && off.is_none() {
            return;
        }
        // `on > off` with None < Some, like C#'s -1 sentinel.
        self.win32_input_mode = on > off;
    }

    /// True (and consumes a reply) when a reply may be sent at `now_ms`. `bundled_conpty` lifts
    /// the win32-input-mode requirement: the raw reply needs no key-event encoding there.
    pub fn try_acquire(&mut self, enabled: bool, now_ms: i64, bundled_conpty: bool) -> bool {
        if !enabled {
            return false;
        }
        if !bundled_conpty && !self.win32_input_mode {
            return false;
        }
        if self.replies >= MAX_REPLIES_PER_SESSION {
            return false;
        }
        // C# starts `_lastReplyUtc` at DateTime.MinValue, i.e. "long ago": no interval applies
        // to the first reply.
        if self.last_reply_ms.is_some_and(|last| now_ms.saturating_sub(last) < MIN_INTERVAL_MS) {
            return false;
        }
        self.replies += 1;
        self.last_reply_ms = Some(now_ms);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T0: i64 = 1_793_966_400_000; // 2026-11-06T12:00:00Z
    const MINUTE: i64 = 60_000;

    fn win32_gate() -> ProgramStatusProbeGate {
        let mut g = ProgramStatusProbeGate::new();
        g.observe_output("\x1b[?9001h\x1b[?1004h\x1b[?25l"); // conhost's opening bytes
        g
    }

    #[test]
    fn disabled_setting_never_replies() {
        let mut g = win32_gate();
        assert!(!g.try_acquire(false, T0, false));
    }

    #[test]
    fn no_reply_until_the_conpty_asked_for_win32_input_mode() {
        let mut g = ProgramStatusProbeGate::new();
        assert!(!g.try_acquire(true, T0, false));
        g.observe_output("plain output, no mode change");
        assert!(!g.try_acquire(true, T0, false));
        g.observe_output("\x1b[?9001h");
        assert!(g.try_acquire(true, T0, false));
    }

    #[test]
    fn win32_input_mode_off_stops_replies_and_last_toggle_in_a_chunk_wins() {
        let mut g = win32_gate();
        g.observe_output("\x1b[?9001l");
        assert!(!g.win32_input_mode());
        g.observe_output("\x1b[?9001l...\x1b[?9001h");
        assert!(g.win32_input_mode());
        g.observe_output("\x1b[?9001h...\x1b[?9001l");
        assert!(!g.win32_input_mode());
        assert!(!g.try_acquire(true, T0, false));
    }

    #[test]
    fn at_most_one_reply_per_interval() {
        let mut g = win32_gate();
        assert!(g.try_acquire(true, T0, false));
        assert!(!g.try_acquire(true, T0 + 1_000, false));
        assert!(!g.try_acquire(true, T0 + MIN_INTERVAL_MS - 1, false));
        assert!(g.try_acquire(true, T0 + MIN_INTERVAL_MS, false));
    }

    #[test]
    fn is_enabled_follows_the_mode_then_the_conpty() {
        for (mode, bundled, expected) in [
            (ProgramStatusProbeMode::Auto, true, true),   // bundled ConPTY answers
            (ProgramStatusProbeMode::Auto, false, false), // inbox ConPTY does not
            (ProgramStatusProbeMode::On, false, true),    // forced on (inbox experiments)
            (ProgramStatusProbeMode::Off, true, false),   // forced off
        ] {
            assert_eq!(ProgramStatusProbeGate::is_enabled(mode, bundled), expected, "{mode:?} {bundled}");
        }
    }

    // The two C# tests about the stale `AnswerProgramStatusProbe` key and the mode round trip
    // through state.json live with the settings model (`models::app_state` tests).

    #[test]
    fn bundled_conpty_needs_no_win32_input_mode_and_gets_the_raw_reply() {
        let mut g = ProgramStatusProbeGate::new(); // no ?9001h seen
        assert!(!g.try_acquire(true, T0, false));
        assert!(g.try_acquire(true, T0, true));

        assert_eq!(ProgramStatusProbeGate::reply_for(true), "\x1b]7501;?\x1b\\");
        assert_eq!(ProgramStatusProbeGate::reply_for(false), probe_reply_for_conpty());
    }

    #[test]
    fn bundled_conpty_is_still_rate_limited_and_capped() {
        let mut g = ProgramStatusProbeGate::new();
        assert!(g.try_acquire(true, T0, true));
        assert!(!g.try_acquire(true, T0 + 1_000, true));
        assert!(g.try_acquire(true, T0 + MINUTE, true));
        assert!(g.try_acquire(true, T0 + 2 * MINUTE, true));
        assert!(!g.try_acquire(true, T0 + 3 * MINUTE, true));
    }

    #[test]
    fn lifetime_cap_per_session() {
        let mut g = win32_gate();
        let granted = (0..20).filter(|i| g.try_acquire(true, T0 + i * MINUTE, false)).count();
        assert_eq!(granted, MAX_REPLIES_PER_SESSION as usize);
    }

    #[test]
    fn a_clock_that_goes_backwards_never_panics() {
        let mut g = win32_gate();
        assert!(g.try_acquire(true, i64::MAX, false));
        assert!(!g.try_acquire(true, i64::MIN, false));
    }
}
