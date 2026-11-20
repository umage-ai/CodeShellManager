//! Run commands and Open with through the real UI (egui_kittest + AccessKit) on the demo backend:
//! clicks become tool calls against the real engine's schemas.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::model::InspectorTab;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn last_call(h: &Harness<'static, csm_ui::CsmApp>) -> (String, serde_json::Value) {
    let c = h.state().submitted().last().cloned().expect("a call was submitted");
    (c.id, c.params)
}

fn act(h: &mut Harness<'static, csm_ui::CsmApp>, a: Action) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, a);
    settle(h);
}

#[test]
fn the_pane_run_button_submits_run_start_for_the_default() {
    let mut h = h();
    h.get_by_label("Run dev · Odulodo.Web").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("run_start".to_string(), json!({"session_id": "web"})));
}

#[test]
fn the_runs_tab_lists_the_commands_and_starts_and_stops_them() {
    let mut h = h();
    act(&mut h, Action::Tool("inspector_show", json!({"tab": "runs"})));
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Runs));
    for label in ["Run command: dev", "Run command: test", "Run command: build"] {
        assert!(h.query_by_label(label).is_some(), "{label}");
    }
    // dev is running: its button stops it; build isn't: its button starts it.
    h.get_by_label("Stop dev").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("run_stop".to_string(), json!({"session_id": "web", "run_id": "dev"})));
    h.get_by_label("Run build").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("run_start".to_string(), json!({"session_id": "web", "run_id": "build"})));
}

#[test]
fn edit_commands_opens_the_editor_and_save_submits_run_edit() {
    let mut h = h();
    act(&mut h, Action::EditRuns("web".into()));
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Runs));
    h.get_by_label("Save").click();
    settle(&mut h);
    let (id, p) = last_call(&h);
    assert_eq!(id, "run_edit");
    assert_eq!(p["session_id"], "web");
    let runs = p["runs"].as_array().unwrap();
    assert_eq!(runs.len(), 3);
    assert_eq!(runs[0]["command_line"], "npm run dev");
    assert_eq!(runs.iter().filter(|r| r["is_default"] == true).count(), 1);
}

#[test]
fn the_open_with_button_opens_the_default_editor() {
    let mut h = h();
    let b = h.get_all_by_label_contains("Open in VS Code (Code.exe)").next().expect("the { } button");
    b.click();
    settle(&mut h);
    let (id, p) = last_call(&h);
    assert_eq!(id, "open_with");
    assert_eq!(p["entry"], "vscode");
}

#[test]
fn the_open_with_editor_saves_through_open_with_set() {
    let mut h = h();
    act(&mut h, Action::OpenWithEditor);
    assert!(csm_ui::screens::open_with::is_open(&h.ctx));
    for _ in 0..5 {
        h.step();
    }
    h.get_by_label("Save").click();
    settle(&mut h);
    let (id, p) = last_call(&h);
    assert_eq!(id, "open_with_set");
    assert_eq!(p["entries"].as_array().unwrap().len(), 3);
    assert!(!csm_ui::screens::open_with::is_open(&h.ctx));
}
