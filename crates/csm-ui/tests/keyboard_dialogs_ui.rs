//! Keyboard in the modal dialogs (`docs/design/keyboard.md`, section 3): Tab and Shift+Tab move
//! between the controls, Enter presses the focused button (or the default), Space toggles the
//! checkbox of the close dialog, and Escape cancels. The pure rules are unit-tested in
//! `dialog_keys.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::app::{Action, ConfirmState, Overlay, PromptKind, PromptState};
use csm_ui::dialog_keys::{self, Slot};
use csm_ui::screens::overlays::{CLOSE_ID, CONFIRM_ID, PROMPT_ID};
use csm_ui::theme::ThemeKind;
use egui::Key;
use egui_kittest::Harness;
use egui::Id;
use serde_json::json;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn tool_ids(h: &Harness<'static, csm_ui::CsmApp>) -> Vec<String> {
    h.state().submitted().iter().map(|c| c.id.clone()).collect()
}

/// A yes/no dialog whose yes button runs `group_new`, so a press shows up as a submitted tool.
fn open_confirm(h: &mut Harness<'static, csm_ui::CsmApp>, also: bool) {
    let also = also.then(|| ("Copy link".to_string(), Box::new(Action::Tool("session_close", json!({})))));
    h.state_mut().ui.confirm = Some(ConfirmState {
        safe_default: false,
        title: "Restart now?".into(),
        body: "The change needs a new process.".into(),
        yes_label: "Restart".into(),
        no_label: "Later".into(),
        yes: Box::new(Action::Tool("group_new", json!({"name": "yes"}))),
        also,
    });
    h.state_mut().ui.overlay = Overlay::Confirm;
    settle(h);
}

fn open_prompt(h: &mut Harness<'static, csm_ui::CsmApp>, text: &str) {
    h.state_mut().ui.prompt = Some(PromptState { kind: PromptKind::NewGroup, text: text.into(), focus: true });
    h.state_mut().ui.overlay = Overlay::Prompt;
    settle(h);
}

fn open_close_ask(h: &mut Harness<'static, csm_ui::CsmApp>) {
    h.state_mut().ui.overlay = Overlay::CloseAsk;
    settle(h);
}

#[test]
fn enter_presses_the_default_yes_button_of_a_confirm_dialog() {
    let mut h = h();
    open_confirm(&mut h, false);
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "Enter closes the dialog");
    assert!(tool_ids(&h).contains(&"group_new".to_string()), "Enter runs the default (yes) action");
}

#[test]
fn tab_moves_to_no_and_enter_presses_it_without_running_yes() {
    let mut h = h();
    open_confirm(&mut h, false);
    h.key_press(Key::Tab);
    settle(&mut h);
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(CONFIRM_ID)), Some(Slot::Button(0)), "the first Tab reaches yes");
    h.key_press(Key::Tab);
    settle(&mut h);
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(CONFIRM_ID)), Some(Slot::Button(1)), "the second Tab reaches no");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "Enter on no closes the dialog");
    assert!(!tool_ids(&h).iter().any(|t| t == "group_new" || t == "session_close"), "no runs nothing");
}

#[test]
fn shift_tab_from_the_start_reaches_the_last_button() {
    let mut h = h();
    open_confirm(&mut h, true);
    h.key_press_modifiers(egui::Modifiers::SHIFT, Key::Tab);
    settle(&mut h);
    // With "also" the buttons are yes, also, no: Shift+Tab from nothing lands on no.
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(CONFIRM_ID)), Some(Slot::Button(2)));
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None);
    assert!(!tool_ids(&h).iter().any(|t| t == "group_new" || t == "session_close"), "no runs nothing");
}

#[test]
fn escape_cancels_a_confirm_dialog_and_runs_nothing() {
    let mut h = h();
    open_confirm(&mut h, false);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "Esc closes the dialog");
    assert!(!tool_ids(&h).iter().any(|t| t == "group_new" || t == "session_close"), "Esc runs nothing");
}

#[test]
fn a_dialog_closed_by_keys_forgets_its_focus() {
    let mut h = h();
    open_confirm(&mut h, false);
    h.key_press(Key::Tab);
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    open_confirm(&mut h, false);
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(CONFIRM_ID)), None, "a new dialog starts with no focus, so Enter takes the default");
}

#[test]
fn prompt_tab_moves_from_the_field_to_ok_and_enter_submits() {
    let mut h = h();
    open_prompt(&mut h, "Clients");
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(PROMPT_ID)), Some(Slot::Field), "a prompt opens in its text field");
    h.key_press(Key::Tab);
    settle(&mut h);
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(PROMPT_ID)), Some(Slot::Button(0)), "Tab from the field reaches OK");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "OK closes the prompt");
    let call = h.state().submitted().iter().find(|c| c.id == "group_new").cloned().expect("OK creates the group");
    assert_eq!(call.params["name"], "Clients");
}

#[test]
fn enter_in_the_prompt_field_still_submits_the_text() {
    let mut h = h();
    open_prompt(&mut h, "Clients");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None);
    assert!(tool_ids(&h).contains(&"group_new".to_string()), "Enter in the field submits");
}

#[test]
fn prompt_cancel_is_reached_by_tab_and_closes_without_a_group() {
    let mut h = h();
    open_prompt(&mut h, "Clients");
    h.key_press(Key::Tab);
    h.key_press(Key::Tab);
    settle(&mut h);
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(PROMPT_ID)), Some(Slot::Button(1)), "the second Tab reaches Cancel");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "Cancel closes the prompt");
    assert!(!tool_ids(&h).iter().any(|t| t == "group_new" || t == "session_close"), "Cancel creates nothing");
}

#[test]
fn a_prompt_tab_cycle_comes_back_to_the_field() {
    let mut h = h();
    open_prompt(&mut h, "Clients");
    for _ in 0..3 {
        h.key_press(Key::Tab);
        settle(&mut h);
    }
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(PROMPT_ID)), Some(Slot::Field), "Tab goes field, OK, Cancel, field");
}

#[test]
fn space_toggles_the_remember_checkbox_of_the_close_dialog() {
    let mut h = h();
    open_close_ask(&mut h);
    let before = h.state().ui.close_remember;
    h.key_press(Key::Tab);
    settle(&mut h);
    assert_eq!(dialog_keys::current(&h.ctx, Id::new(CLOSE_ID)), Some(Slot::Field), "the first Tab reaches the checkbox");
    h.key_press(Key::Space);
    settle(&mut h);
    assert_ne!(h.state().ui.close_remember, before, "Space toggles it");
}

#[test]
fn escape_keeps_the_window_open_from_the_close_dialog() {
    let mut h = h();
    open_close_ask(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "Esc closes the question");
    assert!(!tool_ids(&h).iter().any(|t| t == "group_new" || t == "session_close"));
}

/// The quick-open (Ctrl+T) with its folder field focused and no folder chosen.
fn open_quick(h: &mut Harness<'static, csm_ui::CsmApp>) {
    h.state_mut().ui.quick = csm_ui::app::QuickOpenState { focus_input: true, ..Default::default() };
    h.state_mut().ui.overlay = Overlay::QuickOpen;
    settle(h);
}

fn quick_focus(h: &Harness<'static, csm_ui::CsmApp>) -> Option<Slot> {
    dialog_keys::current(&h.ctx, Id::new(csm_ui::screens::palette::QUICK_ID))
}

// The Tab stops of the New session dialog, in the order they read on screen (the demo has no
// Windows Terminal profiles, so no Profile row): This PC, SSH, WSL, the folder field, Browse, the
// folder list, the command row, the group, Start and keep open, Start.
const PLACE_THIS_PC: usize = 0;
const FIELD: usize = 3;
const BROWSE: usize = 4;
const LIST: usize = 5;
const COMMAND_ROW: usize = 6;
const GROUP: usize = 7;
const KEEP_OPEN: usize = 8;
const START: usize = 9;

fn tab_to(h: &mut Harness<'static, csm_ui::CsmApp>, stop: usize) {
    for _ in 0..24 {
        if quick_focus(h) == Some(Slot::Button(stop)) {
            return;
        }
        h.key_press(Key::Tab);
        settle(h);
    }
    panic!("Tab never reached stop {stop}: at {:?}", quick_focus(h));
}

#[test]
fn quick_open_tab_walks_every_control_in_visual_order() {
    let mut h = h();
    open_quick(&mut h);
    assert_eq!(quick_focus(&h), Some(Slot::Button(FIELD)), "the quick-open opens in its folder field");
    for (stop, what) in [(BROWSE, "Browse"), (LIST, "the folder list"), (COMMAND_ROW, "the command row"), (GROUP, "the group"), (KEEP_OPEN, "Start and keep open"), (START, "Start")] {
        h.key_press(Key::Tab);
        settle(&mut h);
        assert_eq!(quick_focus(&h), Some(Slot::Button(stop)), "Tab reaches {what}");
    }
    h.key_press(Key::Tab);
    settle(&mut h);
    assert_eq!(quick_focus(&h), Some(Slot::Button(PLACE_THIS_PC)), "Tab wraps to the place selector (This PC first)");
    h.key_press(Key::Tab);
    h.key_press(Key::Tab);
    settle(&mut h);
    assert_eq!(quick_focus(&h), Some(Slot::Button(2)), "SSH then WSL, left to right");
    h.key_press_modifiers(egui::Modifiers::SHIFT, Key::Tab);
    settle(&mut h);
    assert_eq!(quick_focus(&h), Some(Slot::Button(1)), "Shift+Tab goes back");
    assert_eq!(h.state().ui.overlay, Overlay::QuickOpen, "Tab closes nothing");
    assert_eq!(h.state().ui.quick.cmd, 0, "Tab no longer cycles the command");
}

#[test]
fn quick_open_left_and_right_cycle_the_command_on_its_row() {
    let mut h = h();
    open_quick(&mut h);
    let commands = csm_ui::screens::palette::quick_commands(h.state()).len();
    tab_to(&mut h, COMMAND_ROW);
    h.key_press(Key::ArrowRight);
    settle(&mut h);
    if commands > 1 {
        assert_eq!(h.state().ui.quick.cmd, 1, "Right moves to the next command");
    }
    h.key_press(Key::ArrowLeft);
    settle(&mut h);
    assert_eq!(h.state().ui.quick.cmd, 0, "Left goes back");
    assert_eq!(h.state().ui.overlay, Overlay::QuickOpen, "the arrows stay in the dialog");
}

#[test]
fn quick_open_enter_on_the_command_row_starts_the_session() {
    let mut h = h();
    open_quick(&mut h);
    h.state_mut().ui.quick.query = r"C:\GitHub\Odulodo.Admin".into();
    tab_to(&mut h, COMMAND_ROW);
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "Enter on the command row starts the session and closes");
    assert!(tool_ids(&h).iter().any(|t| t == "session_new"), "the session is started");
}

#[test]
fn quick_open_enter_presses_the_focused_button() {
    let mut h = h();
    open_quick(&mut h);
    // SSH is a button of the place selector: Enter picks it, and the dialog stays open.
    tab_to(&mut h, 1);
    assert!(!h.state().ui.quick.remote);
    h.key_press(Key::Enter);
    settle(&mut h);
    assert!(h.state().ui.quick.remote, "Enter on SSH switches to a remote connection");
    assert_eq!(h.state().ui.overlay, Overlay::QuickOpen, "a button does not close the dialog");
}

#[test]
fn quick_open_escape_closes_it_and_forgets_its_focus() {
    let mut h = h();
    open_quick(&mut h);
    h.key_press(Key::Tab);
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::None, "Esc closes the quick-open");
    open_quick(&mut h);
    assert_eq!(quick_focus(&h), Some(Slot::Button(FIELD)), "the next opening starts in the folder field");
}
