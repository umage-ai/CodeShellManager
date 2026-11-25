//! The New session dialog offers to create a typed folder that does not exist: a row on This PC,
//! one confirm, then `session_new { create_folder: true }`. The demo answers `folder_probe` from a
//! fixture (C:\GitHub exists; anything under it that the demo doesn't list is missing).
#![allow(clippy::unwrap_used, clippy::expect_used)]
// The fixture's folders are drive-letter paths, which only count as absolute on Windows.
#![cfg(windows)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::app::Overlay;
use csm_ui::theme::ThemeKind;
use egui::Event;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn open_with_typed(path: &str) -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(&mut h);
    h.event(Event::Text(path.into()));
    // Long enough for the typed path to rest, be probed and answered.
    for _ in 0..40 {
        h.step();
    }
    settle(&mut h);
    h
}

fn calls(h: &Harness<'static, csm_ui::CsmApp>, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

#[test]
fn a_missing_folder_shows_a_create_row_and_the_probe_asked_once() {
    let h = open_with_typed(r"C:\GitHub\ShapesByMe");
    assert_eq!(calls(&h, "folder_probe"), vec![json!({"path": r"C:\GitHub\ShapesByMe"})]);
    assert!(h.query_by_label(r"Create folder: C:\GitHub\ShapesByMe").is_some(), "the create row is there");
}

#[test]
fn enter_asks_before_creating_and_yes_starts_with_create_folder() {
    let mut h = open_with_typed(r"C:\GitHub\a\b");
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert!(calls(&h, "session_new").is_empty(), "nothing starts before the answer");
    assert_eq!(h.state().ui.overlay, Overlay::Confirm);
    let c = h.state().ui.confirm.clone().expect("a confirm");
    assert!(c.body.contains(r"a\b") && c.body.contains(r"C:\GitHub"), "{}", c.body);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, *c.yes);
    settle(&mut h);
    let news = calls(&h, "session_new");
    assert_eq!(news.len(), 1, "{news:?}");
    assert_eq!(news[0]["folder"], r"C:\GitHub\a\b");
    assert_eq!(news[0]["create_folder"], true);
}

#[test]
fn a_folder_that_exists_or_cannot_be_made_has_no_create_row() {
    let h = open_with_typed(r"C:\GitHub\Odulodo.Web");
    assert!(h.query_by_label(r"Create folder: C:\GitHub\Odulodo.Web").is_none());
    let h = open_with_typed(r"Q:\nowhere\x");
    assert!(h.query_by_label(r"Create folder: Q:\nowhere\x").is_none());
}
