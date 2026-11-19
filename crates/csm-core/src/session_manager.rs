//! The session list and group list (C# `SessionManager`), without the change events.
//!
//! Ported because the sidebar-reorder scenario tests exercise it end to end and because its
//! `load_from_state` carries two migrations. The C# class raises `SessionAdded` /
//! `SessionRemoved` / `SessionsChanged` / `GroupsChanged` events; a Rust caller learns what
//! happened from the return values and notifies its own observers. Identifiers and the clock
//! are inputs (this crate has neither a random source nor a clock).

use crate::models::{AppState, SessionGroup, SessionStatus, ShellSession, get_file_name};
use crate::session_order;
use crate::time::Timestamp;

/// Owns the ordered session list (live and dormant, every group) and the ordered group list.
#[derive(Debug, Default)]
pub struct SessionManager {
    sessions: Vec<ShellSession>,
    groups: Vec<SessionGroup>,
}

/// Everything [`SessionManager::create_session`] needs. `id` and `now` replace C#'s
/// `Guid.NewGuid()` and `DateTime.UtcNow`.
#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    pub id: String,
    pub now: Timestamp,
    pub name: &'a str,
    pub folder: &'a str,
    pub command: &'a str,
    pub args: &'a str,
    pub group_id: Option<&'a str>,
    pub color_override: Option<&'a str>,
    pub after_session_id: Option<&'a str>,
}

impl SessionManager {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn sessions(&self) -> &[ShellSession] {
        &self.sessions
    }

    pub fn groups(&self) -> &[SessionGroup] {
        &self.groups
    }

    pub fn session(&self, id: &str) -> Option<&ShellSession> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn session_mut(&mut self, id: &str) -> Option<&mut ShellSession> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    /// Creates a session and returns it. A blank `name` becomes the folder's leaf name. It is
    /// inserted right after `after_session_id` when that session exists, else appended.
    pub fn create_session(&mut self, new: NewSession<'_>) -> &mut ShellSession {
        let name = if new.name.trim().is_empty() {
            // C# parity: `Path.GetFileName(..) ?? command` never falls back (GetFileName
            // returns "" rather than null), so a folderless blank-named session stays blank.
            get_file_name(new.folder.trim_end_matches(['/', '\\'])).to_owned()
        } else {
            new.name.to_owned()
        };
        let session = ShellSession {
            id: new.id,
            name,
            working_folder: new.folder.to_owned(),
            command: new.command.to_owned(),
            args: new.args.to_owned(),
            group_id: new.group_id.unwrap_or_default().to_owned(),
            color_override: new.color_override.map(str::to_owned),
            status: SessionStatus::Running,
            created_at: new.now.clone(),
            last_activity_at: new.now,
            ..Default::default()
        };

        let insert_at = new
            .after_session_id
            .filter(|a| !a.is_empty())
            .and_then(|a| self.sessions.iter().position(|s| s.id == a))
            .map(|i| i + 1)
            .filter(|i| *i <= self.sessions.len());
        let at = insert_at.unwrap_or(self.sessions.len());
        self.sessions.insert(at, session);
        &mut self.sessions[at]
    }

    /// Removes a session, returning it if it existed.
    pub fn remove_session(&mut self, id: &str) -> Option<ShellSession> {
        let idx = self.sessions.iter().position(|s| s.id == id)?;
        Some(self.sessions.remove(idx))
    }

    pub fn rename_session(&mut self, id: &str, new_name: &str) {
        if let Some(s) = self.session_mut(id) {
            s.name = new_name.to_owned();
        }
    }

    /// Reorders the session list in place using a comparison. Sorting is a one-shot apply; the
    /// user can keep refining by drag-reorder afterwards. The sort is stable.
    pub fn sort_sessions(&mut self, mut cmp: impl FnMut(&ShellSession, &ShellSession) -> std::cmp::Ordering) {
        self.sessions.sort_by(|a, b| cmp(a, b));
    }

    /// Moves a session to sit immediately before (or after) another, by id. Deliberately no
    /// index-based overload: this list holds dormant sessions and every group, so an index
    /// computed anywhere else means a different slot here, which is how a dragged session used
    /// to land elsewhere. Returns true when the order changed.
    pub fn move_session_relative(&mut self, session_id: &str, anchor_id: &str, after: bool) -> bool {
        session_order::move_relative(&mut self.sessions, |s| s.id.as_str(), session_id, anchor_id, after)
    }

    pub fn update_status(&mut self, id: &str, status: SessionStatus) {
        if let Some(s) = self.session_mut(id) {
            s.status = status;
        }
    }

    /// Adds a group at the end; `sort_order` is its position.
    pub fn add_group(&mut self, id: String, name: &str) -> &SessionGroup {
        let sort_order = i32::try_from(self.groups.len()).unwrap_or(i32::MAX);
        self.groups.push(SessionGroup { id, name: name.to_owned(), sort_order, is_expanded: true });
        &self.groups[self.groups.len() - 1]
    }

    /// Renames a group; a blank name is ignored, a non-blank one is trimmed.
    pub fn rename_group(&mut self, id: &str, new_name: &str) {
        if new_name.trim().is_empty() {
            return;
        }
        if let Some(g) = self.groups.iter_mut().find(|g| g.id == id) {
            g.name = new_name.trim().to_owned();
        }
    }

    /// Removes a group. Sessions assigned to it become ungrouped; they are not deleted.
    pub fn remove_group(&mut self, id: &str) {
        let Some(idx) = self.groups.iter().position(|g| g.id == id) else {
            return;
        };
        for s in &mut self.sessions {
            if s.group_id == id {
                s.group_id.clear();
            }
        }
        self.groups.remove(idx);
    }

    /// Moves a group to a new position (0-based; `len` means "insert at end") and renumbers
    /// `sort_order` so the order survives persistence.
    pub fn move_group(&mut self, id: &str, new_index: i64) {
        let Some(cur) = self.groups.iter().position(|g| g.id == id) else {
            return;
        };
        let len = self.groups.len();
        let mut target = usize::try_from(new_index.clamp(0, i64::try_from(len).unwrap_or(i64::MAX))).unwrap_or(len);
        // After the removal, every index above `cur` shifts down by one.
        if cur < target {
            target -= 1;
        }
        if cur == target {
            return;
        }
        let group = self.groups.remove(cur);
        self.groups.insert(target.min(self.groups.len()), group);
        for (i, g) in self.groups.iter_mut().enumerate() {
            g.sort_order = i32::try_from(i).unwrap_or(i32::MAX);
        }
    }

    /// Assigns a session to a group (`None` or empty = ungrouped).
    pub fn set_session_group(&mut self, session_id: &str, group_id: Option<&str>) {
        if let Some(s) = self.session_mut(session_id) {
            s.group_id = group_id.unwrap_or_default().to_owned();
        }
    }

    /// Replaces the contents from a loaded state. Sessions are configs only (they relaunch).
    ///
    /// Two migrations: `last_activity_at` is backfilled from `created_at` for sessions saved
    /// before the field existed; and the legacy auto-created single "Default" group
    /// (`sort_order` 0) is dropped, once, gated on `legacy_default_group_cleared` so a
    /// user-named "Default" group created later survives. The flag is set on the state
    /// regardless of whether the heuristic matched.
    pub fn load_from_state(&mut self, state: &mut AppState) {
        self.sessions.clear();
        self.groups = state.groups.clone();

        for s in &state.sessions {
            let mut s = s.clone();
            if s.last_activity_at.is_never() {
                s.last_activity_at = s.created_at.clone();
            }
            self.sessions.push(s);
        }

        if !state.settings.legacy_default_group_cleared {
            if self.groups.len() == 1 && self.groups[0].name == "Default" && self.groups[0].sort_order == 0 {
                let legacy_id = self.groups[0].id.clone();
                for s in &mut self.sessions {
                    if s.group_id == legacy_id {
                        s.group_id.clear();
                    }
                }
                self.groups.clear();
            }
            state.settings.legacy_default_group_cleared = true;
        }
    }

    /// Writes the sessions and groups back into a state for saving.
    pub fn populate_state(&self, state: &mut AppState) {
        state.sessions = self.sessions.clone();
        state.groups = self.groups.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_order::{SessionPlacement, align_to, cluster_anchor_for};
    use crate::sidebar_drop::{SidebarRow, SidebarRowKind, resolve};
    use std::collections::HashMap;

    const ROW_H: f64 = 40.0;
    const CLUSTER_H: f64 = 22.0;
    const HEADER_H: f64 = 26.0;

    #[derive(Default)]
    struct World {
        manager: SessionManager,
        grid: Vec<String>,             // _vm.Sessions ids
        repo: HashMap<String, String>, // SessionViewModel.RepoRoot
        mode: &'static str,            // None | FilterStrip | Inline
        active_group: Option<String>,
        groups: Vec<String>,
    }

    impl World {
        fn new() -> Self {
            Self { mode: "None", ..Default::default() }
        }

        fn add(mut self, id: &str) -> Self {
            self.add_full(id, "", false, None);
            self
        }

        fn add_group(mut self, id: &str, group: &str) -> Self {
            self.add_full(id, group, false, None);
            self
        }

        fn add_dormant(mut self, id: &str, group: &str) -> Self {
            self.add_full(id, group, true, None);
            self
        }

        fn add_repo(mut self, id: &str, repo: &str) -> Self {
            self.add_full(id, "", false, Some(repo));
            self
        }

        fn add_full(&mut self, id: &str, group: &str, dormant: bool, repo: Option<&str>) {
            let s = self.manager.create_session(NewSession {
                id: id.to_owned(),
                now: Timestamp::never(),
                name: id,
                folder: &format!(r"C:\src\{id}"),
                command: "claude",
                args: "",
                group_id: Some(group),
                color_override: None,
                after_session_id: None,
            });
            s.is_dormant = dormant;
            if !dormant {
                self.grid.push(id.to_owned());
            }
            if let Some(r) = repo {
                self.repo.insert(id.to_owned(), r.to_owned());
            }
        }

        fn root_of(&self, id: &str) -> Option<&String> {
            self.repo.get(id)
        }

        // Mirrors RebuildSidebarOrder (incl. dormant rows obeying the group filter, #142).
        fn render(&self) -> Vec<SidebarRow> {
            let mut rows: Vec<(SidebarRowKind, Option<String>)> = Vec::new();

            let live_rows = |rows: &mut Vec<(SidebarRowKind, Option<String>)>, live: Vec<&ShellSession>| {
                let mut i = 0;
                while i < live.len() {
                    let mut j = i;
                    let root = self.root_of(&live[i].id);
                    while root.is_some() && j + 1 < live.len() && self.root_of(&live[j + 1].id) == root {
                        j += 1;
                    }
                    if j > i {
                        rows.push((SidebarRowKind::ClusterHeader, None));
                    }
                    for s in &live[i..=j] {
                        rows.push((SidebarRowKind::Session, Some(s.id.clone())));
                    }
                    i = j + 1;
                }
            };

            if self.mode == "Inline" && !self.groups.is_empty() {
                let mut all = vec![String::new()];
                all.extend(self.groups.iter().cloned());
                for g in all {
                    let members: Vec<&ShellSession> = self.manager.sessions().iter().filter(|s| s.group_id == g).collect();
                    if g.is_empty() && members.is_empty() {
                        continue;
                    }
                    rows.push((SidebarRowKind::GroupHeader, (!g.is_empty()).then(|| g.clone())));
                    live_rows(&mut rows, members.iter().copied().filter(|s| !s.is_dormant).collect());
                    rows.extend(members.iter().filter(|s| s.is_dormant).map(|s| (SidebarRowKind::Dormant, Some(s.id.clone()))));
                }
            } else {
                let visible = |s: &ShellSession| {
                    self.mode != "FilterStrip" || self.active_group.is_none() || self.active_group.as_deref() == Some(s.group_id.as_str())
                };
                live_rows(&mut rows, self.manager.sessions().iter().filter(|s| !s.is_dormant && visible(s)).collect());
                rows.extend(
                    self.manager.sessions().iter().filter(|s| s.is_dormant && visible(s)).map(|s| (SidebarRowKind::Dormant, Some(s.id.clone()))),
                );
            }

            let mut y = 0.0;
            rows.into_iter()
                .map(|(kind, id)| {
                    let h = match kind {
                        SidebarRowKind::ClusterHeader => CLUSTER_H,
                        SidebarRowKind::GroupHeader => HEADER_H,
                        _ => ROW_H,
                    };
                    let row = SidebarRow { kind, id, top: y, height: h };
                    y += h;
                    row
                })
                .collect()
        }

        fn above_row(&self, id: &str) -> f64 {
            let r = self.render().into_iter().find(|r| r.kind == SidebarRowKind::Session && r.id.as_deref() == Some(id)).unwrap();
            r.top + r.height * 0.25
        }

        fn below_everything(&self) -> f64 {
            self.render().iter().rev().find(|r| r.kind == SidebarRowKind::Session).unwrap().bottom() - 2.0
        }

        // Mirrors MainWindow.ApplySessionDrop.
        fn drop_on(&mut self, dragged: &str, y: f64) {
            let t = resolve(&self.render(), y).unwrap();
            if let Some(b) = &t.before_session_id {
                self.manager.move_session_relative(dragged, b, false);
            } else if let Some(a) = &t.after_session_id {
                self.manager.move_session_relative(dragged, a, true);
            }
            let current = self.manager.session(dragged).map(|s| s.group_id.clone()).unwrap_or_default();
            if t.assigns_group && current != t.group_id.clone().unwrap_or_default() {
                self.manager.set_session_group(dragged, t.group_id.as_deref());
            }
            let reference: Vec<String> = self.manager.sessions().iter().map(|s| s.id.clone()).collect();
            align_to(&mut self.grid, &reference, |s| s.as_str());
        }

        fn sidebar(&self) -> Vec<String> {
            self.render().into_iter().filter(|r| matches!(r.kind, SidebarRowKind::Session | SidebarRowKind::Dormant)).filter_map(|r| r.id).collect()
        }

        fn live_in(&self, group: &str) -> Vec<String> {
            self.manager.sessions().iter().filter(|s| s.group_id == group && !s.is_dormant).map(|s| s.id.clone()).collect()
        }
    }

    fn v(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn scenario1_moving_down_one_slot_does_not_overshoot() {
        let mut w = World::new().add("a").add("b").add("c");
        let y = w.above_row("c");
        w.drop_on("a", y);
        assert_eq!(w.sidebar(), v(&["b", "a", "c"])); // was [b, c, a]
    }

    #[test]
    fn scenario2_a_worktree_cluster_header_does_not_shift_the_drop() {
        let mut w = World::new().add("web").add_repo("api", "R").add_repo("api (PS)", "R").add("docs");
        let y = w.above_row("api");
        w.drop_on("docs", y);
        assert_eq!(w.sidebar(), v(&["web", "docs", "api", "api (PS)"])); // was [web, api, docs, api (PS)]
    }

    #[test]
    fn scenario3_filter_strip_drop_lands_where_aimed_within_the_visible_group() {
        let mut w = World { mode: "FilterStrip", active_group: Some("claude".into()), groups: v(&["claude", "ps"]), ..Default::default() }
            .add_group("p1", "ps")
            .add_group("c1", "claude")
            .add_group("p2", "ps")
            .add_group("c2", "claude")
            .add_group("c3", "claude");
        let y = w.above_row("c2");
        w.drop_on("c3", y);
        assert_eq!(w.live_in("claude"), v(&["c1", "c3", "c2"])); // was [c3, c1, c2]
        assert_eq!(w.live_in("ps"), v(&["p1", "p2"])); // the hidden group is untouched
    }

    fn inline_world() -> World {
        World { mode: "Inline", groups: v(&["claude", "ps"]), ..Default::default() }
            .add_dormant("zz", "claude")
            .add_group("c1", "claude")
            .add_group("c2", "claude")
            .add_group("p1", "ps")
    }

    #[test]
    fn scenario4_inline_cross_group_drop_with_a_sleeping_session_lands_where_aimed() {
        let mut w = inline_world();
        let y = w.above_row("c2");
        w.drop_on("p1", y);
        assert_eq!(w.live_in("claude"), v(&["c1", "p1", "c2"])); // was [p1, c1, c2]
        assert!(w.live_in("ps").is_empty());
    }

    #[test]
    fn scenario5_the_pane_grid_follows_the_sidebar_after_a_drop() {
        let mut w = inline_world();
        let y = w.above_row("c2");
        w.drop_on("p1", y);
        let live_sidebar_order: Vec<String> = w.manager.sessions().iter().filter(|s| !s.is_dormant).map(|s| s.id.clone()).collect();
        assert_eq!(live_sidebar_order, w.grid); // grid was [c1, p1, c2] vs sidebar [p1, c1, c2]
    }

    #[test]
    fn scenario6_a_ps_session_dragged_away_from_its_claude_session_stays_put() {
        // The drop itself was always right here; what undid it was the auto-cluster pass
        // re-sorting every sibling on the next Add/Remove. That pass now only ever places a
        // session created in this run, and an unrelated new session has nothing to be placed
        // next to.
        let mut w = World::new().add_repo("api", "R").add_repo("api (PS)", "R").add("web");
        let y = w.below_everything();
        w.drop_on("api (PS)", y);
        assert_eq!(w.sidebar(), v(&["api", "web", "api (PS)"]));

        w = w.add_repo("new", "N");
        let placements: Vec<SessionPlacement> = w
            .manager
            .sessions()
            .iter()
            .map(|s| SessionPlacement {
                id: s.id.clone(),
                group_id: s.group_id.clone(),
                is_live: !s.is_dormant,
                repo_root: w.repo.get(&s.id).cloned(),
            })
            .collect();
        assert_eq!(cluster_anchor_for(&placements, "new"), None);
        assert_eq!(w.sidebar(), v(&["api", "web", "api (PS)", "new"]));
    }

    #[test]
    fn scenario6b_dropping_within_the_cluster_keeps_both_siblings_together() {
        let mut w = World::new().add("web").add_repo("api", "R").add_repo("api (PS)", "R");
        let y = w.above_row("api");
        w.drop_on("api (PS)", y);
        assert_eq!(w.sidebar(), v(&["web", "api (PS)", "api"]));
        assert!(w.render().iter().any(|r| r.kind == SidebarRowKind::ClusterHeader));
    }

    // ---- additions: SessionManager behaviour that has no C# unit test of its own ----

    fn new_session<'a>(id: &str, after: Option<&'a str>) -> NewSession<'a> {
        NewSession {
            id: id.to_owned(),
            now: Timestamp::from_epoch_ms(1_000),
            name: "",
            folder: r"C:\src\proj\",
            command: "claude",
            args: "",
            group_id: None,
            color_override: None,
            after_session_id: after,
        }
    }

    #[test]
    fn create_session_names_from_the_folder_and_inserts_after_a_parent() {
        let mut m = SessionManager::new();
        assert_eq!(m.create_session(new_session("a", None)).name, "proj");
        m.create_session(new_session("b", None));
        m.create_session(new_session("c", Some("a")));
        m.create_session(new_session("d", Some("missing")));
        let ids: Vec<&str> = m.sessions().iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, ["a", "c", "b", "d"]);
        assert_eq!(m.session("a").map(|s| s.status), Some(SessionStatus::Running));
    }

    #[test]
    fn load_from_state_backfills_last_activity_and_drops_the_legacy_default_group_once() {
        let mut state = AppState::default();
        state.groups.push(SessionGroup { id: "legacy".into(), name: "Default".into(), sort_order: 0, is_expanded: true });
        state.sessions.push(ShellSession {
            id: "s".into(),
            group_id: "legacy".into(),
            created_at: Timestamp::from_epoch_ms(5_000),
            ..Default::default()
        });
        let mut m = SessionManager::new();
        m.load_from_state(&mut state);
        assert_eq!(m.sessions()[0].last_activity_at, Timestamp::from_epoch_ms(5_000));
        assert!(m.groups().is_empty());
        assert_eq!(m.sessions()[0].group_id, "");
        assert!(state.settings.legacy_default_group_cleared);

        // A user-named "Default" group created later survives, because the flag is now set.
        state.groups.clear();
        state.groups.push(SessionGroup { id: "mine".into(), name: "Default".into(), sort_order: 0, is_expanded: true });
        m.load_from_state(&mut state);
        assert_eq!(m.groups().len(), 1);
    }

    #[test]
    fn group_operations() {
        let mut m = SessionManager::new();
        m.add_group("g1".into(), "one");
        m.add_group("g2".into(), "two");
        m.add_group("g3".into(), "three");
        m.move_group("g1", 3); // "Count is a legal insert-at-end target"
        let order: Vec<(&str, i32)> = m.groups().iter().map(|g| (g.id.as_str(), g.sort_order)).collect();
        assert_eq!(order, [("g2", 0), ("g3", 1), ("g1", 2)]);
        m.move_group("g1", 0);
        assert_eq!(m.groups()[0].id, "g1");
        m.move_group("g1", -5);
        m.move_group("nope", 1);
        m.rename_group("g2", "  Renamed ");
        m.rename_group("g3", "   ");
        assert_eq!((m.groups()[1].name.as_str(), m.groups()[2].name.as_str()), ("Renamed", "three"));

        m.create_session(NewSession { group_id: Some("g2"), ..new_session("s", None) });
        m.remove_group("g2");
        assert_eq!(m.session("s").map(|s| s.group_id.as_str()), Some(""));
        assert_eq!(m.groups().len(), 2);
    }
}
