//! The sidebar's multi-select and bulk menu, the sort control and Duplicate, through the real UI
//! on the static demo backend and a real engine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_ui::app::{Menu, Overlay};
use csm_ui::model::Status;
use csm_ui::theme::ThemeKind;
use egui::{Event, Modifiers, PointerButton};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

type H = Harness<'static, csm_ui::CsmApp>;

fn h() -> H {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn has(h: &H, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

fn calls_since(h: &H, before: usize, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted()[before..].iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

fn order(h: &H) -> Vec<String> {
    h.state().model.sessions.iter().map(|s| s.id.clone()).collect()
}

fn selected(h: &H) -> Vec<String> {
    let mut v = h.state().ui.selection.ids().to_vec();
    v.sort();
    v
}

/// A click on a sidebar row with modifiers held.
fn click_row(h: &mut H, name: &str, m: Modifiers) {
    h.get_by_label(&format!("Session: {name}")).click_modifiers(m);
    settle(h);
}

fn open_row_menu(h: &mut H, name: &str) {
    h.get_by_label(&format!("Session: {name}")).click_secondary();
    settle(h);
    assert!(matches!(h.state().ui.menu, Some(Menu::Row { .. })), "the row menu opened");
}

#[test]
fn ctrl_and_shift_click_select_rows_without_focusing_and_esc_clears() {
    let mut h = h();
    assert_eq!(h.state().model.active.as_deref(), Some("web"));
    let before = h.state().submitted().len();

    click_row(&mut h, "Odulodo", Modifiers::CTRL);
    click_row(&mut h, "Odulodo.Web (PS)", Modifiers::CTRL);
    assert_eq!(selected(&h), ["odulodo", "webps"]);
    assert!(calls_since(&h, before, "session_focus").is_empty(), "selecting is not focusing");
    assert_eq!(h.state().model.active.as_deref(), Some("web"));
    // Ctrl-click again toggles it off.
    click_row(&mut h, "Odulodo", Modifiers::CTRL);
    assert_eq!(selected(&h), ["webps"]);

    // Shift-click: the visible range from the anchor (the last Ctrl-clicked row) up to the target.
    click_row(&mut h, "Odulodo.Web (PS)", Modifiers::SHIFT);
    assert_eq!(selected(&h), ["odulodo", "web", "webps", "wt"]);

    // Esc clears; a plain click clears too (and focuses).
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(selected(&h).is_empty());
    click_row(&mut h, "Odulodo", Modifiers::CTRL);
    click_row(&mut h, "search-sort", Modifiers::NONE);
    assert!(selected(&h).is_empty());
    assert_eq!(calls_since(&h, before, "session_focus"), vec![json!({"session_id": "wt"})]);
}

#[test]
fn the_row_menu_acts_on_the_whole_selection_with_counts() {
    let mut h = h();
    click_row(&mut h, "Odulodo.Web (PS)", Modifiers::CTRL);
    click_row(&mut h, "search-sort", Modifiers::CTRL);

    // A row outside the selection gets its own, single-session menu.
    open_row_menu(&mut h, "Odulodo");
    assert!(has(&h, "Duplicate session"));
    assert!(!has(&h, "Sleep (2)"));
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, csm_ui::Action::CloseMenu);
    settle(&mut h);

    open_row_menu(&mut h, "search-sort");
    assert!(has(&h, "2 SESSIONS SELECTED"));
    assert!(has(&h, "Restart (2)…") && has(&h, "Close (2)…"));
    assert!(!has(&h, "Duplicate session"), "single-session entries are gone");
    let before = h.state().submitted().len();
    h.get_by_label("Sleep (2)").click();
    settle(&mut h);
    let sleeps = calls_since(&h, before, "session_sleep_many");
    assert_eq!(sleeps, vec![json!({"session_ids": ["wt", "webps"]})], "in sidebar order");
    assert_eq!(h.state().model.session("wt").unwrap().status, Status::Asleep);
    assert_eq!(h.state().model.session("webps").unwrap().status, Status::Asleep);

    // Close asks first, then closes all of them with force.
    open_row_menu(&mut h, "search-sort");
    assert!(has(&h, "Wake (2)"), "asleep now: Wake instead of Sleep");
    h.get_by_label("Close (2)…").click();
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::Confirm);
    assert!(calls_since(&h, before, "session_close_many").is_empty(), "nothing closed before the answer");
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(calls_since(&h, before, "session_close_many"), vec![json!({"session_ids": ["wt", "webps"], "force": true})]);
    assert!(h.state().model.session("wt").is_none() && h.state().model.session("webps").is_none());
    assert!(selected(&h).is_empty(), "closed sessions leave the selection");
}

#[test]
fn dragging_a_selection_onto_a_rail_tab_moves_all_of_it() {
    let mut h = h();
    click_row(&mut h, "Odulodo", Modifiers::CTRL);
    click_row(&mut h, "Odulodo.Web (PS)", Modifiers::CTRL);
    let before = h.state().submitted().len();
    let from = h.get_by_label("Session: Odulodo.Web (PS)").rect().center();
    let to = h.get_by_label("Group tab: Umage").rect().center();
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    assert!(has(&h, "Odulodo.Web (PS) + 1 more") || h.state().ui.drags.row.is_some(), "the ghost carries the selection");
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    settle(&mut h);
    assert_eq!(calls_since(&h, before, "session_move_many"), vec![json!({"session_ids": ["odulodo", "webps"], "group_id": "umage"})]);
    assert!(calls_since(&h, before, "session_move_to_group").is_empty());
    for id in ["odulodo", "webps"] {
        assert_eq!(h.state().model.session(id).unwrap().group.as_deref(), Some("umage"));
    }
}

#[test]
fn dragging_a_selection_between_rows_moves_it_as_one_block() {
    let mut h = h();
    click_row(&mut h, "Odulodo", Modifiers::CTRL);
    click_row(&mut h, "Odulodo.Web", Modifiers::CTRL);
    let before = h.state().submitted().len();
    // Drag Odulodo.Web to just below the tab's last live row.
    let from = h.get_by_label("Session: Odulodo.Web").rect().center();
    let last = h.get_by_label("Session: Odulodo.Web (PS)").rect();
    let to = egui::pos2(last.center().x, last.max.y - 3.0);
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    settle(&mut h);
    assert_eq!(
        calls_since(&h, before, "session_move_many"),
        vec![json!({"session_ids": ["odulodo", "web"], "group_id": "odulodo", "after": "webps"})]
    );
    let tab: Vec<String> = order(&h).into_iter().filter(|id| ["odulodo", "web", "wt", "webps"].contains(&id.as_str())).collect();
    assert_eq!(tab, ["wt", "webps", "odulodo", "web"]);
}

#[test]
fn the_sort_control_sorts_the_tab_once_and_flips_on_a_second_pick() {
    let mut h = h();
    h.get_by_label("Sort sessions").click();
    settle(&mut h);
    assert!(has(&h, "SORT ODULODO"), "the rail tab is the scope");
    assert!(has(&h, "Last active"));
    let before = h.state().submitted().len();
    h.get_by_label("Name").click();
    settle(&mut h);
    assert_eq!(calls_since(&h, before, "session_sort"), vec![json!({"by": "name", "descending": false, "group": "odulodo"})]);
    let tab: Vec<String> = order(&h).into_iter().filter(|id| ["odulodo", "web", "wt", "webps"].contains(&id.as_str())).collect();
    assert_eq!(tab, ["odulodo", "web", "webps", "wt"], "Odulodo, Odulodo.Web, Odulodo.Web (PS), search-sort");
    assert_eq!(h.state().ui.sort, Some((csm_core::session_sort::SortField::Name, false)));

    // Picking it again flips the direction; the palette opens the same menu.
    h.state_mut().ui.menu = None;
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, csm_ui::Action::PaletteTool("session_sort"));
    settle(&mut h);
    assert!(matches!(h.state().ui.menu, Some(Menu::Sort { .. })));
    h.get_by_label("Name").click();
    settle(&mut h);
    assert_eq!(calls_since(&h, before, "session_sort").last().unwrap()["descending"], true);
    let tab: Vec<String> = order(&h).into_iter().filter(|id| ["odulodo", "web", "wt", "webps"].contains(&id.as_str())).collect();
    assert_eq!(tab, ["wt", "webps", "web", "odulodo"]);
}

#[test]
fn ctrl_alt_t_duplicates_the_active_session_after_it() {
    let mut h = h();
    let before = h.state().submitted().len();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::ALT, egui::Key::T);
    settle(&mut h);
    assert_eq!(calls_since(&h, before, "session_duplicate"), vec![json!({"session_id": "web"})]);
    assert_ne!(h.state().ui.overlay, Overlay::QuickOpen, "not taken by Ctrl+T");
    let o = order(&h);
    let at = o.iter().position(|x| x == "web").unwrap();
    let copy = h.state().model.session(&o[at + 1]).unwrap().clone();
    assert_eq!(copy.name, "Odulodo.Web (2)");
    assert_eq!((copy.folder.as_str(), copy.group.as_deref()), ("C:\\GitHub\\Odulodo.Web", Some("odulodo")));
    assert_eq!(h.state().model.active.as_deref(), Some(copy.id.as_str()), "the copy is focused");

    // Ctrl+Shift+T reopens; it no longer opens the new-session panel as well.
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, egui::Key::T);
    settle(&mut h);
    assert_ne!(h.state().ui.overlay, Overlay::QuickOpen);
    assert_eq!(h.state().submitted().last().unwrap().id, "session_reopen_closed");

    // The pane menu offers it too.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, csm_ui::Action::OpenMenu(Menu::Pane { session: "web".into(), at: egui::pos2(900.0, 120.0) }));
    settle(&mut h);
    h.get_by_label("Duplicate").click();
    settle(&mut h);
    assert_eq!(h.state().submitted().last().unwrap().params, json!({"session_id": "web"}));
    assert!(h.state().model.sessions.iter().any(|s| s.name == "Odulodo.Web (3)"));
}

#[test]
fn esc_in_a_focused_terminal_reaches_the_program_and_keeps_the_selection() {
    let mut h = h();
    click_row(&mut h, "Odulodo", Modifiers::CTRL);
    click_row(&mut h, "search-sort", Modifiers::CTRL);
    let term = h.state().host.lock().unwrap().views.values().next().map(|v| v.id()).expect("a terminal pane is drawn");
    h.ctx.memory_mut(|m| m.request_focus(term));
    settle(&mut h);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(term), "the terminal has keyboard focus");
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert_eq!(selected(&h), ["odulodo", "wt"], "Esc went to the terminal, not to the selection");

    // With the focus elsewhere, Esc clears it.
    h.ctx.memory_mut(|m| m.surrender_focus(term));
    settle(&mut h);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert!(selected(&h).is_empty());
}

#[test]
fn move_to_group_from_the_palette_opens_the_selection_menu_and_never_ungroups() {
    let mut h = h();
    let ctx = h.ctx.clone();
    let before = h.state().submitted().len();
    // Nothing selected: a hint, no call.
    h.state_mut().run(&ctx, csm_ui::Action::PaletteTool("session_move_many"));
    settle(&mut h);
    assert!(h.state().ui.menu.is_none());
    click_row(&mut h, "Odulodo", Modifiers::CTRL);
    click_row(&mut h, "search-sort", Modifiers::CTRL);
    h.state_mut().run(&ctx, csm_ui::Action::PaletteTool("session_move_many"));
    settle(&mut h);
    assert!(calls_since(&h, before, "session_move_many").is_empty(), "nothing moves before a group is picked");
    assert!(matches!(h.state().ui.menu, Some(Menu::Row { .. })));
    assert!(has(&h, "Move to group (2)"));
    assert_eq!(h.state().model.session("odulodo").unwrap().group.as_deref(), Some("odulodo"));
}

#[test]
fn dropping_a_selection_on_the_tab_it_is_in_moves_nothing() {
    let mut h = h();
    click_row(&mut h, "Odulodo", Modifiers::CTRL);
    click_row(&mut h, "Odulodo.Web (PS)", Modifiers::CTRL);
    let before = h.state().submitted().len();
    let o = order(&h);
    let from = h.get_by_label("Session: Odulodo.Web (PS)").rect().center();
    let to = h.get_by_label("Group tab: Odulodo").rect().center();
    h.event(Event::PointerMoved(from));
    h.step();
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.step();
    for i in 1..=6 {
        h.event(Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    h.step();
    settle(&mut h);
    assert!(calls_since(&h, before, "session_move_many").is_empty());
    assert_eq!(order(&h), o);
}
