//! One status vocabulary (docs/plan.md section 4): every session is in exactly one of these, and
//! the sidebar, pane header, palette, tray, inbox and `session_wait` all use the same value.
//!
//! OSC 7501 drives it when the program speaks it; the screen heuristic fills in when it doesn't
//! (the WPF app's fallback rule, given one face).

use csm_core::models::AlertType;
use csm_core::program_status_payload::{BlockedKind, ProgramState};
use serde::{Deserialize, Serialize};

use crate::session::{Phase, SessionRt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Launching: waiting for the first measured size, the Claude stagger, or the process.
    Starting,
    /// The program reports work in progress (see `progress`).
    Working,
    /// Waiting for the person to approve something.
    NeedsYouApproval,
    /// Waiting for an answer, input or sign-in.
    NeedsYouQuestion,
    /// Finished a turn the person has not looked at yet.
    DoneUnseen,
    /// Failed a turn (or reported an error) the person has not looked at yet.
    Error,
    Idle,
    /// No process: kept in the sidebar, woken on demand.
    Asleep,
    /// The process ended by itself; the pane stays until closed or restarted.
    Exited,
}

impl Status {
    pub fn id(self) -> &'static str {
        match self {
            Status::Starting => "starting",
            Status::Working => "working",
            Status::NeedsYouApproval => "needs_you_approval",
            Status::NeedsYouQuestion => "needs_you_question",
            Status::DoneUnseen => "done_unseen",
            Status::Error => "error",
            Status::Idle => "idle",
            Status::Asleep => "asleep",
            Status::Exited => "exited",
        }
    }

    pub fn from_id(s: &str) -> Option<Status> {
        [
            Status::Starting,
            Status::Working,
            Status::NeedsYouApproval,
            Status::NeedsYouQuestion,
            Status::DoneUnseen,
            Status::Error,
            Status::Idle,
            Status::Asleep,
            Status::Exited,
        ]
        .into_iter()
        .find(|v| v.id() == s)
    }

    /// One plain word for the UI ("Needs you").
    pub fn label(self) -> &'static str {
        match self {
            Status::Starting => "Starting",
            Status::Working => "Working",
            Status::NeedsYouApproval | Status::NeedsYouQuestion => "Needs you",
            Status::DoneUnseen => "Done",
            Status::Error => "Error",
            Status::Idle => "Idle",
            Status::Asleep => "Asleep",
            Status::Exited => "Exited",
        }
    }

    pub fn needs_you(self) -> bool {
        matches!(self, Status::NeedsYouApproval | Status::NeedsYouQuestion)
    }

    /// In the inbox: needs you, or a result not yet seen.
    pub fn in_inbox(self) -> bool {
        self.needs_you() || matches!(self, Status::DoneUnseen | Status::Error)
    }

    /// Mid-turn: input sent now would land in the middle of the program's work.
    pub fn is_mid_turn(self) -> bool {
        self == Status::Working
    }
}

/// What a `session_wait` waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WaitUntil {
    /// Not working and not starting: idle, a result (done/error), or exited. Not "needs you".
    Idle,
    /// A turn finished: done, error or exited.
    Done,
    /// Waiting for the person (approval or question).
    NeedsYou,
    /// The process exited, the session went to sleep or was closed.
    Exit,
    /// Any status change from the status at the call.
    AnyChange,
}

impl WaitUntil {
    /// Whether `status` satisfies the wait (`start` is the status when it began).
    pub fn reached(self, status: Status, start: Status) -> bool {
        match self {
            WaitUntil::Idle => {
                matches!(status, Status::Idle | Status::DoneUnseen | Status::Error | Status::Exited)
            }
            WaitUntil::Done => matches!(status, Status::DoneUnseen | Status::Error | Status::Exited),
            WaitUntil::NeedsYou => status.needs_you(),
            WaitUntil::Exit => matches!(status, Status::Exited | Status::Asleep),
            WaitUntil::AnyChange => status != start,
        }
    }
}

/// The unified status of a session plus its progress and message.
pub(crate) fn derive(is_dormant: bool, restarting: bool, rt: Option<&SessionRt>, mark: Option<&crate::plugins::PluginMark>) -> (Status, Option<u8>, Option<String>) {
    if is_dormant {
        return (Status::Asleep, None, None);
    }
    let Some(rt) = rt else {
        return (if restarting { Status::Starting } else { Status::Asleep }, None, None);
    };
    if matches!(rt.phase, Phase::Starting) {
        return (Status::Starting, None, None);
    }
    // A process add-on that opened this session may say what it is doing (a remote agent the app
    // cannot see into). It outranks what the screen suggests, but not a process that has ended.
    if let Some(s) = mark.and_then(|m| m.status).filter(|_| !matches!(rt.phase, Phase::Exited { .. })) {
        return (s, None, None);
    }
    let sum = rt.records.summarize();
    let reported = match sum.state {
        Some(ProgramState::Blocked) => Some(if sum.kind == BlockedKind::Permission { Status::NeedsYouApproval } else { Status::NeedsYouQuestion }),
        Some(ProgramState::Working) => Some(Status::Working),
        Some(ProgramState::Error) => Some(Status::Error),
        Some(ProgramState::Done) => Some(Status::DoneUnseen),
        Some(ProgramState::Idle | ProgramState::Clear) | None => None,
    };
    if let Some(s) = reported {
        let progress = if s == Status::Working { sum.progress } else { None };
        return (s, progress, sum.message);
    }
    if let Some(a) = rt.addon_status {
        let s = match a {
            csm_core::addons::ProfileStatus::NeedsYouApproval => Status::NeedsYouApproval,
            csm_core::addons::ProfileStatus::NeedsYouQuestion => Status::NeedsYouQuestion,
            csm_core::addons::ProfileStatus::Working => Status::Working,
            csm_core::addons::ProfileStatus::Idle => Status::Idle,
            csm_core::addons::ProfileStatus::Done => Status::DoneUnseen,
        };
        return (s, None, None);
    }
    if let Some(alert) = rt.heuristic {
        let s = match alert {
            AlertType::ToolApproval => Status::NeedsYouApproval,
            AlertType::InputRequired | AlertType::Question => Status::NeedsYouQuestion,
            AlertType::Idle => Status::DoneUnseen,
        };
        return (s, None, rt.heuristic_message.clone());
    }
    if matches!(rt.phase, Phase::Exited { .. }) {
        return (Status::Exited, None, None);
    }
    (Status::Idle, None, sum.message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_wait_rules() {
        for s in [Status::Starting, Status::Working, Status::NeedsYouApproval, Status::Asleep, Status::Exited] {
            assert_eq!(Status::from_id(s.id()), Some(s));
            assert_eq!(serde_json::to_value(s).unwrap(), s.id());
        }
        assert!(WaitUntil::Idle.reached(Status::DoneUnseen, Status::Working));
        assert!(!WaitUntil::Idle.reached(Status::NeedsYouQuestion, Status::Working));
        assert!(WaitUntil::Exit.reached(Status::Asleep, Status::Idle));
        assert!(!WaitUntil::AnyChange.reached(Status::Idle, Status::Idle));
    }
}
