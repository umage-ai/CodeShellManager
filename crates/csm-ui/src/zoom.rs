//! The UI zoom (#193): Settings › Appearance "Interface size", Ctrl+= / Ctrl+- / Ctrl+0, and
//! `settings_set { key: "UiZoomPercent" }` from anywhere. Applied with egui's zoom factor, so every
//! window, font, icon and spacing token scales together; the terminal's cells do not (they follow
//! the terminal font size, see `TerminalView::show`).
//!
//! egui's own keyboard zoom (Cmd+Plus/Minus/0) is switched off in [`crate::app::setup`]: it ran
//! after the frame, unpersisted, and its Ctrl+Shift+- would also have taken Ctrl+_ (the
//! terminal's ^_ undo) away from the focused pane.

use csm_core::models::{UI_ZOOM_DEFAULT, UI_ZOOM_RANGE};
use egui::{Key, Modifiers};
use serde_json::json;

use crate::app::{Action, CsmApp};

/// The steps Ctrl+= and Ctrl+- move between, as browsers do.
pub const STEPS: [i32; 8] = [80, 90, 100, 110, 125, 150, 175, 200];

/// A stored percentage made usable: out of range or nonsense is the default.
pub fn sanitize(percent: f64) -> i32 {
    if !percent.is_finite() {
        return UI_ZOOM_DEFAULT;
    }
    let p = percent.round() as i64;
    i32::try_from(p).ok().filter(|p| UI_ZOOM_RANGE.contains(p)).unwrap_or(UI_ZOOM_DEFAULT)
}

/// The next step up from `current` (the largest stays).
pub fn step_in(current: i32) -> i32 {
    STEPS.iter().copied().find(|s| *s > current).unwrap_or(*UI_ZOOM_RANGE.end())
}

/// The next step down from `current` (the smallest stays).
pub fn step_out(current: i32) -> i32 {
    STEPS.iter().rev().copied().find(|s| *s < current).unwrap_or(*UI_ZOOM_RANGE.start())
}

/// egui's zoom factor for a percentage.
pub fn factor(percent: i32) -> f32 {
    sanitize(f64::from(percent)) as f32 / 100.0
}

/// Sets egui's zoom factor when the setting changed. Cheap when it didn't.
pub fn apply(ctx: &egui::Context, percent: i32) {
    let want = factor(percent);
    if (ctx.zoom_factor() - want).abs() > f32::EPSILON {
        ctx.set_zoom_factor(want);
    }
}

/// Ctrl+= (or Ctrl+Plus, where `+` has its own key), Ctrl+- and Ctrl+0. Ctrl+- is taken only
/// without Shift, so Ctrl+_ still reaches the terminal. Each press is a `settings_set` call; the
/// preference moves at once, so a second press before the reply steps on from the first.
pub(crate) fn shortcuts(app: &mut CsmApp, ctx: &egui::Context) {
    let cmd = Modifiers::COMMAND;
    let (zoom_in, zoom_out, reset) = ctx.input_mut(|i| {
        let zin = crate::app::consume_exact(i, cmd, Key::Equals) || crate::app::consume_exact(i, cmd, Key::Plus);
        let zout = consume_without_shift(i, Key::Minus);
        let reset = crate::app::consume_exact(i, cmd, Key::Num0);
        (zin, zout, reset)
    });
    let cur = app.prefs.ui_zoom_percent;
    let next = if reset {
        UI_ZOOM_DEFAULT
    } else if zoom_in {
        step_in(cur)
    } else if zoom_out {
        step_out(cur)
    } else {
        return;
    };
    app.prefs.ui_zoom_percent = next;
    // Always a call, even at the end of the range or already at 100%: the shortcut visibly does
    // something (the toast-free reply), and the setting is written once more.
    app.run(ctx, Action::Tool("settings_set", json!({"key": "UiZoomPercent", "value": next})));
}

/// Command and the key, with neither Shift nor Alt.
fn consume_without_shift(i: &mut egui::InputState, key: Key) -> bool {
    let mut hit = false;
    i.events.retain(|e| {
        let take = matches!(e, egui::Event::Key { key: k, pressed: true, modifiers, .. }
            if *k == key && modifiers.command && !modifiers.shift && !modifiers.alt);
        hit |= take;
        !take
    });
    hit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_walk_the_range_and_stop_at_its_ends() {
        assert_eq!(step_in(100), 110);
        assert_eq!(step_in(112), 125, "between steps: the next one");
        assert_eq!(step_in(200), 200);
        assert_eq!(step_out(100), 90);
        assert_eq!(step_out(80), 80);
        assert_eq!(step_out(112), 110);
    }

    #[test]
    fn nonsense_is_the_default() {
        assert_eq!(sanitize(125.0), 125);
        assert_eq!(sanitize(5.0), UI_ZOOM_DEFAULT);
        assert_eq!(sanitize(f64::NAN), UI_ZOOM_DEFAULT);
        assert_eq!(sanitize(1e12), UI_ZOOM_DEFAULT);
        assert!((factor(150) - 1.5).abs() < f32::EPSILON);
    }
}
