//! Keyboard focus: the regions F6 cycles through, and the geometry that moves between panes
//! (Ctrl+Alt+Arrow). Pure: no egui context, so the rules are tested without a window. The app
//! records which regions and panes it drew last frame and feeds them in (`app.rs`).

use std::cmp::Ordering;

use egui::Rect;

/// A part of the window that keyboard focus can be in. The declaration order is the F6 cycle
/// (`docs/design/keyboard.md`, section 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusRegion {
    TitleBar,
    RailTabs,
    SidebarList,
    InboxPanel,
    ScreenStrip,
    Panes,
    Inspector,
}

impl FocusRegion {
    /// Every region, in cycle order.
    pub const ORDER: [FocusRegion; 7] = [
        FocusRegion::TitleBar,
        FocusRegion::RailTabs,
        FocusRegion::SidebarList,
        FocusRegion::InboxPanel,
        FocusRegion::ScreenStrip,
        FocusRegion::Panes,
        FocusRegion::Inspector,
    ];

    /// The place in [`FocusRegion::ORDER`].
    fn rank(self) -> usize {
        Self::ORDER.iter().position(|r| *r == self).unwrap_or(0)
    }
}

/// The region F6 (`step` 1) or Shift+F6 (`step` -1) moves to from `current`, among the
/// `visible` regions. It wraps around. A `current` that is not visible moves to the next
/// visible region in cycle order (or the previous one, going back). `None` when nothing is
/// visible.
pub fn next_region(visible: &[FocusRegion], current: FocusRegion, step: i32) -> Option<FocusRegion> {
    let shown: Vec<FocusRegion> = FocusRegion::ORDER.into_iter().filter(|r| visible.contains(r)).collect();
    let n = i32::try_from(shown.len()).ok().filter(|n| *n > 0)?;
    match shown.iter().position(|r| *r == current) {
        Some(p) => {
            let at = i32::try_from(p).unwrap_or(0);
            shown.get((at + step).rem_euclid(n) as usize).copied()
        }
        None if step > 0 => shown.iter().find(|r| r.rank() > current.rank()).or(shown.first()).copied(),
        None => shown.iter().rev().find(|r| r.rank() < current.rank()).or(shown.last()).copied(),
    }
}

/// A direction on screen (Ctrl+Alt+Arrow).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

/// The pane in `dir` from `from`, among `others` (their rects; `from` is not one of them): the
/// index into `others` of the best one. A pane is a candidate when its centre lies that way.
/// Candidates that overlap `from` across the axis win; then the nearest along the axis; then the
/// nearest across it. `None` when no pane lies that way.
pub fn neighbour(from: Rect, others: &[Rect], dir: Dir) -> Option<usize> {
    let c = from.center();
    let mut best: Option<(usize, Score)> = None;
    for (i, r) in others.iter().enumerate() {
        let rc = r.center();
        let (along, across, overlap) = match dir {
            Dir::Right => (rc.x - c.x, (rc.y - c.y).abs(), spans(from.y_range(), r.y_range())),
            Dir::Left => (c.x - rc.x, (rc.y - c.y).abs(), spans(from.y_range(), r.y_range())),
            Dir::Down => (rc.y - c.y, (rc.x - c.x).abs(), spans(from.x_range(), r.x_range())),
            Dir::Up => (c.y - rc.y, (rc.x - c.x).abs(), spans(from.x_range(), r.x_range())),
        };
        if along <= 0.0 || !along.is_finite() || !across.is_finite() {
            continue;
        }
        let score = Score { apart: !overlap, along, across };
        if best.as_ref().is_none_or(|(_, b)| score.order(b) == Ordering::Less) {
            best = Some((i, score));
        }
    }
    best.map(|(i, _)| i)
}

/// How good a candidate is; the smaller, the better.
struct Score {
    /// False when the candidate overlaps `from` across the axis (false sorts first).
    apart: bool,
    along: f32,
    across: f32,
}

impl Score {
    fn order(&self, other: &Score) -> Ordering {
        self.apart.cmp(&other.apart).then(self.along.total_cmp(&other.along)).then(self.across.total_cmp(&other.across))
    }
}

/// Whether two ranges along an axis share any length (touching counts).
fn spans(a: egui::Rangef, b: egui::Rangef) -> bool {
    a.min <= b.max && b.min <= a.max
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, pos2};

    fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect::from_min_max(pos2(x0, y0), pos2(x1, y1))
    }

    #[test]
    fn f6_cycles_the_visible_regions_and_wraps() {
        let all = FocusRegion::ORDER;
        assert_eq!(next_region(&all, FocusRegion::TitleBar, 1), Some(FocusRegion::RailTabs));
        assert_eq!(next_region(&all, FocusRegion::Inspector, 1), Some(FocusRegion::TitleBar));
        assert_eq!(next_region(&all, FocusRegion::TitleBar, -1), Some(FocusRegion::Inspector));
        assert_eq!(next_region(&all, FocusRegion::Panes, -1), Some(FocusRegion::ScreenStrip));
    }

    #[test]
    fn hidden_regions_are_skipped() {
        // No rail, no inbox, no screen strip, no inspector: title bar, sidebar, panes.
        let visible = [FocusRegion::TitleBar, FocusRegion::SidebarList, FocusRegion::Panes];
        assert_eq!(next_region(&visible, FocusRegion::SidebarList, 1), Some(FocusRegion::Panes));
        assert_eq!(next_region(&visible, FocusRegion::Panes, 1), Some(FocusRegion::TitleBar));
        assert_eq!(next_region(&visible, FocusRegion::TitleBar, -1), Some(FocusRegion::Panes));
    }

    #[test]
    fn a_region_that_is_no_longer_visible_moves_to_its_neighbour_in_order() {
        // The inspector closed while it had focus: forward goes to the next one that is shown,
        // backward to the previous one.
        let visible = [FocusRegion::TitleBar, FocusRegion::SidebarList, FocusRegion::Panes];
        assert_eq!(next_region(&visible, FocusRegion::Inspector, 1), Some(FocusRegion::TitleBar));
        assert_eq!(next_region(&visible, FocusRegion::Inspector, -1), Some(FocusRegion::Panes));
        assert_eq!(next_region(&visible, FocusRegion::RailTabs, 1), Some(FocusRegion::SidebarList));
    }

    #[test]
    fn one_visible_region_stays_put_and_none_gives_none() {
        assert_eq!(next_region(&[FocusRegion::Panes], FocusRegion::Panes, 1), Some(FocusRegion::Panes));
        assert_eq!(next_region(&[], FocusRegion::Panes, 1), None);
    }

    /// Four panes in a 2 x 2 grid with a gap between them.
    fn grid() -> [Rect; 4] {
        [rect(0.0, 0.0, 100.0, 100.0), rect(110.0, 0.0, 210.0, 100.0), rect(0.0, 110.0, 100.0, 210.0), rect(110.0, 110.0, 210.0, 210.0)]
    }

    #[test]
    fn neighbours_in_a_grid() {
        let g = grid();
        // A top-left, B top-right, C bottom-left, D bottom-right.
        let pick = |from: usize, dir: Dir| {
            let others: Vec<Rect> = g.iter().enumerate().filter(|(i, _)| *i != from).map(|(_, r)| *r).collect();
            neighbour(g[from], &others, dir).map(|i| if i < from { i } else { i + 1 })
        };
        assert_eq!(pick(0, Dir::Right), Some(1));
        assert_eq!(pick(0, Dir::Down), Some(2));
        assert_eq!(pick(3, Dir::Left), Some(2));
        assert_eq!(pick(3, Dir::Up), Some(1));
        // Nothing past the edge.
        assert_eq!(pick(0, Dir::Left), None);
        assert_eq!(pick(0, Dir::Up), None);
        assert_eq!(pick(3, Dir::Right), None);
    }

    #[test]
    fn an_overlapping_pane_beats_a_nearer_one_in_another_row() {
        // From a tall pane on the left: a pane far to the right in the same rows, against one
        // just to the right but lower down, out of the rows.
        let from = rect(0.0, 0.0, 100.0, 100.0);
        let far_same_row = rect(600.0, 0.0, 700.0, 100.0);
        let near_lower = rect(110.0, 300.0, 200.0, 400.0);
        assert_eq!(neighbour(from, &[near_lower, far_same_row], Dir::Right), Some(1));
    }

    #[test]
    fn the_nearest_along_the_axis_wins_among_overlapping_panes() {
        let from = rect(0.0, 0.0, 100.0, 100.0);
        let far = rect(500.0, 0.0, 600.0, 100.0);
        let near = rect(120.0, 20.0, 200.0, 80.0);
        assert_eq!(neighbour(from, &[far, near], Dir::Right), Some(1));
    }

    #[test]
    fn a_pane_behind_the_origin_is_not_a_neighbour_that_way() {
        let from = rect(200.0, 0.0, 300.0, 100.0);
        let left = rect(0.0, 0.0, 100.0, 100.0);
        assert_eq!(neighbour(from, &[left], Dir::Right), None);
        assert_eq!(neighbour(from, &[left], Dir::Left), Some(0));
    }

    #[test]
    fn no_panes_no_neighbour() {
        assert_eq!(neighbour(rect(0.0, 0.0, 1.0, 1.0), &[], Dir::Right), None);
    }
}
