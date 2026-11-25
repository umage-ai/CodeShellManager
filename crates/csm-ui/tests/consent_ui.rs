//! Consent cards and confirm dialogs: bounded, one card at a time, honest wording, input guards
//! (SEC-7, SEC-8, SEC-15, SEC-18), the terminal not typing behind a dialog (COR-1) and settings
//! fields that Escape does not save (COR-10).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::app::{Action, ConfirmState, Overlay};
use csm_ui::dialog_keys;
use csm_ui::screens::overlays::CONFIRM_ID;
use csm_ui::theme::ThemeKind;
use egui::{Id, Key};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::{Value, json};

type H = Harness<'static, csm_ui::CsmApp>;

fn fresh() -> H {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn card(id: u64, client: &str, params: Value) -> Value {
    json!({"request_id": id, "client": client, "title": "Send text to a session", "summary": "Types into a session", "tool": "session_send_text", "effects": [], "params": params})
}

fn has(h: &H, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

fn tool_ids(h: &H) -> Vec<String> {
    h.state().submitted().iter().map(|c| c.id.clone()).collect()
}

#[test]
fn a_huge_request_keeps_the_header_and_the_buttons_on_screen() {
    let mut h = fresh();
    let big: String = (0..400).map(|i| format!("line {i} of a very long parameter\n")).collect();
    h.state_mut().ui.consents = vec![card(1, "test-mcp", json!({"session_id": "odulodo", "text": big}))];
    settle(&mut h);
    let screen = h.ctx.content_rect();
    for label in ["Allow once", "Don't allow"] {
        let r = h.get_by_label(label).rect();
        assert!(screen.contains_rect(r), "{label} is on screen: {r:?} in {screen:?}");
    }
    let head = h.query_by_label_contains("wants to run").expect("the header names the client").rect();
    assert!(screen.contains_rect(head), "the header is on screen: {head:?}");
    // The card is no taller than a bounded box plus its fixed lines.
    let buttons = h.get_by_label("Allow once").rect();
    assert!(head.min.y > screen.min.y && buttons.max.y <= screen.max.y);
    assert!(buttons.min.y - head.min.y < 420.0, "the card does not grow with the parameters: {}", buttons.min.y - head.min.y);
}

#[test]
fn only_one_card_shows_at_a_time_and_says_how_many_wait() {
    let mut h = fresh();
    h.state_mut().ui.consents = (1..=3).map(|i| card(i, "test-mcp", json!({"n": i}))).collect();
    settle(&mut h);
    assert!(has(&h, "+2 waiting"));
    let buttons: Vec<_> = h.query_all_by_label("Allow once").collect();
    assert_eq!(buttons.len(), 1, "one card, not a stack");
    h.get_by_label("Allow once").click();
    settle(&mut h);
    let c = h.state().submitted().iter().find(|c| c.id == "consent_answer").cloned().expect("an answer");
    assert_eq!(c.params["request_id"], 1, "the oldest request is answered first");
}

#[test]
fn the_card_marks_invisible_characters_and_words_the_grant_honestly() {
    let mut h = fresh();
    h.state_mut().ui.consents = vec![card(1, "Trusted\u{202E}Client", json!({"session_id": "odulodo", "text": "x"}))];
    settle(&mut h);
    assert!(h.query_by_label_contains("U+202E").is_some(), "the override is shown as a marker, not obeyed");
    assert!(has(&h, "Always allow clients named “TrustedClient”"), "the grant says it is by name");
    assert!(h.query_by_label_contains("Always for this client").is_none());
    assert!(has(&h, "on session Odulodo"), "the target session is named");
}

#[test]
fn a_card_asked_for_text_shows_what_will_be_typed() {
    let mut h = fresh();
    let mut c = card(1, "test-mcp", json!({"task_id": "t1", "session_id": "odulodo"}));
    c["will_type"] = json!("Task t1 from the user: do the thing");
    h.state_mut().ui.consents = vec![c];
    settle(&mut h);
    assert!(h.query_by_label_contains("Task t1 from the user: do the thing").is_some());
}

#[test]
fn a_click_in_the_first_moments_of_a_card_does_not_allow_it() {
    let mut app = app_with(backend(ThemeKind::Mocha));
    app.ui.no_dialog_delay = false;
    let mut h = harness(app);
    settle(&mut h);
    h.state_mut().ui.consents = vec![card(1, "test-mcp", json!({}))];
    h.step();
    h.get_by_label("Allow once").click();
    h.step();
    assert!(!tool_ids(&h).iter().any(|t| t == "consent_answer"), "too early: nothing answered");
    h.get_by_label("Don't allow").click();
    h.step();
    assert!(tool_ids(&h).iter().any(|t| t == "consent_answer"), "declining never waits");
}

fn restart_dialog(h: &mut H, safe: bool) {
    h.state_mut().ui.confirm = Some(ConfirmState {
        safe_default: safe,
        title: "Enable Danger?".into(),
        body: (0..300).map(|i| format!("Launch profile {i} runs `thing --flag`\n")).collect(),
        yes_label: "Enable".into(),
        no_label: "Cancel".into(),
        yes: Box::new(Action::Tool("group_new", json!({"name": "yes"}))),
        also: None,
    });
    h.state_mut().ui.overlay = Overlay::Confirm;
    settle(h);
}

#[test]
fn a_long_confirm_body_scrolls_and_leaves_the_title_and_buttons_in_view() {
    let mut h = fresh();
    restart_dialog(&mut h, false);
    let screen = h.ctx.content_rect();
    for label in ["Enable", "Cancel", "Enable Danger?"] {
        let r = h.get_by_label(label).rect();
        assert!(screen.contains_rect(r), "{label} is on screen: {r:?}");
    }
}

#[test]
fn enter_declines_a_confirm_an_agent_or_plugin_caused() {
    let mut h = fresh();
    restart_dialog(&mut h, true);
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None);
    assert!(!tool_ids(&h).contains(&"group_new".to_string()), "Enter took the safe default: nothing ran");
}

#[test]
fn enter_just_after_a_dialog_opens_answers_nothing_and_a_held_key_never_repeats() {
    let mut app = app_with(backend(ThemeKind::Mocha));
    app.ui.no_dialog_delay = false;
    let mut h = harness(app);
    settle(&mut h);
    restart_dialog(&mut h, false);
    // Enter goes down and is held (no release), so egui marks the next press a repeat.
    h.event(egui::Event::Key { key: Key::Enter, physical_key: Some(Key::Enter), pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert_eq!(h.state().ui.overlay, Overlay::Confirm, "too early: still asking");
    assert!(!tool_ids(&h).contains(&"group_new".to_string()));
    // After the wait, a repeat of a held Enter is ignored; a fresh press answers.
    std::thread::sleep(std::time::Duration::from_millis(450));
    h.event(egui::Event::Key { key: Key::Enter, physical_key: Some(Key::Enter), pressed: true, repeat: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.event(egui::Event::Key { key: Key::Enter, physical_key: Some(Key::Enter), pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert_eq!(h.state().ui.overlay, Overlay::Confirm, "a repeat is not a press");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None);
    assert!(tool_ids(&h).contains(&"group_new".to_string()));
}

#[test]
fn a_question_that_arrives_while_a_dialog_is_open_waits_its_turn() {
    let mut h = fresh();
    restart_dialog(&mut h, false);
    // An async answer (the port card, a restart offer) must not replace the open dialog.
    let later = ConfirmState {
        safe_default: true,
        title: "Port 3000 is elsewhere".into(),
        body: "b".into(),
        yes_label: "Forward".into(),
        no_label: "Cancel".into(),
        yes: Box::new(Action::Tool("group_new", json!({"name": "later"}))),
        also: None,
    };
    h.state_mut().offer_confirm(later);
    settle(&mut h);
    assert_eq!(h.state().ui.confirm.clone().unwrap().title, "Enable Danger?", "the open dialog stays");
    assert_eq!(h.state().ui.confirm_queue.len(), 1);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::Confirm, "the queued question appears when the first closes");
    assert_eq!(h.state().ui.confirm.clone().unwrap().title, "Port 3000 is elsewhere");
    assert!(h.state().ui.confirm_queue.is_empty());
    let _ = Id::new(CONFIRM_ID);
}

#[test]
fn the_terminal_takes_no_keys_behind_an_open_dialog_and_the_busy_enter_is_the_dialogs_alone() {
    let mut h = fresh();
    let modal = |h: &H| h.ctx.data(|d| d.get_temp::<bool>(csm_ui::terminal::view::modal_id())).unwrap_or(false);
    assert!(!modal(&h), "no dialog: the terminal may type");
    restart_dialog(&mut h, false);
    assert!(modal(&h), "a confirm is open: the terminal is not focused");
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!modal(&h), "closed again");
    // Busy and Help go through the dialog keys like the rest.
    for overlay in [Overlay::Busy, Overlay::Help] {
        h.state_mut().ui.overlay = overlay;
        settle(&mut h);
        assert!(modal(&h), "{overlay:?} blocks the terminal");
        let (id, _) = csm_ui::screens::overlays::dialog_controls(h.state()).expect("a dialog with keys");
        let _ = id;
        h.key_press(Key::Escape);
        settle(&mut h);
        assert_eq!(h.state().ui.overlay, Overlay::None, "{overlay:?}: Esc closes it");
    }
    h.state_mut().ui.overlay = Overlay::Busy;
    settle(&mut h);
    assert!(dialog_keys::current(&h.ctx, Id::new(csm_ui::screens::overlays::BUSY_ID)).is_none());
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "Enter keeps running");
    assert!(!tool_ids(&h).contains(&"app_quit".to_string()), "and quits nothing");
}

#[test]
fn escape_puts_a_settings_field_back_and_enter_saves_it() {
    let mut h = fresh();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::General;
    settle(&mut h);
    command_field(&h).click();
    command_field(&h).type_text("zzz");
    h.step();
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!tool_ids(&h).contains(&"settings_set".to_string()), "Escape saved nothing: {:?}", tool_ids(&h));
    assert!(!h.state().ui.settings.drafts.contains_key("DefaultCommand"), "the draft is gone");
    // Escape also left the page; open it again.
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(&mut h);
    command_field(&h).click();
    command_field(&h).type_text("aaa");
    h.step();
    h.key_press(Key::Enter);
    settle(&mut h);
    let set = h.state().submitted().iter().find(|c| c.id == "settings_set").cloned().expect("Enter saved");
    assert_eq!(set.params["key"], "DefaultCommand");
}

fn command_field<'h>(h: &'h H) -> egui_kittest::Node<'h> {
    h.get_all_by_label("Default command").find(|n| n.accesskit_node().role() == egui::accesskit::Role::TextInput).expect("the text field")
}
