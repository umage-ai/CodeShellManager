//! Maps a drop Y on the sidebar to a target expressed as "before / after session X" (C#
//! `SidebarDropResolver`).
//!
//! The previous implementation produced an INDEX, and that was the bug: it was computed in one
//! list (all visible rows: cluster headers, dormant rows and a filtered group included) and
//! applied to two others, with a remove-then-insert that overshot every downward move by one.
//! An anchor id means the same thing in every list, so none of that can recur. The UI only
//! measures the rows.

/// What a rendered sidebar row is, as far as a session drop is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarRowKind {
    /// A live session row, or a launching placeholder. The only valid drop anchor.
    Session,
    /// A sleeping session. Not draggable and not an anchor: dormant rows always trail the live
    /// rows of their section, so a drop among them means "after the live rows".
    Dormant,
    /// An inline group header. Starts a section; the row id is the group id, or `None` for the
    /// Ungrouped section.
    GroupHeader,
    /// A "repo (N)" worktree cluster header. Decoration, not a session.
    ClusterHeader,
}

/// One rendered sidebar row, top to bottom, in the sidebar's own coordinates. `top`/`height`
/// describe the row's layout SLOT (margins included), so adjacent slots touch and the boundary
/// between two rows is a single Y.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarRow {
    pub kind: SidebarRowKind,
    pub id: Option<String>,
    pub top: f64,
    pub height: f64,
}

impl SidebarRow {
    pub fn new(kind: SidebarRowKind, id: Option<&str>, top: f64, height: f64) -> Self {
        Self {
            kind,
            id: id.map(str::to_owned),
            top,
            height,
        }
    }

    pub fn bottom(&self) -> f64 {
        self.top + self.height
    }

    pub fn mid(&self) -> f64 {
        self.top + self.height / 2.0
    }
}

/// Where a dropped session goes. At most one of `before_session_id` / `after_session_id` is
/// set; both `None` means "keep its position" (a drop into a group section that shows no live
/// rows). `group_id` is only meaningful when `assigns_group`, i.e. in inline-headers mode,
/// where the section under the cursor decides the group. `indicator_y` is where to draw the
/// insertion line, computed from the same decision so the line can never disagree with the
/// drop.
#[derive(Debug, Clone, PartialEq)]
pub struct SidebarDropTarget {
    pub assigns_group: bool,
    pub group_id: Option<String>,
    pub before_session_id: Option<String>,
    pub after_session_id: Option<String>,
    pub indicator_y: f64,
}

/// Resolves a drop at `y` over `rows`. `None` for an empty sidebar.
pub fn resolve(rows: &[SidebarRow], y: f64) -> Option<SidebarDropTarget> {
    if rows.is_empty() {
        return None;
    }

    // Inline-headers mode renders group headers; flat modes (None, FilterStrip) don't. In a
    // flat mode the whole list is one section and the group never changes.
    let mut start = 0usize;
    let mut end = rows.len();
    let mut header_index: Option<usize> = None;
    for (i, row) in rows.iter().enumerate() {
        if row.kind != SidebarRowKind::GroupHeader {
            continue;
        }
        if header_index.is_none() || row.top <= y {
            header_index = Some(i);
        }
        if row.top > y {
            break;
        }
    }
    let sectioned = header_index.is_some();
    if let Some(h) = header_index {
        start = h + 1;
        end = rows
            .iter()
            .enumerate()
            .skip(start)
            .find(|(_, r)| r.kind == SidebarRowKind::GroupHeader)
            .map_or(rows.len(), |(i, _)| i);
    }
    let group_id = header_index
        .and_then(|h| rows.get(h))
        .and_then(|r| r.id.clone());

    // First live row whose midline is below the cursor: insert before it.
    let mut last_session: Option<usize> = None;
    for i in start..end {
        let Some(row) = rows.get(i) else { break };
        if row.kind != SidebarRowKind::Session {
            continue;
        }
        if y < row.mid() {
            // Draw the line above any cluster header that introduces this row: once the
            // dropped session sits before the run, it renders above that header too.
            let mut top = i;
            while top > start
                && rows
                    .get(top - 1)
                    .is_some_and(|r| r.kind == SidebarRowKind::ClusterHeader)
            {
                top -= 1;
            }
            let indicator_y = rows.get(top).map_or(row.top, |r| r.top);
            return Some(SidebarDropTarget {
                assigns_group: sectioned,
                group_id,
                before_session_id: row.id.clone(),
                after_session_id: None,
                indicator_y,
            });
        }
        last_session = Some(i);
    }

    if let Some(last) = last_session.and_then(|i| rows.get(i)) {
        return Some(SidebarDropTarget {
            assigns_group: sectioned,
            group_id,
            before_session_id: None,
            after_session_id: last.id.clone(),
            indicator_y: last.bottom(),
        });
    }

    // A section with no live rows (empty, collapsed, or only sleeping members): the drop
    // assigns the group and leaves the position alone; with no live neighbours there is
    // nothing to be positioned relative to.
    let indicator_y = match header_index.and_then(|h| rows.get(h)) {
        Some(header) => header.bottom(),
        None => rows.first().map_or(0.0, |r| r.top),
    };
    Some(SidebarDropTarget {
        assigns_group: sectioned,
        group_id,
        before_session_id: None,
        after_session_id: None,
        indicator_y,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: f64 = 40.0;

    type Spec = (SidebarRowKind, Option<&'static str>, f64);

    // Rows stacked top to bottom with no gaps, as layout slots.
    fn stack(rows: &[Spec]) -> Vec<SidebarRow> {
        let mut y = 0.0;
        rows.iter()
            .map(|(kind, id, height)| {
                let row = SidebarRow::new(*kind, *id, y, *height);
                y += height;
                row
            })
            .collect()
    }

    fn live(id: &'static str) -> Spec {
        (SidebarRowKind::Session, Some(id), H)
    }
    fn asleep(id: &'static str) -> Spec {
        (SidebarRowKind::Dormant, Some(id), H)
    }
    fn header(group: Option<&'static str>) -> Spec {
        (SidebarRowKind::GroupHeader, group, 26.0)
    }
    fn cluster() -> Spec {
        (SidebarRowKind::ClusterHeader, None, 22.0)
    }

    #[test]
    fn empty_sidebar_has_no_target() {
        assert_eq!(resolve(&[], 10.0), None);
    }

    #[test]
    fn top_half_of_a_row_means_before_it() {
        let rows = stack(&[live("a"), live("b"), live("c")]);
        let t = resolve(&rows, rows[1].top + 5.0).unwrap();
        assert_eq!(t.before_session_id.as_deref(), Some("b"));
        assert_eq!(t.after_session_id, None);
        assert_eq!(t.indicator_y, rows[1].top);
    }

    #[test]
    fn bottom_half_of_a_row_means_before_the_next_one() {
        let rows = stack(&[live("a"), live("b"), live("c")]);
        let t = resolve(&rows, rows[1].bottom() - 5.0).unwrap();
        assert_eq!(t.before_session_id.as_deref(), Some("c"));
    }

    #[test]
    fn bottom_half_of_the_last_row_means_after_it() {
        let rows = stack(&[live("a"), live("b")]);
        let t = resolve(&rows, rows[1].bottom() - 5.0).unwrap();
        assert_eq!(t.before_session_id, None);
        assert_eq!(t.after_session_id.as_deref(), Some("b"));
        assert_eq!(t.indicator_y, rows[1].bottom());
    }

    #[test]
    fn flat_mode_never_assigns_a_group() {
        let rows = stack(&[live("a"), live("b")]);
        assert!(!resolve(&rows, 5.0).unwrap().assigns_group);
    }

    #[test]
    fn a_cluster_header_is_not_counted_as_a_row() {
        // Cluster header above api: aiming at api's top half means "before api", and the line
        // is drawn above the header, because that is where the dropped row will render.
        let rows = stack(&[
            live("web"),
            cluster(),
            live("api"),
            live("api (PS)"),
            live("docs"),
        ]);
        let t = resolve(&rows, rows[2].top + 5.0).unwrap();
        assert_eq!(t.before_session_id.as_deref(), Some("api"));
        assert_eq!(t.indicator_y, rows[1].top);
    }

    #[test]
    fn dormant_rows_are_not_anchors_a_drop_among_them_lands_after_the_live_rows() {
        let rows = stack(&[live("a"), live("b"), asleep("zz1"), asleep("zz2")]);
        let t = resolve(&rows, rows[2].top + 5.0).unwrap();
        assert_eq!(t.before_session_id, None);
        assert_eq!(t.after_session_id.as_deref(), Some("b"));
    }

    #[test]
    fn inline_the_section_under_the_cursor_decides_the_group() {
        let rows = stack(&[
            header(None),
            live("u1"),
            header(Some("claude")),
            live("c1"),
            live("c2"),
            header(Some("ps")),
            live("p1"),
        ]);
        let t = resolve(&rows, rows[4].top + 5.0).unwrap();
        assert!(t.assigns_group);
        assert_eq!(t.group_id.as_deref(), Some("claude"));
        assert_eq!(t.before_session_id.as_deref(), Some("c2"));
    }

    #[test]
    fn inline_the_ungrouped_section_is_group_none() {
        let rows = stack(&[header(None), live("u1"), header(Some("claude")), live("c1")]);
        let t = resolve(&rows, rows[1].bottom() - 5.0).unwrap();
        assert!(t.assigns_group);
        assert_eq!(t.group_id, None);
        assert_eq!(t.after_session_id.as_deref(), Some("u1"));
    }

    #[test]
    fn inline_below_the_last_live_row_of_a_section_stays_in_that_section() {
        // Must not slide into the next group's first slot.
        let rows = stack(&[
            header(Some("claude")),
            live("c1"),
            live("c2"),
            header(Some("ps")),
            live("p1"),
        ]);
        let t = resolve(&rows, rows[2].bottom() - 5.0).unwrap();
        assert_eq!(t.group_id.as_deref(), Some("claude"));
        assert_eq!(t.after_session_id.as_deref(), Some("c2"));
    }

    #[test]
    fn inline_a_section_with_no_live_rows_assigns_the_group_and_keeps_the_position() {
        let rows = stack(&[
            header(Some("claude")),
            live("c1"),
            header(Some("ps")),
            asleep("zz"),
            header(Some("misc")),
        ]);
        let t = resolve(&rows, rows[3].top + 5.0).unwrap();
        assert_eq!(t.group_id.as_deref(), Some("ps"));
        assert_eq!(t.before_session_id, None);
        assert_eq!(t.after_session_id, None);
        assert_eq!(t.indicator_y, rows[2].bottom());
    }

    #[test]
    fn inline_above_the_first_header_belongs_to_the_first_section() {
        let rows = stack(&[header(Some("claude")), live("c1")]);
        let t = resolve(&rows, -3.0).unwrap();
        assert_eq!(t.group_id.as_deref(), Some("claude"));
        assert_eq!(t.before_session_id.as_deref(), Some("c1"));
    }

    #[test]
    fn nan_and_infinite_coordinates_do_not_panic() {
        let rows = stack(&[live("a"), live("b")]);
        for y in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            assert!(resolve(&rows, y).is_some());
        }
    }
}
