//! Settings > Add-ons (docs/addons.md): the add-ons folder, each add-on's state and problems, and
//! the consent dialog's text. Every button is a tool call (`addon_reload`, `addon_open_folder`,
//! `addon_disable`); enabling goes through a consent dialog that lists what the add-on contributes
//! before `addon_enable` runs.

use csm_core::addons::status::{AddonStatus, Tone};
use csm_engine::addons::{AddonState, AddonSummary};
use egui::{Rect, Sense, Ui, Vec2, pos2};
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::screens::settings::{caption, section};
use crate::theme::{self, Tokens, size, space, text};
use crate::widgets::{self, ButtonKind};

/// The most serious `csm/status` among the add-ons, with its text. `None` when no running add-on
/// set one, or every one is `idle` (the title-bar dot is then hidden). Stopped add-ons have no
/// status: the engine clears it when the process stops.
pub fn worst_status(addons: &[AddonSummary]) -> Option<(Tone, String)> {
    worst_of(addons.iter().filter_map(|a| a.process.as_ref().and_then(|p| p.status.as_ref())))
}

/// The worst of some statuses (see [`worst_status`]).
pub fn worst_of<'a>(statuses: impl IntoIterator<Item = &'a AddonStatus>) -> Option<(Tone, String)> {
    statuses.into_iter().filter(|s| s.tone != Tone::Idle).max_by_key(|s| s.tone).map(|s| (s.tone, s.text.clone()))
}

fn tone_color(t: &Tokens, tone: Tone) -> egui::Color32 {
    match tone {
        Tone::Idle => t.muted,
        Tone::Ok => t.green,
        Tone::Warn => t.yellow,
        Tone::Error => t.red,
    }
}

/// The title bar's dot for the worst add-on status, at `x`, centred on `cy`. A click opens
/// Settings > Add-ons; the text is the tooltip and the accessible name.
pub fn title_dot(ui: &Ui, tone: Tone, text: &str, x: f32, cy: f32, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let d = size::STATUS_DOT;
    let rect = Rect::from_min_size(pos2(x, cy - d), Vec2::new(d, d * 2.0));
    let resp = ui.interact(rect, ui.id().with("tb-addon-status"), Sense::click());
    ui.painter().circle_filled(pos2(x + d / 2.0, cy), d / 2.0, tone_color(&t, tone));
    let label = format!("Add-on status: {text}");
    resp.widget_info({
        let label = label.clone();
        move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone())
    });
    if resp.on_hover_text(&label).on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
        actions.push(Action::OpenAddonSettings);
    }
}

/// One status line in an add-on's row: its tone's dot, then the text (plain text, never markup).
fn status_line(ui: &mut Ui, s: &AddonStatus) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let d = size::STATUS_DOT;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(d), Sense::hover());
        ui.painter().circle_filled(rect.center(), d / 2.0, tone_color(&t, s.tone));
        caption(ui, &format!("Status: {}", s.text));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(tone: Tone, text: &str) -> AddonStatus {
        AddonStatus { text: text.into(), tone }
    }

    #[test]
    fn the_dot_shows_the_worst_tone_and_its_text() {
        let list = [st(Tone::Ok, "fine"), st(Tone::Error, "relay down"), st(Tone::Warn, "slow")];
        assert_eq!(worst_of(&list), Some((Tone::Error, "relay down".into())));
    }

    #[test]
    fn the_dot_is_hidden_when_nothing_is_set_or_all_are_idle() {
        let none: [AddonStatus; 0] = [];
        assert_eq!(worst_of(&none), None, "no status set");
        assert_eq!(worst_of(&[st(Tone::Idle, "quiet"), st(Tone::Idle, "also quiet")]), None, "idle hides it");
    }
}

/// The Settings page for add-ons.
pub fn show(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    section(ui, "Add-ons");
    caption(ui, "Add-ons are built in or folders in the add-ons folder beside state.json. Each one is off until you enable it; enabling shows what it adds first.");
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Secondary, None, "Reload", None).clicked() {
            actions.push(Action::Tool("addon_reload", json!({})));
        }
        if widgets::button(ui, ButtonKind::Secondary, None, "Open folder", None).clicked() {
            actions.push(Action::Tool("addon_open_folder", json!({})));
        }
        if widgets::button(ui, ButtonKind::Secondary, None, "Install samples", None).clicked() {
            actions.push(Action::Tool("addon_install_samples", json!({})));
        }
    });
    ui.add_space(space::MD);
    if app.model.addons.is_empty() {
        ui.label(egui::RichText::new("No add-ons found.").font(theme::regular(text::BODY)).color(t.muted));
        return;
    }
    let list = app.model.addons.clone();
    for a in &list {
        row(ui, a, actions);
        if a.id == csm_engine::addons::CLAUDE_USAGE_ID && a.state == AddonState::Loaded {
            crate::screens::usage::history_controls(ui, app, actions);
        }
        ui.add_space(space::SM);
    }
}

fn row(ui: &mut Ui, a: &AddonSummary, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        let title = if a.version.is_empty() { a.name.clone() } else { format!("{} {}", a.name, a.version) };
        ui.label(egui::RichText::new(title).font(theme::medium(text::BODY)).color(t.fg));
        ui.label(egui::RichText::new(format!("{} · {}", kind_label(a.kind), state_label(a.state))).font(theme::regular(text::CAPTION)).color(t.muted));
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
    if let Some(rt) = &a.process {
        let mut line = format!("Process: {}", process_label(rt.state));
        if let Some(pid) = rt.pid {
            line.push_str(&format!(" · pid {pid}"));
        }
        if rt.restarts > 0 {
            line.push_str(&format!(" · restarted {} time(s)", rt.restarts));
        }
        if !rt.tools.is_empty() {
            line.push_str(&format!(" · {} tool(s)", rt.tools.len()));
        }
        caption(ui, &line);
        if let Some(s) = &rt.status {
            status_line(ui, s);
        }
        if let Some(e) = &rt.last_error {
            caption(ui, &format!("Last error: {e}"));
        }
        ui.horizontal(|ui| {
            if widgets::button(ui, ButtonKind::Secondary, None, "Restart", None).clicked() {
                actions.push(Action::Tool("plugin_restart", json!({"id": a.id})));
            }
        });
    }
    if !a.description.is_empty() {
        caption(ui, &a.description);
    }
    for p in &a.problems {
        caption(ui, &format!("Skipped: {p}"));
    }
}

fn process_label(s: csm_engine::RunState) -> &'static str {
    use csm_engine::RunState;
    match s {
        RunState::Starting => "starting",
        RunState::Running => "running",
        RunState::Crashed => "crashed, starting again soon",
        RunState::GaveUp => "gave up after repeated crashes",
        RunState::Stopped => "stopped",
    }
}

fn kind_label(kind: &str) -> &str {
    if kind == "builtin" { "built in" } else { kind }
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
        let command = a.process_command_line.clone().unwrap_or_default();
        lines.push(format!("Process add-on. It runs `{command}` as a program, with its working folder set to the add-on folder. It may request: {effects}. Its tools appear in the palette, the CLI and MCP; a tool that uses an effect asks before it runs for MCP clients."));
        if a.process_effects.iter().any(|e| e == "stores_data") {
            lines.push("Stores its own data: it keeps files in a folder of its own (addon-data/<id>) and nowhere else. This needs no approval card.".into());
        }
        if !a.new_sessions.is_empty() {
            // The New session form: its title and the tools it runs when the person uses it.
            let forms: Vec<String> = a.new_sessions.iter().map(|f| format!("\"{}\" (picker: {}; submit: {} runs {})", f.title, f.choices_tool, f.submit, f.tool)).collect();
            lines.push(format!("New session forms: {}. They show in the New session dialog while it runs, and each runs only its own tools when the person uses it.", forms.join("; ")));
        }
        if !a.process_calls.is_empty() {
            let list: Vec<String> = a.process_calls.iter().map(|c| format!("{} ({})", c.tool, c.effects)).collect();
            lines.push(format!(
                "Standing permission: enabling this add-on lets it run exactly these app tools without asking each time: {}. Any other tool it calls asks as usual. If the file changes, this ends until you approve it again.",
                list.join("; ")
            ));
        }
    }
    if a.kind == "builtin" {
        lines.push("Built into the app: it runs no program of its own. Enabling is saved; Disable turns it off again, and what it recorded is kept.".into());
    } else {
        lines.push("Enabling is saved. If the file changes later, it is turned off until you enable it again.".into());
    }
    lines.join("\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod dot_tests {
    use super::*;
    use csm_engine::addons::{CommandSummary, ProfileSummary, RunTemplateSummary};

    #[test]
    fn consent_text_lists_what_is_contributed() {
        let a = AddonSummary {
            id: "demo".into(),
            name: "Demo".into(),
            version: "0.1.0".into(),
            description: "A demo".into(),
            kind: "process",
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
            process_command_line: None,
            new_sessions: vec![csm_engine::addons::NewSessionSummary { id: "agent".into(), title: "New demo agent".into(), submit: "Start".into(), choices_tool: "demo.projects".into(), tool: "demo.launch".into() }],
            process_calls: vec![],
            process: None,
            themes: vec![],
            problems: vec![],
        };
        let body = consent_body(&a);
        assert!(body.contains("demo.agent") && body.contains("demo.test") && body.contains("/review"));
        assert!(body.contains("calls session_list"));
        assert!(body.contains("\"New demo agent\" (picker: demo.projects; submit: Start runs demo.launch)"), "{body}");
        assert!(body.contains("saved"));
        // The exact commands that will run are in the text, not just their names.
        assert!(body.contains("`my-agent --plain`"), "{body}");
        assert!(body.contains("`cargo test`"), "{body}");
    }

    #[test]
    fn consent_text_lists_the_standing_tools_and_the_data_folder() {
        let a = AddonSummary {
            id: "phone".into(),
            name: "Phone".into(),
            version: "0.1.0".into(),
            description: String::new(),
            kind: "process",
            author: None,
            homepage: None,
            state: AddonState::Disabled,
            message: None,
            folder: "phone".into(),
            path: "x".into(),
            digest: Some("abc".into()),
            profiles: vec![],
            run_templates: vec![],
            slash: vec![],
            commands: vec![],
            process_effects: vec!["stores_data".into()],
            process_command_line: Some("phone-helper".into()),
            new_sessions: Vec::new(),
            process_calls: vec![csm_engine::addons::StandingCall {
                tool: "session_wake".into(),
                title: "Wake".into(),
                effects: "starts programs".into(),
                asks: true,
            }],
            process: None,
            themes: vec![],
            problems: vec![],
        };
        let body = consent_body(&a);
        assert!(body.contains("Standing permission") && body.contains("session_wake (starts programs)"), "{body}");
        assert!(body.contains("Stores its own data"), "{body}");
        assert!(body.contains("turned off until you enable it again"), "{body}");
    }
}
