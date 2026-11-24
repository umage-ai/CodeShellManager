//! The theme-fun sample pack (Emacs, Windows 98, Blossom) drawn with its `[theme.style]`: corners,
//! 3D borders, title bar colours and fonts. Output: $CSM_SCREENS_OUT or <target>/screens/.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::theme::ThemeKind;

type H = egui_kittest::Harness<'static, csm_ui::CsmApp>;

fn pack() -> csm_engine::addons::AddonSummary {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("samples").join("addons").join("theme-fun");
    let text = std::fs::read_to_string(dir.join("addon.toml")).unwrap();
    let parsed = csm_core::addons::parse(&text, "theme-fun").unwrap();
    // What the engine's scan worker does: read and check each font the themes name.
    let mut fonts = Vec::new();
    for t in &parsed.themes {
        for rel in t.style.font_files() {
            if !fonts.iter().any(|(p, _): &(String, _)| p == rel) {
                let bytes = csm_services::addons::read_theme_font(&dir, rel).unwrap();
                fonts.push((rel.to_owned(), csm_engine::addons::FontBytes(std::sync::Arc::new(bytes))));
            }
        }
    }
    csm_engine::addons::AddonSummary {
        id: "theme-fun".into(),
        name: "Fun themes".into(),
        version: "0.1.0".into(),
        description: String::new(),
        kind: "pack",
        author: None,
        homepage: None,
        state: csm_engine::addons::AddonState::Loaded,
        message: None,
        folder: "theme-fun".into(),
        path: String::new(),
        digest: None,
        profiles: vec![],
        run_templates: vec![],
        slash: vec![],
        commands: vec![],
        process_effects: vec![],
        process_command_line: None,
        new_sessions: Vec::new(),
        process_calls: vec![],
        process: None,
        themes: parsed.themes,
        theme_fonts: fonts,
        problems: vec![],
    }
}

fn out_dir() -> std::path::PathBuf {
    let d = std::env::var_os("CSM_SCREENS_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("screens"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn choose(h: &mut H, mode: ThemeKind, id: &str) {
    let engine = h.state().dispatch.engine().expect("an engine port");
    let key = if mode == ThemeKind::Mocha { "ThemeDark" } else { "ThemeLight" };
    engine.call(csm_engine::Caller::Ui, "settings_set", serde_json::json!({"key": key, "value": id})).unwrap();
    for _ in 0..8 {
        h.step();
    }
}

fn shoot(name: &str, mode: ThemeKind, id: &str, scene: impl FnOnce(&mut H)) -> usize {
    let mut b = backend(mode);
    b.model_mut().addons = vec![pack()];
    b.publish();
    let mut app = app_with(b);
    app.window_controls = true;
    let mut h = harness_wgpu_sized(app, 1.0, egui::vec2(1280.0, 800.0));
    settle(&mut h);
    choose(&mut h, mode, id);
    settle(&mut h);
    scene(&mut h);
    settle(&mut h);
    let img = h.render().expect("render");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).unwrap();
    eprintln!("wrote {}", path.display());
    img.pixels().map(|p| p.0).collect::<std::collections::HashSet<[u8; 4]>>().len()
}

fn settings(h: &mut H) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::Appearance;
}

fn palette(h: &mut H) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenPalette);
}

#[test]
fn fun_themes_render() {
    let themes = [("emacs", ThemeKind::Mocha, "theme-fun.emacs"), ("win98", ThemeKind::Latte, "theme-fun.win98"), ("blossom", ThemeKind::Latte, "theme-fun.blossom")];
    for (name, mode, id) in themes {
        let main = shoot(&format!("31-{name}-main"), mode, id, |_| {});
        shoot(&format!("31-{name}-settings"), mode, id, settings);
        shoot(&format!("31-{name}-palette"), mode, id, palette);
        assert!(main > 100, "{name} looks blank");
    }
}
