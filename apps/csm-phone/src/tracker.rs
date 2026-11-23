//! The host-wide session tracker (design section 7.1): one list of sessions for the whole add-on,
//! read from CSM on the worker, and the decision of which sessions have just started to need the
//! person. The tracker does not depend on any phone being connected: a push is decided here, and a
//! phone that is closed still gets it.
//!
//! A session that moves into `needs_approval` or `needs_question` starts one episode, reported once
//! by [`Tracker::observe`]. The episode ends when the status leaves that group; the next move into
//! it starts a new one. The first list after the add-on starts is the baseline: a session already
//! waiting then is part of the baseline and is not reported (a restart must not push again).

use std::collections::BTreeMap;

use csm_remote_proto::msg::{PhoneStatus, SessionInfo};

/// Seconds between two reads of CSM's list. One read is in flight at a time.
pub const REFRESH_SECS: u64 = 2;

/// Whether a status asks the person for something.
pub fn needs_you(status: PhoneStatus) -> bool {
    matches!(status, PhoneStatus::NeedsApproval | PhoneStatus::NeedsQuestion)
}

#[derive(Debug, Default)]
pub struct Tracker {
    /// The last list CSM answered with, `since` stamped. `None` until the first answer.
    last: Option<Vec<SessionInfo>>,
    /// Per session: its status and when it took that status (Unix seconds, the add-on's clock).
    seen: BTreeMap<String, (PhoneStatus, u64)>,
}

/// What one read of CSM's list started.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Refresh {
    /// Sessions that started to need the person in this read (one episode each).
    pub started: Vec<SessionInfo>,
}

impl Tracker {
    /// The last list CSM answered with, if any.
    pub fn last(&self) -> Option<&[SessionInfo]> {
        self.last.as_deref()
    }

    /// Takes the list from CSM. `since` is kept while a session keeps its status and stamped with
    /// `now` when the status changes (or the session is new).
    pub fn observe(&mut self, mut list: Vec<SessionInfo>, now: u64) -> Refresh {
        let baseline = self.last.is_none();
        let mut next = BTreeMap::new();
        let mut started = Vec::new();
        for s in &mut list {
            let prev = self.seen.get(&s.id).copied();
            let (since, changed) = match prev {
                Some((status, since)) if status == s.status => (since, false),
                _ => (now, true),
            };
            s.since = Some(since);
            if changed && needs_you(s.status) && !baseline {
                started.push(s.clone());
            }
            next.insert(s.id.clone(), (s.status, since));
        }
        self.seen = next;
        self.last = Some(list);
        Refresh { started }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::*;

    fn s(id: &str, status: PhoneStatus) -> SessionInfo {
        SessionInfo { id: id.into(), name: format!("name {id}"), folder: "/w".into(), status, branch: None, message: None, group: None, profile: None, since: None }
    }

    #[test]
    fn a_session_already_waiting_at_the_baseline_is_not_reported() {
        let mut t = Tracker::default();
        let r = t.observe(vec![s("a", PhoneStatus::NeedsApproval), s("b", PhoneStatus::Working)], 100);
        assert!(r.started.is_empty(), "a restart must not push for a session already waiting");
        assert_eq!(t.last().map(<[SessionInfo]>::len), Some(2));
    }

    #[test]
    fn moving_into_a_needs_you_status_starts_one_episode_each_time() {
        let mut t = Tracker::default();
        t.observe(vec![s("a", PhoneStatus::Working)], 100);
        let r = t.observe(vec![s("a", PhoneStatus::NeedsApproval)], 110);
        assert_eq!(r.started.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["a"]);
        assert!(t.observe(vec![s("a", PhoneStatus::NeedsApproval)], 112).started.is_empty(), "the same episode is not reported again");
        assert!(t.observe(vec![s("a", PhoneStatus::Working)], 120).started.is_empty());
        let again = t.observe(vec![s("a", PhoneStatus::NeedsQuestion)], 130);
        assert_eq!(again.started.len(), 1, "a new episode after the status left the group");
    }

    #[test]
    fn a_session_that_appears_already_waiting_after_the_baseline_is_reported() {
        let mut t = Tracker::default();
        t.observe(vec![s("a", PhoneStatus::Idle)], 100);
        let r = t.observe(vec![s("a", PhoneStatus::Idle), s("n", PhoneStatus::NeedsQuestion)], 105);
        assert_eq!(r.started.iter().map(|x| x.id.as_str()).collect::<Vec<_>>(), vec!["n"]);
    }

    #[test]
    fn since_is_kept_while_the_status_stays_and_restamped_when_it_changes() {
        let mut t = Tracker::default();
        t.observe(vec![s("a", PhoneStatus::Working)], 100);
        t.observe(vec![s("a", PhoneStatus::Working)], 150);
        assert_eq!(t.last().and_then(|l| l.first()).and_then(|x| x.since), Some(100));
        t.observe(vec![s("a", PhoneStatus::Idle)], 160);
        assert_eq!(t.last().and_then(|l| l.first()).and_then(|x| x.since), Some(160));
    }

    #[test]
    fn a_session_that_disappears_and_comes_back_waiting_is_a_new_episode() {
        let mut t = Tracker::default();
        t.observe(vec![s("a", PhoneStatus::Working)], 100);
        t.observe(vec![s("a", PhoneStatus::NeedsApproval)], 110);
        t.observe(vec![], 120);
        let r = t.observe(vec![s("a", PhoneStatus::NeedsApproval)], 130);
        assert_eq!(r.started.len(), 1);
    }
}
