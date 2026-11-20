//! Settings → General → Claude commands: the slash commands a Claude pane's ⋯ menu and the
//! palette offer (`ClaudeCommands`; the WPF ⚡ Commands panel's list by default). Every change is
//! a `settings_set` of the whole list; the list shown is what the engine answered.

use csm_core::models::SlashCommand;
use egui::{Align2, Id, Rect, Sense, Ui, Vec2, pos2, vec2};
use serde_json::json;

use super::settings::{caption, section};
use crate::app::{Action, CsmApp};
use crate::theme::{self, Tokens, size, space, text};
use crate::widgets::{self, ButtonKind};

const KEY: &str = "ClaudeCommands";
/// Draft keys for the two "add" fields (kept with the other settings drafts).
const NEW_CMD: &str = "#ClaudeCommands.command";
const NEW_DESC: &str = "#ClaudeCommands.description";

fn set(list: &[SlashCommand]) -> Action {
    Action::Tool("settings_set", json!({"key": KEY, "value": list}))
}

pub fn show(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    section(ui, "Claude commands");
    caption(ui, "Typed into a Claude session from its ⋯ menu (Claude commands) or the palette, then Enter.");
    let list = super::menus::claude_commands(app);
    widgets::card_frame(&t).inner_margin(egui::Margin::symmetric(space::SM as i8, space::XS as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        for (i, c) in list.iter().enumerate() {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL), Sense::hover());
            let cr = ui.painter().text(pos2(r.min.x + space::XS, r.center().y), Align2::LEFT_CENTER, &c.command, theme::mono(text::MONO), t.fg);
            ui.painter().text(pos2(cr.max.x + space::SM, r.center().y), Align2::LEFT_CENTER, &c.description, theme::regular(text::CAPTION), t.muted);
            let mut x = r.max.x;
            let mut btn = |ui: &mut Ui, name: &str, tip: &str| {
                x -= size::CONTROL_SM;
                let b = Rect::from_center_size(pos2(x + size::CONTROL_SM / 2.0, r.center().y), Vec2::splat(size::CONTROL_SM));
                x -= space::XXS;
                widgets::icon_button_at(ui, b, Id::new(("claude-cmd", i, name.to_string())), name, false, tip).clicked()
            };
            if btn(ui, "x", &format!("Remove “{}”", c.command)) {
                let mut v = list.clone();
                v.remove(i);
                actions.push(set(&v));
            }
            if i > 0 && btn(ui, "chevU", &format!("Move “{}” up", c.command)) {
                let mut v = list.clone();
                v.swap(i, i - 1);
                actions.push(set(&v));
            }
        }
        if list.is_empty() {
            ui.label(
                egui::RichText::new("No commands: Claude panes show no Claude commands menu.").font(theme::regular(text::CAPTION)).color(t.muted),
            );
        }
    });
    let drafts = &mut app.ui.settings.drafts;
    let mut cmd = drafts.get(NEW_CMD).cloned().unwrap_or_default();
    let mut desc = drafts.get(NEW_DESC).cloned().unwrap_or_default();
    let mut add = false;
    ui.horizontal(|ui| {
        let r = ui.add(
            egui::TextEdit::singleline(&mut cmd)
                .id(Id::new("settings-new-claude-command"))
                .hint_text(egui::RichText::new("/review").color(t.faint))
                .font(theme::mono(text::MONO))
                .desired_width(size::FIELD_W / 2.0),
        );
        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "New Claude command"));
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let d = ui.add(
            egui::TextEdit::singleline(&mut desc)
                .id(Id::new("settings-new-claude-command-desc"))
                .hint_text(egui::RichText::new("what it does").color(t.faint))
                .desired_width(size::FIELD_W / 2.0),
        );
        d.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "New Claude command description"));
        add = enter || widgets::button(ui, ButtonKind::Secondary, Some("plus"), "Add", None).clicked();
    });
    if add && let Some(c) = SlashCommand::clean_command(&cmd) {
        if !list.iter().any(|x| x.command == c) {
            let mut v = list.clone();
            v.push(SlashCommand::new(&c, desc.trim()));
            actions.push(set(&v));
        }
        cmd.clear();
        desc.clear();
    }
    for (k, v) in [(NEW_CMD, cmd), (NEW_DESC, desc)] {
        if v.is_empty() {
            drafts.remove(k);
        } else {
            drafts.insert(k.to_string(), v);
        }
    }
    if list != SlashCommand::defaults() && widgets::button(ui, ButtonKind::Ghost, Some("restart"), "Reset Claude commands", None).clicked() {
        actions.push(set(&SlashCommand::defaults()));
    }
}
