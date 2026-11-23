//! Snapshot deltas: per-session upserts and removals, the sidebar order, and the small fields as
//! one block. The server sends a full snapshot at attach and after a reconnect, then deltas keyed
//! by version. A client that gets a delta whose `from` is not its version asks for a snapshot.
//!
//! [`diff`] and [`Delta::apply`] are inverses: `apply(diff(a, b), a) == b` for every `a` and `b`
//! (property-tested). Applying never panics, and changes nothing when the base version differs.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{ClosedView, GroupInfo, GroupView, OpenWithEntry, SessionView, Snapshot};

/// The fields of a snapshot other than the version and the sessions. Sent whole when any changes:
/// they are small.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SmallFields {
    pub groups: Vec<GroupInfo>,
    pub view: GroupView,
    pub rail_tab: Option<String>,
    pub layout_key: String,
    pub inbox: Vec<String>,
    pub focused: Option<String>,
    pub inbox_open: bool,
    pub inbox_height: Option<f32>,
    pub inspector_width: Option<f32>,
    pub recently_closed: Vec<ClosedView>,
    pub open_with: Vec<OpenWithEntry>,
}

impl SmallFields {
    /// The small fields of a snapshot. Destructures it without `..`, so a field added to
    /// [`Snapshot`] fails to compile here until the delta carries it.
    pub fn of(snap: &Snapshot) -> Self {
        let Snapshot {
            version: _,
            sessions: _,
            groups,
            view,
            rail_tab,
            layout_key,
            inbox,
            focused,
            inbox_open,
            inbox_height,
            inspector_width,
            recently_closed,
            open_with,
        } = snap;
        Self {
            groups: groups.clone(),
            view: *view,
            rail_tab: rail_tab.clone(),
            layout_key: layout_key.clone(),
            inbox: inbox.clone(),
            focused: focused.clone(),
            inbox_open: *inbox_open,
            inbox_height: *inbox_height,
            inspector_width: *inspector_width,
            recently_closed: recently_closed.clone(),
            open_with: open_with.clone(),
        }
    }

    fn put(self, snap: &mut Snapshot) {
        snap.groups = self.groups;
        snap.view = self.view;
        snap.rail_tab = self.rail_tab;
        snap.layout_key = self.layout_key;
        snap.inbox = self.inbox;
        snap.focused = self.focused;
        snap.inbox_open = self.inbox_open;
        snap.inbox_height = self.inbox_height;
        snap.inspector_width = self.inspector_width;
        snap.recently_closed = self.recently_closed;
        snap.open_with = self.open_with;
    }
}

/// One change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DeltaOp {
    /// A session was added or changed (a new one goes last until an `Order` places it).
    Upsert { session: Box<SessionView> },
    Remove { id: String },
    /// The sidebar order of the sessions that exist after the upserts and removals.
    Order { ids: Vec<String> },
    /// The small fields, whole.
    Small { fields: Box<SmallFields> },
}

/// What changed from snapshot version `from` to version `to`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    pub from: u64,
    pub to: u64,
    pub ops: Vec<DeltaOp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaError {
    /// The delta starts from another version than the snapshot has: ask for a full snapshot.
    Base { have: u64, want: u64 },
    /// `to` is not after `from`.
    Backwards,
}

impl std::fmt::Display for DeltaError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Base { have, want } => write!(f, "the delta starts at version {want}, the snapshot is at {have}"),
            Self::Backwards => f.write_str("the delta does not move forward"),
        }
    }
}

impl std::error::Error for DeltaError {}

impl Delta {
    /// Applies the delta. Nothing changes on an error.
    pub fn apply(&self, snap: &mut Snapshot) -> Result<(), DeltaError> {
        if self.from != snap.version {
            return Err(DeltaError::Base { have: snap.version, want: self.from });
        }
        if self.to <= self.from {
            return Err(DeltaError::Backwards);
        }
        for op in &self.ops {
            match op {
                DeltaOp::Upsert { session } => match snap.sessions.iter_mut().find(|s| s.id == session.id) {
                    Some(slot) => *slot = (**session).clone(),
                    None => snap.sessions.push((**session).clone()),
                },
                DeltaOp::Remove { id } => snap.sessions.retain(|s| s.id != *id),
                DeltaOp::Order { ids } => reorder(&mut snap.sessions, ids),
                DeltaOp::Small { fields } => (**fields).clone().put(snap),
            }
        }
        snap.version = self.to;
        Ok(())
    }
}

/// Puts the sessions in `ids` order; a session `ids` does not name keeps its relative place after
/// the named ones, and an id with no session is ignored.
fn reorder(sessions: &mut Vec<SessionView>, ids: &[String]) {
    let mut by_id: HashMap<String, SessionView> = HashMap::with_capacity(sessions.len());
    let mut rest = Vec::new();
    let named: std::collections::HashSet<&str> = ids.iter().map(String::as_str).collect();
    for s in sessions.drain(..) {
        if named.contains(s.id.as_str()) {
            by_id.insert(s.id.clone(), s);
        } else {
            rest.push(s);
        }
    }
    for id in ids {
        if let Some(s) = by_id.remove(id) {
            sessions.push(s);
        }
    }
    sessions.extend(rest);
}

/// The delta that turns `old` into `new`. `new.version` must be after `old.version`.
pub fn diff(old: &Snapshot, new: &Snapshot) -> Delta {
    let mut ops = Vec::new();
    let old_by_id: HashMap<&str, &SessionView> = old.sessions.iter().map(|s| (s.id.as_str(), s)).collect();
    let new_ids: std::collections::HashSet<&str> = new.sessions.iter().map(|s| s.id.as_str()).collect();
    for s in &old.sessions {
        if !new_ids.contains(s.id.as_str()) {
            ops.push(DeltaOp::Remove { id: s.id.clone() });
        }
    }
    for s in &new.sessions {
        if old_by_id.get(s.id.as_str()) != Some(&s) {
            ops.push(DeltaOp::Upsert { session: Box::new(s.clone()) });
        }
    }
    // The order the receiver will have after the ops above.
    let mut after: Vec<&str> = old.sessions.iter().map(|s| s.id.as_str()).filter(|id| new_ids.contains(id)).collect();
    for s in &new.sessions {
        if !old_by_id.contains_key(s.id.as_str()) {
            after.push(s.id.as_str());
        }
    }
    let want: Vec<&str> = new.sessions.iter().map(|s| s.id.as_str()).collect();
    if after != want {
        ops.push(DeltaOp::Order { ids: want.iter().map(|s| (*s).to_owned()).collect() });
    }
    let (a, b) = (SmallFields::of(old), SmallFields::of(new));
    if a != b {
        ops.push(DeltaOp::Small { fields: Box::new(b) });
    }
    Delta { from: old.version, to: new.version, ops }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Status;

    fn session(id: &str) -> SessionView {
        SessionView {
            id: id.into(),
            name: id.into(),
            kind: "local".into(),
            folder: String::new(),
            folder_short: String::new(),
            annotation: None,
            owner_addon: None,
            command: "claude".into(),
            group_id: String::new(),
            accent: "#000000".into(),
            accent_override: false,
            status: Status::Idle,
            status_label: "idle".into(),
            progress: None,
            status_message: None,
            live: true,
            asleep: false,
            exit_code: None,
            branch: None,
            git_dirty: false,
            git_upstream: None,
            git_ahead: None,
            git_behind: None,
            git_changes: 0,
            git_summary: None,
            worktree_of: None,
            repo_root: None,
            git_busy: false,
            title: None,
            is_claude: true,
            claude_session_id: None,
            reports_status: false,
            remote_control: false,
            remote_control_url: None,
            last_activity_ms: None,
            inbox_since_ms: None,
            status_since_ms: None,
            project: String::new(),
            project_name: String::new(),
            backend: None,
            runs: vec![],
            open_with: vec![],
            subagents_running: 0,
            subagents_total: 0,
        }
    }

    fn snap(version: u64, ids: &[&str]) -> Snapshot {
        Snapshot { version, sessions: ids.iter().map(|i| session(i)).collect(), layout_key: "__ALL__".into(), inbox_open: true, ..Snapshot::default() }
    }

    #[test]
    fn a_delta_carries_only_what_changed() {
        let a = snap(1, &["a", "b", "c"]);
        let mut b = snap(2, &["a", "b", "c"]);
        b.sessions[1].status = Status::Working;
        let d = diff(&a, &b);
        assert_eq!(d.ops.len(), 1, "{d:?}");
        assert!(matches!(&d.ops[0], DeltaOp::Upsert { session } if session.id == "b"));
        let mut got = a.clone();
        d.apply(&mut got).unwrap();
        assert_eq!(got, b);
    }

    #[test]
    fn add_remove_reorder_and_small_fields() {
        let a = snap(5, &["a", "b", "c"]);
        let mut b = snap(6, &["d", "c", "a"]);
        b.focused = Some("d".into());
        b.inbox = vec!["c".into()];
        let d = diff(&a, &b);
        let mut got = a.clone();
        d.apply(&mut got).unwrap();
        assert_eq!(got, b);
        assert_eq!(got.version, 6);
        // Equal content: no ops, only the version moves.
        let c = snap(7, &["d", "c", "a"]);
        let mut c2 = b.clone();
        c2.focused = None;
        c2.inbox.clear();
        let d = diff(&c2, &c);
        assert!(d.ops.is_empty(), "{d:?}");
    }

    #[test]
    fn a_wrong_base_changes_nothing() {
        let a = snap(1, &["a"]);
        let d = Delta { from: 9, to: 10, ops: vec![DeltaOp::Remove { id: "a".into() }] };
        let mut got = a.clone();
        assert_eq!(d.apply(&mut got), Err(DeltaError::Base { have: 1, want: 9 }));
        assert_eq!(got, a);
        let back = Delta { from: 1, to: 1, ops: vec![] };
        assert_eq!(back.apply(&mut got), Err(DeltaError::Backwards));
        assert_eq!(got, a);
    }

    /// A tiny xorshift generator: the property test needs no dependency.
    struct Rng(u64);
    impl Rng {
        fn below(&mut self, n: usize) -> usize {
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            (self.0.wrapping_mul(0x2545_F491_4F6C_DD1D) % n.max(1) as u64) as usize
        }
    }

    fn random_snapshot(rng: &mut Rng, version: u64) -> Snapshot {
        let mut ids: Vec<String> = (0..rng.below(8)).map(|_| format!("s{}", rng.below(12))).collect();
        ids.sort();
        ids.dedup();
        for i in (1..ids.len()).rev() {
            ids.swap(i, rng.below(i + 1));
        }
        let mut s = Snapshot { version, layout_key: "__ALL__".into(), inbox_open: rng.below(2) == 0, ..Snapshot::default() };
        for id in &ids {
            let mut v = session(id);
            v.name = format!("n{}", rng.below(3));
            v.status = [Status::Idle, Status::Working, Status::DoneUnseen][rng.below(3)];
            v.git_changes = rng.below(3);
            s.sessions.push(v);
        }
        s.inbox = ids.iter().filter(|_| rng.below(3) == 0).cloned().collect();
        s.focused = ids.first().filter(|_| rng.below(2) == 0).cloned();
        s.rail_tab = (rng.below(3) == 0).then(|| "g1".to_owned());
        s.inbox_height = (rng.below(2) == 0).then_some(120.5);
        s
    }

    #[test]
    fn diff_then_apply_is_the_identity_for_random_snapshots() {
        let mut rng = Rng(0x1234_5678_9abc_def1);
        for round in 0..2000u64 {
            let a = random_snapshot(&mut rng, round);
            let step = rng.below(5) as u64;
            let b = random_snapshot(&mut rng, round + 1 + step);
            let d = diff(&a, &b);
            let mut got = a.clone();
            d.apply(&mut got).unwrap();
            assert_eq!(got, b, "round {round}: {d:?}");
            // Over the wire and back.
            let wire: Delta = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
            let mut again = a.clone();
            wire.apply(&mut again).unwrap();
            assert_eq!(again, b);
        }
    }

    #[test]
    fn hostile_ops_do_not_panic() {
        let mut s = snap(1, &["a", "b"]);
        let d = Delta {
            from: 1,
            to: 2,
            ops: vec![
                DeltaOp::Remove { id: "nope".into() },
                DeltaOp::Order { ids: vec!["zz".into(), "b".into(), "b".into(), "a".into()] },
                DeltaOp::Upsert { session: Box::new(session("a")) },
            ],
        };
        d.apply(&mut s).unwrap();
        let ids: Vec<&str> = s.sessions.iter().map(|x| x.id.as_str()).collect();
        assert_eq!(ids, ["b", "a"]);
    }
}
