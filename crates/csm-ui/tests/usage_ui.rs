//! Claude usage through the real UI (egui_kittest + AccessKit) on the demo backend's fixture:
//! the inspector's Usage tab, Settings → Usage's cost per project, and that the demo (whose
//! numbers are a fixture) never polls the usage tools.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

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

fn has(h: &Harness<'static, csm_ui::CsmApp>, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

fn usage_calls(h: &Harness<'static, csm_ui::CsmApp>) -> usize {
    h.state().submitted().iter().filter(|c| c.id.starts_with("usage_") && c.id != "usage_stats").count()
}

fn show_usage_tab(h: &mut Harness<'static, csm_ui::CsmApp>) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("inspector_show", json!({"tab": "usage"})));
    settle(h);
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Usage));
}

#[test]
fn the_usage_tab_shows_the_focused_claude_sessions_numbers() {
    let mut h = h();
    show_usage_tab(&mut h);
    assert_eq!(h.state().model.active.as_deref(), Some("web"));
    // The tab strip has a Usage tab (icon-only when the panel is narrow; the name is its label).
    assert!(has(&h, "Inspector: Usage"));
    // The gauge, labelled for screen readers, and the fill in words.
    assert!(has(&h, "Context fill 74%"), "the gauge");
    assert!(has(&h, "148k of 200k · 74%"));
    assert!(has(&h, "claude-opus-5-5"));
    // Costs say what they are, and unpriced requests are never folded into $0.
    assert!(has(&h, "API-EQUIVALENT COST"));
    assert!(has(&h, "$18.42 + 3 unpriced"), "the total, with the unpriced requests counted");
    assert!(has(&h, "$4.87"), "today");
    assert!(has(&h, "$3.32 + 3 unpriced"), "subagents");
    assert!(h.query_by_label_contains("No price for qwen3.8:27b").is_some());
    assert!(has(&h, "93%"), "cache hits");
    assert!(has(&h, "96k written"), "the last cache miss");
    assert!(has(&h, "after 7m idle"));
    assert!(has(&h, "last 167k → 21k (auto)"), "the last compaction");
    assert!(h.query_by_label_contains("not what a subscription bills").is_some());
    // The demo's numbers are a fixture: nothing is polled, and there is no refresh.
    assert_eq!(usage_calls(&h), 0);
    assert!(!has(&h, "Refresh usage"));

    // Clicking the tab strip goes through `inspector_show` like the other tabs.
    h.get_by_label("Inspector: Notes").click();
    settle(&mut h);
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Notes));
    h.get_by_label("Inspector: Usage").click();
    settle(&mut h);
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Usage));
    assert!(h.state().submitted().iter().any(|c| c.id == "inspector_show" && c.params == json!({"tab": "usage"})));
}

#[test]
fn a_session_that_does_not_run_claude_says_so() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("session_focus", json!({"session_id": "webps"})));
    settle(&mut h);
    show_usage_tab(&mut h);
    assert_eq!(h.state().model.active.as_deref(), Some("webps"));
    assert!(h.query_by_label_contains("doesn't run claude").is_some());
    assert!(!has(&h, "API-EQUIVALENT COST"));
}

#[test]
fn a_claude_session_without_numbers_yet_waits_for_them() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("session_focus", json!({"session_id": "craft"})));
    settle(&mut h);
    show_usage_tab(&mut h);
    assert!(h.query_by_label_contains("Reading Claude's transcript").is_some());
}

#[test]
fn settings_usage_lists_cost_per_project_today_and_this_week() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(&mut h);
    h.get_by_label("Settings: Usage").click();
    settle(&mut h);
    assert!(has(&h, "Claude cost per project") || h.query_by_label_contains("CLAUDE COST PER PROJECT").is_some());
    assert!(has(&h, "umage/CodeShellManager-rs"));
    assert!(has(&h, "$7.90"), "today");
    assert!(has(&h, "$31.70"), "this week");
    assert!(has(&h, "$61.33"), "the week's total");
    assert!(h.query_by_label_contains("No price for qwen3.8:27b").is_some());
    assert_eq!(usage_calls(&h), 0, "the demo's summary is a fixture");
}
