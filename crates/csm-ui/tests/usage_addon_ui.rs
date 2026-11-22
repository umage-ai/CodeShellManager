//! The built-in Claude usage add-on in the UI (demo backend): it is listed in Settings › Add-ons,
//! and the Usage tab, the dashboard, its palette entry and the Claude part of Settings › Usage
//! hide while it is off and come back when it is turned on, through `addon_disable` /
//! `addon_enable` like any add-on. The sidebar's context fill stays.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::*;
use csm_engine::addons::{BUILTIN_DIGEST, BUILTIN_VERSION, CLAUDE_USAGE_ID};
use csm_ui::Action;
use csm_ui::model::InspectorTab;
use csm_ui::screens::palette::{PalRow, rows};
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

fn run(h: &mut Harness<'static, csm_ui::CsmApp>, a: Action) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, a);
    settle(h);
}

fn palette_has_dashboard(h: &Harness<'static, csm_ui::CsmApp>) -> bool {
    rows(h.state(), ">").iter().any(|r| matches!(r, PalRow::Tool { id, .. } if id == "usage_report"))
        || rows(h.state(), "usage report").iter().any(|r| matches!(r, PalRow::Tool { id, .. } if id == "usage_report"))
}

fn set_usage(h: &mut Harness<'static, csm_ui::CsmApp>, on: bool) {
    if on {
        run(h, Action::Tool("addon_enable", json!({"id": CLAUDE_USAGE_ID, "version": BUILTIN_VERSION, "digest": BUILTIN_DIGEST})));
    } else {
        run(h, Action::Tool("addon_disable", json!({"id": CLAUDE_USAGE_ID})));
    }
    assert_eq!(h.state().model.claude_usage_on(), on);
}

#[test]
fn the_usage_screens_hide_while_the_add_on_is_off_and_come_back_when_it_is_on() {
    let mut h = h();
    // The demo has it on: the tab, the palette's dashboard entry.
    run(&mut h, Action::Tool("inspector_show", json!({"tab": "usage"})));
    assert!(has(&h, "Inspector: Usage"));
    assert!(palette_has_dashboard(&h));
    let chips_before = h.state().model.usage.chips.len();
    assert!(chips_before > 0);

    set_usage(&mut h, false);
    assert!(!has(&h, "Inspector: Usage"), "the tab is gone");
    assert!(has(&h, "Inspector: Notes"), "the inspector falls back to Notes");
    assert!(!has(&h, "Context fill 74%"));
    assert!(!palette_has_dashboard(&h), "the palette leaves the dashboard out");
    // The dashboard does not open; it says why.
    run(&mut h, Action::OpenUsage(None));
    assert!(!h.state().ui.usage_page.open);
    assert!(h.state().ui.toasts.iter().any(|t| t.text.contains("Claude usage is off")));
    // The sidebar's chips stay (the context fill does not need the add-on).
    assert_eq!(h.state().model.usage.chips.len(), chips_before);
    // Settings › Usage points to Settings › Add-ons instead of the cost per project.
    run(&mut h, Action::OpenSettings);
    h.get_by_label("Settings: Usage").click();
    settle(&mut h);
    assert!(h.query_by_label_contains("It is a built-in add-on").is_some());
    assert!(!has(&h, "Open the usage dashboard"));
    assert!(!has(&h, "Read past Claude history"));
    h.get_by_label("Go to Add-ons").click();
    settle(&mut h);
    // Listed with the add-ons, built in, with Enable….
    assert!(h.query_by_label_contains("built in · disabled").is_some());
    h.get_by_label("Enable…").click();
    settle(&mut h);
    assert!(h.query_by_label_contains("Built into the app").is_some(), "the consent dialog says what it is");
    run(&mut h, Action::CloseSettings);

    set_usage(&mut h, true);
    run(&mut h, Action::Tool("inspector_show", json!({"tab": "usage"})));
    assert!(has(&h, "Inspector: Usage"));
    assert_eq!(h.state().model.inspector, Some(InspectorTab::Usage));
    assert!(has(&h, "Context fill 74%"));
    assert!(palette_has_dashboard(&h));
    run(&mut h, Action::OpenUsage(None));
    assert!(h.state().ui.usage_page.open);

    // Turned off while the dashboard is open: it closes.
    set_usage(&mut h, false);
    assert!(!h.state().ui.usage_page.open);
}

#[test]
fn settings_usage_offers_the_history_switch_while_the_add_on_is_on() {
    let mut h = h();
    run(&mut h, Action::OpenSettings);
    h.get_by_label("Settings: Usage").click();
    settle(&mut h);
    assert!(has(&h, "Read past Claude history"));
    assert!(!has(&h, "Forget imported history…"), "only while the history setting is off");
    h.get_by_label("Read past Claude history").click();
    settle(&mut h);
    let set: Vec<_> = h.state().submitted().iter().filter(|c| c.id == "settings_set").map(|c| c.params.clone()).collect();
    assert!(set.contains(&json!({"key": "ClaudeUsageHistory", "value": false})), "{set:?}");
}
