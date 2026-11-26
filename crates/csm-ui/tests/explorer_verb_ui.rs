//! Explorer's "Open in CodeShellManager" in the window: `session_new_dialog` (what a second launch
//! of `codeshell --open-folder` calls) opens the New session dialog with the folder typed in and
//! starts nothing; Settings has the switch for the per-user entry. A real engine with an in-memory
//! registry, so nothing real is touched.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::app::Overlay;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn h() -> Harness<'static, csm_ui::CsmApp> {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    h
}

fn calls(h: &Harness<'static, csm_ui::CsmApp>, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

#[test]
fn the_dialog_tool_opens_the_new_session_dialog_on_the_folder_and_starts_nothing() {
    let mut h = h();
    let root = tempfile::tempdir().unwrap();
    let engine = h.state().dispatch.engine().expect("an engine port");
    // As the second process calls it: the CLI caller, through the engine's own tool.
    engine.call(csm_engine::Caller::Cli, "session_new_dialog", json!({"folder": root.path().display().to_string()})).unwrap();
    for _ in 0..20 {
        h.step();
    }
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::QuickOpen, "the New session dialog is open");
    assert!(h.state().ui.quick.edit.is_none(), "a new session, not an edit");
    assert_eq!(std::path::Path::new(&h.state().ui.quick.query), root.path());
    assert!(calls(&h, "session_new").is_empty(), "nothing starts until the person confirms in the dialog");
}

#[test]
fn a_refused_folder_opens_no_dialog() {
    let mut h = h();
    let engine = h.state().dispatch.engine().expect("an engine port");
    let r = engine.call(csm_engine::Caller::Cli, "session_new_dialog", json!({"folder": "shell:Downloads"}));
    assert!(r.is_err());
    for _ in 0..20 {
        h.step();
    }
    settle(&mut h);
    assert_ne!(h.state().ui.overlay, Overlay::QuickOpen);
    assert!(calls(&h, "session_new").is_empty());
}

#[test]
fn the_palette_entry_of_the_tool_opens_the_dialog_without_a_folder() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::PaletteTool("session_new_dialog".into()));
    settle(&mut h);
    assert_eq!(h.state().ui.overlay, Overlay::QuickOpen);
    assert!(h.state().ui.quick.query.is_empty());
}

#[test]
fn the_settings_switch_reads_the_registry_and_calls_the_tool() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(&mut h);
    assert_eq!(calls(&h, "app_explorer_menu_get").len(), 1, "asked when Settings opens");
    assert_eq!(h.state().ui.settings.explorer_menu["shown"], true);
    h.get_by_label_contains("in Explorer's right-click menu").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "app_explorer_menu_set"), vec![json!({"shown": false})]);
    assert_eq!(h.state().ui.settings.explorer_menu["shown"], false);
    h.get_by_label_contains("in Explorer's right-click menu").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "app_explorer_menu_set").last().unwrap(), &json!({"shown": true}));
}

#[test]
fn the_switch_is_hidden_where_there_is_no_explorer() {
    let mut h = h();
    h.state_mut().ui.settings.explorer_menu = json!({"supported": false});
    let ctx = h.ctx.clone();
    h.state_mut().ui.settings.open = true;
    let _ = ctx;
    settle(&mut h);
    assert!(h.query_by_label_contains("in Explorer's right-click menu").is_none());
}
