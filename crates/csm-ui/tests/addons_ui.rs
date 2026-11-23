//! Add-ons in the UI: enabled add-ons' palette commands, the slash menu's merged list, and the
//! consent dialog's text. The engine is the demo's; add-on data comes through the model, as the
//! engine would publish it.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_core::models::SlashCommand;
use csm_engine::addons::{AddonState, AddonSummary, CommandSummary, ProfileSummary};
use csm_ui::screens::palette::{PalRow, rows};
use csm_ui::theme::ThemeKind;
use egui_kittest::kittest::Queryable;
use serde_json::json;

fn addon(id: &str, state: AddonState, command_title: &str) -> AddonSummary {
    AddonSummary {
        id: id.into(),
        name: format!("{id} pack"),
        version: "0.1.0".into(),
        description: "d".into(),
        kind: "pack",
        author: None,
        homepage: None,
        state,
        message: None,
        folder: id.into(),
        path: format!("addons/{id}"),
        digest: Some(format!("{id}-digest")),
        profiles: vec![ProfileSummary { id: format!("{id}.agent"), name: "agent".into(), command_line: "agent --plain".into(), resume: "re-run" }],
        run_templates: vec![],
        slash: vec![SlashCommand::new("/review", "Review the diff")],
        commands: vec![CommandSummary { id: format!("{id}.cmd"), title: command_title.into(), tool: "session_list".into(), params: json!({"include_asleep": false}) }],
        process_effects: vec![],
        process_command_line: None,
        new_sessions: Vec::new(),
        process_calls: vec![],
        process: None,
        themes: vec![],
        problems: vec![],
    }
}

#[test]
fn only_enabled_add_ons_contribute_palette_commands() {
    let mut b = backend(ThemeKind::Mocha);
    b.model_mut().addons = vec![addon("demo", AddonState::Loaded, "Demo list"), addon("off", AddonState::Disabled, "Hidden list")];
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    h.step();
    let titles: Vec<String> = rows(h.state(), ">")
        .into_iter()
        .filter_map(|r| match r {
            PalRow::Addon { title, tool, .. } => {
                assert_eq!(tool, "session_list", "the palette row calls the add-on's tool");
                Some(title)
            }
            _ => None,
        })
        .collect();
    assert_eq!(titles, vec!["Demo list".to_string()]);
}

#[test]
fn a_typed_query_finds_an_add_on_command_by_its_title() {
    let mut b = backend(ThemeKind::Mocha);
    b.model_mut().addons = vec![addon("demo", AddonState::Loaded, "Sweep the quiet sessions")];
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    h.step();
    let found = rows(h.state(), "sweep").iter().any(|r| matches!(r, PalRow::Addon { title, .. } if title.contains("Sweep")));
    assert!(found, "typing part of the title finds the command");
}

#[test]
fn the_slash_menu_uses_the_engine_merged_list() {
    let mut b = backend(ThemeKind::Mocha);
    b.model_mut().slash_presets = vec![SlashCommand::new("/help", "Show Claude help"), SlashCommand::new("/review", "Review (Demo pack)")];
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    h.step();
    let list = csm_ui::screens::menus::claude_commands(h.state());
    assert!(list.iter().any(|c| c.command == "/review" && c.description.contains("Demo pack")));
    assert_eq!(list.len(), 2);
}

#[test]
fn a_process_add_on_shows_its_state_its_last_error_and_a_restart_button() {
    use csm_engine::{PluginRuntime, RunState};
    let mut b = backend(ThemeKind::Mocha);
    let mut a = addon("proc", AddonState::Loaded, "Unused");
    a.kind = "process";
    a.process_command_line = Some("helper --stdio".into());
    let mut rt = PluginRuntime::starting();
    rt.state = RunState::Crashed;
    rt.restarts = 2;
    rt.last_error = Some("exited (exit status: 3)".into());
    a.process = Some(rt);
    b.model_mut().addons = vec![a];
    b.publish();
    let mut h = harness(app_with(b));
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, csm_ui::app::Action::OpenSettings);
    h.state_mut().ui.settings.section = csm_ui::app::SettingsSection::Addons;
    settle(&mut h);
    assert!(h.query_by_label_contains("Process: crashed").is_some(), "the process state is shown");
    assert!(h.query_by_label_contains("Last error: exited").is_some(), "the last error is shown");
    assert!(h.query_by_label_contains("restarted 2 time(s)").is_some());
    // The button is a tool call; clicking it must not panic (the demo backend answers it).
    h.get_by_label("Restart").click();
    settle(&mut h);
}

fn remote(addon: &str, id: &str, label: &str) -> csm_engine::RemoteView {
    csm_engine::RemoteView {
        addon: addon.into(),
        addon_name: format!("{addon} add-on"),
        id: id.into(),
        label: label.into(),
        detail: "idle".into(),
        opens: "session",
    }
}

#[test]
fn an_add_on_s_remotes_are_palette_rows_that_call_remote_open() {
    let mut b = backend(ThemeKind::Mocha);
    b.model_mut().remotes = vec![remote("demo", "a-1", "Build box"), remote("demo", "a-2", "Staging")];
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    h.step();
    // Listed after ">", titled "Remote… › add-on › label".
    let all: Vec<String> = rows(h.state(), ">")
        .into_iter()
        .filter_map(|r| match r {
            PalRow::Remote { title, .. } => Some(title),
            _ => None,
        })
        .collect();
    assert_eq!(all, vec!["Remote… › demo add-on › Build box".to_string(), "Remote… › demo add-on › Staging".to_string()]);
    // Typing finds one by its label, and only that one.
    let found: Vec<(String, String)> = rows(h.state(), "stag")
        .into_iter()
        .filter_map(|r| match r {
            PalRow::Remote { addon, remote_id, .. } => Some((addon, remote_id)),
            _ => None,
        })
        .collect();
    assert_eq!(found, vec![("demo".to_string(), "a-2".to_string())]);
}
