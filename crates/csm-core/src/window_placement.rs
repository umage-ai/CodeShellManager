//! Decisions behind restoring the main window's bounds (C# `WindowPlacementMath`, #181): which
//! monitor the saved rectangle belongs on, whether it is still reachable, and what to do when
//! it isn't.
//!
//! Everything is in physical pixels, the only coordinate space that means the same thing on
//! every monitor of a per-monitor-DPI-v2 process. Coordinates are `i64` and arithmetic
//! saturates: the saved numbers come from `state.json`, which is untrusted, and C#'s unchecked
//! `int` math would silently wrap where Rust's debug build would panic.

use crate::models::{WindowBounds, WindowPlacementState};

/// A rectangle in physical screen pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PixelRect {
    pub left: i64,
    pub top: i64,
    pub width: i64,
    pub height: i64,
}

impl PixelRect {
    pub const fn new(left: i64, top: i64, width: i64, height: i64) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }

    pub fn right(&self) -> i64 {
        self.left.saturating_add(self.width)
    }

    pub fn bottom(&self) -> i64 {
        self.top.saturating_add(self.height)
    }

    /// The overlap, or the empty rect (all zeros) when there is none.
    pub fn intersect(&self, o: &PixelRect) -> PixelRect {
        let l = self.left.max(o.left);
        let t = self.top.max(o.top);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        if r > l && b > t {
            PixelRect::new(l, t, r.saturating_sub(l), b.saturating_sub(t))
        } else {
            PixelRect::default()
        }
    }

    pub fn area(&self) -> i64 {
        self.width.saturating_mul(self.height)
    }
}

/// One display: its full bounds and work area (physical pixels) and its DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonitorArea {
    pub bounds: PixelRect,
    pub work: PixelRect,
    pub dpi: i64,
    pub is_primary: bool,
}

/// Height of the strip at the top of the window that must stay reachable (DIPs).
pub const CAPTION_DIP: i64 = 32;

/// How much of that strip must be on a work area for the window to count as visible (DIPs).
pub const MIN_VISIBLE_WIDTH_DIP: i64 = 120;

/// The normal rectangle to restore and the monitor it lands on (an index into `monitors`).
/// `None` when there is nothing usable saved (first run, or garbage) and the caller keeps its
/// defaults.
///
/// `placement` is the physical-pixel bounds saved by this build (preferred). `legacy` is DIP
/// bounds saved by an older build, in an unknown scale, see [`from_legacy_dips`]: a wrong guess
/// only misplaces the window by the scale ratio, and the visibility rules still guarantee it
/// ends up reachable.
pub fn resolve(
    placement: Option<&WindowPlacementState>,
    legacy: Option<&WindowBounds>,
    monitors: &[MonitorArea],
    min_width_dip: f64,
    min_height_dip: f64,
) -> Option<(PixelRect, usize)> {
    let primary = monitors
        .iter()
        .position(|m| m.is_primary)
        .or(if monitors.is_empty() { None } else { Some(0) })?;

    let (mut rect, saved_dpi) = match placement {
        Some(p) if p.width > 0 && p.height > 0 && p.dpi > 0 => (
            PixelRect::new(
                i64::from(p.left),
                i64::from(p.top),
                i64::from(p.width),
                i64::from(p.height),
            ),
            i64::from(p.dpi),
        ),
        _ => match legacy {
            Some(l) if usable_legacy(l) => from_legacy_dips(l, monitors, primary)?,
            _ => return None,
        },
    };

    let home = home(&rect, monitors);
    let target = home.or_else(|| nearest(&rect, monitors)).unwrap_or(primary);
    let target_monitor = monitors.get(target)?;

    // Same apparent size if that monitor's scale changed since the save. The top-left corner
    // stays put: it is what the user placed.
    rect = rescale(&rect, saved_dpi, target_monitor.dpi);

    // Reachable: leave it exactly where the user put it, even hanging off an edge or spanning
    // two monitors. Restoring is not the place to second-guess a layout.
    if home.is_some() && is_caption_visible(&rect, target_monitor.dpi, monitors) {
        return Some((
            ensure_min_size(&rect, target_monitor, min_width_dip, min_height_dip),
            target,
        ));
    }

    // Caption out of reach (monitor unplugged, rearranged, resolution dropped): fit it into the
    // nearest monitor's work area, moving it the shortest distance, to the nearest edge rather
    // than the centre, so it stays roughly where it was.
    rect = ensure_min_size(&rect, target_monitor, min_width_dip, min_height_dip);
    Some((clamp_into(&rect, &target_monitor.work), target))
}

fn usable_legacy(l: &WindowBounds) -> bool {
    [l.left, l.top, l.width, l.height]
        .iter()
        .all(|v| v.is_finite())
        && l.width > 0.0
        && l.height > 0.0
}

/// Legacy DIP bounds to physical pixels, plus the DPI they are taken to have been saved at. The
/// scale they were saved at is unknown: v0.10.0 (per-monitor v2, the usual upgrade source)
/// saved pixels / the DPI of the monitor the window was on; older system-DPI builds saved
/// pixels / the primary's DPI at the time. So each monitor's DPI is tried as the scale, and a
/// candidate is consistent when the scaled rect's own monitor ([`home`]) is the monitor whose
/// DPI was used. Several can be consistent (a small window near a shared edge); the primary
/// wins that tie, then the largest share of the rect on its monitor. If none is, the primary's
/// scale is used and the visibility rules in [`resolve`] take over.
pub fn from_legacy_dips(
    legacy: &WindowBounds,
    monitors: &[MonitorArea],
    primary: usize,
) -> Option<(PixelRect, i64)> {
    let scaled = |dpi: i64| {
        let s = dpi as f64 / 96.0;
        PixelRect::new(
            round(legacy.left * s),
            round(legacy.top * s),
            round(legacy.width * s),
            round(legacy.height * s),
        )
    };

    // (monitor index, scaled rect, share of the rect that is on that monitor)
    let mut best: Option<(usize, PixelRect, f64)> = None;
    for (i, m) in monitors.iter().enumerate() {
        let r = scaled(m.dpi);
        if home(&r, monitors) != Some(i) {
            continue;
        }
        let share = r.intersect(&m.bounds).area() as f64 / r.area().max(1) as f64;
        let better = match &best {
            None => true,
            Some((bi, _, bshare)) => {
                let (bp, mp) = (
                    monitors.get(*bi).is_some_and(|b| b.is_primary),
                    m.is_primary,
                );
                // OrderByDescending(IsPrimary).ThenByDescending(share), first of equals wins.
                (mp && !bp) || (mp == bp && share > *bshare)
            }
        };
        if better {
            best = Some((i, r, share));
        }
    }
    match best {
        Some((i, r, _)) => Some((r, monitors.get(i)?.dpi)),
        None => {
            let p = monitors.get(primary)?;
            Some((scaled(p.dpi), p.dpi))
        }
    }
}

/// The monitor (index) a screen rect is (mostly) on: largest overlap, as Windows picks it;
/// `None` when it overlaps none. The first of equal overlaps wins.
pub fn home(rect: &PixelRect, monitors: &[MonitorArea]) -> Option<usize> {
    let mut best: Option<(usize, i64)> = None;
    for (i, m) in monitors.iter().enumerate() {
        let area = rect.intersect(&m.bounds).area();
        if area > 0 && best.is_none_or(|(_, a)| area > a) {
            best = Some((i, area));
        }
    }
    best.map(|(i, _)| i)
}

/// What to save for a normal-state window: its screen rect (GetWindowRect) and the DPI of the
/// monitor that RECT is on, not of whatever monitor the window reports, which is the question
/// that goes wrong for a snapped or spanning window.
pub fn save_from_screen(
    screen: &PixelRect,
    monitors: &[MonitorArea],
) -> Option<WindowPlacementState> {
    if screen.width <= 0 || screen.height <= 0 || monitors.is_empty() {
        return None;
    }
    let idx = home(screen, monitors).or_else(|| nearest(screen, monitors))?;
    to_state(screen, monitors.get(idx)?)
}

/// What to save for a maximized/minimized window: its normal rect from GetWindowPlacement
/// (workspace coordinates). Each monitor's work-area offset is tried, and the one under which
/// the rect lands most on that same monitor is taken: the rect's own monitor, which need not be
/// the one the (maximized) window is on now. That monitor supplies the DPI.
pub fn save_from_workspace(
    workspace: &PixelRect,
    monitors: &[MonitorArea],
) -> Option<WindowPlacementState> {
    if workspace.width <= 0 || workspace.height <= 0 || monitors.is_empty() {
        return None;
    }
    let mut hit: Option<(&MonitorArea, PixelRect, i64)> = None;
    for m in monitors {
        let s = workspace_to_screen(workspace, m);
        let area = s.intersect(&m.bounds).area();
        if area > 0 && hit.is_none_or(|(_, _, a)| area > a) {
            hit = Some((m, s, area));
        }
    }
    if let Some((m, s, _)) = hit {
        return to_state(&s, m);
    }
    let m = monitors.get(nearest(workspace, monitors)?)?;
    to_state(&workspace_to_screen(workspace, m), m)
}

fn to_state(r: &PixelRect, m: &MonitorArea) -> Option<WindowPlacementState> {
    Some(WindowPlacementState {
        left: i32::try_from(r.left).ok()?,
        top: i32::try_from(r.top).ok()?,
        width: i32::try_from(r.width).ok()?,
        height: i32::try_from(r.height).ok()?,
        dpi: i32::try_from(m.dpi).ok()?,
    })
}

/// True when the top strip of the window (where the caption is) overlaps some monitor's work
/// area by enough to grab it with the mouse.
pub fn is_caption_visible(rect: &PixelRect, dpi: i64, monitors: &[MonitorArea]) -> bool {
    let strip = rect.height.min(scale(CAPTION_DIP, dpi));
    let caption = PixelRect::new(rect.left, rect.top, rect.width, strip);
    let need_w = rect.width.min(scale(MIN_VISIBLE_WIDTH_DIP, dpi));
    let need_h = (strip / 2).max(1);
    monitors.iter().any(|m| {
        let i = caption.intersect(&m.work);
        i.width >= need_w && i.height >= need_h
    })
}

/// Scales the size (not the position) from one DPI to another.
pub fn rescale(r: &PixelRect, from_dpi: i64, to_dpi: i64) -> PixelRect {
    if from_dpi <= 0 || to_dpi <= 0 || from_dpi == to_dpi {
        return *r;
    }
    let f = to_dpi as f64 / from_dpi as f64;
    PixelRect {
        width: round(r.width as f64 * f),
        height: round(r.height as f64 * f),
        ..*r
    }
}

/// Moves `r` the shortest distance that puts it inside `area` (shrinking it first if it is
/// larger than the area).
pub fn clamp_into(r: &PixelRect, area: &PixelRect) -> PixelRect {
    let w = r.width.min(area.width);
    let h = r.height.min(area.height);
    // C# Math.Clamp throws when min > max; max(left, right - w) can't invert: w <= area.width.
    let l = r.left.min(area.right().saturating_sub(w)).max(area.left);
    let t = r.top.min(area.bottom().saturating_sub(h)).max(area.top);
    PixelRect::new(l, t, w, h)
}

/// Screen to workspace coordinates. `WINDOWPLACEMENT.rcNormalPosition` is in workspace
/// coordinates, relative to the work area, so a taskbar docked top or left shifts them. Save
/// and restore both convert through the window's monitor, so a round trip is exact whichever
/// way Windows defines the offset on a secondary monitor.
pub fn screen_to_workspace(r: &PixelRect, m: &MonitorArea) -> PixelRect {
    PixelRect {
        left: r
            .left
            .saturating_sub(m.work.left.saturating_sub(m.bounds.left)),
        top: r
            .top
            .saturating_sub(m.work.top.saturating_sub(m.bounds.top)),
        ..*r
    }
}

/// The inverse of [`screen_to_workspace`].
pub fn workspace_to_screen(r: &PixelRect, m: &MonitorArea) -> PixelRect {
    PixelRect {
        left: r
            .left
            .saturating_add(m.work.left.saturating_sub(m.bounds.left)),
        top: r
            .top
            .saturating_add(m.work.top.saturating_sub(m.bounds.top)),
        ..*r
    }
}

/// Grows a too-small rect to the window's minimum size (capped at the work area).
fn ensure_min_size(
    r: &PixelRect,
    m: &MonitorArea,
    min_width_dip: f64,
    min_height_dip: f64,
) -> PixelRect {
    let min_w = m.work.width.min(ceil(min_width_dip * m.dpi as f64 / 96.0));
    let min_h = m
        .work
        .height
        .min(ceil(min_height_dip * m.dpi as f64 / 96.0));
    PixelRect {
        width: r.width.max(min_w),
        height: r.height.max(min_h),
        ..*r
    }
}

/// The monitor whose bounds are nearest the rect's centre (squared distance); the first of
/// equals wins. `None` only for an empty monitor list.
fn nearest(r: &PixelRect, monitors: &[MonitorArea]) -> Option<usize> {
    let cx = r.left.saturating_add(r.width / 2);
    let cy = r.top.saturating_add(r.height / 2);
    let dist = |m: &MonitorArea| {
        let dx = if cx < m.bounds.left {
            m.bounds.left.saturating_sub(cx)
        } else if cx > m.bounds.right() {
            cx.saturating_sub(m.bounds.right())
        } else {
            0
        };
        let dy = if cy < m.bounds.top {
            m.bounds.top.saturating_sub(cy)
        } else if cy > m.bounds.bottom() {
            cy.saturating_sub(m.bounds.bottom())
        } else {
            0
        };
        dx.saturating_mul(dx).saturating_add(dy.saturating_mul(dy))
    };
    let mut best: Option<(usize, i64)> = None;
    for (i, m) in monitors.iter().enumerate() {
        let d = dist(m);
        if best.is_none_or(|(_, bd)| d < bd) {
            best = Some((i, d));
        }
    }
    best.map(|(i, _)| i)
}

fn scale(dip: i64, dpi: i64) -> i64 {
    round(dip as f64 * dpi.max(96) as f64 / 96.0)
}

/// `Math.Round(v, MidpointRounding.AwayFromZero)`; NaN becomes 0 and out-of-range saturates.
fn round(v: f64) -> i64 {
    v.round() as i64
}

fn ceil(v: f64) -> i64 {
    v.ceil() as i64
}

#[cfg(test)]
mod tests {
    use super::*;

    // The standard rig: a 1920x1080 primary at 100% with a 48px taskbar at the bottom, and a
    // 3840x2160 secondary at 150% to its right.
    fn primary() -> MonitorArea {
        MonitorArea {
            bounds: PixelRect::new(0, 0, 1920, 1080),
            work: PixelRect::new(0, 0, 1920, 1032),
            dpi: 96,
            is_primary: true,
        }
    }

    fn secondary() -> MonitorArea {
        MonitorArea {
            bounds: PixelRect::new(1920, 0, 3840, 2160),
            work: PixelRect::new(1920, 0, 3840, 2088),
            dpi: 144,
            is_primary: false,
        }
    }

    fn both() -> Vec<MonitorArea> {
        vec![primary(), secondary()]
    }

    fn p(l: i32, t: i32, w: i32, h: i32, dpi: i32) -> WindowPlacementState {
        WindowPlacementState {
            left: l,
            top: t,
            width: w,
            height: h,
            dpi,
        }
    }

    fn rect(l: i64, t: i64, w: i64, h: i64) -> PixelRect {
        PixelRect::new(l, t, w, h)
    }

    fn res(
        placement: Option<&WindowPlacementState>,
        legacy: Option<&WindowBounds>,
        monitors: &[MonitorArea],
    ) -> Option<(PixelRect, usize)> {
        resolve(placement, legacy, monitors, 0.0, 0.0)
    }

    fn legacy(l: f64, t: f64, w: f64, h: f64) -> WindowBounds {
        WindowBounds {
            left: l,
            top: t,
            width: w,
            height: h,
        }
    }

    #[test]
    fn nothing_saved_resolves_to_none() {
        assert_eq!(res(None, None, &both()), None);
        assert_eq!(res(Some(&p(0, 0, 0, 0, 96)), None, &both()), None);
        assert_eq!(
            res(None, Some(&legacy(0.0, 0.0, f64::NAN, 10.0)), &both()),
            None
        );
    }

    #[test]
    fn no_monitors_resolves_to_none() {
        assert_eq!(res(Some(&p(10, 10, 800, 600, 96)), None, &[]), None);
    }

    #[test]
    fn placement_on_the_secondary_is_restored_exactly() {
        // The drift case: same pixels back, every run, on a monitor whose scale differs from
        // the primary's.
        let (r, m) = res(Some(&p(2200, 300, 2400, 1500, 144)), None, &both()).unwrap();
        assert_eq!(r, rect(2200, 300, 2400, 1500));
        assert_eq!(m, 1);
    }

    // A rig where the workspace offsets differ per monitor: primary with its taskbar docked at
    // the TOP (workspace (0,0) = screen (0,48)), 150% secondary with a left taskbar.
    fn top_bar_primary() -> MonitorArea {
        MonitorArea {
            bounds: PixelRect::new(0, 0, 1920, 1080),
            work: PixelRect::new(0, 48, 1920, 1032),
            dpi: 96,
            is_primary: true,
        }
    }

    fn left_bar_secondary(dpi: i64) -> MonitorArea {
        MonitorArea {
            bounds: PixelRect::new(1920, 0, 3840, 2160),
            work: PixelRect::new(1992, 0, 3768, 2160),
            dpi,
            is_primary: false,
        }
    }

    /// One app run: restore what was saved, then save what is on screen the way
    /// MainWindow.CaptureWindowState does: GetWindowRect for a normal window, the workspace
    /// rcNormalPosition (as SetWindowPlacement was given it) for a maximized one.
    fn run(
        saved: &WindowPlacementState,
        monitors: &[MonitorArea],
        maximized: bool,
    ) -> WindowPlacementState {
        let (r, mi) = res(Some(saved), None, monitors).unwrap();
        let m = monitors.get(mi).unwrap();
        if maximized {
            save_from_workspace(&screen_to_workspace(&r, m), monitors).unwrap()
        } else {
            save_from_screen(&r, monitors).unwrap()
        }
    }

    fn tuple(s: &WindowPlacementState) -> (i32, i32, i32, i32, i32) {
        (s.left, s.top, s.width, s.height, s.dpi)
    }

    #[test]
    fn round_trip_is_stable_across_runs_with_per_monitor_offsets() {
        let monitors = [top_bar_primary(), left_bar_secondary(144)];
        for maximized in [false, true] {
            for start in [p(2500, 400, 1800, 1200, 144), p(100, 200, 1000, 700, 96)] {
                let mut cur = start;
                for _ in 0..10 {
                    cur = run(&cur, &monitors, maximized);
                }
                assert_eq!(tuple(&cur), tuple(&start), "maximized={maximized}");
            }
        }
    }

    #[test]
    fn monitor_scale_change_rescales_once_then_stays_put() {
        let mut cur = run(
            &p(2500, 400, 1800, 1200, 144),
            &[top_bar_primary(), left_bar_secondary(144)],
            false,
        );
        // The secondary goes 150% -> 200%: same apparent size, applied once.
        let after = [top_bar_primary(), left_bar_secondary(192)];
        cur = run(&cur, &after, false);
        assert_eq!(tuple(&cur), (2500, 400, 2400, 1600, 192));
        for _ in 0..5 {
            cur = run(&cur, &after, false);
        }
        assert_eq!(tuple(&cur), (2500, 400, 2400, 1600, 192));
    }

    #[test]
    fn saved_normal_rect_takes_offset_and_dpi_from_the_monitor_it_is_on() {
        // A maximized window on the PRIMARY whose normal (pre-maximize / pre-snap) rect is on
        // the secondary: taking offset and DPI from the window's current monitor would give top
        // 348 at 96 dpi: the wrong place, and a rescale that shrinks it next run.
        let monitors = [top_bar_primary(), left_bar_secondary(144)];
        let s = save_from_workspace(&rect(2128, 300, 1500, 900), &monitors).unwrap();
        assert_eq!(tuple(&s), (2200, 300, 1500, 900, 144));

        // A rect on the primary picks up the primary's top-taskbar offset instead.
        let s = save_from_workspace(&rect(100, 100, 800, 600), &monitors).unwrap();
        assert_eq!((s.left, s.top, s.dpi), (100, 148, 96));
    }

    #[test]
    fn saved_screen_rect_takes_dpi_from_the_monitor_holding_most_of_it() {
        let monitors = both();
        assert_eq!(
            save_from_screen(&rect(1500, 100, 1200, 800), &monitors)
                .unwrap()
                .dpi,
            144
        );
        assert_eq!(
            save_from_screen(&rect(1000, 100, 1200, 800), &monitors)
                .unwrap()
                .dpi,
            96
        );
        // Off every monitor: the nearest one's.
        assert_eq!(
            save_from_screen(&rect(9000, 100, 800, 600), &monitors)
                .unwrap()
                .dpi,
            144
        );
    }

    #[test]
    fn a_window_spanning_both_monitors_is_left_alone() {
        // Mostly on the secondary, so that is the DPI it was saved at.
        let (r, m) = res(Some(&p(1500, 100, 1200, 800, 144)), None, &both()).unwrap();
        assert_eq!(r, rect(1500, 100, 1200, 800));
        assert_eq!(m, 1);
    }

    #[test]
    fn monitor_scale_change_keeps_the_apparent_size() {
        // Saved at 100% on the secondary, which is now at 150%: 1.5x the pixels, same corner.
        let (r, _) = res(Some(&p(2000, 100, 1000, 800, 96)), None, &both()).unwrap();
        assert_eq!(r, rect(2000, 100, 1500, 1200));
    }

    #[test]
    fn unplugged_monitor_falls_back_to_a_visible_position() {
        // Saved on the secondary; now only the primary exists.
        let only = [primary()];
        let (r, m) = res(Some(&p(2200, 300, 1500, 1000, 144)), None, &only).unwrap();
        assert_eq!(m, 0);
        // 1500x1000 @144 -> 1000x667 @96, pushed to the nearest (right) edge of the work area.
        assert_eq!(r, rect(920, 300, 1000, 667));
        assert!(is_caption_visible(&r, 96, &only));
    }

    #[test]
    fn monitor_left_of_primary_gone_lands_on_the_left_edge() {
        let (r, _) = res(Some(&p(-1700, 200, 1200, 800, 96)), None, &[primary()]).unwrap();
        assert_eq!(r, rect(0, 200, 1200, 800));
    }

    #[test]
    fn caption_above_every_work_area_is_pulled_down() {
        // Body on screen, title bar not: unreachable with the mouse.
        let (r, _) = res(Some(&p(100, -200, 1000, 700, 96)), None, &[primary()]).unwrap();
        assert_eq!(r, rect(100, 0, 1000, 700));
    }

    #[test]
    fn caption_under_the_taskbar_is_pulled_up() {
        // Top edge inside the taskbar band (1032..1080): on the monitor, not on its work area.
        let (r, _) = res(Some(&p(100, 1040, 800, 600, 96)), None, &[primary()]).unwrap();
        assert_eq!(r, rect(100, 432, 800, 600));
    }

    #[test]
    fn slightly_off_edge_window_is_kept_where_it_is() {
        let (r, _) = res(Some(&p(-8, -4, 1000, 700, 96)), None, &[primary()]).unwrap();
        assert_eq!(r, rect(-8, -4, 1000, 700));
    }

    #[test]
    fn off_screen_window_larger_than_the_work_area_is_shrunk_to_fit() {
        let (r, _) = res(Some(&p(5000, 5000, 3000, 2000, 96)), None, &[primary()]).unwrap();
        assert_eq!(r, primary().work);
    }

    #[test]
    fn minimum_size_is_enforced_in_physical_pixels() {
        let (r, _) = resolve(
            Some(&p(2000, 100, 300, 200, 144)),
            None,
            &both(),
            800.0,
            500.0,
        )
        .unwrap();
        assert_eq!(r, rect(2000, 100, 1200, 750));
    }

    #[test]
    fn legacy_dip_bounds_are_scaled_by_the_primary_dpi() {
        // Saved by a system-DPI build at 150% primary: DIPs x 1.5 = pixels.
        let primary150 = MonitorArea {
            dpi: 144,
            ..primary()
        };
        let (r, _) = res(
            None,
            Some(&legacy(100.0, 50.0, 1000.0, 600.0)),
            &[primary150],
        )
        .unwrap();
        assert_eq!(r, rect(150, 75, 1500, 900));
    }

    #[test]
    fn legacy_v0_10_bounds_on_a_150_percent_secondary_land_back_on_it() {
        // v0.10.0 (per-monitor v2) saved DIPs = pixels x 96 / THAT monitor's DPI. A window at
        // pixels (2200, 300, 2400x1500) on the 150% secondary was saved as (1466.67, 200,
        // 1600x1000). Read at the primary's 100% it would land mostly on the primary at
        // two-thirds the size.
        let (r, m) = res(
            None,
            Some(&legacy(2200.0 / 1.5, 200.0, 1600.0, 1000.0)),
            &both(),
        )
        .unwrap();
        assert_eq!(r, rect(2200, 300, 2400, 1500));
        assert_eq!(m, 1);
    }

    #[test]
    fn legacy_bounds_on_the_primary_stay_on_the_primary() {
        let (r, m) = res(None, Some(&legacy(100.0, 100.0, 1000.0, 700.0)), &both()).unwrap();
        assert_eq!(r, rect(100, 100, 1000, 700));
        assert_eq!(m, 0);
    }

    #[test]
    fn legacy_bounds_ambiguous_between_monitors_prefer_the_primary() {
        // DIP (1300, 100, 400x300) is consistent both as 100% pixels on the primary and as 150%
        // pixels (1950, 150, ...) on the secondary. Nothing in the file says which.
        let (r, m) = res(None, Some(&legacy(1300.0, 100.0, 400.0, 300.0)), &both()).unwrap();
        assert_eq!(m, 0);
        assert_eq!(r, rect(1300, 100, 400, 300));
    }

    #[test]
    fn legacy_bounds_far_off_screen_still_produce_a_visible_window() {
        let only = [primary()];
        let (r, _) = res(None, Some(&legacy(9000.0, 9000.0, 1280.0, 800.0)), &only).unwrap();
        assert!(is_caption_visible(&r, 96, &only));
        assert_eq!(r, rect(640, 232, 1280, 800));
    }

    #[test]
    fn placement_wins_over_legacy_bounds() {
        let (r, _) = res(
            Some(&p(2200, 300, 2400, 1500, 144)),
            Some(&legacy(0.0, 0.0, 500.0, 500.0)),
            &both(),
        )
        .unwrap();
        assert_eq!(r, rect(2200, 300, 2400, 1500));
    }

    #[test]
    fn workspace_conversion_round_trips_and_follows_a_top_taskbar() {
        // Taskbar docked at the top: workspace (0,0) is screen (0,48).
        let top_bar = top_bar_primary();
        let screen = rect(100, 148, 800, 600);
        let ws = screen_to_workspace(&screen, &top_bar);
        assert_eq!(ws, rect(100, 100, 800, 600));
        assert_eq!(workspace_to_screen(&ws, &top_bar), screen);
    }

    #[test]
    fn rescale_scales_size_only() {
        for (from, to, w, expected_w) in [
            (96, 144, 1000, 1500),
            (144, 96, 1500, 1000),
            (120, 120, 1000, 1000),
        ] {
            assert_eq!(
                rescale(&rect(7, 9, w, w), from, to),
                rect(7, 9, expected_w, expected_w)
            );
        }
    }

    // ---- additions: hostile saved values ----

    #[test]
    fn extreme_saved_values_never_panic() {
        let extremes = [i32::MIN, i32::MAX, 0, -1, 1];
        for &a in &extremes {
            for &b in &extremes {
                for &dpi in &[i32::MAX, 1, 96] {
                    let pl = p(
                        a,
                        b,
                        a.saturating_abs().max(1),
                        b.saturating_abs().max(1),
                        dpi,
                    );
                    let _ = res(Some(&pl), None, &both());
                    let _ = res(Some(&pl), None, &[primary()]);
                    let _ = resolve(Some(&pl), None, &both(), f64::MAX, f64::NAN);
                    let r = rect(i64::from(a), i64::from(b), i64::from(a), i64::from(b));
                    let _ = save_from_screen(&r, &both());
                    let _ = save_from_workspace(&r, &both());
                    let _ = clamp_into(&r, &primary().work);
                }
            }
        }
        let huge = legacy(f64::MAX, f64::MIN, f64::MAX, f64::INFINITY);
        let _ = res(None, Some(&huge), &both());
        let _ = res(None, Some(&legacy(1e300, 1e300, 1e300, 1e300)), &both());
    }
}
