//! Themes from an add-on pack, end to end: a real engine reads the sample Themes pack from an
//! add-ons folder, Settings › Appearance lists its themes beside the built-ins, a click switches
//! the window's colours live, and turning the add-on off falls back to Mocha and says so.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use csm_engine::{Caller, Engine, State, StateOptions};
use csm_ui::app::{Action, SettingsSection};
use csm_ui::terminal::TerminalRenderer;
use csm_ui::terminal::fonts::{FontSet, embedded_mono};
use csm_ui::theme::Tokens;
use csm_ui::{CsmApp, EngineBackend, EngineOptions};
use egui::{Color32, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

type H = Harness<'static, CsmApp>;

fn until(h: &mut H, what: &str, f: impl Fn(&mut H) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !f(h) {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        h.step();
        std::thread::sleep(Duration::from_millis(5));
    }
    for _ in 0..3 {
        h.step();
    }
}

fn bg(h: &H) -> Color32 {
    Tokens::get(&h.ctx).bg
}

#[test]
fn appearance_lists_add_on_themes_and_switches_live() {
    let dir = tempfile::tempdir().unwrap();
    let folder = dir.path().join("addons").join("themes");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("addon.toml"), include_str!("../../../samples/addons/themes/addon.toml")).unwrap();
    let options = StateOptions { save_path: Some(dir.path().join("state.json")), restore_on_start: false, ..StateOptions::default() };
    let engine = Engine::start(State::new(csm_core::models::AppState::default(), options), csm_ui::app_tools()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while engine.view().addons.is_empty() {
        assert!(Instant::now() < deadline, "the add-on scan did not finish");
        std::thread::sleep(Duration::from_millis(5));
    }
    let list = engine.call(Caller::Ui, "addon_list", json!({})).unwrap().data["addons"].clone();
    let a = list.as_array().unwrap().iter().find(|a| a["id"] == "themes").unwrap().clone();
    engine.call(Caller::Ui, "addon_enable", json!({"id": "themes", "version": a["version"], "digest": a["digest"]})).unwrap();

    let backend = EngineBackend::new(engine.clone(), EngineOptions::default());
    let fonts = FontSet::new(embedded_mono().unwrap(), Vec::new(), csm_ui::theme::text::TERMINAL, true);
    let app = CsmApp::new(engine.clone(), Box::new(backend), TerminalRenderer::new(fonts));
    let mut h: H = Harness::builder().with_size(vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_ui_state(|ui, app: &mut CsmApp| app.frame(ui), app);
    let mocha = Tokens::for_kind(csm_ui::theme::ThemeKind::Mocha);
    until(&mut h, "the first frame", |h| bg(h) == mocha.bg);

    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    h.state_mut().ui.settings.section = SettingsSection::Appearance;
    until(&mut h, "the add-on's themes in Appearance", |h| h.query_by_label("Dark theme: Catppuccin Frappé").is_some());
    for label in ["Dark theme: Catppuccin Mocha", "Dark theme: Catppuccin Macchiato", "Dark theme: High contrast", "Light theme: Catppuccin Latte"] {
        assert!(h.query_by_label(label).is_some(), "{label} is listed");
    }
    assert!(h.query_by_label("Light theme: Catppuccin Frappé").is_none(), "a dark theme is not offered for light mode");

    // A click switches the window live.
    h.get_by_label("Dark theme: Catppuccin Frappé").click();
    until(&mut h, "Frappé's base", |h| bg(h) == Color32::from_rgb(0x30, 0x34, 0x46));
    let got = engine.call(Caller::Ui, "settings_get", json!({"key": "ThemeDark"})).unwrap().data["value"].clone();
    assert_eq!(got, "themes.frappe", "the engine has the choice");
    assert_eq!(Tokens::get(&h.ctx).fg, Color32::from_rgb(0xc6, 0xd0, 0xf5));
    assert!(Tokens::get(&h.ctx).dark());

    // Light mode draws the light choice (Latte), and back.
    engine.call(Caller::Ui, "theme_set", json!({"mode": "light"})).unwrap();
    until(&mut h, "Latte", |h| bg(h) == Tokens::for_kind(csm_ui::theme::ThemeKind::Latte).bg);
    engine.call(Caller::Ui, "theme_set", json!({"mode": "dark"})).unwrap();
    until(&mut h, "Frappé again", |h| bg(h) == Color32::from_rgb(0x30, 0x34, 0x46));

    // The add-on turned off: Mocha is drawn, and the page says why.
    engine.call(Caller::Ui, "addon_disable", json!({"id": "themes"})).unwrap();
    until(&mut h, "the fallback to Mocha", |h| bg(h) == mocha.bg);
    assert_eq!(Tokens::get(&h.ctx), mocha, "exactly the built-in");
    until(&mut h, "the fallback note", |h| h.query_by_label_contains("is not available").is_some());
    engine.shutdown();
}
