//! Multi-select in the sidebar (C# `MainViewModel.SelectedSessionIds`, `ToggleSelection`,
//! `SetRangeSelection`, `ResolveActionTargets` and `MainWindow`'s `_selectionAnchorId`).
//!
//! Ctrl-click toggles a row and makes it the anchor; Shift-click selects every visible row
//! between the anchor and the clicked one (the anchor stays, so Shift-clicks can grow or
//! shrink the range); a plain click clears the selection and moves the anchor. An action from a
//! row's menu applies to the whole selection when that row is part of it, and to that row alone
//! otherwise.

/// The selected session ids and the range anchor.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    ids: Vec<String>,
    anchor: Option<String>,
}

impl Selection {
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.ids.iter().any(|x| x == id)
    }

    /// The selected ids, in the order they were selected.
    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    pub fn anchor(&self) -> Option<&str> {
        self.anchor.as_deref()
    }

    /// A plain click: nothing selected, the clicked row is the new anchor.
    pub fn click(&mut self, id: &str) {
        self.ids.clear();
        self.anchor = Some(id.to_owned());
    }

    /// Esc: nothing selected (the anchor stays).
    pub fn clear(&mut self) {
        self.ids.clear();
    }

    /// Ctrl-click: add or remove the row; it becomes the anchor either way.
    pub fn toggle(&mut self, id: &str) {
        if let Some(i) = self.ids.iter().position(|x| x == id) {
            self.ids.remove(i);
        } else {
            self.ids.push(id.to_owned());
        }
        self.anchor = Some(id.to_owned());
    }

    /// Shift-click: exactly the rows of `visible` (top to bottom) between the anchor and
    /// `target`, inclusive. Without an anchor (or one no longer visible), `fallback` (the active
    /// session) is tried, and then the target alone. A target that isn't visible selects
    /// nothing.
    pub fn range(&mut self, visible: &[impl AsRef<str>], target: &str, fallback: Option<&str>) {
        self.ids.clear();
        let pos = |id: &str| visible.iter().position(|v| v.as_ref() == id);
        let Some(t) = pos(target) else { return };
        let a = self.anchor.as_deref().and_then(pos).or_else(|| fallback.and_then(pos)).unwrap_or(t);
        let (lo, hi) = if a <= t { (a, t) } else { (t, a) };
        self.ids = visible.iter().skip(lo).take(hi - lo + 1).map(|v| v.as_ref().to_owned()).collect();
        if self.anchor.as_deref().and_then(pos).is_none() {
            self.anchor = visible.get(a).map(|v| v.as_ref().to_owned());
        }
    }

    /// Forgets ids that are gone (closed sessions).
    pub fn retain(&mut self, exists: impl Fn(&str) -> bool) {
        self.ids.retain(|id| exists(id));
        if self.anchor.as_deref().is_some_and(|a| !exists(a)) {
            self.anchor = None;
        }
    }

    /// The sessions an action on `target` applies to, in `order` (the sidebar order): the whole
    /// selection when `target` is part of it, otherwise `target` alone.
    pub fn targets(&self, target: &str, order: &[impl AsRef<str>]) -> Vec<String> {
        if !self.contains(target) {
            return vec![target.to_owned()];
        }
        let mut v: Vec<String> = order.iter().map(AsRef::as_ref).filter(|id| self.contains(id)).map(str::to_owned).collect();
        // Selected ids the order doesn't know keep their selection order, after the rest.
        for id in &self.ids {
            if !v.contains(id) {
                v.push(id.clone());
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROWS: [&str; 5] = ["a", "b", "c", "d", "e"];

    #[test]
    fn ctrl_click_toggles_and_moves_the_anchor() {
        let mut s = Selection::default();
        s.toggle("b");
        s.toggle("d");
        assert_eq!(s.ids(), ["b", "d"]);
        assert_eq!(s.anchor(), Some("d"));
        s.toggle("b");
        assert_eq!(s.ids(), ["d"]);
        assert_eq!(s.anchor(), Some("b"));
    }

    #[test]
    fn shift_click_selects_the_visible_range_either_direction() {
        let mut s = Selection::default();
        s.click("b");
        s.range(&ROWS, "d", None);
        assert_eq!(s.ids(), ["b", "c", "d"]);
        // The anchor stays: a second shift-click above it re-ranges from b.
        s.range(&ROWS, "a", None);
        assert_eq!(s.ids(), ["a", "b"]);
        assert_eq!(s.anchor(), Some("b"));
    }

    #[test]
    fn shift_click_without_an_anchor_uses_the_active_session_or_the_target() {
        let mut s = Selection::default();
        s.range(&ROWS, "c", Some("e"));
        assert_eq!(s.ids(), ["c", "d", "e"]);
        assert_eq!(s.anchor(), Some("e"));
        let mut s = Selection::default();
        s.range(&ROWS, "c", None);
        assert_eq!(s.ids(), ["c"]);
        // An anchor that is no longer visible (another rail tab) falls back too.
        let mut s = Selection::default();
        s.click("zz");
        s.range(&ROWS, "b", Some("a"));
        assert_eq!(s.ids(), ["a", "b"]);
    }

    #[test]
    fn shift_click_on_a_row_that_is_not_visible_selects_nothing() {
        let mut s = Selection::default();
        s.toggle("a");
        s.range(&ROWS, "nope", None);
        assert!(s.is_empty());
    }

    #[test]
    fn plain_click_and_clear_empty_the_selection() {
        let mut s = Selection::default();
        s.toggle("a");
        s.toggle("b");
        s.click("c");
        assert!(s.is_empty());
        assert_eq!(s.anchor(), Some("c"));
        s.toggle("a");
        s.clear();
        assert!(s.is_empty());
    }

    #[test]
    fn targets_are_the_selection_in_sidebar_order_only_when_the_row_is_selected() {
        let mut s = Selection::default();
        s.toggle("d");
        s.toggle("b");
        assert_eq!(s.targets("d", &ROWS), ["b", "d"]);
        assert_eq!(s.targets("c", &ROWS), ["c"], "a row outside the selection acts alone");
        assert_eq!(Selection::default().targets("a", &ROWS), ["a"]);
    }

    #[test]
    fn retain_drops_closed_sessions_and_a_gone_anchor() {
        let mut s = Selection::default();
        s.toggle("a");
        s.toggle("b");
        s.retain(|id| id != "b");
        assert_eq!(s.ids(), ["a"]);
        assert_eq!(s.anchor(), None);
    }
}
