//! A session's own look (a Windows Terminal profile's) in the UI: the renderer draws its
//! colours, and the quick-open's Profile row stamps one onto a new or edited session.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_core::look::{Look, LookColors};
use csm_ui::Action;
use csm_ui::app::ProfilePick;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

const MAGENTA: [u8; 4] = [0xff, 0x00, 0xff, 0xff];

fn calls(h: &Harness<'static, csm_ui::CsmApp>, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

#[test]
fn a_session_with_a_look_renders_with_its_background_colour() {
    let render = |look: Option<Look>| {
        let mut b = backend(ThemeKind::Mocha);
        let active = b.model_mut().active.clone().expect("the demo has an active session");
        if let Some(s) = b.model_mut().sessions.iter_mut().find(|s| s.id == active) {
            s.look = look;
        }
        b.publish();
        let mut h = harness_wgpu(app_with(b), 1.0);
        settle(&mut h);
        h.step();
        // Pixels of exactly the look's background.
        h.render().expect("render").pixels().filter(|p| p.0 == MAGENTA).count()
    };
    assert_eq!(render(None), 0, "the theme has no pure magenta");
    let look = Look {
        colors: Some(LookColors { background: Some("#ff00ff".into()), ..Default::default() }),
        padding: Some(vec![20]),
        font_size: Some(16),
        ..Default::default()
    };
    // The pane body is filled with the session's background: a large block of it.
    let n = render(Some(look));
    assert!(n > 40_000, "only {n} pixels of the look's background");
}

#[test]
fn the_profile_row_shows_when_import_is_on_and_stamps_the_profile_on_a_new_session() {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    let ctx = h.ctx.clone();
    // Off (the default): no Profile row, no file read.
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(&mut h);
    assert!(h.query_by_label("Profile: None").is_none());
    assert!(calls(&h, "profile_list").is_empty());
    h.state_mut().run(&ctx, Action::CloseOverlays);
    // The Settings toggle turns it on in the engine.
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(&mut h);
    h.get_by_label("Settings: Appearance").click();
    settle(&mut h);
    h.get_by_label("Offer Windows Terminal profiles").scroll_to_me();
    settle(&mut h);
    h.get_by_label("Offer Windows Terminal profiles").click();
    settle(&mut h);
    let engine = h.state().dispatch.engine().expect("an engine port");
    let on = engine.call(csm_engine::Caller::Ui, "settings_get", json!({"key": "ImportWindowsTerminalProfiles"})).unwrap();
    assert_eq!(on.data["value"], true);
    h.state_mut().run(&ctx, Action::CloseSettings);
    settle(&mut h);

    // On: the panel asks for the profiles once.
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(&mut h);
    assert_eq!(calls(&h, "profile_list").len(), 1);
    // The demo engine's own Windows Terminal settings (never this machine's files).
    let dir = "C:\\GitHub\\Odulodo";
    assert!(h.query_by_label("Profile: Ubuntu").is_some());
    h.get_by_label("Profile: PowerShell").click();
    settle(&mut h);
    assert_eq!(h.state().ui.quick.profile, ProfilePick::Profile("PowerShell".into()));
    assert_eq!(h.state().ui.quick.query, dir, "an empty folder takes the profile's starting directory");
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let p = calls(&h, "session_new").pop().expect("session_new");
    assert_eq!(p["folder"], dir);
    assert_eq!(p["command"], "pwsh.exe");
    assert_eq!(p["args"], "-NoLogo");
    assert_eq!(p["name"], "PowerShell", "a new session is named after its profile, as in the WPF dialog");
    // Campbell Powershell, a built-in scheme the settings name but do not define.
    assert_eq!(p["look"]["colors"]["background"], "#012456");
    assert_eq!(p["look"]["cursor_shape"], "bar");
    // The demo applied it: the new session carries the look.
    let s = h.state().model.sessions.last().expect("the new session");
    assert_eq!(s.folder, dir);
    assert_eq!(s.look.as_ref().and_then(|l| l.font_size), Some(11));
}

#[test]
fn editing_a_session_can_clear_or_replace_its_look() {
    let mut b = backend(ThemeKind::Mocha);
    let active = b.model_mut().active.clone().unwrap();
    if let Some(s) = b.model_mut().sessions.iter_mut().find(|s| s.id == active) {
        s.look = Some(Look { font_size: Some(18), ..Default::default() });
    }
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    let ctx = h.ctx.clone();
    // Import is off, but a session with a look can still drop it.
    h.state_mut().run(&ctx, Action::EditSession(active.clone()));
    settle(&mut h);
    assert!(h.query_by_label("Profile: Keep current").is_some());
    assert_eq!(h.state().ui.quick.profile, ProfilePick::Keep);
    h.get_by_label("Profile: None").click();
    settle(&mut h);
    h.get_by_label("Save").click();
    settle(&mut h);
    let p = calls(&h, "session_look_set").pop().expect("session_look_set");
    assert_eq!(p, json!({"session_id": active, "clear": true}));
    assert!(h.state().model.session(&active).unwrap().look.is_none());
    // Keep (the default) sends nothing.
    h.state_mut().run(&ctx, Action::EditSession(active.clone()));
    settle(&mut h);
    h.get_by_label("Save").click();
    settle(&mut h);
    assert_eq!(calls(&h, "session_look_set").len(), 1);

    // With import on, a profile replaces the look; the session's folder and command stay.
    h.state_mut().run(&ctx, Action::Tool("settings_set", json!({"key": "ImportWindowsTerminalProfiles", "value": true})));
    settle(&mut h);
    let before = h.state().model.session(&active).cloned().unwrap();
    h.state_mut().run(&ctx, Action::EditSession(active.clone()));
    settle(&mut h);
    h.get_by_label("Profile: Ubuntu").click();
    settle(&mut h);
    h.get_by_label("Save").click();
    settle(&mut h);
    let p = calls(&h, "session_look_set").pop().expect("session_look_set");
    assert_eq!(p["session_id"], active.as_str());
    assert_eq!(p["look"]["font_size"], 13);
    assert_eq!(p["look"]["cursor_shape"], "underline");
    let after = h.state().model.session(&active).cloned().unwrap();
    assert_eq!(after.look.as_ref().and_then(|l| l.padding.clone()), Some(vec![12, 12, 12, 12]));
    assert_eq!((after.folder, after.command), (before.folder, before.command));
}

#[test]
fn a_profile_command_line_with_a_quoted_program_splits_into_program_and_arguments() {
    use csm_ui::screens::palette::split_command;
    assert_eq!(
        split_command(r#""C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo"#),
        (r"C:\Program Files\PowerShell\7\pwsh.exe".to_string(), Some("-NoLogo".to_string()))
    );
    assert_eq!(split_command(r#""C:\Program Files\Git\bin\bash.exe""#), (r"C:\Program Files\Git\bin\bash.exe".to_string(), None));
    assert_eq!(split_command("claude --continue"), ("claude".to_string(), Some("--continue".to_string())));
    // An unterminated quote is left as typed.
    assert_eq!(split_command(r#""C:\a b"#), (r#""C:\a"#.to_string(), Some("b".to_string())));
}

#[test]
fn the_palette_sends_session_look_set_to_the_edit_panel() {
    let mut h = harness(app_with(backend(ThemeKind::Mocha)));
    settle(&mut h);
    let ctx = h.ctx.clone();
    let active = h.state().model.active.clone().expect("an active session");
    h.state_mut().run(&ctx, Action::PaletteTool("session_look_set".into()));
    settle(&mut h);
    assert_eq!(h.state().ui.quick.edit.as_deref(), Some(active.as_str()), "the Profile row lives in Edit session");
    assert!(calls(&h, "session_look_set").is_empty(), "no call without a look");
}
