//! Remembering where windows are: the main window and every torn-off group or pane window.
//!
//! A [`WindowTracker`] per window does two things, both from frame code and neither blocking:
//!
//! - **Restore**, once, on the window's first frame with a native scale: the saved bounds
//!   (`UiModel::windows`, i.e. the state file) are checked against the monitors with
//!   `csm_core::window_placement::resolve` (off-screen recovery, DPI change, minimum size) and
//!   sent as viewport commands. Commands rather than `ViewportBuilder` positions because
//!   egui-winit converts a command with the window's *current* scale, so a physical pixel stays a
//!   physical pixel; a builder position is converted before the window knows its monitor.
//! - **Save**: each frame the window's outer rect is read from its `ViewportInfo` (physical
//!   pixels = points × pixels per point), and once it has stayed put for [`SAVE_AFTER_S`] it is
//!   submitted as `window_placement_set`. A maximized window keeps the last normal rect it had
//!   (winit has no "restore bounds"), a minimized one saves nothing. Closing or docking a window
//!   flushes a pending save at once.
//!
//! The monitor list ([`Monitors`]) is read by the host from winit's event loop (outside frame
//! code) and only copied here. Without it (tests, a host that can't tell) the saved rect is used
//! as is and the DPI comes from the window's own scale.
//!
//! Saved rects are the window's outer rect (the state file's `LastPlacement` is the C#
//! `GetWindowRect`). Restoring sets the inner size, so the decoration (outer minus inner, scaled
//! to the target monitor's DPI) is taken off. Where the platform reports no position (Wayland)
//! the inner size is saved at (0, 0) and only the size is restored.

use std::sync::{Arc, Mutex};

use csm_core::models::{WindowPlacementState, WindowSlot};
use csm_core::window_placement::{self as wp, MonitorArea, PixelRect};
use egui::{Vec2, ViewportCommand, ViewportInfo, pos2, vec2};
use serde_json::{Value, json};

use crate::terminal::lock;

/// The monitors, as the host last read them (physical pixels). Empty: unknown.
pub type Monitors = Arc<Mutex<Vec<MonitorArea>>>;

/// How long a window must stay put before its bounds are saved (a drag or a resize is many
/// frames; saving each would rewrite `state.json` many times a second).
pub const SAVE_AFTER_S: f64 = 0.5;

/// What a window's `ViewportInfo` says about where it is, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observed {
    /// The outer rect; `None` where the platform reports no position (Wayland).
    pub outer: Option<PixelRect>,
    /// Inner (client) size.
    pub inner: Option<(i64, i64)>,
    /// The window's native scale (1.0 = 96 DPI).
    pub native_ppp: f32,
    /// Native scale × zoom: what egui-winit multiplies points by.
    pub ppp: f32,
    pub maximized: bool,
    pub minimized: bool,
}

fn px(v: f32) -> i64 {
    // NaN becomes 0 and out-of-range saturates (`as` on floats); the values are the window's own.
    v.round() as i64
}

impl Observed {
    /// `None` until the window has a native scale (the first frame of a real window has one;
    /// a test harness that sets no viewport info never does).
    pub fn from_info(info: &ViewportInfo, zoom: f32) -> Option<Observed> {
        let native_ppp = info.native_pixels_per_point.filter(|p| p.is_finite() && *p > 0.0)?;
        let ppp = native_ppp * if zoom.is_finite() && zoom > 0.0 { zoom } else { 1.0 };
        let rect = |r: egui::Rect| PixelRect::new(px(r.min.x * ppp), px(r.min.y * ppp), px(r.width() * ppp), px(r.height() * ppp));
        Some(Observed {
            outer: info.outer_rect.map(rect),
            inner: info.inner_rect.map(rect).map(|r| (r.width, r.height)),
            native_ppp,
            ppp,
            maximized: info.maximized == Some(true) || info.fullscreen == Some(true),
            minimized: info.minimized == Some(true),
        })
    }

    /// The rect to save for a normal-state window.
    fn rect(&self) -> Option<PixelRect> {
        match (self.outer, self.inner) {
            (Some(o), _) => Some(o),
            (None, Some((w, h))) => Some(PixelRect::new(0, 0, w, h)),
            (None, None) => None,
        }
        .filter(|r| r.width > 0 && r.height > 0)
    }

    /// Outer minus inner size (physical pixels at the current scale).
    fn decoration(&self) -> (i64, i64) {
        match (self.outer, self.inner) {
            (Some(o), Some((w, h))) => ((o.width - w).max(0), (o.height - h).max(0)),
            _ => (0, 0),
        }
    }
}

fn dpi_of(native_ppp: f32) -> i64 {
    px(native_ppp * 96.0).max(1)
}

/// The placement to save for a normal-state window at `rect`: the DPI of the monitor holding
/// most of it, or (monitors unknown) the window's own.
pub fn placement_of(rect: &PixelRect, native_ppp: f32, monitors: &[MonitorArea]) -> Option<WindowPlacementState> {
    wp::save_from_screen(rect, monitors).or_else(|| {
        Some(WindowPlacementState {
            left: i32::try_from(rect.left).ok()?,
            top: i32::try_from(rect.top).ok()?,
            width: i32::try_from(rect.width).ok().filter(|w| *w > 0)?,
            height: i32::try_from(rect.height).ok().filter(|h| *h > 0)?,
            dpi: i32::try_from(dpi_of(native_ppp)).ok()?,
        })
    })
}

/// The viewport commands that put a window back at `saved`, given where it is now. `min_dip` is
/// the window's minimum inner size (DIPs). Empty when there is nothing usable saved.
pub fn restore_commands(saved: &WindowSlot, now: &Observed, monitors: &[MonitorArea], min_dip: Vec2) -> Vec<ViewportCommand> {
    let p = &saved.placement;
    let (rect, target_dpi) = if monitors.is_empty() {
        if p.width <= 0 || p.height <= 0 {
            return Vec::new();
        }
        (PixelRect::new(i64::from(p.left), i64::from(p.top), i64::from(p.width), i64::from(p.height)), dpi_of(now.native_ppp))
    } else {
        let Some((r, i)) = wp::resolve(Some(p), None, monitors, f64::from(min_dip.x), f64::from(min_dip.y)) else { return Vec::new() };
        (r, monitors.get(i).map_or_else(|| dpi_of(now.native_ppp), |m| m.dpi.max(1)))
    };
    let zoom = now.ppp / now.native_ppp;
    let target_ppp = target_dpi as f32 / 96.0 * zoom;
    // The decoration scales with the monitor's DPI (the caption does).
    let (dw, dh) = now.decoration();
    let scale = target_dpi as f32 / dpi_of(now.native_ppp) as f32;
    let inner_w = (rect.width - px(dw as f32 * scale)).max(1);
    let inner_h = (rect.height - px(dh as f32 * scale)).max(1);
    let mut cmds = Vec::new();
    let can_position = now.outer.is_some();
    if can_position {
        // At the current scale: lands on the target monitor (which may change the scale).
        cmds.push(ViewportCommand::OuterPosition(pos2(rect.left as f32 / now.ppp, rect.top as f32 / now.ppp)));
    }
    // egui-winit converts each command with the scale the window has when it runs, i.e. the
    // target monitor's once the move above has landed.
    cmds.push(ViewportCommand::InnerSize(vec2(inner_w as f32 / target_ppp, inner_h as f32 / target_ppp)));
    if can_position {
        cmds.push(ViewportCommand::OuterPosition(pos2(rect.left as f32 / target_ppp, rect.top as f32 / target_ppp)));
    }
    if saved.maximized {
        cmds.push(ViewportCommand::Maximized(true));
    }
    cmds
}

/// `window_placement_set` params.
pub fn params(window: &str, slot: &WindowSlot) -> Value {
    let p = &slot.placement;
    json!({"window": window, "left": p.left, "top": p.top, "width": p.width, "height": p.height, "dpi": p.dpi, "maximized": slot.maximized})
}

/// One window's restore and debounced save. See the module docs.
#[derive(Debug, Clone)]
pub struct WindowTracker {
    /// `main`, `group:<id>`, `pane:<session id>`.
    pub key: String,
    restored: bool,
    /// What the state file has (as far as this window knows).
    saved: Option<WindowSlot>,
    /// The last normal-state rect seen (kept while maximized).
    normal: Option<WindowPlacementState>,
    /// A change waiting to settle: (bounds, when first seen).
    pending: Option<(WindowSlot, f64)>,
    /// Restore commands were sent: until then, what the window reports is where it was before
    /// they landed (the default rect, not yet maximized) and is not taken as its bounds.
    quiet_until: f64,
}

impl WindowTracker {
    pub fn new(key: impl Into<String>) -> Self {
        WindowTracker { key: key.into(), restored: false, saved: None, normal: None, pending: None, quiet_until: f64::NEG_INFINITY }
    }

    /// Whether the saved bounds were applied (or there were none).
    pub fn restored(&self) -> bool {
        self.restored
    }

    /// First frame (at `time`): the commands that restore `saved`. Later frames: nothing. When
    /// commands are sent, observations are ignored for [`SAVE_AFTER_S`] while they land, so the
    /// default rect the window opened at never replaces the saved normal rect (a window restored
    /// maximized would otherwise remember the default rect as its normal bounds).
    pub fn restore(&mut self, saved: Option<WindowSlot>, now: &Observed, monitors: &[MonitorArea], min_dip: Vec2, time: f64) -> Vec<ViewportCommand> {
        if self.restored {
            return Vec::new();
        }
        self.restored = true;
        self.saved = saved;
        self.normal = saved.map(|s| s.placement);
        let cmds = saved.map_or_else(Vec::new, |s| restore_commands(&s, now, monitors, min_dip));
        if !cmds.is_empty() {
            self.quiet_until = time + SAVE_AFTER_S;
        }
        cmds
    }

    /// This frame's bounds; the `window_placement_set` params once they have settled.
    pub fn observe(&mut self, now: &Observed, monitors: &[MonitorArea], time: f64) -> Option<Value> {
        if now.minimized || time < self.quiet_until {
            return None;
        }
        if !now.maximized
            && let Some(r) = now.rect()
        {
            self.normal = placement_of(&r, now.native_ppp, monitors).or(self.normal);
        }
        let slot = WindowSlot { placement: self.normal?, maximized: now.maximized };
        if self.saved == Some(slot) {
            self.pending = None;
            return None;
        }
        match self.pending {
            Some((p, since)) if p == slot => {
                if time - since >= SAVE_AFTER_S {
                    self.pending = None;
                    self.saved = Some(slot);
                    return Some(params(&self.key, &slot));
                }
            }
            _ => self.pending = Some((slot, time)),
        }
        None
    }

    /// A change still settling, now (the window is closing or docking).
    pub fn flush(&mut self) -> Option<Value> {
        let (slot, _) = self.pending.take()?;
        self.saved = Some(slot);
        Some(params(&self.key, &slot))
    }

    /// Whether a change is waiting to settle (the window should repaint to finish it).
    pub fn pending(&self) -> bool {
        self.pending.is_some()
    }

    /// One frame of a real window: restore on the first, then observe. Viewport commands go to
    /// the viewport `ctx` is drawing. Returns the params to submit, if any.
    pub fn step(&mut self, ctx: &egui::Context, saved: Option<WindowSlot>, monitors: &Monitors, min_dip: Vec2) -> Option<Value> {
        let (info, time) = ctx.input(|i| (i.viewport().clone(), i.time));
        let now = Observed::from_info(&info, ctx.zoom_factor())?;
        let monitors = lock(monitors).clone();
        for c in self.restore(saved, &now, &monitors, min_dip, time) {
            ctx.send_viewport_cmd(c);
        }
        let out = self.observe(&now, &monitors, time);
        if self.pending() || time < self.quiet_until {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(SAVE_AFTER_S));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn primary() -> MonitorArea {
        MonitorArea { bounds: PixelRect::new(0, 0, 1920, 1080), work: PixelRect::new(0, 0, 1920, 1032), dpi: 96, is_primary: true }
    }

    fn secondary() -> MonitorArea {
        MonitorArea { bounds: PixelRect::new(1920, 0, 3840, 2160), work: PixelRect::new(1920, 0, 3840, 2088), dpi: 144, is_primary: false }
    }

    fn slot(l: i32, t: i32, w: i32, h: i32, dpi: i32, maximized: bool) -> WindowSlot {
        WindowSlot { placement: WindowPlacementState { left: l, top: t, width: w, height: h, dpi }, maximized }
    }

    /// A window at 100% on the primary: outer (100,100) 1296x839, inner 1280x800.
    fn info(outer: [f32; 4], ppp: f32) -> ViewportInfo {
        ViewportInfo {
            native_pixels_per_point: Some(ppp),
            outer_rect: Some(egui::Rect::from_min_size(pos2(outer[0], outer[1]), vec2(outer[2], outer[3]))),
            inner_rect: Some(egui::Rect::from_min_size(pos2(outer[0] + 8.0, outer[1] + 31.0), vec2(outer[2] - 16.0, outer[3] - 39.0))),
            maximized: Some(false),
            minimized: Some(false),
            ..ViewportInfo::default()
        }
    }

    #[test]
    fn info_converts_points_to_physical_pixels_with_scale_and_zoom() {
        let o = Observed::from_info(&info([100.0, 50.0, 800.0, 600.0], 1.5), 1.0).unwrap();
        assert_eq!(o.outer, Some(PixelRect::new(150, 75, 1200, 900)));
        // 16x39 points at 150%: 24 x (900 - round(841.5)).
        assert_eq!(o.decoration(), (24, 58));
        let z = Observed::from_info(&info([100.0, 50.0, 800.0, 600.0], 1.0), 2.0).unwrap();
        assert_eq!(z.ppp, 2.0);
        assert_eq!(z.outer, Some(PixelRect::new(200, 100, 1600, 1200)));
        assert!(Observed::from_info(&ViewportInfo::default(), 1.0).is_none(), "no scale: not a real window yet");
        let nan = ViewportInfo { native_pixels_per_point: Some(f32::NAN), ..ViewportInfo::default() };
        assert!(Observed::from_info(&nan, 1.0).is_none());
    }

    #[test]
    fn restoring_onto_a_scaled_secondary_lands_in_physical_pixels() {
        // The window opens on the 100% primary; the saved rect is on the 150% secondary.
        let now = Observed::from_info(&info([100.0, 100.0, 1296.0, 839.0], 1.0), 1.0).unwrap();
        let cmds = restore_commands(&slot(2200, 300, 2400, 1500, 144, false), &now, &[primary(), secondary()], Vec2::ZERO);
        // Moved at today's scale, sized at the target's (decoration 16x39 @100% is 24x59 @150%),
        // placed again at the target's.
        assert_eq!(
            cmds,
            vec![
                ViewportCommand::OuterPosition(pos2(2200.0, 300.0)),
                ViewportCommand::InnerSize(vec2((2400.0 - 24.0) / 1.5, (1500.0 - 59.0) / 1.5)),
                ViewportCommand::OuterPosition(pos2(2200.0 / 1.5, 300.0 / 1.5)),
            ]
        );
    }

    #[test]
    fn an_off_screen_rect_is_pulled_back_and_maximized_is_reapplied() {
        let now = Observed::from_info(&info([0.0, 0.0, 800.0, 600.0], 1.0), 1.0).unwrap();
        // Saved on a monitor that is gone.
        let cmds = restore_commands(&slot(5000, 300, 1000, 700, 96, true), &now, &[primary()], Vec2::ZERO);
        assert_eq!(cmds.first(), Some(&ViewportCommand::OuterPosition(pos2(920.0, 300.0))));
        assert_eq!(cmds.last(), Some(&ViewportCommand::Maximized(true)));
        // Garbage saves nothing to restore.
        assert!(restore_commands(&slot(0, 0, 0, 0, 96, false), &now, &[primary()], Vec2::ZERO).is_empty());
        assert!(restore_commands(&slot(0, 0, -5, 10, 96, false), &now, &[], Vec2::ZERO).is_empty());
    }

    #[test]
    fn without_a_position_only_the_size_is_restored() {
        // Wayland: no outer rect.
        let mut i = info([0.0, 0.0, 800.0, 600.0], 1.0);
        i.outer_rect = None;
        let now = Observed::from_info(&i, 1.0).unwrap();
        let cmds = restore_commands(&slot(0, 0, 1000, 700, 96, false), &now, &[], Vec2::ZERO);
        assert_eq!(cmds, vec![ViewportCommand::InnerSize(vec2(1000.0, 700.0))]);
        // And what is saved is the inner size at (0, 0).
        let mut t = WindowTracker::new("main");
        t.restore(None, &now, &[], Vec2::ZERO, 0.0);
        assert!(t.observe(&now, &[], 0.0).is_none());
        let p = t.observe(&now, &[], 1.0).unwrap();
        assert_eq!((p["left"].as_i64(), p["width"].as_i64(), p["height"].as_i64()), (Some(0), Some(784), Some(561)));
    }

    #[test]
    fn a_move_is_saved_once_it_settles_and_not_before() {
        let monitors = [primary(), secondary()];
        let mut t = WindowTracker::new("group:g");
        let at = |x: f32| Observed::from_info(&info([x, 100.0, 960.0, 560.0], 1.0), 1.0).unwrap();
        assert!(t.restore(Some(slot(100, 100, 960, 560, 96, false)), &at(100.0), &monitors, Vec2::ZERO, 0.0).len() >= 2);
        // Where it was saved: nothing to do.
        assert!(t.observe(&at(100.0), &monitors, 0.6).is_none());
        assert!(!t.pending());
        // Dragged: every frame differs, nothing is sent.
        for (i, x) in [150.0, 220.0, 300.0].into_iter().enumerate() {
            assert!(t.observe(&at(x), &monitors, 1.0 + i as f64 * 0.016).is_none());
        }
        assert!(t.pending());
        assert!(t.observe(&at(300.0), &monitors, 1.2).is_none(), "not settled yet");
        let p = t.observe(&at(300.0), &monitors, 1.6).unwrap();
        assert_eq!(p, json!({"window": "group:g", "left": 300, "top": 100, "width": 960, "height": 560, "dpi": 96, "maximized": false}));
        assert!(t.observe(&at(300.0), &monitors, 3.0).is_none(), "saved once");
        // Onto the secondary: its DPI is what is saved.
        t.observe(&at(2000.0), &monitors, 4.0);
        assert_eq!(t.observe(&at(2000.0), &monitors, 5.0).unwrap()["dpi"], 144);
    }

    #[test]
    fn maximized_keeps_the_normal_rect_minimized_saves_nothing_and_flush_sends_at_once() {
        let monitors = [primary()];
        let mut t = WindowTracker::new("main");
        let normal = Observed::from_info(&info([100.0, 100.0, 1000.0, 700.0], 1.0), 1.0).unwrap();
        t.restore(None, &normal, &monitors, Vec2::ZERO, 0.0);
        t.observe(&normal, &monitors, 0.0);
        assert!(t.observe(&normal, &monitors, 1.0).is_some());
        let mut maxed = Observed::from_info(&info([-8.0, -8.0, 1936.0, 1048.0], 1.0), 1.0).unwrap();
        maxed.maximized = true;
        t.observe(&maxed, &monitors, 2.0);
        let p = t.flush().unwrap();
        assert_eq!((p["left"].as_i64(), p["width"].as_i64(), p["maximized"].as_bool()), (Some(100), Some(1000), Some(true)));
        assert!(t.flush().is_none());
        let mut mini = normal;
        mini.minimized = true;
        mini.outer = Some(PixelRect::new(-32000, -32000, 160, 28));
        assert!(t.observe(&mini, &monitors, 3.0).is_none());
        assert!(!t.pending(), "a minimized window's rect is never taken");
    }

    #[test]
    fn a_window_restored_maximized_saves_its_saved_normal_rect() {
        let mut t = WindowTracker::new("main");
        let mut maxed = Observed::from_info(&info([-8.0, -8.0, 1936.0, 1048.0], 1.0), 1.0).unwrap();
        maxed.maximized = true;
        t.restore(Some(slot(200, 150, 1200, 800, 96, true)), &maxed, &[primary()], Vec2::ZERO, 0.0);
        assert!(t.observe(&maxed, &[primary()], 0.0).is_none());
        assert!(t.observe(&maxed, &[primary()], 5.0).is_none(), "unchanged: nothing to save");
    }

    #[test]
    fn the_default_rect_seen_before_the_restore_lands_is_not_saved() {
        // As a real window opens: normal at its default rect on the first frame (the restore
        // commands are sent then), maximized from the next frame on.
        let monitors = [primary()];
        let mut t = WindowTracker::new("main");
        let default = Observed::from_info(&info([100.0, 100.0, 1296.0, 839.0], 1.0), 1.0).unwrap();
        let cmds = t.restore(Some(slot(200, 150, 1200, 800, 96, true)), &default, &monitors, Vec2::ZERO, 10.0);
        assert_eq!(cmds.last(), Some(&ViewportCommand::Maximized(true)));
        assert!(t.observe(&default, &monitors, 10.0).is_none());
        let mut maxed = Observed::from_info(&info([-8.0, -8.0, 1936.0, 1048.0], 1.0), 1.0).unwrap();
        maxed.maximized = true;
        for time in [10.02, 10.3, 11.0, 12.0] {
            assert!(t.observe(&maxed, &monitors, time).is_none(), "nothing changed at {time}");
        }
        assert!(t.flush().is_none(), "the saved normal rect is kept, not the default one");
        // Un-maximized by the user: back at the saved normal rect, which is what is kept.
        let back = Observed::from_info(&info([200.0, 150.0, 1200.0, 800.0], 1.0), 1.0).unwrap();
        t.observe(&back, &monitors, 13.0);
        assert_eq!(t.observe(&back, &monitors, 14.0).unwrap()["left"], 200);
    }

    #[test]
    fn placement_falls_back_to_the_windows_own_scale_without_monitors() {
        let p = placement_of(&PixelRect::new(10, 20, 300, 200), 1.25, &[]).unwrap();
        assert_eq!((p.left, p.top, p.width, p.height, p.dpi), (10, 20, 300, 200, 120));
        assert!(placement_of(&PixelRect::new(i64::MAX, 0, 300, 200), 1.0, &[]).is_none());
    }
}
