//! Keyboard focus regions (F6, Shift+F6, Esc) and pane movement (Ctrl+Alt+Arrow) through the real
//! UI on the demo backend. The geometry and the cycle order are unit-tested in `focus.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::app::Overlay;
use csm_ui::focus::{self, Dir, FocusRegion};
use csm_ui::theme::ThemeKind;
use egui::{Key, Modifiers};
use egui_kittest::Harness;
use serde_json::Value;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn regions(h: &Harness<'static, csm_ui::CsmApp>) -> Vec<FocusRegion> {
    h.state().ui.region_rects.iter().map(|(r, _)| *r).collect()
}

fn focus_calls(h: &Harness<'static, csm_ui::CsmApp>) -> Vec<Value> {
    h.state().submitted().iter().filter(|c| c.id == "session_focus").map(|c| c.params.clone()).collect()
}

#[test]
fn the_panes_and_the_sidebar_are_drawn_regions() {
    let h = h();
    let got = regions(&h);
    assert!(got.contains(&FocusRegion::TitleBar), "{got:?}");
    assert!(got.contains(&FocusRegion::Panes), "{got:?}");
    assert!(got.contains(&FocusRegion::SidebarList), "{got:?}");
    assert_eq!(h.state().ui.region, None, "the terminal has the keys until F6");
}

#[test]
fn f6_cycles_every_drawn_region_and_shift_f6_goes_back() {
    let mut h = h();
    let shown = regions(&h);
    assert!(shown.len() >= 3, "{shown:?}");
    let mut seen = Vec::new();
    for _ in 0..shown.len() {
        h.key_press(Key::F6);
        settle(&mut h);
        seen.push(h.state().ui.region.expect("F6 sets a region"));
    }
    // One full turn visits each drawn region once, in cycle order, and comes back to the start.
    let mut expect: Vec<FocusRegion> = FocusRegion::ORDER.into_iter().filter(|r| shown.contains(r)).collect();
    let panes_at = expect.iter().position(|r| *r == FocusRegion::Panes).unwrap_or(0);
    expect.rotate_left(panes_at + 1);
    assert_eq!(seen, expect);
    // Shift+F6 goes back one region.
    h.key_press_modifiers(Modifiers::SHIFT, Key::F6);
    settle(&mut h);
    assert_eq!(h.state().ui.region, expect.get(expect.len() - 2).copied());
}

#[test]
fn esc_leaves_a_region_for_the_terminal_and_the_terminal_keeps_its_escape() {
    let mut h = h();
    h.key_press(Key::F6);
    settle(&mut h);
    assert!(h.state().ui.region.is_some_and(|r| r != FocusRegion::Panes));
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.region, None);
    // With no region, Esc is not taken by the UI: nothing closes, no region is set.
    h.key_press(Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().ui.region, None);
    assert_eq!(h.state().ui.overlay, Overlay::None);
}

#[test]
fn f6_does_not_reach_through_an_open_menu() {
    let mut h = h();
    h.key_press_modifiers(Modifiers::COMMAND, Key::Period);
    settle(&mut h);
    assert!(h.state().ui.menu.is_some(), "Ctrl+. opens the pane menu");
    h.key_press(Key::F6);
    settle(&mut h);
    assert_eq!(h.state().ui.region, None);
}

#[test]
fn ctrl_alt_arrow_moves_to_the_pane_the_geometry_picks() {
    let mut h = h();
    let keys = [(Key::ArrowLeft, Dir::Left), (Key::ArrowRight, Dir::Right), (Key::ArrowUp, Dir::Up), (Key::ArrowDown, Dir::Down)];
    for (key, dir) in keys {
        // Each press is judged against the panes as they are now: a move can change the active pane.
        let Some(active) = h.state().model.active.clone() else { panic!("the demo has an active session") };
        let rects = h.state().ui.pane_rects.clone();
        let Some((_, from)) = rects.iter().find(|(id, _)| *id == active).cloned() else {
            // The active session is not in a visible pane (a sleeping or minimised one): nothing to move.
            continue;
        };
        let others: Vec<(csm_ui::model::SessionId, egui::Rect)> = rects.iter().filter(|(id, _)| *id != active).cloned().collect();
        let rect_list: Vec<egui::Rect> = others.iter().map(|(_, r)| *r).collect();
        let want = focus::neighbour(from, &rect_list, dir).and_then(|i| others.get(i)).map(|(id, _)| id.clone());
        let before = focus_calls(&h).len();
        h.key_press_modifiers(Modifiers::COMMAND | Modifiers::ALT, key);
        settle(&mut h);
        let calls = focus_calls(&h);
        match want {
            Some(id) => {
                assert_eq!(calls.len(), before + 1, "{dir:?} should move to a pane");
                assert_eq!(calls.last().and_then(|p| p.get("session_id")).and_then(Value::as_str), Some(id.as_str()));
            }
            None => assert_eq!(calls.len(), before, "{dir:?} has no pane to move to"),
        }
    }
}

#[test]
fn a_mouse_press_ends_the_region_mode() {
    let mut h = h();
    h.key_press(Key::F6);
    settle(&mut h);
    assert!(h.state().ui.region.is_some());
    let at = h.state().ui.region_rects.first().map(|(_, r)| r.center()).unwrap_or(egui::Pos2::ZERO);
    h.event(egui::Event::PointerMoved(at));
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    settle(&mut h);
    assert_eq!(h.state().ui.region, None);
}

#[test]
fn f6_into_the_sidebar_focuses_a_row_and_the_arrows_move_it() {
    let mut h = h();
    // F6 until the sidebar list has the region. Entering it puts the keyboard focus on a row.
    for _ in 0..8 {
        if h.state().ui.region == Some(FocusRegion::SidebarList) {
            break;
        }
        h.key_press(Key::F6);
        settle(&mut h);
    }
    assert_eq!(h.state().ui.region, Some(FocusRegion::SidebarList));
    let rows: Vec<egui::Id> = h.state().ui.sidebar_nav.iter().map(|(id, _)| *id).collect();
    let before = h.ctx.memory(|m| m.focused());
    assert!(before.is_some_and(|f| rows.contains(&f)), "F6 gives the sidebar a row: {before:?} of {rows:?}");
    h.key_press(Key::ArrowDown);
    settle(&mut h);
    let after = h.ctx.memory(|m| m.focused());
    assert_ne!(before, after, "ArrowDown moves the focus to the next row");
    assert!(after.is_some_and(|f| rows.contains(&f)), "the focus stays on a sidebar row");
    assert_eq!(h.state().ui.region, Some(FocusRegion::SidebarList), "the region stays the sidebar");
}
