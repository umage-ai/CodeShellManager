//! One title bar: the frameless main window's minimize, maximize/restore and close (Windows),
//! and what closing the window does (`CloseAction`: ask, keep running in the tray, quit). Through
//! the real UI on the static demo backend and a real engine.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_core::models::CloseAction;
use csm_ui::Action;
use csm_ui::app::Overlay;
use csm_ui::theme::{ThemeKind, size, space};
use egui::{Key, ViewportCommand, ViewportId};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

type H = Harness<'static, csm_ui::CsmApp>;

fn mk(window_controls: bool, tray: bool) -> H {
    let mut app = app_with(backend(ThemeKind::Mocha));
    app.window_controls = window_controls;
    app.background.tray_available = tray;
    let mut h = harness(app);
    settle(&mut h);
    h
}

fn calls(h: &H, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

/// The viewport commands of the last frame.
fn commands(h: &H) -> Vec<ViewportCommand> {
    h.output().viewport_output.get(&ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
}

/// What eframe does after a close (`ViewportCommand::Close`, the OS ✕, Alt+F4): a close request
/// the next frame sees in its viewport info.
fn deliver_close(h: &mut H) {
    h.input_mut().viewports.entry(ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    h.step();
}

fn set(h: &mut H, key: &str, value: serde_json::Value) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("settings_set", json!({"key": key, "value": value})));
    settle(h);
}

fn saved(h: &H, key: &str) -> serde_json::Value {
    h.state().dispatch.engine().call(csm_engine::Caller::Ui, "settings_get", json!({"key": key})).unwrap().data["value"].clone()
}

// ── The window's own buttons ────────────────────────────────────────────────────────────────

/// The title bar's own button with this label (pane headers have a Minimize and a Maximize too).
fn caption<'a>(h: &'a H, label: &'a str) -> Option<egui_kittest::Node<'a>> {
    h.query_all_by_label(label).find(|n| n.rect().max.y <= size::TITLE_BAR + 0.5)
}

fn click_caption(h: &mut H, label: &str) {
    caption(h, label).unwrap_or_else(|| panic!("no {label} in the title bar")).click();
    h.step();
}

#[test]
fn the_title_bar_has_minimize_maximize_and_close_only_when_frameless() {
    let h0 = mk(false, true);
    for label in ["Minimize", "Maximize", "Restore", "Close"] {
        assert!(caption(&h0, label).is_none(), "{label} with the OS frame");
    }
    let h1 = mk(true, true);
    for label in ["Minimize", "Maximize", "Close"] {
        assert!(caption(&h1, label).is_some(), "{label}");
    }
    assert!(caption(&h1, "Restore").is_none(), "not maximized: Maximize, not Restore");
    // Set apart from the bar's own buttons, which are all still there.
    for label in ["Quit: sessions get time to exit", "Settings  Ctrl+,", "Recent sessions  Ctrl+Shift+E"] {
        assert!(h1.query_by_label(label).is_some(), "{label}");
    }
    let close = caption(&h1, "Close").unwrap().rect();
    let min = caption(&h1, "Minimize").unwrap().rect();
    let quit = h1.get_by_label("Quit: sessions get time to exit").rect();
    assert!(quit.max.x + space::MD <= min.min.x + 0.5, "a gap between the power button and the window's buttons");
    assert!((close.max.x - 1280.0).abs() < 0.5, "Close is at the far right: {close:?}");
    assert!((close.width() - size::CAPTION_BUTTON_W).abs() < 0.5);
    assert!(close.height() >= size::TITLE_BAR - size::HAIRLINE - 0.5, "the bar's full height: {close:?}");
}

#[test]
fn minimize_and_maximize_send_their_viewport_commands_and_restore_follows_the_state() {
    let mut h = mk(true, true);
    click_caption(&mut h, "Minimize");
    assert!(commands(&h).contains(&ViewportCommand::Minimized(true)), "{:?}", commands(&h));
    click_caption(&mut h, "Maximize");
    assert!(commands(&h).contains(&ViewportCommand::Maximized(true)), "{:?}", commands(&h));

    // Maximized (as the OS reports it): the button is Restore and restores.
    h.input_mut().viewports.entry(ViewportId::ROOT).or_default().maximized = Some(true);
    h.step();
    h.step();
    assert!(caption(&h, "Maximize").is_none());
    click_caption(&mut h, "Restore");
    assert!(commands(&h).contains(&ViewportCommand::Maximized(false)), "{:?}", commands(&h));
}

#[test]
fn close_is_the_os_close_request_and_follows_the_same_path() {
    let mut h = mk(true, true);
    click_caption(&mut h, "Close");
    assert!(commands(&h).contains(&ViewportCommand::Close), "{:?}", commands(&h));
    assert_eq!(h.state().ui.overlay, Overlay::None, "nothing until the request comes back");
    deliver_close(&mut h);
    settle(&mut h);
    // The request was cancelled (the app decides) and the default asks.
    assert_eq!(h.state().ui.overlay, Overlay::CloseAsk);
}

// ── When I close the window ─────────────────────────────────────────────────────────────────

#[test]
fn ask_shows_the_dialog_and_enter_keeps_running_in_the_tray() {
    let mut h = mk(true, true);
    assert_eq!(h.state().prefs.close_action, CloseAction::Ask, "Ask is the default");
    deliver_close(&mut h);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::CloseAsk);
    assert!(h.query_by_label("Keep CodeShellManager running?").is_some());
    assert!(h.query_by_label("Your sessions keep running in the tray. Quit stops them.").is_some());
    assert!(h.query_by_label("Keep running in tray").is_some());
    assert!(h.query_by_label("Quit").is_some());
    assert!(h.query_by_label("Remember my choice").is_some());
    assert!(!h.state().ui.close_remember, "unchecked by default");
    let sets = calls(&h, "settings_set").len();
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(h.state().background.hidden, "hidden to the tray");
    assert_eq!(h.state().ui.overlay, Overlay::None);
    assert!(calls(&h, "app_quit").is_empty());
    assert!(calls(&h, "settings_set")[sets..].iter().all(|c| c["key"] != "CloseAction"), "not remembered");
    assert_eq!(h.state().prefs.close_action, CloseAction::Ask);
}

#[test]
fn ask_then_quit_takes_the_usual_quit_path() {
    let mut h = mk(true, true);
    deliver_close(&mut h);
    settle(&mut h);
    h.get_by_label("Quit").click();
    settle(&mut h);
    assert!(!h.state().background.hidden);
    // The demo has busy sessions: the busy warning, as the power button and Ctrl+Q show it.
    assert_eq!(h.state().ui.overlay, Overlay::Busy);
    h.get_by_label("Close anyway").click();
    settle(&mut h);
    assert_eq!(calls(&h, "app_quit").len(), 1);
    assert!(h.state().model.shutdown.is_some(), "the shutdown board");
    assert_eq!(h.state().prefs.close_action, CloseAction::Ask, "not remembered");
}

#[test]
fn esc_cancels_and_the_window_stays() {
    let mut h = mk(true, true);
    deliver_close(&mut h);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::CloseAsk);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None);
    assert!(!h.state().background.hidden);
    assert!(calls(&h, "app_quit").is_empty());
}

#[test]
fn remember_my_choice_saves_tray_and_the_next_close_does_not_ask() {
    let mut h = mk(true, true);
    deliver_close(&mut h);
    settle(&mut h);
    h.get_by_label("Remember my choice").click();
    h.step();
    h.get_by_label("Keep running in tray").click();
    settle(&mut h);
    assert!(h.state().background.hidden);
    assert!(calls(&h, "settings_set").contains(&json!({"key": "CloseAction", "value": "Tray"})));
    assert_eq!(h.state().prefs.close_action, CloseAction::Tray);
    assert_eq!(saved(&h, "CloseAction"), 1, "persisted");

    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("window_show", json!({})));
    settle(&mut h);
    deliver_close(&mut h);
    settle(&mut h);
    assert!(h.state().background.hidden, "hidden again, without asking");
    assert_ne!(h.state().ui.overlay, Overlay::CloseAsk);
}

#[test]
fn remember_my_choice_saves_quit_and_the_next_close_quits() {
    let mut h = mk(true, true);
    deliver_close(&mut h);
    settle(&mut h);
    h.get_by_label("Remember my choice").click();
    h.step();
    h.get_by_label("Quit").click();
    settle(&mut h);
    assert_eq!(h.state().prefs.close_action, CloseAction::Quit);
    assert_eq!(saved(&h, "CloseAction"), 2);
    assert_eq!(h.state().ui.overlay, Overlay::Busy);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::CloseOverlays);
    deliver_close(&mut h);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::Busy, "straight to the quit path");
}

#[test]
fn tray_hides_without_asking_and_says_so_once_ever() {
    let mut h = mk(true, true);
    set(&mut h, "CloseAction", json!("Tray"));
    deliver_close(&mut h);
    settle(&mut h);
    assert!(h.state().background.hidden);
    let said = h.state_mut().take_notifications();
    assert_eq!(said.len(), 1);
    assert_eq!(format!("{}. {}", said[0].title, said[0].body), "CodeShellManager is still running. Click the tray icon to open it.");
    assert_eq!(saved(&h, "TrayNoticeShown"), true, "persisted");

    // Another run that already said it: nothing.
    let mut h2 = mk(true, true);
    set(&mut h2, "TrayNoticeShown", json!(true));
    set(&mut h2, "CloseAction", json!("Tray"));
    deliver_close(&mut h2);
    settle(&mut h2);
    assert!(h2.state().background.hidden);
    assert!(h2.state_mut().take_notifications().is_empty());
}

#[test]
fn quit_mode_closes_as_the_window_did_before_the_setting() {
    let mut h = mk(true, true);
    set(&mut h, "CloseAction", json!("Quit"));
    deliver_close(&mut h);
    settle(&mut h);
    assert!(!h.state().background.hidden);
    assert_eq!(h.state().ui.overlay, Overlay::Busy);
}

#[test]
fn without_a_tray_close_quits_whatever_the_setting_and_settings_hide_it() {
    for action in ["Ask", "Tray"] {
        let mut h = mk(true, false);
        set(&mut h, "CloseAction", json!(action));
        deliver_close(&mut h);
        settle(&mut h);
        assert!(!h.state().background.hidden, "{action}");
        assert_eq!(h.state().ui.overlay, Overlay::Busy, "{action}: the quit path");
    }
    let mut h = mk(true, false);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(&mut h);
    assert!(h.query_by_label_contains("When I close the window").is_none(), "no tray: no setting");
    let mut h = mk(true, true);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(&mut h);
    assert!(h.query_by_label("When I close the window: Ask every time").is_some());
}
