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
