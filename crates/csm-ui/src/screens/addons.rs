//! Settings > Add-ons (docs/addons.md): the add-ons folder, each add-on's state and problems, and
//! the consent dialog's text. Every button is a tool call (`addon_reload`, `addon_open_folder`,
//! `addon_disable`); enabling goes through a consent dialog that lists what the add-on contributes
//! before `addon_enable` runs.

use csm_engine::addons::{AddonState, AddonSummary};
use egui::Ui;
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::screens::settings::{caption, section};
use crate::theme::{self, Tokens, space, text};
use crate::widgets::{self, ButtonKind};

/// The Settings page for add-ons.
pub fn show(ui: &mut Ui, app: &CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    section(ui, "Add-ons");
    caption(ui, "Add-ons are folders in the add-ons folder beside state.json. Each one is off until you enable it; enabling shows what it adds first.");
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Secondary, None, "Reload", None).clicked() {
            actions.push(Action::Tool("addon_reload", json!({})));
        }
        if widgets::button(ui, ButtonKind::Secondary, None, "Open folder", None).clicked() {
            actions.push(Action::Tool("addon_open_folder", json!({})));
        }
    });
    ui.add_space(space::MD);
    if app.model.addons.is_empty() {
        ui.label(egui::RichText::new("No add-ons found.").font(theme::regular(text::BODY)).color(t.muted));
        return;
    }
    for a in &app.model.addons {
        row(ui, a, actions);
        ui.add_space(space::SM);
    }
}

fn row(ui: &mut Ui, a: &AddonSummary, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let title = if a.version.is_empty() { a.name.clone() } else { format!("{} {}", a.name, a.version) };
        ui.label(egui::RichText::new(title).font(theme::medium(text::BODY)).color(t.fg));
        ui.label(egui::RichText::new(format!("{} · {}", a.kind, state_label(a.state))).font(theme::regular(text::CAPTION)).color(t.muted));
        if a.state == AddonState::Loaded && widgets::button(ui, ButtonKind::Ghost, None, "Disable", None).clicked() {
            actions.push(Action::Tool("addon_disable", json!({"id": a.id})));
        }
        if a.state == AddonState::Disabled && widgets::button(ui, ButtonKind::Primary, None, "Enable…", None).clicked() {
            actions.push(Action::AddonConsent(a.id.clone()));
        }
    });
    if let Some(m) = &a.message {
        caption(ui, m);
    }
    if !a.description.is_empty() {
        caption(ui, &a.description);
    }
    for p in &a.problems {
        caption(ui, &format!("Skipped: {p}"));
    }
}

fn state_label(s: AddonState) -> &'static str {
    match s {
        AddonState::Loaded => "enabled",
        AddonState::Disabled => "disabled",
        AddonState::Error => "error",
    }
}

/// The consent dialog's body: what the add-on contributes, in plain lines.
pub fn consent_body(a: &AddonSummary) -> String {
    let mut lines = Vec::new();
    if !a.description.is_empty() {
        lines.push(a.description.clone());
    }
    if let Some(author) = &a.author {
        lines.push(format!("By {author}."));
    }
    // The exact command lines are shown: the person approves what will run, not just a name.
    if !a.profiles.is_empty() {
        let names: Vec<String> = a.profiles.iter().map(|p| format!("{} runs `{}` (on restore: {})", p.id, p.command_line, p.resume)).collect();
        lines.push(format!("Launch profiles: {}.", names.join("; ")));
    }
    if !a.run_templates.is_empty() {
        let names: Vec<String> = a.run_templates.iter().map(|r| format!("{} runs `{}`", r.id, r.command_line)).collect();
        lines.push(format!("Run commands, run in a shell when started: {}.", names.join("; ")));
    }
    if !a.slash.is_empty() {
        let names: Vec<&str> = a.slash.iter().map(|s| s.command.as_str()).collect();
        lines.push(format!("Claude slash presets: {}.", names.join(", ")));
    }
    if !a.commands.is_empty() {
        let names: Vec<String> = a.commands.iter().map(|c| format!("{} (calls {} with {})", c.title, c.tool, c.params)).collect();
        lines.push(format!("Palette commands: {}.", names.join("; ")));
    }
    if a.kind == "process" {
        let effects = if a.process_effects.is_empty() { "none declared".to_owned() } else { a.process_effects.join(", ") };
        lines.push(format!("Process add-on. It may request: {effects}. Process add-ons do not run yet, so enabling adds nothing else."));
    }
    lines.push("Enabling is saved. If the file changes later, it is turned off until you enable it again.".into());
    lines.join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use csm_engine::addons::{CommandSummary, ProfileSummary, RunTemplateSummary};

    #[test]
    fn consent_text_lists_what_is_contributed() {
        let a = AddonSummary {
            id: "demo".into(),
            name: "Demo".into(),
            version: "0.1.0".into(),
            description: "A demo".into(),
            kind: "pack",
            author: Some("Someone".into()),
            homepage: None,
            state: AddonState::Disabled,
            message: None,
            folder: "demo".into(),
            path: "x".into(),
            digest: Some("abc".into()),
            profiles: vec![ProfileSummary { id: "demo.agent".into(), name: "agent".into(), command_line: "my-agent --plain".into(), resume: "re-run" }],
            run_templates: vec![RunTemplateSummary { id: "demo.test".into(), name: "test".into(), command_line: "cargo test".into(), cwd: None }],
            slash: vec![csm_core::models::SlashCommand::new("/review", "Review")],
            commands: vec![CommandSummary { id: "demo.list".into(), title: "List".into(), tool: "session_list".into(), params: json!({}) }],
            process_effects: vec![],
            problems: vec![],
        };
        let body = consent_body(&a);
        assert!(body.contains("demo.agent") && body.contains("demo.test") && body.contains("/review"));
        assert!(body.contains("calls session_list"));
        assert!(body.contains("saved"));
        // The exact commands that will run are in the text, not just their names.
        assert!(body.contains("`my-agent --plain`"), "{body}");
        assert!(body.contains("`cargo test`"), "{body}");
    }
}
