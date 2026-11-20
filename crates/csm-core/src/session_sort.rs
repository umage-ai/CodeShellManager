//! The sidebar's one-shot sort (C# `MainWindow.ApplySort` + `SessionManager.SortSessions`).
//!
//! Sorting rewrites the session order once; the person keeps refining it by dragging. It never
//! re-sorts by itself (the saved order is the person's, see `session_order`).
//!
//! Carried over from the WPF app:
//! - Fields: name, folder (case-insensitive), last active, and the git fields branch, dirty and
//!   repo (the leaf of the repo root). Same branch ties on folder (worktrees stay together);
//!   same dirtiness ties on name; same repo ties on folder.
//! - A field's natural direction: text ascends, last active and dirty descend (newest / dirty
//!   first).
//!
//! Deliberate differences:
//! - The sort is stable (C# `List.Sort` is not), so equal keys keep their current order.
//! - Sessions without a branch or repo (asleep, or not in git) stay at the end in *both*
//!   directions. The C# comment says they "sink to the bottom", but its descending sort
//!   reversed the whole comparison and floated them to the top.
//! - A scope: one group's sessions can be sorted among their own slots, leaving every other
//!   session where it is.

use std::cmp::Ordering;

/// What to sort by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortField {
    Name,
    Folder,
    LastActive,
    Branch,
    Dirty,
    Repo,
}

impl SortField {
    pub const ALL: [SortField; 6] = [SortField::Name, SortField::Folder, SortField::LastActive, SortField::Branch, SortField::Dirty, SortField::Repo];

    pub fn id(self) -> &'static str {
        match self {
            SortField::Name => "name",
            SortField::Folder => "folder",
            SortField::LastActive => "last_active",
            SortField::Branch => "branch",
            SortField::Dirty => "dirty",
            SortField::Repo => "repo",
        }
    }

    pub fn from_id(s: &str) -> Option<SortField> {
        SortField::ALL.into_iter().find(|f| f.id() == s)
    }

    pub fn label(self) -> &'static str {
        match self {
            SortField::Name => "Name",
            SortField::Folder => "Folder",
            SortField::LastActive => "Last active",
            SortField::Branch => "Branch",
            SortField::Dirty => "Dirty",
            SortField::Repo => "Repo",
        }
    }

    /// The direction a field starts in: text ascends; timestamps and dirty descend.
    pub fn default_descending(self) -> bool {
        matches!(self, SortField::LastActive | SortField::Dirty)
    }

    /// A git field (the WPF menu's "Git ▸" submenu).
    pub fn is_git(self) -> bool {
        matches!(self, SortField::Branch | SortField::Dirty | SortField::Repo)
    }
}

/// What the sort needs to know about one session. Git keys are `None` when unknown (asleep, or
/// not a git folder).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SortKey {
    pub id: String,
    pub name: String,
    pub folder: String,
    /// Epoch ms of the last activity; `None` sorts as the oldest.
    pub last_active_ms: Option<i64>,
    pub branch: Option<String>,
    pub dirty: Option<bool>,
    /// The main working tree's root; compared by its leaf name.
    pub repo_root: Option<String>,
}

/// `StringComparison.OrdinalIgnoreCase`, approximated with per-scalar lowercasing.
fn cmp_ignore_case(a: &str, b: &str) -> Ordering {
    a.chars().flat_map(char::to_lowercase).cmp(b.chars().flat_map(char::to_lowercase))
}

/// The repo's display name: the last path segment of its root.
pub fn repo_name(root: &str) -> &str {
    let t = root.trim_end_matches(['/', '\\']);
    t.rsplit(['/', '\\']).next().unwrap_or(t)
}

fn text_key(k: &SortKey, field: SortField) -> Option<&str> {
    match field {
        SortField::Branch => k.branch.as_deref(),
        SortField::Repo => k.repo_root.as_deref().map(repo_name),
        _ => None,
    }
    .filter(|s| !s.is_empty())
}

/// The comparison without direction or the "unknown last" rule.
fn base(a: &SortKey, b: &SortKey, field: SortField) -> Ordering {
    match field {
        SortField::Name => cmp_ignore_case(&a.name, &b.name),
        SortField::Folder => cmp_ignore_case(&a.folder, &b.folder),
        SortField::LastActive => a.last_active_ms.unwrap_or(i64::MIN).cmp(&b.last_active_ms.unwrap_or(i64::MIN)),
        SortField::Branch | SortField::Repo => {
            let (x, y) = (text_key(a, field).unwrap_or(""), text_key(b, field).unwrap_or(""));
            cmp_ignore_case(x, y).then_with(|| cmp_ignore_case(&a.folder, &b.folder))
        }
        // false < true: ascending is clean first.
        SortField::Dirty => a.dirty.unwrap_or(false).cmp(&b.dirty.unwrap_or(false)).then_with(|| cmp_ignore_case(&a.name, &b.name)),
    }
}

/// The full comparison: unknown branch / repo last in either direction, then the field.
pub fn compare(a: &SortKey, b: &SortKey, field: SortField, descending: bool) -> Ordering {
    let unknown = |k: &SortKey| matches!(field, SortField::Branch | SortField::Repo) && text_key(k, field).is_none();
    let first = unknown(a).cmp(&unknown(b));
    if first != Ordering::Equal {
        return first;
    }
    let c = base(a, b, field);
    if descending { c.reverse() } else { c }
}

/// The ids of `keys` in sorted order (stable: equal keys keep their order).
pub fn sorted_ids(keys: &[SortKey], field: SortField, descending: bool) -> Vec<String> {
    let mut v: Vec<&SortKey> = keys.iter().collect();
    v.sort_by(|a, b| compare(a, b, field, descending));
    v.into_iter().map(|k| k.id.clone()).collect()
}

/// Puts the items `in_scope` selects into the order `sorted` gives, using only the slots they
/// already occupy: every other item stays exactly where it is. Items in scope but missing from
/// `sorted` keep their relative order after the ones it names. Returns true when anything moved.
pub fn reorder_in_place<T>(list: &mut Vec<T>, key: impl Fn(&T) -> &str, in_scope: impl Fn(&T) -> bool, sorted: &[String]) -> bool {
    let slots: Vec<usize> = list.iter().enumerate().filter(|(_, t)| in_scope(t)).map(|(i, _)| i).collect();
    if slots.len() < 2 {
        return false;
    }
    let rank = |t: &T| sorted.iter().position(|s| s == key(t)).unwrap_or(usize::MAX);
    // Take the scoped items out (back to front, so earlier slots stay valid), then sort them.
    let mut taken: Vec<(usize, T)> = Vec::with_capacity(slots.len());
    for &i in slots.iter().rev() {
        let item = list.remove(i);
        taken.push((i, item));
    }
    taken.reverse();
    let before: Vec<usize> = taken.iter().map(|(i, _)| *i).collect();
    let mut items: Vec<(usize, T)> = taken;
    items.sort_by_key(|(_, t)| rank(t));
    let moved = items.iter().map(|(i, _)| *i).ne(before.iter().copied());
    for (&slot, (_, item)) in slots.iter().zip(items) {
        list.insert(slot.min(list.len()), item);
    }
    moved
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(id: &str, name: &str, folder: &str) -> SortKey {
        SortKey { id: id.into(), name: name.into(), folder: folder.into(), ..SortKey::default() }
    }

    fn ids(v: &[String]) -> Vec<&str> {
        v.iter().map(String::as_str).collect()
    }

    #[test]
    fn name_sorts_case_insensitively_and_flips() {
        let keys = [k("1", "beta", ""), k("2", "Alpha", ""), k("3", "gamma", "")];
        assert_eq!(ids(&sorted_ids(&keys, SortField::Name, false)), ["2", "1", "3"]);
        assert_eq!(ids(&sorted_ids(&keys, SortField::Name, true)), ["3", "1", "2"]);
    }

    #[test]
    fn equal_keys_keep_their_order() {
        let keys = [k("a", "same", ""), k("b", "Same", ""), k("c", "SAME", "")];
        assert_eq!(ids(&sorted_ids(&keys, SortField::Name, false)), ["a", "b", "c"]);
        assert_eq!(ids(&sorted_ids(&keys, SortField::Name, true)), ["a", "b", "c"]);
    }

    #[test]
    fn folder_sorts_by_path() {
        let keys = [k("1", "", r"C:\src\web"), k("2", "", r"c:\SRC\api"), k("3", "", r"C:\a")];
        assert_eq!(ids(&sorted_ids(&keys, SortField::Folder, false)), ["3", "2", "1"]);
    }

    #[test]
    fn last_active_defaults_to_newest_first_and_unknown_is_oldest() {
        let mut a = k("old", "", "");
        a.last_active_ms = Some(10);
        let mut b = k("new", "", "");
        b.last_active_ms = Some(30);
        let c = k("never", "", "");
        let keys = [a, c, b];
        let desc = SortField::LastActive.default_descending();
        assert!(desc);
        assert_eq!(ids(&sorted_ids(&keys, SortField::LastActive, desc)), ["new", "old", "never"]);
        assert_eq!(ids(&sorted_ids(&keys, SortField::LastActive, false)), ["never", "old", "new"]);
    }

    fn g(id: &str, folder: &str, branch: Option<&str>, dirty: Option<bool>, repo: Option<&str>) -> SortKey {
        SortKey {
            id: id.into(),
            name: id.into(),
            folder: folder.into(),
            branch: branch.map(Into::into),
            dirty,
            repo_root: repo.map(Into::into),
            ..SortKey::default()
        }
    }

    #[test]
    fn branch_ties_on_folder_and_no_branch_stays_last_both_ways() {
        // The unknown ones still order among themselves by folder (reversed when descending).
        let keys = [
            g("none", "a", None, None, None),
            g("main-b", r"C:\b", Some("main"), None, None),
            g("feat", r"C:\z", Some("feat/x"), None, None),
            g("main-a", r"C:\a", Some("Main"), None, None),
            g("empty", "b", Some(""), None, None),
        ];
        assert_eq!(ids(&sorted_ids(&keys, SortField::Branch, false)), ["feat", "main-a", "main-b", "none", "empty"]);
        assert_eq!(ids(&sorted_ids(&keys, SortField::Branch, true)), ["main-b", "main-a", "feat", "empty", "none"]);
    }

    #[test]
    fn dirty_defaults_to_dirty_first_tied_on_name() {
        let keys = [
            g("b-clean", "", None, Some(false), None),
            g("z-dirty", "", None, Some(true), None),
            g("a-dirty", "", None, Some(true), None),
            g("asleep", "", None, None, None),
        ];
        let desc = SortField::Dirty.default_descending();
        assert_eq!(ids(&sorted_ids(&keys, SortField::Dirty, desc)), ["z-dirty", "a-dirty", "b-clean", "asleep"]);
        assert_eq!(ids(&sorted_ids(&keys, SortField::Dirty, false)), ["asleep", "b-clean", "a-dirty", "z-dirty"]);
    }

    #[test]
    fn repo_compares_the_leaf_and_keeps_worktrees_together() {
        let keys = [
            g("web-wt", r"C:\src\web\.worktrees\x", None, None, Some(r"C:\src\web\")),
            g("api", r"C:\src\api", None, None, Some(r"C:\src\api")),
            g("plain", r"C:\tmp", None, None, None),
            g("web", r"C:\src\web", None, None, Some("/home/me/WEB")),
        ];
        assert_eq!(ids(&sorted_ids(&keys, SortField::Repo, false)), ["api", "web", "web-wt", "plain"]);
        assert_eq!(repo_name(r"C:\src\web\"), "web");
        assert_eq!(repo_name("/"), "");
        assert_eq!(repo_name(""), "");
    }

    #[test]
    fn reorder_in_place_sorts_only_the_scope_in_its_own_slots() {
        // Group G = {g1, g2, g3} interleaved with others.
        let mut list = vec!["g3", "x", "g1", "y", "g2"];
        let sorted = vec!["g1".to_string(), "g2".to_string(), "g3".to_string()];
        assert!(reorder_in_place(&mut list, |s| s, |s| s.starts_with('g'), &sorted));
        assert_eq!(list, ["g1", "x", "g2", "y", "g3"]);
        // Already in order: nothing moves.
        assert!(!reorder_in_place(&mut list, |s| s, |s| s.starts_with('g'), &sorted));
    }

    #[test]
    fn reorder_in_place_everything_and_unknown_ids_go_last() {
        let mut list = vec!["c", "a", "b", "q"];
        let sorted = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        assert!(reorder_in_place(&mut list, |s| s, |_| true, &sorted));
        assert_eq!(list, ["a", "b", "c", "q"]);
        let mut empty: Vec<&str> = vec![];
        assert!(!reorder_in_place(&mut empty, |s| s, |_| true, &sorted));
    }

    #[test]
    fn field_ids_round_trip() {
        for f in SortField::ALL {
            assert_eq!(SortField::from_id(f.id()), Some(f));
        }
        assert_eq!(SortField::from_id("size"), None);
    }
}
