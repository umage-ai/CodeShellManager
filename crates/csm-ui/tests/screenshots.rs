//! Renders each screen of the mock-up offscreen (real wgpu renderer) to PNGs for review.
//! Output: $CSM_SCREENS_OUT or <target>/screens/. `docs/screens/` holds the committed set.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_ui::Action;
use csm_ui::demo::DemoBackend;
use csm_ui::model::{InspectorTab, LayoutMode};
use csm_ui::theme::ThemeKind;

fn out_dir() -> std::path::PathBuf {
    let d = std::env::var_os("CSM_SCREENS_OUT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..").join("..").join("target").join("screens"));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn shoot(
    name: &str,
    theme: ThemeKind,
    setup: impl FnOnce(&mut DemoBackend),
    after: impl FnOnce(&mut egui_kittest::Harness<'static, csm_ui::CsmApp>),
) {
    shoot_sized(name, theme, egui::vec2(1280.0, 800.0), setup, after);
}

fn shoot_sized(
    name: &str,
    theme: ThemeKind,
    size: egui::Vec2,
    setup: impl FnOnce(&mut DemoBackend),
    after: impl FnOnce(&mut egui_kittest::Harness<'static, csm_ui::CsmApp>),
) {
    shoot_host(name, theme, size, None, setup, after);
}

fn shoot_host(
    name: &str,
    theme: ThemeKind,
    size: egui::Vec2,
    host: Option<csm_ui::demo::peer::DemoPeerHost>,
    setup: impl FnOnce(&mut DemoBackend),
    after: impl FnOnce(&mut egui_kittest::Harness<'static, csm_ui::CsmApp>),
) {
    let mut b = backend(theme);
    setup(&mut b);
    b.publish();
    let mut app = match &host {
        Some(host) => app_with_peer(b, host),
        None => app_with(b),
    };
    // As on Windows: one title bar, with the window's own minimize, maximize and close.
    app.window_controls = true;
    app.background.tray_available = true;
    let mut h = harness_wgpu_sized(app, 1.0, size);
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
    shoot(
        "03-free",
        ThemeKind::Mocha,
        |b| {
            let m = b.model_mut();
            m.active = Some("odulodo".into());
            if let Some(l) = m.layouts.get_mut("odulodo") {
                l.mode = LayoutMode::Free;
            }
        },
        |_| {},
    );
    shoot(
        "04-overview",
        ThemeKind::Mocha,
        |b| {
            let m = b.model_mut();
            m.rail = csm_ui::model::Scope::All;
            if let Some(l) = m.layouts.get_mut("all") {
                l.mode = LayoutMode::Overview;
            }
        },
        |_| {},
    );
    shoot(
        "05-grid",
        ThemeKind::Mocha,
        |b| {
            let m = b.model_mut();
            if let Some(l) = m.layouts.get_mut("odulodo") {
                l.mode = LayoutMode::Grid;
                l.grid = csm_ui::model::GridPreset::new(3, 2);
            }
        },
        |_| {},
    );
    shoot("06-inspector-runs", ThemeKind::Mocha, |b| b.model_mut().inspector = Some(InspectorTab::Runs), |_| {});
    shoot(
        "06b-inspector-runs-many",
        ThemeKind::Mocha,
        |b| {
            let m = b.model_mut();
            m.inspector = Some(InspectorTab::Runs);
            if let Some(web) = m.sessions.iter_mut().find(|s| s.id == "web") {
                web.runs = (0..50)
                    .map(|i| csm_ui::model::RunView {
                        id: format!("cmd{i}"),
                        label: format!("pkg{i}:build"),
                        command_line: format!("pnpm run pkg{i}:build"),
                        is_default: i == 0,
                        mode: "process".into(),
                        ..Default::default()
                    })
                    .collect();
            }
        },
        |_| {},
    );
    shoot("06-inspector-git", ThemeKind::Mocha, |b| b.model_mut().inspector = Some(InspectorTab::Git), |_| {});
    shoot("06-inspector-session", ThemeKind::Mocha, |b| b.model_mut().inspector = Some(InspectorTab::Session), |_| {});
    shoot("06-inspector-notes", ThemeKind::Latte, |b| b.model_mut().inspector = Some(InspectorTab::Notes), |_| {});
    shoot("06-inspector-usage", ThemeKind::Mocha, |b| b.model_mut().inspector = Some(InspectorTab::Usage), |_| {});
    shoot("06-inspector-usage-latte", ThemeKind::Latte, |b| b.model_mut().inspector = Some(InspectorTab::Usage), |_| {});
    shoot("20-inspector-docs", ThemeKind::Mocha, |b| b.model_mut().inspector = Some(InspectorTab::Docs), |_| {});
    shoot("20-inspector-docs-latte", ThemeKind::Latte, |b| b.model_mut().inspector = Some(InspectorTab::Docs), |_| {});
    shoot(
        "07-palette",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenPalette);
            h.state_mut().ui.palette.query = "odu".into();
        },
    );
    shoot(
        "08-new-session",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
            h.state_mut().ui.quick.query = "odu".into();
        },
    );
    shoot(
        "08-new-session-remote",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
            h.state_mut().ui.quick.remote = true;
        },
    );
    shoot(
        "09-menu-pane",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Pane { session: "web".into(), at: egui::pos2(1270.0, 130.0) }));
            h.state_mut().ui.submenu = Some("Open with".into());
        },
    );
    shoot(
        "09-menu-row",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Row { session: "webps".into(), at: egui::pos2(300.0, 330.0) }));
        },
    );
    shoot(
        "09-menu-group",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenMenu(csm_ui::app::Menu::Group { group: "odulodo".into(), at: egui::pos2(56.0, 90.0) }));
        },
    );
    shoot("10-restore", ThemeKind::Mocha, |b| b.set_restore(3), |_| {});
    shoot("10-shutdown", ThemeKind::Mocha, |b| b.set_shutdown(9.0), |_| {});
    shoot(
        "10-busy",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::AskQuit);
        },
    );
    // The title bar's Close under the pointer, then closing the window (CloseAction Ask).
    for (name, theme) in [("01-main-close-hover", ThemeKind::Mocha), ("01-main-close-hover-latte", ThemeKind::Latte)] {
        let at = egui::pos2(1280.0 - csm_ui::theme::size::CAPTION_BUTTON_W / 2.0, csm_ui::theme::size::TITLE_BAR / 2.0);
        shoot(name, theme, |_| {}, |h| h.hover_at(at));
    }
    for (name, theme) in [("10-close-ask", ThemeKind::Mocha), ("10-close-ask-latte", ThemeKind::Latte)] {
        shoot(
            name,
            theme,
            |_| {},
            |h| {
                h.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
                h.step();
            },
        );
    }
    shoot("11-inbox-closed", ThemeKind::Mocha, |b| b.model_mut().inbox_panel.open = false, |_| {});
    // The Recent menu (Ctrl+Shift+E), with one session closed so "Recently closed" shows.
    for (name, theme) in [("18-recent", ThemeKind::Mocha), ("18-recent-latte", ThemeKind::Latte)] {
        shoot(
            name,
            theme,
            |_| {},
            |h| {
                h.key_press_modifiers(egui::Modifiers::CTRL | egui::Modifiers::COMMAND, egui::Key::W);
                settle(h);
                h.key_press_modifiers(egui::Modifiers::CTRL | egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::E);
            },
        );
    }
    shoot(
        "13-settings-general",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenSettings);
        },
    );
    shoot(
        "13-settings-about-latte",
        ThemeKind::Latte,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenSettings);
            h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::About;
        },
    );
    shoot(
        "13-settings-appearance",
        ThemeKind::Latte,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenSettings);
            h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::Appearance;
        },
    );
    shoot(
        "13-settings-mcp",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenSettings);
            h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::Mcp;
        },
    );
    shoot(
        "13-settings-about",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenSettings);
            h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::About;
        },
    );
    shoot(
        "14-edit-session",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::EditSession("web".into()));
        },
    );
    shoot(
        "14-edit-session-ssh",
        ThemeKind::Latte,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::Tool("view_set", serde_json::json!({"rail_tab": "ops"})));
            h.state_mut().run(&ctx, Action::EditSession("build".into()));
        },
    );
    shoot(
        "15-help",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::ShowHelp);
        },
    );
    shoot(
        "15-rename-in-place",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::StartRename("web".into()));
        },
    );
    shoot_sized("16-compact-narrow", ThemeKind::Mocha, egui::vec2(900.0, 640.0), |_| {}, |_| {});
    shoot_sized(
        "16-compact-peek",
        ThemeKind::Latte,
        egui::vec2(900.0, 640.0),
        |_| {},
        |h| {
            h.state_mut().ui.peek_sidebar = true;
        },
    );
    shoot(
        "17-empty-group",
        ThemeKind::Mocha,
        |b| {
            let m = b.model_mut();
            m.groups.push(csm_ui::model::GroupView { id: "client".into(), name: "Client X".into(), torn_off: false, collapsed: false });
            m.rail = csm_ui::model::Scope::Group("client".into());
        },
        |_| {},
    );
    shoot(
        "17-first-run",
        ThemeKind::Latte,
        |b| {
            let m = b.model_mut();
            m.sessions.clear();
            m.groups.clear();
            m.rail = csm_ui::model::Scope::All;
            m.active = None;
        },
        |_| {},
    );
    // A group in its own window (a deferred viewport; the test harness embeds it as a window).
    shoot(
        "12-torn-group",
        ThemeKind::Mocha,
        |b| {
            let m = b.model_mut();
            m.group_mode = csm_ui::model::GroupMode::Headers;
            if let Some(g) = m.groups.iter_mut().find(|g| g.id == "ops") {
                g.torn_off = true;
            }
        },
        |_| {},
    );
}

/// The Usage dashboard (the demo answers `usage_report` from its request history).
#[test]
fn usage_dashboard_screens() {
    use csm_ui::screens::usage_page::{Drill, Range};
    shoot(
        "21-usage",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenUsage(None));
        },
    );
    shoot(
        "21-usage-30d-latte",
        ThemeKind::Latte,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenUsage(None));
            h.state_mut().ui.usage_page.range = Range::Month;
        },
    );
    shoot(
        "21-usage-session",
        ThemeKind::Mocha,
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenUsage(Some(Drill::Session { key: "rs".into(), label: "CodeShellManager-rs".into() })));
            h.state_mut().ui.usage_page.range = Range::Month;
        },
    );
    shoot_sized(
        "21-usage-narrow-latte",
        ThemeKind::Latte,
        egui::vec2(900.0, 700.0),
        |_| {},
        |h| {
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenUsage(None));
            h.state_mut().ui.usage_page.group = Some(csm_core::usage_report::GroupBy::Model);
        },
    );
}

/// The sample Themes pack, enabled, as the engine would publish it.
fn themes_pack() -> csm_engine::addons::AddonSummary {
    let text = include_str!("../../../samples/addons/themes/addon.toml");
    let parsed = csm_core::addons::parse(text, "themes").unwrap();
    csm_engine::addons::AddonSummary {
        id: "themes".into(),
        name: "Themes".into(),
        version: "0.1.0".into(),
        description: String::new(),
        kind: "pack",
        author: None,
        homepage: None,
        state: csm_engine::addons::AddonState::Loaded,
        message: None,
        folder: "themes".into(),
        path: String::new(),
        digest: None,
        profiles: vec![],
        run_templates: vec![],
        slash: vec![],
        commands: vec![],
        process_effects: vec![],
        process_command_line: None,
        process_calls: vec![],
        process: None,
        themes: parsed.themes,
        problems: vec![],
    }
}

/// Add-on themes (samples/addons/themes): the main window and Settings › Appearance.
#[test]
fn theme_screens() {
    for (name, id) in [("21-theme-frappe", "themes.frappe"), ("21-theme-macchiato", "themes.macchiato"), ("21-theme-high-contrast", "themes.high-contrast")] {
        shoot(name, ThemeKind::Mocha, |b| b.model_mut().addons = vec![themes_pack()], |h| dark_theme(h, id));
    }
    shoot(
        "21-theme-settings",
        ThemeKind::Mocha,
        |b| b.model_mut().addons = vec![themes_pack()],
        |h| {
            dark_theme(h, "themes.frappe");
            let ctx = h.ctx.clone();
            h.state_mut().run(&ctx, Action::OpenSettings);
            h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::Appearance;
        },
    );
}

/// Chooses the dark theme the way another client would (`settings_set`; the demo engine has no
/// add-ons, so `theme_set` would refuse an add-on id): the app follows the change event.
fn dark_theme(h: &mut egui_kittest::Harness<'static, csm_ui::CsmApp>, id: &str) {
    let engine = h.state().dispatch.engine().expect("an engine port");
    engine.call(csm_engine::Caller::Ui, "settings_set", serde_json::json!({"key": "ThemeDark", "value": id})).unwrap();
    for _ in 0..5 {
        h.step();
    }
}

/// Remote access: Settings on every step, with the title bar indicator in each state it has.
#[test]
fn remote_access_screens() {
    use csm_ui::demo::peer::DemoPeerHost;
    use csm_ui::screens::remote::TOGGLE_LABEL;
    use egui_kittest::kittest::Queryable;

    type H = egui_kittest::Harness<'static, csm_ui::CsmApp>;
    fn page(h: &mut H) {
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::OpenRemoteAccess);
        wait_for(h, "the page", |a| a.ui.remote.status.available && !a.ui.remote.interfaces.is_empty());
    }
    fn walk(h: &mut H, to_firewall: bool) {
        page(h);
        h.get_by_label(TOGGLE_LABEL).click();
        settle(h);
        if to_firewall {
            h.get_by_label("Next").click();
            settle(h);
        }
    }
    fn on(h: &mut H) {
        walk(h, true);
        h.get_by_label("Turn on and pair a device").click();
        wait_for(h, "on", |a| a.ui.remote.status.enabled && a.ui.remote.link.is_some());
    }
    let size = egui::vec2(1280.0, 800.0);
    let now = NOW_MS / 1000;
    let go = |name: &str, theme: ThemeKind, host: DemoPeerHost, after: &dyn Fn(&mut H, &DemoPeerHost)| {
        let h2 = host.clone();
        shoot_host(name, theme, size, Some(host), |_| {}, |h| after(h, &h2));
    };

    go("22-remote-off", ThemeKind::Mocha, DemoPeerHost::new(now), &|h, _| page(h));
    go("22-remote-setup-address", ThemeKind::Mocha, DemoPeerHost::new(now), &|h, _| walk(h, false));
    go("22-remote-setup-firewall", ThemeKind::Mocha, DemoPeerHost::new(now), &|h, _| walk(h, true));
    go("22-remote-pairing", ThemeKind::Mocha, DemoPeerHost::new(now), &|h, _| on(h));
    go("22-remote-pairing-latte", ThemeKind::Latte, DemoPeerHost::new(now), &|h, _| on(h));
    go("22-remote-pending", ThemeKind::Mocha, DemoPeerHost::new(now), &|h, host| {
        on(h);
        host.ask_to_pair("Allan's laptop", "4821");
        wait_for(h, "the question", |a| a.ui.remote.status.pending_pair.is_some());
    });
    go("22-remote-connected", ThemeKind::Mocha, DemoPeerHost::new(now).with_device("Allan's laptop", 3 * 86_400).with_device("Tablet", 5 * 3600), &|h, host| {
        on(h);
        host.set_connected("Allan's laptop", true);
        wait_for(h, "connected", |a| a.ui.remote.status.connected == 1 && a.ui.remote.devices.len() == 2);
    });
    // The indicator on the main window, from another page.
    go("22-remote-indicator", ThemeKind::Mocha, DemoPeerHost::new(now).with_device("Allan's laptop", 3 * 86_400), &|h, host| {
        on(h);
        host.set_connected("Allan's laptop", true);
        wait_for(h, "connected", |a| a.ui.remote.status.connected == 1);
        let ctx = h.ctx.clone();
        h.state_mut().run(&ctx, Action::CloseSettings);
    });
}
