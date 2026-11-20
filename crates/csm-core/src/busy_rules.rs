//! Whether a live session is "busy" (doing something that closing the app would cut off), and
//! why (C# `SessionBusyRules`).
//!
//! - A session that speaks OSC 7501 is judged by its report alone: `working` (with progress)
//!   and `blocked` (mid-turn) are busy; idle/done/error are not. Its output is ignored: an idle
//!   TUI may still repaint.
//! - Any other session is busy while it has produced output within [`RECENT_OUTPUT_WINDOW_MS`].
//! - A running run command is busy either way.

use crate::program_status_payload::ProgramState;

/// How recent the last PTY output must be for a non-7501 session to count as busy (4 s).
pub const RECENT_OUTPUT_WINDOW_MS: i64 = 4_000;

/// Why the session is busy, or `None` when it is idle. Pure function of its inputs; times are
/// epoch milliseconds.
pub fn reason(
    has_program_status: bool,
    agent_state: Option<ProgramState>,
    progress: Option<u8>,
    running_run_commands: u32,
    last_output_ms: Option<i64>,
    now_ms: i64,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::with_capacity(2);

    if has_program_status {
        match agent_state {
            Some(ProgramState::Working) => parts.push(match progress {
                Some(p) => format!("working · {p}%"),
                None => "working".to_owned(),
            }),
            Some(ProgramState::Blocked) => parts.push("waiting for you mid-task".to_owned()),
            _ => {}
        }
    } else if let Some(last) = last_output_ms {
        // A negative age is output that landed after `now` was read (the PTY reader runs on its
        // own thread): that is as recent as it gets, not clock noise.
        if now_ms.saturating_sub(last) <= RECENT_OUTPUT_WINDOW_MS {
            parts.push("producing output".to_owned());
        }
    }

    match running_run_commands {
        0 => {}
        1 => parts.push("1 run command running".to_owned()),
        n => parts.push(format!("{n} run commands running")),
    }

    if parts.is_empty() { None } else { Some(parts.join(" · ")) }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_793_966_400_000;

    fn reason_of(status: bool, state: Option<ProgramState>, progress: Option<u8>, runs: u32, last: Option<i64>) -> Option<String> {
        reason(status, state, progress, runs, last, NOW)
    }

    fn idle() -> Option<String> {
        reason_of(false, None, None, 0, None)
    }

    #[test]
    fn fresh_session_no_output_is_idle() {
        assert_eq!(idle(), None);
    }

    #[test]
    fn quiet_shell_is_idle() {
        assert_eq!(reason_of(false, None, None, 0, Some(NOW - 2 * 60_000)), None);
    }

    #[test]
    fn recent_output_without_program_status_is_busy() {
        assert_eq!(reason_of(false, None, None, 0, Some(NOW - 1_000)).as_deref(), Some("producing output"));
    }

    #[test]
    fn output_exactly_at_window_edge_is_busy() {
        assert!(reason_of(false, None, None, 0, Some(NOW - RECENT_OUTPUT_WINDOW_MS)).is_some());
    }

    #[test]
    fn output_just_outside_window_is_idle() {
        assert_eq!(reason_of(false, None, None, 0, Some(NOW - RECENT_OUTPUT_WINDOW_MS - 1)), None);
    }

    #[test]
    fn working_with_progress() {
        assert_eq!(reason_of(true, Some(ProgramState::Working), Some(40), 0, None).as_deref(), Some("working · 40%"));
    }

    #[test]
    fn working_indeterminate() {
        assert_eq!(reason_of(true, Some(ProgramState::Working), None, 0, None).as_deref(), Some("working"));
    }

    #[test]
    fn blocked_is_busy_mid_task() {
        assert_eq!(reason_of(true, Some(ProgramState::Blocked), None, 0, None).as_deref(), Some("waiting for you mid-task"));
    }

    #[test]
    fn program_status_settled_states_are_idle_even_with_recent_output() {
        // A 7501 session is judged by its report: an idle TUI may still repaint.
        for state in [ProgramState::Idle, ProgramState::Done, ProgramState::Error] {
            assert_eq!(reason_of(true, Some(state), None, 0, Some(NOW)), None, "{state:?}");
        }
    }

    #[test]
    fn program_status_no_records_ignores_output() {
        assert_eq!(reason_of(true, None, None, 0, Some(NOW)), None);
    }

    #[test]
    fn running_run_command_is_busy_even_when_terminal_is_quiet() {
        assert_eq!(reason_of(false, None, None, 1, None).as_deref(), Some("1 run command running"));
    }

    #[test]
    fn reasons_combine() {
        assert_eq!(reason_of(true, Some(ProgramState::Working), None, 3, None).as_deref(), Some("working · 3 run commands running"));
    }

    #[test]
    fn output_after_now_was_read_is_busy() {
        // The PTY reader thread can stamp output just after the close handler read the clock.
        assert!(reason_of(false, None, None, 0, Some(NOW + 5)).is_some());
    }

    #[test]
    fn extreme_timestamps_do_not_overflow() {
        assert!(reason(false, None, None, 0, Some(i64::MIN), i64::MAX).is_none());
        assert!(reason(false, None, None, 0, Some(i64::MAX), i64::MIN).is_some());
    }
}
