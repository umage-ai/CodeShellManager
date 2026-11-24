//! Keyboard in the lists (`docs/design/keyboard.md`, sections 2 and 3): the sidebar rows, the
//! rail tabs and the inbox move with the arrows, Space selects, F2 renames the focused item, and
//! the tab chords run the rail tools. The rules (clamping, Home and End) are unit-tested in
//! `list_keys.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::app::InlineTarget;
use csm_ui::model::Scope;
use csm_ui::theme::ThemeKind;
use egui::{Id, Key, Modifiers};
use egui_kittest::Harness;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

/// The sidebar's focus order as drawn: the session row ids, in order.
fn rows(h: &Harness<'static, csm_ui::CsmApp>) -> Vec<String> {
    h.state().ui.sidebar_nav.iter().filter_map(|(_, s)| s.clone()).collect()
}

/// Gives the keyboard focus to the session row `id`, as Tab or the arrows would.
fn focus_row(h: &mut Harness<'static, csm_ui::CsmApp>, id: &str) {
    h.ctx.memory_mut(|m| m.request_focus(Id::new(("session-row", id.to_owned()))));
    settle(h);
}

fn focused(h: &Harness<'static, csm_ui::CsmApp>) -> Option<Id> {
    h.ctx.memory(|m| m.focused())
}

fn tool_ids(h: &Harness<'static, csm_ui::CsmApp>) -> Vec<String> {
    h.state().submitted().iter().map(|c| c.id.clone()).collect()
}

#[test]
fn arrows_move_the_focus_down_and_up_the_session_rows_and_stop_at_the_ends() {
    let mut h = h();
    let ids = rows(&h);
    assert!(ids.len() >= 3, "the demo shows several rows: {ids:?}");
    focus_row(&mut h, &ids[0]);
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert_eq!(focused(&h), Some(Id::new(("session-row", ids[1].clone()))), "Down moves to the next row");
    h.key_press(Key::ArrowUp);
    settle(&mut h);
    assert_eq!(focused(&h), Some(Id::new(("session-row", ids[0].clone()))), "Up moves back");
    h.key_press(Key::ArrowUp);
    settle(&mut h);
    assert_eq!(focused(&h), Some(Id::new(("session-row", ids[0].clone()))), "Up on the first row stays on it");
    h.key_press(Key::End);
    settle(&mut h);
    assert_eq!(focused(&h), Some(Id::new(("session-row", ids.last().unwrap().clone()))), "End goes to the last row");
    h.key_press(Key::Home);
    settle(&mut h);
    assert_eq!(focused(&h), Some(Id::new(("session-row", ids[0].clone()))), "Home goes to the first row");
}

#[test]
fn space_toggles_the_focused_row_in_the_selection() {
    let mut h = h();
    let ids = rows(&h);
    focus_row(&mut h, &ids[1]);
    h.key_press(Key::Space);
    settle(&mut h);
    assert!(h.state().ui.selection.contains(&ids[1]), "Space selects the focused row");
    h.key_press(Key::Space);
    settle(&mut h);
    assert!(!h.state().ui.selection.contains(&ids[1]), "a second Space takes it out again");
}

#[test]
fn shift_arrow_extends_the_selection_to_the_row_the_focus_lands_on() {
    let mut h = h();
    let ids = rows(&h);
    focus_row(&mut h, &ids[0]);
    h.key_press_modifiers(Modifiers::SHIFT, Key::ArrowDown);
    settle(&mut h);
    assert_eq!(focused(&h), Some(Id::new(("session-row", ids[1].clone()))), "Shift+Down still moves the focus");
    assert!(h.state().ui.selection.contains(&ids[1]), "and selects the row it lands on");
}

#[test]
fn f2_renames_the_focused_row_and_not_the_active_session() {
    let mut h = h();
    let ids = rows(&h);
    let active = h.state().model.active.clone();
    let other = ids.iter().find(|i| Some(i.as_str()) != active.as_deref()).cloned().expect("a row that is not active");
    focus_row(&mut h, &other);
    h.key_press(Key::F2);
    settle(&mut h);
    assert!(
        matches!(h.state().ui.inline.as_ref().map(|e| &e.target), Some(InlineTarget::Session(s)) if *s == other),
        "F2 edits the focused row's name: {:?}",
        h.state().ui.inline.as_ref().map(|e| e.target.clone())
    );
}

#[test]
fn the_rail_tabs_move_the_focus_along_the_rail() {
    let mut h = h();
    let group = h.state().model.groups.first().map(|g| g.id.clone()).expect("the demo has a group");
    let all = Id::new(("rail-tab", Scope::All.key()));
    let first_group = Id::new(("rail-tab", Scope::Group(group).key()));
    h.ctx.memory_mut(|m| m.request_focus(all));
    settle(&mut h);
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert_eq!(focused(&h), Some(first_group), "Down moves from All to the first group tab");
    h.key_press(Key::ArrowUp);
    settle(&mut h);
    assert_eq!(focused(&h), Some(all), "Up goes back to All");
    h.key_press(Key::ArrowRight);
    settle(&mut h);
    assert_eq!(focused(&h), Some(first_group), "Right moves along the rail too");
}

#[test]
fn the_inbox_rows_move_with_the_arrows() {
    let mut h = h();
    let needs: Vec<String> = h.state().model.needs_you().iter().map(|s| s.id.clone()).collect();
    if needs.len() < 2 {
        return;
    }
    h.ctx.memory_mut(|m| m.request_focus(Id::new(("inbox-row", needs[0].clone()))));
    settle(&mut h);
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert_eq!(focused(&h), Some(Id::new(("inbox-row", needs[1].clone()))), "Down moves to the next inbox row");
}

#[test]
fn ctrl_alt_page_keys_run_the_rail_tab_tools() {
    let mut h = h();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::ALT, Key::PageDown);
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::ALT, Key::PageUp);
    settle(&mut h);
    let ids = tool_ids(&h);
    assert!(ids.iter().any(|i| i == "rail_tab_next"), "{ids:?}");
    assert!(ids.iter().any(|i| i == "rail_tab_prev"), "{ids:?}");
}

#[test]
fn ctrl_page_down_with_no_screen_open_keeps_the_sessions_tab() {
    let mut h = h();
    h.key_press_modifiers(Modifiers::COMMAND, Key::PageDown);
    settle(&mut h);
    let active = h.state().ui.screens.lock().unwrap().active.clone();
    assert_eq!(active, None, "the Sessions tab stays");
}
