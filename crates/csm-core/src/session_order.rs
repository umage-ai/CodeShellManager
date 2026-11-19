//! Order bookkeeping shared by the sidebar (which renders the session manager's list) and the
//! pane grid (which renders only live sessions) (C# `SessionOrder`).
//!
//! The two lists hold different things (the manager's includes dormant sessions, the grid's
//! only live ones), so a position in one is not a position in the other. The session manager's
//! order is the single source of truth (it is what `state.json` persists and what the sidebar
//! shows); the grid is re-aligned to it by [`align_to`] rather than moved by the same index.
//! Applying one list's index to the other is exactly what used to make a drag land somewhere
//! else.

use std::collections::HashMap;

/// Moves `id` so it sits immediately before (or after) `anchor_id`. The anchor's index is found
/// AFTER removal, so a downward move doesn't overshoot by one. Returns true when the order
/// changed. Unknown ids, and a move relative to itself, are no-ops.
pub fn move_relative<T>(
    list: &mut Vec<T>,
    key: impl Fn(&T) -> &str,
    id: &str,
    anchor_id: &str,
    after: bool,
) -> bool {
    if id == anchor_id {
        return false;
    }
    let Some(from) = index_of(list, &key, id) else {
        return false;
    };
    if index_of(list, &key, anchor_id).is_none() {
        return false;
    }
    let item = list.remove(from);
    let Some(anchor) = index_of(list, &key, anchor_id) else {
        // Unreachable (the anchor was found above and is a different element); restore.
        list.insert(from.min(list.len()), item);
        return false;
    };
    let to = anchor + usize::from(after);
    list.insert(to.min(list.len()), item);
    to != from
}

/// Reorders `list` in place until its items follow the relative order of `reference`. Items
/// missing from the reference keep their relative order after the known ones. Returns true
/// when anything moved. (The C# version moves items one at a time so a bound
/// `ObservableCollection` sees `Move` events rather than a reset; a plain `Vec` has no
/// observers, so a single stable sort gives the same final order.)
pub fn align_to<T>(
    list: &mut [T],
    reference: &[impl AsRef<str>],
    key: impl Fn(&T) -> &str,
) -> bool {
    let rank = rank_map(reference);
    let rank_of = |item: &T| rank.get(key(item)).copied().unwrap_or(usize::MAX);
    let already = list.windows(2).all(|w| match w {
        [a, b] => rank_of(a) <= rank_of(b),
        _ => true,
    });
    if already {
        return false;
    }
    // Stable, so equal ranks (all the unknown ones) keep their original order.
    list.sort_by_key(|item| rank_of(item));
    true
}

/// Index at which to insert `id` into `list` so it lands in the same relative position it has
/// in `reference`: before the first item that comes after it there. Used when a live session
/// (re)joins the grid. Appends when `id` is not in the reference.
pub fn insertion_index<T>(
    list: &[T],
    reference: &[impl AsRef<str>],
    key: impl Fn(&T) -> &str,
    id: &str,
) -> usize {
    let Some(my_rank) = reference.iter().position(|r| r.as_ref() == id) else {
        return list.len();
    };
    let rank = rank_map(reference);
    list.iter()
        .position(|item| rank.get(key(item)).is_some_and(|r| *r > my_rank))
        .unwrap_or(list.len())
}

/// What [`cluster_anchor_for`] needs to know about a session. `group_id` is empty for
/// ungrouped; `repo_root` is known only for live sessions (`None` until git resolves it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionPlacement {
    pub id: String,
    pub group_id: String,
    pub is_live: bool,
    pub repo_root: Option<String>,
}

impl SessionPlacement {
    pub fn new(id: &str, group_id: &str, is_live: bool, repo_root: Option<&str>) -> Self {
        Self {
            id: id.to_owned(),
            group_id: group_id.to_owned(),
            is_live,
            repo_root: repo_root.map(str::to_owned),
        }
    }
}

/// For a NEWLY created session that has just learned its repo root: the session to place it
/// after, so it joins its worktree siblings; `None` when there is nothing to do (no live
/// sibling in its group, or it already sits next to one).
///
/// Only ever called for sessions created in this run. It never moves anything else, and is
/// never consulted for restored sessions: the saved order is the user's, and re-sorting it on
/// every restore is what used to undo a drag. `ordered` is every session in sidebar order.
pub fn cluster_anchor_for(ordered: &[SessionPlacement], id: &str) -> Option<String> {
    let me = ordered.iter().find(|s| s.id == id)?;
    let my_root = me.repo_root.as_deref().filter(|r| !r.is_empty())?;
    if !me.is_live {
        return None;
    }

    // Only the live rows of its own group are ever displayed next to it.
    let peers: Vec<&SessionPlacement> = ordered
        .iter()
        .filter(|s| s.is_live && s.group_id == me.group_id)
        .collect();
    let is_sibling = |s: &SessionPlacement| {
        s.id != id
            && s.repo_root
                .as_deref()
                .is_some_and(|r| equals_ignore_case(r, my_root))
    };

    let at = peers.iter().position(|s| s.id == id)?;
    let prev_is_sibling = at
        .checked_sub(1)
        .and_then(|p| peers.get(p))
        .is_some_and(|s| is_sibling(s));
    let next_is_sibling = peers.get(at + 1).is_some_and(|s| is_sibling(s));
    if prev_is_sibling || next_is_sibling {
        return None;
    }
    peers
        .iter()
        .rev()
        .find(|s| is_sibling(s))
        .map(|s| s.id.clone())
}

/// `StringComparison.OrdinalIgnoreCase`, approximated with per-scalar lowercasing.
fn equals_ignore_case(a: &str, b: &str) -> bool {
    a.chars()
        .flat_map(char::to_lowercase)
        .eq(b.chars().flat_map(char::to_lowercase))
}

fn index_of<T>(list: &[T], key: &impl Fn(&T) -> &str, id: &str) -> Option<usize> {
    list.iter().position(|item| key(item) == id)
}

/// First occurrence wins (C# `TryAdd`).
fn rank_map(reference: &[impl AsRef<str>]) -> HashMap<&str, usize> {
    let mut rank = HashMap::with_capacity(reference.len());
    for (i, r) in reference.iter().enumerate() {
        rank.entry(r.as_ref()).or_insert(i);
    }
    rank
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(ids: &[&str]) -> Vec<String> {
        ids.iter().map(|s| (*s).to_owned()).collect()
    }
    #[allow(clippy::ptr_arg)] // must be Fn(&String) -> &str for Vec<String>
    fn id(s: &String) -> &str {
        s.as_str()
    }

    // ---- MoveRelative ----

    #[test]
    fn moving_down_lands_exactly_before_the_anchor() {
        // The old remove-then-insert-at-old-index put 'a' AFTER 'c'.
        let mut list = l(&["a", "b", "c", "d"]);
        assert!(move_relative(&mut list, id, "a", "c", false));
        assert_eq!(list, l(&["b", "a", "c", "d"]));
    }

    #[test]
    fn moving_up_lands_exactly_before_the_anchor() {
        let mut list = l(&["a", "b", "c", "d"]);
        assert!(move_relative(&mut list, id, "d", "b", false));
        assert_eq!(list, l(&["a", "d", "b", "c"]));
    }

    #[test]
    fn after_places_directly_behind_the_anchor() {
        let mut list = l(&["a", "b", "c", "d"]);
        assert!(move_relative(&mut list, id, "a", "c", true));
        assert_eq!(list, l(&["b", "c", "a", "d"]));
    }

    #[test]
    fn no_op_moves_report_false_and_change_nothing() {
        for (moved, anchor, after) in [
            ("b", "b", false),  // relative to itself
            ("b", "c", false),  // already directly before c
            ("b", "a", true),   // already directly after a
            ("zz", "a", false), // unknown session
            ("a", "zz", false), // unknown anchor
        ] {
            let mut list = l(&["a", "b", "c"]);
            assert!(
                !move_relative(&mut list, id, moved, anchor, after),
                "{moved} {anchor} {after}"
            );
            assert_eq!(list, l(&["a", "b", "c"]));
        }
    }

    // ---- AlignTo ----

    #[test]
    fn align_to_follows_the_reference_ignoring_ids_the_list_does_not_hold() {
        // Reference = SessionManager (with dormant 'zz'); list = the grid (live only).
        let mut grid = l(&["c1", "p1", "c2"]);
        assert!(align_to(&mut grid, &l(&["zz", "c1", "c2", "p1"]), id));
        assert_eq!(grid, l(&["c1", "c2", "p1"]));
    }

    // (C# `AlignTo_raises_only_moves_never_a_reset` asserts ObservableCollection
    // NotifyCollectionChanged actions; there is no observable collection here. Not portable.)

    #[test]
    fn align_to_keeps_unknown_items_after_known_ones_in_their_own_order() {
        let mut grid = l(&["x", "b", "y", "a"]);
        align_to(&mut grid, &l(&["a", "b"]), id);
        assert_eq!(grid, l(&["a", "b", "x", "y"]));
    }

    #[test]
    fn align_to_on_an_aligned_list_reports_false() {
        let mut grid = l(&["a", "b"]);
        assert!(!align_to(&mut grid, &l(&["zz", "a", "b"]), id));
    }

    // ---- InsertionIndex ----

    #[test]
    fn a_woken_session_rejoins_the_grid_at_its_relative_position_not_its_manager_index() {
        // Manager: [zz1, zz2, a, w, b] -> 'w' is at manager index 3, but in the grid [a, b] it
        // belongs at 1. The old code inserted at 3, i.e. appended.
        let grid = l(&["a", "b"]);
        assert_eq!(
            insertion_index(&grid, &l(&["zz1", "zz2", "a", "w", "b"]), id, "w"),
            1
        );
    }

    #[test]
    fn insertion_index_appends_when_nothing_follows_or_the_id_is_unknown() {
        let grid = l(&["a", "b"]);
        assert_eq!(insertion_index(&grid, &l(&["a", "b", "w"]), id, "w"), 2);
        assert_eq!(insertion_index(&grid, &l(&["a", "b"]), id, "nope"), 2);
    }

    // Restart / wake re-insert a VM whose sidebar row never moved. Cases carried over from
    // PR #159 (fix/restart-pane-order). Arguments: (live grid, manager order incl. dormant or
    // not-yet-launched, id, expected).
    #[test]
    fn restart_or_wake_lands_at_its_sidebar_position() {
        let cases: [(&[&str], &[&str], &str, usize); 6] = [
            // Sidebar A, D(dormant), B, C: restarting B while the live list is [A, C].
            (&["A", "C"], &["A", "D", "B", "C"], "B", 1),
            // First session goes first, with a dormant one above it.
            (&["B"], &["D", "A", "B"], "A", 0),
            // Last session goes last, with dormant ones interleaved.
            (&["A", "B"], &["A", "D1", "B", "D2", "C"], "C", 2),
            // Several dormant sessions above do not accumulate an offset.
            (&["A", "C"], &["D1", "D2", "D3", "A", "B", "C"], "B", 1),
            // Restore loop: A and B are in the model but not yet live when C registers.
            (&["E"], &["A", "B", "C", "E"], "C", 0),
            // Nothing dormant: matches the model index.
            (&["A", "C"], &["A", "B", "C"], "B", 1),
        ];
        for (live, model, who, expected) in cases {
            assert_eq!(
                insertion_index(&l(live), &l(model), id, who),
                expected,
                "{who} in {model:?}"
            );
        }
    }

    // ---- ClusterAnchorFor ----

    fn p(sid: &str, repo: &str) -> SessionPlacement {
        SessionPlacement::new(sid, "", true, (!repo.is_empty()).then_some(repo))
    }

    #[test]
    fn a_new_ps_session_is_placed_after_its_claude_sibling() {
        let order = [
            p("api", r"C:\src\api"),
            p("web", r"C:\src\web"),
            p("api (PS)", r"C:\src\api"),
        ];
        assert_eq!(
            cluster_anchor_for(&order, "api (PS)").as_deref(),
            Some("api")
        );
    }

    #[test]
    fn repo_roots_compare_case_insensitively() {
        let order = [
            p("api", r"C:\src\API"),
            p("web", ""),
            p("api (PS)", r"c:\src\api"),
        ];
        assert_eq!(
            cluster_anchor_for(&order, "api (PS)").as_deref(),
            Some("api")
        );
    }

    #[test]
    fn already_next_to_a_sibling_needs_no_move() {
        let order = [p("api", "R"), p("api (PS)", "R"), p("web", "W")];
        assert_eq!(cluster_anchor_for(&order, "api (PS)"), None);
    }

    #[test]
    fn a_dormant_session_between_siblings_does_not_count_as_separating_them() {
        // Dormant rows render after the live ones, so this is visually adjacent already.
        let order = [
            p("api", "R"),
            SessionPlacement::new("zz", "", false, None),
            p("api (PS)", "R"),
        ];
        assert_eq!(cluster_anchor_for(&order, "api (PS)"), None);
    }

    #[test]
    fn a_sibling_in_another_group_is_not_an_anchor() {
        let order = [
            SessionPlacement::new("api", "claude", true, Some("R")),
            SessionPlacement::new("web", "ps", true, Some("W")),
            SessionPlacement::new("api (PS)", "ps", true, Some("R")),
        ];
        assert_eq!(cluster_anchor_for(&order, "api (PS)"), None);
    }

    #[test]
    fn no_repo_root_yet_or_no_siblings_means_no_anchor() {
        assert_eq!(
            cluster_anchor_for(&[p("api", "R"), p("new", "")], "new"),
            None
        );
        assert_eq!(
            cluster_anchor_for(&[p("api", "R"), p("solo", "S")], "solo"),
            None
        );
        assert_eq!(cluster_anchor_for(&[], "ghost"), None);
    }

    #[test]
    fn with_several_siblings_it_goes_after_the_last_one() {
        let order = [p("a1", "R"), p("a2", "R"), p("web", "W"), p("a3", "R")];
        assert_eq!(cluster_anchor_for(&order, "a3").as_deref(), Some("a2"));
    }
}
