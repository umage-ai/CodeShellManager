//! The worktree workflow through the real UI (egui_kittest + AccessKit) on the demo backend:
//! worktree sessions nested in the sidebar (or under a repo header), the session menus' "New
//! session in sibling worktree ▸" and "New worktree from this branch…", and the quick-open's
//! "Also start in sibling worktrees".
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::app::Menu;
use csm_ui::model::{GroupMode, InspectorTab};
use csm_ui::screens::menus::{NEW_WORKTREE, SIBLING_WORKTREE};
use csm_ui::theme::ThemeKind;
use egui::{Event, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use serde_json::json;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn calls(h: &Harness<'static, csm_ui::CsmApp>, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

/// The sidebar's rows, top to bottom, by their accessible label.
fn sidebar_labels(h: &Harness<'static, csm_ui::CsmApp>) -> Vec<String> {
    let mut v: Vec<(f32, String)> = Vec::new();
    for prefix in ["Session: ", "Repo: "] {
        for n in h.query_all_by_label_contains(prefix) {
            // A label-role node carries its text as the value.
            let Some(l) = n.accesskit_node().label().or_else(|| n.value()) else { continue };
            if l.starts_with(prefix) {
                v.push((n.rect().min.y, l));
            }
        }
    }
    v.sort_by(|a, b| a.0.total_cmp(&b.0));
    v.into_iter().map(|(_, l)| l).collect()
}

fn set_view(h: &mut Harness<'static, csm_ui::CsmApp>, view: &str) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("view_set", json!({"view": view})));
    settle(h);
}

#[test]
fn a_worktree_session_nests_under_its_main_checkout_in_every_group_view() {
    let mut h = h();
    for (view, mode) in [("headers", GroupMode::Headers), ("flat", GroupMode::Flat), ("rail", GroupMode::Rail)] {
        set_view(&mut h, view);
        if mode == GroupMode::Rail {
            // The Odulodo tab holds Odulodo.Web and its worktree.
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::Tool("view_set", json!({"rail_tab": "odulodo"})));
            settle(&mut h);
        }
        assert_eq!(h.state().model.group_mode, mode);
        let rows = sidebar_labels(&h);
        let web = rows.iter().position(|l| l == "Session: Odulodo.Web").unwrap_or_else(|| panic!("{view}: {rows:?}"));
        assert_eq!(rows.get(web + 1).map(String::as_str), Some("Session: search-sort"), "{view}: the worktree follows its main checkout: {rows:?}");
        assert!(!rows.iter().any(|l| l.starts_with("Repo: ")), "{view}: no header while the main checkout has a row: {rows:?}");
    }
}

#[test]
fn a_worktree_without_its_main_checkout_sits_under_a_repo_header() {
    let mut h = h();
    set_view(&mut h, "headers");
    // Move the worktree session to another group: its main checkout's row is not in that section.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("session_move_to_group", json!({"session_id": "wt", "group_id": "umage"})));
    settle(&mut h);
    let rows = sidebar_labels(&h);
    let header = rows.iter().position(|l| l == "Repo: Odulodo.Web (1)").unwrap_or_else(|| panic!("{rows:?}"));
    assert_eq!(rows.get(header + 1).map(String::as_str), Some("Session: search-sort"), "{rows:?}");
    // Odulodo.Web's own section has no header.
    assert_eq!(rows.iter().filter(|l| l.starts_with("Repo: ")).count(), 1, "{rows:?}");
}

#[test]
fn the_row_menu_starts_a_session_in_a_sibling_worktree() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(Menu::Row { session: "wt".into(), at: pos2(300.0, 330.0) }));
    settle(&mut h);
    // A right click alone runs no git; opening the submenu reads the worktrees again (once).
    assert!(calls(&h, "worktree_list").is_empty());
    h.get_by_label(SIBLING_WORKTREE).click();
    settle(&mut h);
    assert_eq!(calls(&h, "worktree_list"), vec![json!({"session_id": "wt"})]);
    // Only the worktree without a session is offered: not search-sort (this one), not the main
    // checkout (Odulodo.Web has sessions).
    assert!(h.query_by_label("perf-budget").is_some());
    assert!(h.query_by_label("Odulodo.Web").is_none());
    h.get_by_label("perf-budget").click();
    settle(&mut h);
    let news = calls(&h, "session_new");
    let p = news.last().expect("session_new");
    assert_eq!(p["folder"], "C:\\GitHub\\Odulodo.Web\\.worktrees\\perf-budget");
    assert_eq!(p["command"], "claude");
    assert_eq!(p["group"], "odulodo");
    assert!(h.state().ui.menu.is_none(), "picking closes the menu");
    // The new session nests under its main checkout in the sidebar.
    let rows = sidebar_labels(&h);
    let web = rows.iter().position(|l| l == "Session: Odulodo.Web").unwrap();
    let nested: Vec<&String> = rows.iter().skip(web + 1).take(2).collect();
    assert!(nested.iter().any(|l| *l == "Session: perf-budget"), "{rows:?}");
}

#[test]
fn the_pane_menu_has_the_worktree_items_and_new_worktree_opens_the_git_tab_input() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(Menu::Pane { session: "web".into(), at: pos2(900.0, 80.0) }));
    settle(&mut h);
    assert!(h.query_by_label(SIBLING_WORKTREE).is_some());
    h.get_by_label(NEW_WORKTREE).click();
    settle(&mut h);
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Git));
    assert_eq!(h.state().model.active.as_deref(), Some("web"));
    // The Git tab's New worktree input is open and focused: typing and Enter create it.
    let input = h.get_by_label("New worktree branch");
    assert!(input.is_focused(), "the input has the keyboard");
    h.event(Event::Text("feat/next".into()));
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(calls(&h, "worktree_new").last(), Some(&json!({"session_id": "web", "branch": "feat/next"})));
}

#[test]
fn ssh_and_non_repo_sessions_have_no_worktree_items() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(Menu::Row { session: "build".into(), at: pos2(300.0, 330.0) }));
    settle(&mut h);
    assert!(h.query_by_label(SIBLING_WORKTREE).is_none());
    assert!(h.query_by_label(NEW_WORKTREE).is_none());
    assert!(calls(&h, "worktree_list").is_empty(), "no git read for an SSH session");
}

fn open_quick_on_web(h: &mut Harness<'static, csm_ui::CsmApp>) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(h);
    h.event(Event::Text("Odulodo.Web".into()));
    // Long enough for the folder to count as chosen (the read waits a moment after typing).
    for _ in 0..40 {
        h.step();
    }
    settle(h);
}

#[test]
fn quick_open_offers_sibling_worktrees_and_starts_the_ticked_ones_too() {
    let mut h = h();
    open_quick_and_check(&mut h);
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let news = calls(&h, "session_new");
    assert_eq!(news.len(), 2, "{news:?}");
    assert_eq!(news[0]["folder"], "C:\\GitHub\\Odulodo.Web");
    assert_eq!(news[1]["folder"], "C:\\GitHub\\Odulodo.Web\\.worktrees\\perf-budget");
    assert_eq!(news[1]["command"], news[0]["command"], "the same command");
    assert_eq!(news[1]["group"], news[0]["group"], "the same group");
    assert_eq!(news[1]["focus"], false, "the primary keeps the focus");
}

fn open_quick_and_check(h: &mut Harness<'static, csm_ui::CsmApp>) {
    open_quick_on_web(h);
    // The read went out by folder, once.
    assert_eq!(calls(h, "worktree_list"), vec![json!({"folder": "C:\\GitHub\\Odulodo.Web"})]);
    // Both other worktrees are offered (one already has a session; it's still a choice).
    assert!(h.query_by_label("Sibling worktree: search-sort").is_some());
    h.get_by_label("Sibling worktree: perf-budget").click();
    h.step();
    assert_eq!(h.state().ui.quick.siblings, vec!["C:\\GitHub\\Odulodo.Web\\.worktrees\\perf-budget".to_string()]);
}

#[test]
fn ctrl_enter_starts_the_ticked_siblings_once() {
    let mut h = h();
    open_quick_and_check(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Enter);
    settle(&mut h);
    assert_eq!(calls(&h, "session_new").len(), 2, "here and in the ticked sibling");
    assert!(h.state().ui.quick.siblings.is_empty(), "the ticks are spent");
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let news = calls(&h, "session_new");
    assert_eq!(news.len(), 3, "the second start is the folder only: {news:?}");
    assert_eq!(news[2]["folder"], "C:\\GitHub\\Odulodo.Web");
}

#[test]
fn quick_open_ticks_do_not_follow_to_another_folder() {
    let mut h = h();
    open_quick_and_check(&mut h);
    // Another folder: the ticks for Odulodo.Web are gone and it isn't a repo with worktrees.
    h.state_mut().ui.quick.query = "Odulodo.Admin".into();
    for _ in 0..40 {
        h.step();
    }
    settle(&mut h);
    assert!(h.state().ui.quick.siblings.is_empty());
    assert!(h.query_by_label("Sibling worktree: perf-budget").is_none());
}
