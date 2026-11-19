//! Per-terminal store of OSC 7501 records (C# `ProgramStatusRecords`): the spec's replacement,
//! `clear`, eviction and lifetime rules, plus the aggregation into one [`ProgramStatusSummary`].
//! Single-threaded; the owner calls it from one thread.

use std::collections::HashMap;

use crate::program_status_payload::{BlockedKind, ProgramState, ProgramStatusReport};

/// The one state a session shows, derived from all of its records. `state` is `None` when
/// there are no records (nothing to show).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatusSummary {
    pub state: Option<ProgramState>,
    pub kind: BlockedKind,
    pub progress: Option<u8>,
    pub message: Option<String>,
    pub app: Option<String>,
}

impl ProgramStatusSummary {
    /// No records.
    pub fn empty() -> Self {
        Self {
            state: None,
            kind: BlockedKind::None,
            progress: None,
            message: None,
            app: None,
        }
    }

    /// States that warrant the "needs attention" badge.
    pub fn wants_attention(&self) -> bool {
        matches!(
            self.state,
            Some(ProgramState::Blocked | ProgramState::Done | ProgramState::Error)
        )
    }
}

/// A session just entered an attention state and the toast rate limit allows one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatusNotice {
    pub message: String,
}

fn is_attention(s: ProgramState) -> bool {
    matches!(
        s,
        ProgramState::Blocked | ProgramState::Done | ProgramState::Error
    )
}

fn is_result(s: ProgramState) -> bool {
    matches!(s, ProgramState::Done | ProgramState::Error)
}

#[derive(Debug, Clone)]
struct Entry {
    report: ProgramStatusReport,
    seq: u64,
}

/// See the module docs.
#[derive(Debug, Default)]
pub struct ProgramStatusRecords {
    // The root record is keyed by "": ids are validated non-empty, so it cannot collide.
    records: HashMap<String, Entry>,
    seq: u64,
}

impl ProgramStatusRecords {
    /// Spec cap: 256 records per terminal; the least recently updated is evicted.
    pub const MAX_RECORDS: usize = 256;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn count(&self) -> usize {
        self.records.len()
    }

    /// Applies one validated report. Returns whether the stored records changed.
    pub fn apply(&mut self, report: ProgramStatusReport) -> bool {
        self.apply_noting_attention(report).0
    }

    /// Applies one validated report. Returns `(changed, entered_attention)`.
    ///
    /// `entered_attention` is true when THIS record just entered blocked/done/error: it did not
    /// exist, or held a different state or kind. Judged per record, not from the aggregate: a
    /// long-lived working child outranks the root, so the root's turn-end `done` would never
    /// change the aggregate and the user would get no badge.
    pub fn apply_noting_attention(&mut self, report: ProgramStatusReport) -> (bool, bool) {
        if report.state == ProgramState::Clear {
            return match report.id {
                None => {
                    if self.records.is_empty() {
                        (false, false)
                    } else {
                        self.records.clear();
                        (true, false)
                    }
                }
                Some(id) => (self.remove_subtree(&id), false),
            };
        }

        let key = report.id.clone().unwrap_or_default();
        let entered_attention = is_attention(report.state)
            && self.records.get(&key).is_none_or(|prev| {
                prev.report.state != report.state || prev.report.kind != report.kind
            });

        // Each report fully REPLACES its record: nothing carries over from the previous one.
        self.seq += 1;
        self.records.insert(
            key.clone(),
            Entry {
                report,
                seq: self.seq,
            },
        );

        if self.records.len() > Self::MAX_RECORDS {
            let victim = self
                .records
                .iter()
                .filter(|(k, _)| **k != key)
                .min_by_key(|(_, e)| e.seq)
                .map(|(k, _)| k.clone());
            if let Some(victim) = victim {
                self.records.remove(&victim);
            }
        }
        (true, entered_attention)
    }

    /// The attached process exited, or a new shell prompt began (OSC 133 A). Working, blocked
    /// and idle records are dropped (there is no heartbeat, so nothing else would ever retire
    /// them); done and error persist because they are results the user has not looked at yet.
    pub fn drop_transient(&mut self) -> bool {
        self.remove_where(|r| {
            matches!(
                r.state,
                ProgramState::Working | ProgramState::Blocked | ProgramState::Idle
            )
        })
    }

    /// The user has seen the session. Done and error are retired; live states are left for the
    /// program to change. Returns whether anything was removed.
    pub fn acknowledge(&mut self) -> bool {
        self.has_unacknowledged_results() && self.remove_where(|r| is_result(r.state))
    }

    /// Any done/error record held. Allocation-free: it is checked on every keystroke.
    pub fn has_unacknowledged_results(&self) -> bool {
        self.records.values().any(|e| is_result(e.report.state))
    }

    /// Any record in blocked/done/error, i.e. a reason for the attention badge to stand.
    pub fn has_attention_record(&self) -> bool {
        self.records.values().any(|e| is_attention(e.report.state))
    }

    /// `app` for a record, inherited from the nearest ancestor that has one.
    pub fn effective_app(&self, id: Option<&str>) -> Option<String> {
        let mut current = id;
        loop {
            if let Some(app) = self
                .records
                .get(current.unwrap_or(""))
                .and_then(|e| e.report.app.as_ref())
            {
                return Some(app.clone());
            }
            let cur = current?;
            current = cur.rfind('/').map(|slash| cur.get(..slash).unwrap_or(""));
        }
    }

    /// Aggregates every record into one session state. Precedence:
    /// **blocked > working > error > done > idle**.
    ///
    /// Blocked first because it is the only state that stops on the user. Working outranks
    /// error and done because those two persist until acknowledged while working is live:
    /// ranking a persisted result above it would let a stale child record hide work that is
    /// happening right now, forever. Ties go to the root record, then to the most recently
    /// updated one.
    pub fn summarize(&self) -> ProgramStatusSummary {
        let mut best: Option<(&String, &Entry)> = None;
        for kv in &self.records {
            if best.is_none_or(|b| beats(kv, b)) {
                best = Some(kv);
            }
        }
        let Some((key, entry)) = best else {
            return ProgramStatusSummary::empty();
        };
        let r = &entry.report;
        ProgramStatusSummary {
            state: Some(r.state),
            kind: r.kind,
            progress: r.progress,
            message: r.message.clone(),
            app: self.effective_app(if key.is_empty() {
                None
            } else {
                Some(key.as_str())
            }),
        }
    }

    fn remove_subtree(&mut self, id: &str) -> bool {
        let prefix = format!("{id}/");
        let before = self.records.len();
        self.records
            .retain(|k, _| !(k == id || k.starts_with(&prefix)));
        self.records.len() != before
    }

    fn remove_where(&mut self, pred: impl Fn(&ProgramStatusReport) -> bool) -> bool {
        let before = self.records.len();
        self.records.retain(|_, e| !pred(&e.report));
        self.records.len() != before
    }
}

fn rank(s: ProgramState) -> u8 {
    match s {
        ProgramState::Blocked => 4,
        ProgramState::Working => 3,
        ProgramState::Error => 2,
        ProgramState::Done => 1,
        ProgramState::Idle | ProgramState::Clear => 0,
    }
}

fn beats(a: (&String, &Entry), b: (&String, &Entry)) -> bool {
    let (ra, rb) = (rank(a.1.report.state), rank(b.1.report.state));
    if ra != rb {
        return ra > rb;
    }
    let (a_root, b_root) = (a.0.is_empty(), b.0.is_empty());
    if a_root != b_root {
        return a_root;
    }
    a.1.seq > b.1.seq
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(state: ProgramState) -> ProgramStatusReport {
        ProgramStatusReport::new(state)
    }
    fn rid(state: ProgramState, id: &str) -> ProgramStatusReport {
        ProgramStatusReport {
            id: Some(id.into()),
            ..ProgramStatusReport::new(state)
        }
    }

    #[test]
    fn empty_store_summarises_to_no_state() {
        let store = ProgramStatusRecords::new();
        assert_eq!(store.summarize().state, None);
        assert!(!store.summarize().wants_attention());
    }

    #[test]
    fn each_report_fully_replaces_its_record() {
        let mut store = ProgramStatusRecords::new();
        store.apply(ProgramStatusReport {
            progress: Some(40),
            message: Some("building".into()),
            ..r(ProgramState::Working)
        });
        store.apply(r(ProgramState::Working));
        let s = store.summarize();
        assert_eq!(s.state, Some(ProgramState::Working));
        assert_eq!(s.progress, None); // not carried over
        assert_eq!(s.message, None);
        assert_eq!(store.count(), 1);
    }

    #[test]
    fn clear_without_id_removes_everything() {
        let mut store = ProgramStatusRecords::new();
        store.apply(r(ProgramState::Working));
        store.apply(rid(ProgramState::Working, "a"));
        store.apply(rid(ProgramState::Done, "b/c"));
        assert!(store.apply(r(ProgramState::Clear)));
        assert_eq!(store.count(), 0);
        assert!(!store.apply(r(ProgramState::Clear))); // nothing left to change
    }

    #[test]
    fn clear_with_id_removes_the_record_and_its_children_only() {
        let mut store = ProgramStatusRecords::new();
        store.apply(r(ProgramState::Idle));
        store.apply(rid(ProgramState::Working, "build"));
        store.apply(rid(ProgramState::Working, "build/test"));
        store.apply(rid(ProgramState::Working, "build/test/unit"));
        store.apply(rid(ProgramState::Working, "buildx")); // sibling sharing a prefix
        store.apply(rid(ProgramState::Clear, "build"));
        assert_eq!(store.count(), 2); // root + buildx
        assert_eq!(store.summarize().state, Some(ProgramState::Working));
    }

    #[test]
    fn clear_of_unknown_id_reports_no_change() {
        let mut store = ProgramStatusRecords::new();
        store.apply(r(ProgramState::Working));
        assert!(!store.apply(rid(ProgramState::Clear, "nope")));
    }

    #[test]
    fn store_caps_at_256_and_evicts_least_recently_updated() {
        let mut store = ProgramStatusRecords::new();
        for i in 0..ProgramStatusRecords::MAX_RECORDS {
            store.apply(rid(ProgramState::Working, &format!("r{i}")));
        }
        // Touch r0 so r1 becomes the oldest.
        store.apply(rid(ProgramState::Blocked, "r0"));
        store.apply(rid(ProgramState::Working, "new"));

        assert_eq!(store.count(), ProgramStatusRecords::MAX_RECORDS);
        // r1 was evicted: clearing it changes nothing. r0 survived.
        assert!(!store.apply(rid(ProgramState::Clear, "r1")));
        assert!(store.apply(rid(ProgramState::Clear, "r0")));
    }

    #[test]
    fn precedence_is_blocked_working_error_done_idle() {
        use ProgramState::*;
        for (a, b, expected) in [
            (Blocked, Working, Blocked),
            (Working, Error, Working),
            (Working, Done, Working),
            (Error, Done, Error),
            (Done, Idle, Done),
            (Blocked, Error, Blocked),
        ] {
            // Both orders of arrival, both root/child placements: precedence, not recency, decides.
            for (first, second) in [(a, b), (b, a)] {
                let mut store = ProgramStatusRecords::new();
                store.apply(rid(first, "x"));
                store.apply(rid(second, "y"));
                assert_eq!(
                    store.summarize().state,
                    Some(expected),
                    "{first:?} {second:?}"
                );

                let mut rooted = ProgramStatusRecords::new();
                rooted.apply(r(first));
                rooted.apply(rid(second, "child"));
                assert_eq!(
                    rooted.summarize().state,
                    Some(expected),
                    "{first:?} {second:?} rooted"
                );
            }
        }
    }

    #[test]
    fn a_stale_done_child_never_hides_live_work() {
        let mut store = ProgramStatusRecords::new();
        store.apply(rid(ProgramState::Done, "old")); // persists until acknowledged
        store.apply(ProgramStatusReport {
            progress: Some(10),
            ..r(ProgramState::Working)
        });
        assert_eq!(store.summarize().state, Some(ProgramState::Working));
        assert_eq!(store.summarize().progress, Some(10));
    }

    #[test]
    fn ties_prefer_the_root_then_the_most_recent() {
        let working = |id: Option<&str>, p: u8| ProgramStatusReport {
            id: id.map(str::to_owned),
            progress: Some(p),
            ..r(ProgramState::Working)
        };
        let mut store = ProgramStatusRecords::new();
        store.apply(working(None, 10));
        store.apply(working(Some("a"), 20));
        assert_eq!(store.summarize().progress, Some(10)); // root wins the tie

        let mut children = ProgramStatusRecords::new();
        children.apply(working(Some("a"), 20));
        children.apply(working(Some("b"), 30));
        assert_eq!(children.summarize().progress, Some(30)); // most recent child
    }

    #[test]
    fn summary_carries_kind_and_message_of_the_winner() {
        let mut store = ProgramStatusRecords::new();
        store.apply(ProgramStatusReport {
            message: Some("root".into()),
            ..r(ProgramState::Working)
        });
        store.apply(ProgramStatusReport {
            kind: BlockedKind::Auth,
            message: Some("login".into()),
            ..rid(ProgramState::Blocked, "deploy/eu")
        });
        let s = store.summarize();
        assert_eq!(s.kind, BlockedKind::Auth);
        assert_eq!(s.message.as_deref(), Some("login"));
        assert!(s.wants_attention());
    }

    #[test]
    fn app_is_inherited_from_the_nearest_ancestor() {
        let with_app = |id: Option<&str>, app: Option<&str>| ProgramStatusReport {
            id: id.map(str::to_owned),
            app: app.map(str::to_owned),
            ..r(ProgramState::Working)
        };
        let mut store = ProgramStatusRecords::new();
        store.apply(with_app(None, Some("cargo")));
        store.apply(with_app(Some("build"), Some("rustc")));
        store.apply(with_app(Some("build/test/unit"), None));
        store.apply(with_app(Some("lint"), None));
        assert_eq!(
            store.effective_app(Some("build/test/unit")).as_deref(),
            Some("rustc")
        ); // skips missing build/test
        assert_eq!(store.effective_app(Some("lint")).as_deref(), Some("cargo"));
        assert_eq!(store.effective_app(None).as_deref(), Some("cargo"));
    }

    #[test]
    fn process_exit_drops_live_records_and_keeps_results() {
        let mut store = ProgramStatusRecords::new();
        store.apply(r(ProgramState::Working));
        store.apply(rid(ProgramState::Blocked, "b"));
        store.apply(rid(ProgramState::Idle, "i"));
        store.apply(rid(ProgramState::Done, "d"));
        store.apply(rid(ProgramState::Error, "e"));

        assert!(store.drop_transient());
        assert_eq!(store.count(), 2);
        assert_eq!(store.summarize().state, Some(ProgramState::Error));
        assert!(!store.drop_transient()); // idempotent
    }

    #[test]
    fn entered_attention_is_judged_per_record() {
        let mut store = ProgramStatusRecords::new();
        assert!(
            !store
                .apply_noting_attention(rid(ProgramState::Working, "bg"))
                .1
        );
        assert!(!store.apply_noting_attention(r(ProgramState::Working)).1);

        // Root finishes while a child keeps working: the aggregate stays "working", but the
        // root record entered done, which is the turn end the user must hear about.
        assert!(store.apply_noting_attention(r(ProgramState::Done)).1);
        assert_eq!(store.summarize().state, Some(ProgramState::Working));

        let again = ProgramStatusReport {
            message: Some("again".into()),
            ..r(ProgramState::Done)
        };
        assert!(!store.apply_noting_attention(again).1); // same state restated

        let question = ProgramStatusReport {
            kind: BlockedKind::Question,
            ..r(ProgramState::Blocked)
        };
        assert!(store.apply_noting_attention(question).1);
        let permission = ProgramStatusReport {
            kind: BlockedKind::Permission,
            ..r(ProgramState::Blocked)
        };
        assert!(store.apply_noting_attention(permission).1); // a new kind is a new ask
        assert!(!store.apply_noting_attention(r(ProgramState::Clear)).1);
    }

    #[test]
    fn attention_and_result_flags_track_the_records() {
        let mut store = ProgramStatusRecords::new();
        assert!(!store.has_attention_record());
        store.apply(r(ProgramState::Working));
        assert!(!store.has_attention_record());
        store.apply(rid(ProgramState::Blocked, "b"));
        assert!(store.has_attention_record());
        assert!(!store.has_unacknowledged_results());
        store.apply(rid(ProgramState::Error, "e"));
        assert!(store.has_unacknowledged_results());
        store.acknowledge();
        assert!(!store.has_unacknowledged_results());
        assert!(store.has_attention_record()); // blocked is still live
    }

    #[test]
    fn acknowledge_retires_done_and_error_only() {
        let mut store = ProgramStatusRecords::new();
        store.apply(r(ProgramState::Done));
        store.apply(rid(ProgramState::Error, "e"));
        store.apply(ProgramStatusReport {
            kind: BlockedKind::Permission,
            ..rid(ProgramState::Blocked, "b")
        });

        assert!(store.acknowledge());
        assert_eq!(store.count(), 1);
        assert_eq!(store.summarize().state, Some(ProgramState::Blocked));
        assert!(!store.acknowledge());
    }
}
