//! The standing warning strip under the title bar: a failing save, a read-only start, an import
//! offer. Drawn while the engine reports it; its buttons run tools.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_ui::model::{WarningButton, WarningRow};
use csm_ui::theme::ThemeKind;
use csm_ui::CsmApp;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn with(rows: Vec<WarningRow>) -> Harness<'static, CsmApp> {
    let mut b = backend(ThemeKind::Mocha);
    b.model_mut().warnings = rows;
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    h
}

fn calls(h: &Harness<'static, CsmApp>, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

#[test]
fn a_failing_save_is_shown_until_it_is_gone() {
    let text = "Your sessions are NOT being saved (disk full).";
    let h = with(vec![WarningRow { id: "state_save_failed".into(), text: text.into(), action: None, dismiss: None }]);
    h.get_by_label(text);
    let h = with(vec![]);
    assert!(h.query_by_label_contains("NOT being saved").is_none());
}

#[test]
fn the_import_offer_buttons_run_the_tool() {
    let row = WarningRow {
        id: "beta_import".into(),
        text: "Found sessions from a CodeShellManager beta on this computer.".into(),
        action: Some(WarningButton { label: "Import from the beta".into(), tool: "state_import_beta", params: json!({}) }),
        dismiss: Some(WarningButton { label: "No thanks".into(), tool: "state_import_beta", params: json!({"dismiss": true}) }),
    };
    let mut h = with(vec![row]);
    h.get_by_label("Import from the beta").click_accesskit();
    settle(&mut h);
    h.get_by_label("No thanks").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "state_import_beta"), vec![json!({}), json!({"dismiss": true})]);
}
