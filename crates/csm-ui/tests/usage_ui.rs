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

/// Settles, lets the dashboard notice what changed and ask, and settles again.
fn go(h: &mut Harness<'static, csm_ui::CsmApp>) {
    for _ in 0..3 {
        settle(h);
    }
}

fn last_report_params(h: &Harness<'static, csm_ui::CsmApp>) -> serde_json::Value {
    h.state().submitted().iter().rev().find(|c| c.id == "usage_report").map(|c| c.params.clone()).unwrap_or_default()
}

fn report_cost(h: &Harness<'static, csm_ui::CsmApp>) -> f64 {
    h.state().model.usage.report.as_ref().map_or(0.0, |r| r.report.total.cost_usd)
}

#[test]
fn the_usage_dashboard_switches_ranges_and_drills_down() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenUsage(None));
    go(&mut h);
    assert!(h.state().ui.usage_page.open);
    assert!(has(&h, "Claude usage"));
    assert_eq!(last_report_params(&h), json!({"from": "7d", "group_by": "session"}));
    let week = report_cost(&h);
    assert!(week > 0.0, "the demo answers from its history");
    assert!(has(&h, &csm_core::claude_usage::format_usd(week)), "the cost figure");
    for figure in ["TOKENS", "CACHE HIT RATE", "CACHE-MISS COST", "SAVED BY CACHING"] {
        assert!(has(&h, figure), "{figure}");
    }
    assert!(h.query_by_label_contains("Cost per day, 7 days").is_some(), "the chart, labelled");

    // A longer range: asked again, more cost, more days.
    h.get_by_label("Range: 30 days").click();
    go(&mut h);
    assert_eq!(last_report_params(&h)["from"], "30d");
    assert!(report_cost(&h) > week);
    assert!(h.query_by_label_contains("Cost per day, 30 days").is_some());
    let month = report_cost(&h);

    // A session: its models (and its days).
    h.get_by_label("Usage row: CodeShellManager-rs").click();
    go(&mut h);
    assert_eq!(last_report_params(&h), json!({"from": "30d", "group_by": "model", "session": "rs"}));
    assert!(has(&h, "Usage row: opus-5-5") && has(&h, "Usage row: sonnet-5-5"));
    assert!(report_cost(&h) < month);
    assert!(has(&h, "All usage"), "the breadcrumb leads back");
    assert_eq!(h.state().ui.usage_page.drill.len(), 1);
    h.get_by_label("Group by: Days").click();
    go(&mut h);
    assert_eq!(last_report_params(&h)["group_by"], "day");

    // Back to everything, then a model: its sessions.
    h.get_by_label("All usage").click();
    go(&mut h);
    assert_eq!(last_report_params(&h), json!({"from": "30d", "group_by": "session"}));
    h.get_by_label("Group by: Models").click();
    go(&mut h);
    h.get_by_label("Usage row: sonnet-5-5").click();
    go(&mut h);
    assert_eq!(last_report_params(&h), json!({"from": "30d", "group_by": "session", "model": "claude-sonnet-5-5"}));
    let rows: Vec<String> = h.state().model.usage.report.as_ref().unwrap().report.rows.iter().map(|r| r.label.clone()).collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(has(&h, "Usage row: Odulodo"));

    // Sorting is local: no new call.
    let calls = h.state().submitted().len();
    h.get_by_label("Sort by Requests").click();
    go(&mut h);
    assert_eq!(h.state().ui.usage_page.sort.0, csm_ui::screens::usage_page::SortCol::Requests);
    assert_eq!(h.state().submitted().len(), calls);

    // A custom range: a bad date is caught before it is sent.
    h.get_by_label("Range: Custom").click();
    go(&mut h);
    h.state_mut().ui.usage_page.custom_from = "2026-11-2".into();
    h.get_by_label("Apply").click();
    go(&mut h);
    assert!(h.query_by_label_contains("YYYY-MM-DD").is_some());
    h.state_mut().ui.usage_page.custom_from = "2026-11-20".into();
    h.state_mut().ui.usage_page.custom_to = "2026-11-21".into();
    h.get_by_label("Apply").click();
    go(&mut h);
    assert_eq!(last_report_params(&h)["from"], "2026-11-20");
    assert_eq!(last_report_params(&h)["to"], "2026-11-21");
    assert_eq!(h.state().model.usage.report.as_ref().unwrap().report.days.len(), 2);

    // Esc closes it; nothing toasted.
    h.key_press(egui::Key::Escape);
    go(&mut h);
    assert!(!h.state().ui.usage_page.open);
    assert!(h.state().ui.toasts.is_empty());
}

#[test]
fn the_usage_tab_opens_the_dashboard_on_its_session() {
    let mut h = h();
    show_usage_tab(&mut h);
    h.get_by_label("Usage by model and day").click();
    go(&mut h);
    assert!(h.state().ui.usage_page.open);
    assert_eq!(last_report_params(&h), json!({"from": "7d", "group_by": "model", "session": "web"}));
    assert!(has(&h, "Usage row: opus-5-5"));
    // The palette's entry is the tool.
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::CloseUsage);
    go(&mut h);
    assert!(!h.state().ui.usage_page.open);
}
