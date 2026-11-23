//! The title bar's "Update available" badge and Settings › General › Updates, with a fake check
//! result: on the demo backend (a result put in the model) and on a real engine whose GitHub is a
//! fake (`StateOptions::update`). No test touches the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use csm_engine::{Engine, State, StateOptions, UpdateOptions};
use csm_ui::model::UpdateBadge;
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::theme::ThemeKind;
use csm_ui::{Action, CsmApp, EngineBackend, EngineOptions};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

const URL: &str = "https://github.com/umage-ai/CodeShellManager/releases/tag/v2.1.0";

fn calls(h: &Harness<'static, CsmApp>, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

fn demo_with_update() -> Harness<'static, CsmApp> {
    let mut b = backend(ThemeKind::Mocha);
    b.model_mut().update = Some(UpdateBadge { version: "2.1.0".into(), url: URL.into() });
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    h
}

#[test]
fn the_badge_opens_the_release_page_through_open_url() {
    let mut h = demo_with_update();
    h.get_by_label("Update available: 2.1.0").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "open_url"), vec![json!({"url": URL})]);
    assert!(calls(&h, "update_dismiss").is_empty());
}

#[test]
fn the_badges_cross_dismisses_it() {
    let mut h = demo_with_update();
    h.get_by_label("Dismiss the update notice").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "update_dismiss"), vec![json!({"version": "2.1.0"})]);
    assert!(calls(&h, "open_url").is_empty());
}

#[test]
fn no_result_no_badge() {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    assert!(h.query_by_label_contains(csm_ui::screens::titlebar::UPDATE_BADGE_PREFIX).is_none());
}

#[test]
fn settings_has_the_switch_and_check_now() {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(&mut h);
    h.get_by_label_contains("Check for updates").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "settings_set").last().unwrap(), &json!({"key": "CheckForUpdates", "value": false}), "on by default, so a click turns it off");
    h.get_by_label("Check now").click_accesskit();
    settle(&mut h);
    assert_eq!(calls(&h, "update_check"), vec![json!({})]);
}

/// The real app over an engine whose GitHub answers `v2.1.0`; the running build is 2.0.0.
fn real_app() -> Harness<'static, CsmApp> {
    let fetch: csm_engine::UpdateFetch = Arc::new(|_: &str| Ok(r#"{"tag_name": "v2.1.0", "prerelease": false}"#.to_owned()));
    let options = StateOptions {
        pty: Arc::new(|_| Err(std::io::Error::other("no processes in this test"))),
        restore_on_start: false,
        update: UpdateOptions { fetch: Some(fetch), current: "2.0.0".into(), ..UpdateOptions::default() },
        ..StateOptions::default()
    };
    let engine = Engine::start(State::new(csm_core::models::AppState::default(), options), csm_ui::app_tools()).unwrap();
    let backend = EngineBackend::new(engine.clone(), EngineOptions::default());
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let app = CsmApp::new(Box::new(csm_ui::EnginePort::new(engine)), Box::new(backend), TerminalRenderer::new(fonts));
    Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app)
}

fn step_until(h: &mut Harness<'static, CsmApp>, what: &str, f: impl Fn(&Harness<'static, CsmApp>) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !f(h) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_check_on_the_real_engine_shows_the_badge_and_dismiss_hides_it() {
    let mut h = real_app();
    settle(&mut h);
    assert!(h.query_by_label_contains("Update available").is_none(), "no check yet");
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::Tool("update_check", json!({})));
    step_until(&mut h, "the badge", |h| h.query_by_label("Update available: 2.1.0").is_some());
    assert_eq!(h.state().model.update.as_ref().unwrap().url, URL);
    h.get_by_label("Dismiss the update notice").click_accesskit();
    step_until(&mut h, "the badge to go", |h| h.query_by_label_contains("Update available").is_none());
}
