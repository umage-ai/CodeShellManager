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
fn the_pane_run_button_on_a_running_run_shows_it_instead_of_restarting() {
    let mut h = h();
    // The demo's default run (dev) is running: the button says so and only shows the run.
    let before = h.state().submitted().len();
    h.get_by_label_contains("Running dev, show it").click();
    settle(&mut h);
    let calls: Vec<String> = h.state().submitted()[before..].iter().map(|c| c.id.clone()).collect();
    assert!(!calls.iter().any(|c| c.starts_with("run_")), "no run tool was called: {calls:?}");
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Runs), "the Runs tab shows it");
}

fn run_calls_since(h: &Harness<'static, csm_ui::CsmApp>, before: usize) -> Vec<(String, serde_json::Value)> {
    h.state().submitted()[before..].iter().filter(|c| c.id.starts_with("run_")).map(|c| (c.id.clone(), c.params.clone())).collect()
}

#[test]
fn the_runs_dropdown_shows_a_running_run_and_offers_stop_and_restart_separately() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Runs { session: "web".into(), at: egui::pos2(900.0, 60.0) }));
    for _ in 0..3 {
        h.step();
    }
    let before = h.state().submitted().len();
    h.get_by_label_contains("dev (running, show)").click();
    settle(&mut h);
    assert!(run_calls_since(&h, before).is_empty(), "clicking a running run starts and stops nothing");
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Runs));

    // Stop is its own row.
    act(&mut h, Action::Tool("inspector_show", json!({"tab": "hidden"})));
    h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Runs { session: "web".into(), at: egui::pos2(900.0, 60.0) }));
    for _ in 0..3 {
        h.step();
    }
    h.get_by_label_contains("Stop dev").click();
    settle(&mut h);
    assert_eq!(run_calls_since(&h, before), vec![("run_stop".to_string(), json!({"session_id": "web", "run_id": "dev"}))]);
}

#[test]
fn restart_asks_first_for_a_run_that_has_been_going_and_then_restarts() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Runs { session: "web".into(), at: egui::pos2(900.0, 60.0) }));
    for _ in 0..3 {
        h.step();
    }
    let before = h.state().submitted().len();
    h.get_by_label_contains("Restart dev").click();
    settle(&mut h);
    assert!(run_calls_since(&h, before).is_empty(), "asks first");
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::Confirm);
    let body = h.state().ui.confirm.as_ref().unwrap().body.clone();
    assert!(body.contains("dev") && body.contains("4 min"), "the card names the run and how long it ran: {body}");
    // Keep running: nothing happens.
    h.get_by_label_contains("Keep running").click();
    settle(&mut h);
    assert!(run_calls_since(&h, before).is_empty());
    // Ask again and confirm.
    act(&mut h, Action::Tool("run_restart", json!({"session_id": "web", "run_id": "dev"})));
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::Confirm);
    h.get_by_label("Restart").click();
    settle(&mut h);
    assert_eq!(run_calls_since(&h, before), vec![("run_restart".to_string(), json!({"session_id": "web", "run_id": "dev"}))]);
}

#[test]
fn the_inspector_row_of_a_running_run_selects_it_without_restarting_and_offers_restart() {
    let mut h = h();
    act(&mut h, Action::Tool("inspector_show", json!({"tab": "runs"})));
    let before = h.state().submitted().len();
    h.get_by_label("Run command: dev").click();
    settle(&mut h);
    assert!(run_calls_since(&h, before).is_empty());
    assert!(h.query_by_label_contains("Restart").is_some(), "the output panel offers Restart beside Stop");
}

#[test]
fn f5_on_a_running_default_run_shows_it_and_ctrl_shift_f5_asks_to_restart() {
    let mut h = h();
    let before = h.state().submitted().len();
    h.key_press(egui::Key::F5);
    settle(&mut h);
    assert!(run_calls_since(&h, before).is_empty(), "F5 only shows the running run");
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Runs));
    h.key_press_modifiers(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::F5);
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, csm_ui::app::Overlay::Confirm, "Ctrl+Shift+F5 asks before restarting");
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

/// The demo with `n` run commands on `web` (`cmd0` … , `cmd{n-1}` the default).
fn with_many_runs(n: usize) -> Harness<'static, csm_ui::CsmApp> {
    let mut b = backend(ThemeKind::Mocha);
    let web = b.model_mut().sessions.iter_mut().find(|s| s.id == "web").expect("the demo's web session");
    web.runs = (0..n)
        .map(|i| csm_ui::model::RunView {
            id: format!("cmd{i}"),
            label: format!("pkg{i}:build"),
            command_line: format!("pnpm run pkg{i}:build"),
            is_default: i + 1 == n,
            mode: "process".into(),
            ..Default::default()
        })
        .collect();
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    act(&mut h, Action::Tool("inspector_show", json!({"tab": "runs"})));
    h
}

#[test]
fn fifty_run_commands_scroll_and_the_output_stays_in_view() {
    let mut h = with_many_runs(50);
    let screen = h.ctx.content_rect();
    assert!(h.query_by_label("Run command: pkg0:build").is_some());
    // The list scrolls inside the tab: the output's buttons are still on screen.
    let copy = h.get_by_label("Copy").rect();
    assert!(copy.max.y <= screen.max.y, "the output section is pushed off screen: {copy:?}");
    // Scroll to one near the end (clipped until then) and start it.
    h.get_by_label("Run pkg48:build").scroll_to_me();
    settle(&mut h);
    h.get_by_label("Run pkg48:build").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("run_start".to_string(), json!({"session_id": "web", "run_id": "cmd48"})));
    // The editor shows all fifty rows (scrolling) and still saves them all.
    act(&mut h, Action::EditRuns("web".into()));
    h.get_by_label("Save").click();
    settle(&mut h);
    let (id, p) = last_call(&h);
    assert_eq!(id, "run_edit");
    assert_eq!(p["runs"].as_array().unwrap().len(), 50);
}

#[test]
fn import_from_project_saves_the_edits_then_submits_run_edit_import() {
    let mut h = h();
    act(&mut h, Action::EditRuns("web".into()));
    let before = h.state().submitted().len();
    h.get_by_label("Import from project").click();
    settle(&mut h);
    let calls: Vec<(String, serde_json::Value)> = h.state().submitted().iter().skip(before).map(|c| (c.id.clone(), c.params.clone())).collect();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0].0, "run_edit");
    assert_eq!(calls[0].1["runs"].as_array().unwrap().len(), 3, "the edits are saved first");
    assert_eq!(calls[1], ("run_edit".to_string(), json!({"session_id": "web", "import": true})));
}
