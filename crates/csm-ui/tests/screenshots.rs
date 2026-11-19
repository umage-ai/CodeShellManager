//! Renders each screen of the mock-up offscreen (real wgpu renderer) to PNGs for review.
//! Output: $CSM_SCREENS_OUT or <target>/screens/. `docs/screens/` holds the committed set.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_ui::demo::DemoBackend;
use csm_ui::model::{InspectorTab, LayoutMode};
use csm_ui::theme::ThemeKind;
use csm_ui::Action;

fn out_dir() -> std::path::PathBuf {
    let d = std::env::var_os("CSM_SCREENS_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("screens"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn shoot(name: &str, theme: ThemeKind, setup: impl FnOnce(&mut DemoBackend), after: impl FnOnce(&mut egui_kittest::Harness<'static, csm_ui::CsmApp>)) {
    let mut b = backend(theme);
    setup(&mut b);
    b.publish();
    let mut h = harness_wgpu(app_with(b), 1.0);
    settle(&mut h);
    after(&mut h);
    settle(&mut h);
    let img = h.render().expect("render");
    let path = out_dir().join(format!("{name}.png"));
    img.save(&path).unwrap();
    eprintln!("wrote {}", path.display());
    let distinct: std::collections::HashSet<[u8; 4]> = img.pixels().map(|p| p.0).collect();
    assert!(distinct.len() > 100, "{name} looks blank");
}

#[test]
fn screens() {
    shoot("01-main", ThemeKind::Mocha, |_| {}, |_| {});
    shoot("01-main-latte", ThemeKind::Latte, |_| {}, |_| {});
    shoot("02-headers", ThemeKind::Mocha, |b| b.model_mut().group_mode = csm_ui::model::GroupMode::Headers, |_| {});
    shoot("03-free", ThemeKind::Mocha, |b| {
        let m = b.model_mut();
        m.active = Some("odulodo".into());
        if let Some(l) = m.layouts.get_mut("odulodo") { l.mode = LayoutMode::Free; }
    }, |_| {});
    shoot("04-overview", ThemeKind::Mocha, |b| {
        let m = b.model_mut();
        m.rail = csm_ui::model::Scope::All;
        if let Some(l) = m.layouts.get_mut("all") { l.mode = LayoutMode::Overview; }
    }, |_| {});
    shoot("05-grid", ThemeKind::Mocha, |b| {
        let m = b.model_mut();
        if let Some(l) = m.layouts.get_mut("odulodo") { l.mode = LayoutMode::Grid; l.grid = csm_ui::model::GridPreset::new(3, 2); }
    }, |_| {});
    shoot("06-inspector-runs", ThemeKind::Mocha, |b| b.model_mut().inspector = Some(InspectorTab::Runs), |_| {});
    shoot("06-inspector-git", ThemeKind::Mocha, |b| b.model_mut().inspector = Some(InspectorTab::Git), |_| {});
    shoot("06-inspector-session", ThemeKind::Mocha, |b| b.model_mut().inspector = Some(InspectorTab::Session), |_| {});
    shoot("06-inspector-notes", ThemeKind::Latte, |b| b.model_mut().inspector = Some(InspectorTab::Notes), |_| {});
    shoot("07-palette", ThemeKind::Mocha, |_| {}, |h| {
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::OpenPalette);
        h.state_mut().ui.palette.query = "odu".into();
    });
    shoot("08-new-session", ThemeKind::Mocha, |_| {}, |h| {
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
        h.state_mut().ui.quick.query = "odu".into();
    });
    shoot("08-new-session-remote", ThemeKind::Mocha, |_| {}, |h| {
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
        h.state_mut().ui.quick.remote = true;
    });
    shoot("09-menu-pane", ThemeKind::Mocha, |_| {}, |h| {
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Pane { session: "web".into(), at: egui::pos2(1270.0, 130.0) }));
        h.state_mut().ui.submenu = Some("Open with".into());
    });
    shoot("09-menu-row", ThemeKind::Mocha, |_| {}, |h| {
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Row { session: "webps".into(), at: egui::pos2(300.0, 330.0) }));
    });
    shoot("09-menu-group", ThemeKind::Mocha, |_| {}, |h| {
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Group { group: "odulodo".into(), at: egui::pos2(56.0, 90.0) }));
    });
    shoot("10-restore", ThemeKind::Mocha, |b| b.set_restore(3), |_| {});
    shoot("10-shutdown", ThemeKind::Mocha, |b| b.set_shutdown(9.0), |_| {});
    shoot("10-busy", ThemeKind::Mocha, |_| {}, |h| {
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::AskQuit);
    });
    shoot("11-inbox-closed", ThemeKind::Mocha, |b| b.model_mut().inbox_panel.open = false, |_| {});
}
