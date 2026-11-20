//! Beta.1 small wins through the real UI on the static demo backend and a real engine: the UI
//! zoom (#193), close to tray (#149), "Send to all sessions…" (#59) and the Claude --name
//! setting (#76).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::app::{Menu, Overlay, SettingsSection};
use csm_ui::theme::ThemeKind;
use egui::{Key, Modifiers};
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

fn zoom(h: &Harness<'static, csm_ui::CsmApp>) -> f32 {
    h.ctx.zoom_factor()
}

// ── #193 UI zoom ────────────────────────────────────────────────────────────────────────────

#[test]
fn ctrl_equals_minus_and_zero_step_the_interface_size_through_settings_set() {
    let mut h = h();
    assert!((zoom(&h) - 1.0).abs() < 1e-6);
    h.key_press_modifiers(Modifiers::COMMAND, Key::Equals);
    settle(&mut h);
    assert_eq!(calls(&h, "settings_set").last().unwrap(), &json!({"key": "UiZoomPercent", "value": 110}));
    assert!((zoom(&h) - 1.1).abs() < 1e-6, "{}", zoom(&h));
    // A second press steps on from the first.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Equals);
    settle(&mut h);
    assert!((zoom(&h) - 1.25).abs() < 1e-6, "{}", zoom(&h));
    for _ in 0..4 {
        h.key_press_modifiers(Modifiers::COMMAND, Key::Minus);
        settle(&mut h);
    }
    assert!((zoom(&h) - 0.8).abs() < 1e-6, "stops at 80%: {}", zoom(&h));
    h.key_press_modifiers(Modifiers::COMMAND, Key::Num0);
    settle(&mut h);
    assert!((zoom(&h) - 1.0).abs() < 1e-6);
    assert_eq!(h.state().prefs.ui_zoom_percent, 100);
    // The engine has it (persisted with the settings).
    let saved = h.state().dispatch.engine().call(csm_engine::Caller::Ui, "settings_get", json!({"key": "UiZoomPercent"})).unwrap();
    assert_eq!(saved.data["value"], 100);
}

#[test]
fn ctrl_shift_minus_is_left_to_the_terminal() {
    // Ctrl+_ is ^_ (undo in readline and editors): only Ctrl+- without Shift zooms.
    let mut h = h();
    let before = calls(&h, "settings_set").len();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::Minus);
    settle(&mut h);
    assert_eq!(calls(&h, "settings_set").len(), before);
    assert!((zoom(&h) - 1.0).abs() < 1e-6);
}

#[test]
fn a_zoom_set_by_the_cli_or_an_mcp_client_applies_live_and_the_settings_page_shows_it() {
    let mut h = h();
    // Not through the UI's dispatcher: as `csm settings-set` would (an external caller).
    let ext = csm_engine::Caller::External { client: "csm".into(), session_id: None };
    h.state().dispatch.engine().call(ext, "settings_set", json!({"key": "UiZoomPercent", "value": 150})).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while (zoom(&h) - 1.5).abs() > 1e-6 {
        assert!(std::time::Instant::now() < deadline, "the zoom never followed: {}", zoom(&h));
        h.step();
    }
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    h.state_mut().ui.settings.section = SettingsSection::Appearance;
    settle(&mut h);
    assert!(h.query_all_by_label("Interface size").next().is_some());
    assert_eq!(h.state().ui.settings.num("UiZoomPercent"), Some(150.0));
}

// ── #149 close to tray ──────────────────────────────────────────────────────────────────────

fn request_close(h: &mut Harness<'static, csm_ui::CsmApp>) {
    let ctx = h.ctx.clone();
    // What eframe does for the window's ✕: a close request the frame sees in its viewport info.
    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
    h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    h.step();
}

fn set(h: &mut Harness<'static, csm_ui::CsmApp>, key: &str, value: serde_json::Value) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("settings_set", json!({"key": key, "value": value})));
    settle(h);
}

#[test]
fn closing_quits_by_default_and_hides_to_the_tray_with_the_setting() {
    let mut h = h();
    h.state_mut().background.tray_available = true;
    // Off (the default): closing asks to quit (the demo has busy sessions: the busy warning).
    request_close(&mut h);
    settle(&mut h);
    assert!(!h.state().background.hidden);
    assert_eq!(h.state().ui.overlay, Overlay::Busy);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::CloseOverlays);

    set(&mut h, "CloseToTray", json!(true));
    assert!(h.state().prefs.close_to_tray);
    let quits = calls(&h, "app_quit").len();
    request_close(&mut h);
    settle(&mut h);
    assert!(h.state().background.hidden, "hidden to the tray");
    assert_ne!(h.state().ui.overlay, Overlay::Busy, "no quit question");
    assert_eq!(calls(&h, "app_quit").len(), quits);
    // Once per run, a notification says where the app went.
    let said = h.state_mut().take_notifications();
    assert_eq!(said.len(), 1);
    assert!(said[0].title.contains("still running"), "{:?}", said[0].title);
    assert!(said[0].session_id.is_empty());

    // The tray's Show window (`window_show`) brings it back.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("window_show", json!({})));
    settle(&mut h);
    assert!(!h.state().background.hidden);
    // Hiding again says nothing new.
    request_close(&mut h);
    settle(&mut h);
    assert!(h.state().background.hidden);
    assert!(h.state_mut().take_notifications().is_empty());
}

#[test]
fn without_a_tray_closing_always_quits() {
    let mut h = h();
    set(&mut h, "CloseToTray", json!(true));
    // `tray_available` is false: Linux, or the OS refused the icon.
    request_close(&mut h);
    settle(&mut h);
    assert!(!h.state().background.hidden);
    assert_eq!(h.state().ui.overlay, Overlay::Busy);
}

#[test]
fn hidden_the_logic_keeps_polling_and_a_close_request_quits_without_asking() {
    let mut h = h();
    h.state_mut().background.tray_available = true;
    set(&mut h, "CloseToTray", json!(true));
    request_close(&mut h);
    settle(&mut h);
    assert!(h.state().background.hidden);
    // Hidden, eframe calls only `logic` (no UI pass). Replies still arrive through it.
    let ctx = h.ctx.clone();
    h.state_mut().dispatch.submit("app_info", json!({}));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while h.state().dispatch.busy() {
        assert!(std::time::Instant::now() < deadline, "logic never polled the reply");
        h.state_mut().logic(&ctx);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    h.step();
    // A close that arrives while hidden (logoff) is a real quit, with no dialog nobody would see.
    let quits = calls(&h, "app_quit").len();
    request_close(&mut h);
    assert_eq!(calls(&h, "app_quit").len(), quits + 1);
    assert_ne!(h.state().ui.overlay, Overlay::Busy);
}

// ── #59 Send to all sessions… ───────────────────────────────────────────────────────────────

#[test]
fn the_group_menu_sends_text_to_every_session_after_a_confirm_with_the_count() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(Menu::Group { group: "odulodo".into(), at: egui::pos2(56.0, 90.0) }));
    settle(&mut h);
    h.get_by_label("Send to all sessions…").click();
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::Prompt);
    assert!(h.query_by_label_contains("Send to all sessions in Odulodo").is_some());
    h.get_by_role(egui::accesskit::Role::TextInput).type_text("git pull");
    settle(&mut h);
    h.key_press(Key::Enter);
    settle(&mut h);
    // A confirm with the count, nothing sent yet.
    assert_eq!(h.state().ui.overlay, Overlay::Confirm);
    let (ready, _) = csm_ui::screens::broadcast::counts(&h.state().model, "odulodo");
    assert!(ready > 0);
    let title = h.state().ui.confirm.as_ref().unwrap().title.clone();
    assert_eq!(title, format!("Send to {ready} sessions?"));
    assert!(calls(&h, "session_send_text_many").is_empty());
    h.get_by_label(&format!("Send to {ready}")).click();
    settle(&mut h);
    assert_eq!(calls(&h, "session_send_text_many"), [json!({"group_id": "odulodo", "text": "git pull", "submit": true})]);
}

#[test]
fn the_broadcast_count_leaves_out_sleeping_starting_and_working_sessions() {
    use csm_ui::model::{GroupView, SessionView, Status, UiModel};
    let s = |id: &str, status| SessionView { id: id.into(), group: Some("g".into()), status, ..SessionView::default() };
    let model = UiModel {
        groups: vec![GroupView { id: "g".into(), name: "G".into(), torn_off: false, collapsed: false }],
        sessions: vec![
            s("a", Status::Idle),
            s("b", Status::Question),
            s("c", Status::Working),
            s("d", Status::Asleep),
            s("e", Status::Starting),
            SessionView { id: "x".into(), group: None, ..SessionView::default() },
        ],
        ..UiModel::default()
    };
    assert_eq!(csm_ui::screens::broadcast::counts(&model, "g"), (2, 1));
    let Action::Confirm(c) = csm_ui::screens::broadcast::confirm(&model, "g", "npm test") else { panic!("a confirm") };
    assert_eq!(c.title, "Send to 2 sessions?");
    assert!(c.body.contains("1 working mid-turn will be skipped"), "{}", c.body);
    // Nothing could take it: say so instead of asking "Send to 0 sessions?".
    let none_ready = UiModel { sessions: vec![s("c", Status::Working), s("d", Status::Asleep)], ..model };
    let Action::Toast(t) = csm_ui::screens::broadcast::confirm(&none_ready, "g", "npm test") else { panic!("a toast") };
    assert!(t.contains("working mid-turn"), "{t}");
}

// ── #76 the --name setting ──────────────────────────────────────────────────────────────────

#[test]
fn settings_offers_naming_claude_conversations_and_close_to_tray() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(&mut h);
    let before = calls(&h, "settings_set").len();
    h.get_by_label_contains("Name Claude conversations after the session").click();
    settle(&mut h);
    assert_eq!(calls(&h, "settings_set")[before], json!({"key": "PassSessionNameToClaude", "value": true}));
    h.get_by_label_contains("Keep running in the tray when the window closes").click();
    settle(&mut h);
    assert_eq!(calls(&h, "settings_set")[before + 1], json!({"key": "CloseToTray", "value": true}));
}
