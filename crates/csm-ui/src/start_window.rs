//! Starting minimized, or hidden in the tray (`StartMinimized`, `StartMinimizedToTray`).
//!
//! The setting applies to every start, a start at sign-in (`--autostart`) included; `--autostart`
//! alone does not minimize. Sessions restore as usual either way: that is the engine's work and
//! does not wait for the window.
//!
//! eframe has no "create minimized": it creates the window hidden and shows it after the first
//! frame is painted, then runs the viewport commands that frame sent. So the window is minimized
//! or hidden by commands from its first frame, with two things keeping that from showing:
//!
//! - The window is created inactive (`with_active(false)`): it never takes the keyboard from
//!   whatever the person is doing at sign-in.
//! - Hidden in the tray (Windows): it is created far off-screen, hidden first, and only then moved
//!   to its saved bounds (or centred on the primary monitor when there are none), so nothing is
//!   ever drawn where the person can see it. A maximize is held back until it is shown, because
//!   maximizing a hidden window shows it.
//! - Minimized: the saved bounds are restored first and the minimize comes last, so restoring
//!   from the taskbar returns to them (and to maximized, if it was).
//!
//! Showing the window again is the usual `window_show` (the tray, a notification, another launch
//! of the app through the single-instance activation, `csm` over IPC).

use csm_core::models::{WindowPlacementState, WindowSlot};
use csm_core::window_placement::MonitorArea;
use egui::{Vec2, ViewportBuilder, ViewportCommand, pos2};

/// How the main window opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StartWindow {
    /// Shown and focused, as always.
    #[default]
    Normal,
    /// Minimized to the taskbar (dock).
    Minimized,
    /// Hidden; the tray icon shows it.
    Tray,
}

/// Where a window created off-screen is (logical points; far from any monitor, within the 16-bit
/// range Windows' move messages carry even at 300 %).
pub const OFFSCREEN: egui::Pos2 = pos2(-10_000.0, -10_000.0);

impl StartWindow {
    /// From the settings: the tray only where there can be one (Windows, macOS).
    pub fn choose(start_minimized: bool, to_tray: bool, tray_possible: bool) -> Self {
        match (start_minimized, to_tray && tray_possible) {
            (false, _) => StartWindow::Normal,
            (true, true) => StartWindow::Tray,
            (true, false) => StartWindow::Minimized,
        }
    }

    /// The tray icon could not be made: minimized instead, so the window can still be found.
    pub fn without_tray(self) -> Self {
        if self == StartWindow::Tray { StartWindow::Minimized } else { self }
    }

    /// Whether the window should be created off-screen (hidden in the tray, on Windows, where
    /// placing it there is reliable; macOS would move it back onto a screen).
    pub fn offscreen(self) -> bool {
        self == StartWindow::Tray && cfg!(windows)
    }

    /// The main window's builder for this start.
    pub fn viewport(self, builder: ViewportBuilder, offscreen: bool) -> ViewportBuilder {
        match self {
            StartWindow::Normal => builder,
            StartWindow::Minimized | StartWindow::Tray => {
                let b = builder.with_active(false);
                if offscreen { b.with_position(OFFSCREEN) } else { b }
            }
        }
    }
}

/// The first frame's part of a minimized or hidden start, and a maximize held back until shown.
#[derive(Debug, Clone, Default)]
pub struct StartUp {
    mode: StartWindow,
    /// The window was created at [`OFFSCREEN`] and must be moved even with nothing saved.
    offscreen: bool,
    /// The first frame's commands were sent.
    done: bool,
    maximize_later: bool,
}

impl StartUp {
    pub fn new(mode: StartWindow, offscreen: bool) -> Self {
        StartUp { mode, offscreen, done: false, maximize_later: false }
    }

    pub fn mode(&self) -> StartWindow {
        self.mode
    }

    /// Whether this start hides the window (the host marks it hidden, so its work keeps running).
    pub fn starts_hidden(&self) -> bool {
        self.mode == StartWindow::Tray
    }

    /// The saved bounds the main window's tracker should restore on the first frame: unchanged
    /// for a normal start; a hidden one holds a maximize back, and an off-screen window gets a
    /// centred default when nothing was saved.
    pub fn saved_for_restore(&mut self, saved: Option<WindowSlot>, monitors: &[MonitorArea], size_dip: Vec2) -> Option<WindowSlot> {
        if self.done || self.mode == StartWindow::Normal {
            return saved;
        }
        let mut slot = match saved {
            Some(s) => s,
            None if self.offscreen => default_slot(monitors, size_dip),
            None => return None,
        };
        if self.mode == StartWindow::Tray && slot.maximized {
            slot.maximized = false;
            self.maximize_later = true;
        }
        Some(slot)
    }

    /// Commands that must land before the bounds are restored (hide before moving into view).
    pub fn before_restore(&self) -> Vec<ViewportCommand> {
        match (self.done, self.mode) {
            (false, StartWindow::Tray) => vec![ViewportCommand::Visible(false)],
            _ => Vec::new(),
        }
    }

    /// Commands that must land after the bounds are restored (minimize last, so the taskbar
    /// restores to them). Ends the first-frame part.
    pub fn after_restore(&mut self) -> Vec<ViewportCommand> {
        if std::mem::replace(&mut self.done, true) {
            return Vec::new();
        }
        match self.mode {
            StartWindow::Minimized => vec![ViewportCommand::Minimized(true)],
            StartWindow::Normal | StartWindow::Tray => Vec::new(),
        }
    }

    /// A later frame: once the window is shown (not hidden, not minimized), the maximize held
    /// back from a hidden start.
    pub fn when_shown(&mut self, hidden: bool, minimized: bool) -> Option<ViewportCommand> {
        (self.done && self.maximize_later && !hidden && !minimized).then(|| {
            self.maximize_later = false;
            ViewportCommand::Maximized(true)
        })
    }
}

/// `size_dip` centred in the primary monitor's work area (physical pixels at its DPI), or at
/// (100, 100) at 96 DPI when the monitors are unknown.
pub fn default_slot(monitors: &[MonitorArea], size_dip: Vec2) -> WindowSlot {
    let primary = monitors.iter().find(|m| m.is_primary).or_else(|| monitors.first());
    let clamp = |v: i64| i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX });
    let placement = match primary {
        Some(m) => {
            let dpi = m.dpi.max(1);
            let w = ((f64::from(size_dip.x) * dpi as f64 / 96.0).round() as i64).min(m.work.width).max(1);
            let h = ((f64::from(size_dip.y) * dpi as f64 / 96.0).round() as i64).min(m.work.height).max(1);
            WindowPlacementState {
                left: clamp(m.work.left + (m.work.width - w) / 2),
                top: clamp(m.work.top + (m.work.height - h) / 2),
                width: clamp(w),
                height: clamp(h),
                dpi: clamp(dpi),
            }
        }
        None => WindowPlacementState { left: 100, top: 100, width: size_dip.x.round() as i32, height: size_dip.y.round() as i32, dpi: 96 },
    };
    WindowSlot { placement, maximized: false }
}

impl crate::app::CsmApp {
    /// The main window's placement for this frame (restore on the first, then the debounced save),
    /// wrapped in the first frame's part of a minimized or hidden start. Frame code: commands only.
    pub(crate) fn main_window_step(&mut self, ctx: &egui::Context, min: Vec2) {
        let monitors = crate::terminal::lock(&self.monitors).clone();
        let size = Vec2::new(crate::theme::size::WINDOW[0], crate::theme::size::WINDOW[1]);
        let saved = self.start.saved_for_restore(self.model.windows.get("main").copied(), &monitors, size);
        let before = self.start.before_restore();
        if !before.is_empty() && self.start.starts_hidden() {
            // Hidden from the first frame: the work that goes on while hidden keeps running.
            self.background.hidden = true;
        }
        for c in before {
            ctx.send_viewport_cmd(c);
        }
        if let Some(p) = self.main_window.step(ctx, saved, &self.monitors, min) {
            self.dispatch.submit("window_placement_set", p);
        }
        for c in self.start.after_restore() {
            ctx.send_viewport_cmd(c);
        }
        let minimized = ctx.input(|i| i.viewport().minimized == Some(true));
        if let Some(c) = self.start.when_shown(self.background.hidden, minimized) {
            ctx.send_viewport_cmd(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_core::window_placement::PixelRect;

    fn slot(maximized: bool) -> WindowSlot {
        WindowSlot { placement: WindowPlacementState { left: 10, top: 20, width: 800, height: 600, dpi: 96 }, maximized }
    }

    #[test]
    fn the_settings_choose_the_start_and_the_tray_only_where_there_is_one() {
        assert_eq!(StartWindow::choose(false, true, true), StartWindow::Normal);
        assert_eq!(StartWindow::choose(true, false, true), StartWindow::Minimized);
        assert_eq!(StartWindow::choose(true, true, true), StartWindow::Tray);
        assert_eq!(StartWindow::choose(true, true, false), StartWindow::Minimized, "Linux: no tray");
        assert_eq!(StartWindow::Tray.without_tray(), StartWindow::Minimized);
        assert_eq!(StartWindow::Normal.without_tray(), StartWindow::Normal);
    }

    #[test]
    fn a_minimized_start_creates_the_window_inactive_and_minimizes_it_after_the_restore() {
        let b = StartWindow::Minimized.viewport(ViewportBuilder::default(), false);
        assert_eq!(b.active, Some(false));
        assert_eq!(b.position, None);
        let mut s = StartUp::new(StartWindow::Minimized, false);
        // The saved bounds go through, maximized included (restoring from the taskbar returns to them).
        assert_eq!(s.saved_for_restore(Some(slot(true)), &[], Vec2::new(1280.0, 800.0)), Some(slot(true)));
        assert!(s.before_restore().is_empty());
        assert_eq!(s.after_restore(), vec![ViewportCommand::Minimized(true)]);
        assert!(s.after_restore().is_empty(), "once");
        assert_eq!(s.when_shown(false, false), None);
    }

    #[test]
    fn a_tray_start_hides_first_and_holds_a_maximize_until_shown() {
        let b = StartWindow::Tray.viewport(ViewportBuilder::default(), true);
        assert_eq!((b.active, b.position), (Some(false), Some(OFFSCREEN)));
        let mut s = StartUp::new(StartWindow::Tray, true);
        assert!(s.starts_hidden());
        assert_eq!(s.saved_for_restore(Some(slot(true)), &[], Vec2::new(1280.0, 800.0)), Some(slot(false)));
        assert_eq!(s.before_restore(), vec![ViewportCommand::Visible(false)]);
        assert!(s.after_restore().is_empty());
        assert_eq!(s.when_shown(true, false), None, "still in the tray");
        assert_eq!(s.when_shown(false, false), Some(ViewportCommand::Maximized(true)));
        assert_eq!(s.when_shown(false, false), None, "once");
    }

    #[test]
    fn an_offscreen_window_with_nothing_saved_is_centred_on_the_primary_monitor() {
        let mut s = StartUp::new(StartWindow::Tray, true);
        let m = |left, primary, dpi| MonitorArea {
            bounds: PixelRect::new(left, 0, 1920, 1080),
            work: PixelRect::new(left, 0, 1920, 1040),
            dpi,
            is_primary: primary,
        };
        let got = s.saved_for_restore(None, &[m(-1920, false, 96), m(0, true, 144)], Vec2::new(1280.0, 800.0)).unwrap();
        assert_eq!(got.placement, WindowPlacementState { left: 0, top: 0, width: 1920, height: 1040, dpi: 144 }, "clamped to the work area");
        let got = default_slot(&[m(0, true, 96)], Vec2::new(1280.0, 800.0));
        assert_eq!(got.placement, WindowPlacementState { left: 320, top: 120, width: 1280, height: 800, dpi: 96 });
        assert_eq!(default_slot(&[], Vec2::new(1280.0, 800.0)).placement.left, 100);
        // Not off-screen and nothing saved: the OS's own default position stands.
        assert_eq!(StartUp::new(StartWindow::Minimized, false).saved_for_restore(None, &[], Vec2::new(1.0, 1.0)), None);
    }

    #[test]
    fn a_normal_start_changes_nothing() {
        let b = StartWindow::Normal.viewport(ViewportBuilder::default(), false);
        assert_eq!((b.active, b.position), (None, None));
        let mut s = StartUp::default();
        assert_eq!(s.saved_for_restore(Some(slot(true)), &[], Vec2::ZERO), Some(slot(true)));
        assert!(s.before_restore().is_empty() && s.after_restore().is_empty());
    }
}
