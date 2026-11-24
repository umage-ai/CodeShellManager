//! Keyboard in the popup menus (`docs/design/keyboard.md`, section 3): arrows, Enter, Escape,
//! Shift+F10 on the focused item, and the Recent menu's keys through the shared handler. The pure
//! rules (steps, access letters) are unit-tested in `menu_nav.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::app::{Menu, Overlay};
use csm_ui::focus::FocusRegion;
use csm_ui::theme::ThemeKind;
use egui::{Key, Modifiers};
use egui_kittest::Harness;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn tool_ids(h: &Harness<'static, csm_ui::CsmApp>) -> Vec<String> {
    h.state().submitted().iter().map(|c| c.id.clone()).collect()
}

fn open_pane_menu(h: &mut Harness<'static, csm_ui::CsmApp>) {
    h.key_press_modifiers(Modifiers::COMMAND, Key::Period);
    settle(h);
    assert!(matches!(h.state().ui.menu, Some(Menu::Pane { .. })), "Ctrl+. opens the pane menu");
}

#[test]
fn shift_f10_opens_the_focused_rows_menu_with_its_first_row_highlighted() {
    let mut h = h();
    // Shift+F10 belongs to the terminal while it has the focus.
    h.key_press_modifiers(Modifiers::SHIFT, Key::F10);
    settle(&mut h);
    assert_eq!(h.state().ui.menu, None, "the terminal keeps Shift+F10 when it has the focus");
    // F6 cycles to the sidebar list.
    for _ in 0..FocusRegion::ORDER.len() {
        if h.state().ui.region == Some(FocusRegion::SidebarList) {
            break;
        }
        h.key_press(Key::F6);
        settle(&mut h);
    }
    assert_eq!(h.state().ui.region, Some(FocusRegion::SidebarList), "F6 reaches the sidebar");
    h.key_press_modifiers(Modifiers::SHIFT, Key::F10);
    settle(&mut h);
    assert!(matches!(h.state().ui.menu, Some(Menu::Row { .. })), "Shift+F10 opens the row menu");
    assert_eq!(h.state().ui.menu_sel.len(), 1, "the first row is highlighted");
}

#[test]
fn arrows_move_the_highlight_and_escape_closes() {
    let mut h = h();
    open_pane_menu(&mut h);
    assert!(h.state().ui.menu_sel.is_empty(), "a menu opened with Ctrl+. highlights nothing yet");
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    let first = h.state().ui.menu_sel.clone();
    assert_eq!(first.len(), 1, "Down highlights the first row");
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    let second = h.state().ui.menu_sel.clone();
    assert_ne!(first, second, "Down moves to the next row");
    h.key_press(Key::ArrowUp);
    settle(&mut h);
    assert_eq!(h.state().ui.menu_sel, first, "Up goes back");
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.menu, None, "Escape closes the menu");
    assert!(h.state().ui.menu_sel.is_empty(), "closing clears the highlight");
}

#[test]
fn enter_runs_the_highlighted_row_and_closes_the_menu() {
    let mut h = h();
    open_pane_menu(&mut h);
    h.key_press(Key::Home);
    settle(&mut h);
    let before = tool_ids(&h).len();
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.menu, None, "Enter closes the menu");
    assert!(tool_ids(&h).len() > before || h.state().ui.overlay != Overlay::None, "Enter runs the row's action");
}

#[test]
fn ctrl_period_still_toggles_an_open_pane_menu() {
    let mut h = h();
    open_pane_menu(&mut h);
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    // Ctrl+. is a chord the menu does not read: the shortcut still runs and closes the menu.
    h.key_press_modifiers(Modifiers::COMMAND, Key::Period);
    settle(&mut h);
    assert_eq!(h.state().ui.menu, None, "Ctrl+. toggles the open pane menu off as before");
}

#[test]
fn f6_does_not_move_while_a_menu_is_open() {
    let mut h = h();
    open_pane_menu(&mut h);
    h.key_press(Key::F6);
    settle(&mut h);
    assert_eq!(h.state().ui.region, None);
    assert!(h.state().ui.menu.is_some(), "F6 leaves the menu open");
}

#[test]
fn the_recent_menu_moves_with_arrows_wraps_and_picks_with_enter() {
    let mut h = h();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    settle(&mut h);
    assert!(matches!(h.state().ui.menu, Some(Menu::Recent { .. })), "Ctrl+Shift+E opens Recent");
    assert_eq!(h.state().ui.menu_sel, vec![0], "Recent starts on its first row");
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert_eq!(h.state().ui.menu_sel, vec![1]);
    h.key_press(Key::ArrowUp);
    settle(&mut h);
    h.key_press(Key::ArrowUp);
    settle(&mut h);
    assert_ne!(h.state().ui.menu_sel, vec![0], "Up from the first row wraps to the last");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.menu, None, "Enter picks the row and closes the menu");
}

#[test]
fn a_digit_picks_its_recent_session_and_ctrl_shift_e_closes_recent() {
    let mut h = h();
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    settle(&mut h);
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    settle(&mut h);
    assert_eq!(h.state().ui.menu, None, "Ctrl+Shift+E closes Recent");
    h.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::E);
    settle(&mut h);
    h.key_press(Key::Num1);
    settle(&mut h);
    assert_eq!(h.state().ui.menu, None, "the digit picks the first session and closes the menu");
    assert!(tool_ids(&h).iter().any(|id| id == "session_focus"), "the pick is the session_focus call");
}

#[test]
fn arrows_do_nothing_when_no_menu_is_open() {
    let mut h = h();
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    assert_eq!(h.state().ui.menu, None);
    assert!(h.state().ui.menu_sel.is_empty());
}

fn main_open(h: &Harness<'static, csm_ui::CsmApp>) -> bool {
    matches!(h.state().ui.menu, Some(Menu::Main { .. }))
}

/// Taps Alt: Alt goes down, then up, with nothing else in between.
fn tap_alt(h: &mut Harness<'static, csm_ui::CsmApp>) {
    h.event(egui::Event::ModifiersChanged(Modifiers::ALT));
    h.step();
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    h.step();
    settle(h);
}

/// Every tool id in the main menu's entries, the submenus included.
fn main_menu_tools(entries: &[csm_ui::screens::menus::Entry]) -> Vec<String> {
    use csm_ui::screens::menus::Entry;
    let mut out = Vec::new();
    for e in entries {
        match e {
            Entry::Sub { items, .. } => out.extend(main_menu_tools(items)),
            Entry::Item { action: csm_ui::app::Action::PaletteTool(id), .. } => out.push(id.clone()),
            _ => {}
        }
    }
    out
}

#[test]
fn f10_opens_the_main_menu_and_f10_or_escape_closes_it() {
    let mut h = h();
    h.key_press(Key::F10);
    settle(&mut h);
    assert!(main_open(&h), "F10 opens the main menu");
    h.key_press(Key::F10);
    settle(&mut h);
    assert!(!main_open(&h), "F10 again closes it");
    h.key_press(Key::F10);
    settle(&mut h);
    h.key_press(Key::Escape);
    settle(&mut h);
    assert!(!main_open(&h), "Escape closes it");
}

#[test]
fn a_clean_alt_tap_opens_the_main_menu_and_alt_with_a_key_does_not() {
    let mut h = h();
    tap_alt(&mut h);
    assert!(main_open(&h), "a clean Alt tap opens the main menu");
    tap_alt(&mut h);
    assert!(!main_open(&h), "a second tap closes it");
    // Alt+F is a chord: its key goes to the terminal, and the release is no tap.
    h.key_press_modifiers(Modifiers::ALT, Key::F);
    settle(&mut h);
    assert!(!main_open(&h), "Alt with a key is not a tap");
    // Ctrl with Alt is a chord too.
    h.event(egui::Event::ModifiersChanged(Modifiers::ALT | Modifiers::CTRL));
    h.step();
    h.event(egui::Event::ModifiersChanged(Modifiers::NONE));
    h.step();
    settle(&mut h);
    assert!(!main_open(&h), "Ctrl+Alt is not a tap");
}

#[test]
fn the_main_menu_moves_into_help_and_enter_runs_the_palette() {
    let mut h = h();
    h.key_press(Key::F10);
    settle(&mut h);
    // Help is the last area.
    h.key_press(Key::End);
    settle(&mut h);
    assert_eq!(h.state().ui.menu_sel.len(), 1, "End highlights the last area");
    h.key_press(Key::ArrowRight);
    settle(&mut h);
    assert_eq!(h.state().ui.menu_sel.len(), 2, "Right opens Help, its first row highlighted");
    h.key_press(Key::Enter);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::Palette, "Enter on the first Help row opens the palette");
    assert!(!main_open(&h), "and the menu closes");
}

#[test]
fn the_main_menu_lists_every_tool_the_palette_offers() {
    let h = h();
    let app = h.state();
    assert!(app.model.active.is_some(), "the demo has an active session, so no row is muted");
    let ids = main_menu_tools(&csm_ui::screens::menus::main_entries(app));
    for t in app.dispatch.tools().iter() {
        if t.id == "consent_answer" || t.id == "layout_set" || csm_ui::screens::palette::tool_hidden(app, &t.id) {
            continue;
        }
        assert!(ids.contains(&t.id), "{} is in the main menu", t.id);
    }
}

#[test]
fn every_toolset_has_an_area_in_the_main_menu() {
    for set in csm_engine::Toolset::ALL {
        let area = csm_ui::screens::menus::main_area(set);
        assert!(csm_ui::screens::menus::MAIN_AREAS.contains(&area), "{set:?} maps to a listed area");
    }
}

#[test]
fn tools_that_act_on_a_session_are_muted_without_one() {
    use csm_ui::screens::menus::Entry;
    let mut h = h();
    std::sync::Arc::make_mut(&mut h.state_mut().model).active = None;
    let entries = csm_ui::screens::menus::main_entries(h.state());
    let muted = entries.iter().any(|e| match e {
        Entry::Sub { items, .. } => items.iter().any(|i| matches!(i, Entry::Off { .. })),
        _ => false,
    });
    assert!(muted, "a session tool is shown muted with no active session");
    let ids = main_menu_tools(&entries);
    assert!(!ids.is_empty(), "the other tools stay offered");
}
