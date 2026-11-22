//! Settings → General → Start-up through the real UI on the demo backend and a real engine (whose
//! sign-in entry is in memory): the rows dispatch `app_autostart_set` and `settings_set`, the
//! switch shows what the engine read back, and the tray choice is offered only with a tray.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn calls(h: &Harness<'static, csm_ui::CsmApp>, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

fn open_settings(h: &mut Harness<'static, csm_ui::CsmApp>) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(h);
}

#[test]
fn start_at_sign_in_reads_the_os_entry_and_the_switch_calls_the_tool() {
    let mut h = h();
    open_settings(&mut h);
    assert_eq!(calls(&h, "app_autostart_get").len(), 1, "asked when Settings opens");
    assert_eq!(h.state().ui.settings.autostart["enabled"], false);
    h.get_by_label_contains("Start CodeShellManager when I sign in").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "app_autostart_set"), vec![json!({"enabled": true})]);
    // The switch shows what the engine read back after the change.
    assert_eq!(h.state().ui.settings.autostart["enabled"], true);
    h.get_by_label_contains("Start CodeShellManager when I sign in").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "app_autostart_set").last().unwrap(), &json!({"enabled": false}));
    assert_eq!(h.state().ui.settings.autostart["enabled"], false);
}

#[test]
fn a_change_by_another_caller_is_followed() {
    let mut h = h();
    open_settings(&mut h);
    let engine = h.state().dispatch.engine().clone();
    let cli = engine.call(csm_engine::Caller::Cli, "app_autostart_set", json!({"enabled": true})).unwrap();
    assert_eq!(cli.data["enabled"], true);
    settle(&mut h);
    assert_eq!(h.state().ui.settings.autostart["enabled"], true);
}

#[test]
fn start_minimized_and_the_tray_choice_are_settings() {
    let mut h = h();
    open_settings(&mut h);
    h.get_by_label_contains("Start minimized").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "settings_set").last().unwrap(), &json!({"key": "StartMinimized", "value": true}));
    // No tray (the demo, Linux): no tray choice.
    assert!(h.query_by_label_contains("Start in the tray").is_none());
    h.state_mut().background.tray_available = true;
    settle(&mut h);
    h.get_by_label_contains("Start in the tray").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "settings_set").last().unwrap(), &json!({"key": "StartMinimizedToTray", "value": true}));
    let saved = h.state().dispatch.engine().call(csm_engine::Caller::Ui, "settings_get", json!({"key": "StartMinimizedToTray"})).unwrap();
    assert_eq!(saved.data["value"], true);
}

/// The main window's first frames, run on a bare context (the harness runs several frames while it
/// is built, so its output no longer holds the first): the root viewport's commands per frame.
fn first_frames(app: &mut csm_ui::CsmApp, frames: usize) -> Vec<Vec<egui::ViewportCommand>> {
    let ctx = egui::Context::default();
    (0..frames)
        .map(|_| {
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| app.frame(ui));
            out.textures_delta.clear();
            out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
        })
        .collect()
}

#[test]
fn a_minimized_start_minimizes_the_main_window_on_its_first_frame() {
    use csm_ui::start_window::{StartUp, StartWindow};
    use egui::ViewportCommand as C;
    // What the host sets for `--autostart` (or any start) with Start minimized on.
    let mut app = app_with(backend(ThemeKind::Mocha));
    app.start = StartUp::new(StartWindow::Minimized, false);
    let frames = first_frames(&mut app, 3);
    assert!(frames[0].contains(&C::Minimized(true)), "{frames:?}");
    assert!(frames[1..].iter().all(|f| !f.contains(&C::Minimized(true))), "once: {frames:?}");

    // In the tray: hidden on the first frame, and marked hidden so its work keeps running.
    let mut app = app_with(backend(ThemeKind::Mocha));
    app.start = StartUp::new(StartWindow::Tray, true);
    let frames = first_frames(&mut app, 2);
    assert_eq!(frames[0].first(), Some(&C::Visible(false)), "hidden before anything moves: {frames:?}");
    assert!(app.background.hidden);

    // A normal start sends neither.
    let mut app = app_with(backend(ThemeKind::Mocha));
    let frames = first_frames(&mut app, 2);
    assert!(!frames.iter().flatten().any(|c| matches!(c, C::Minimized(true) | C::Visible(false))), "{frames:?}");
}
