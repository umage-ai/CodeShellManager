//! The inspector's Runs tab: the session's run commands with their state, the output of the
//! selected one (Stop / Copy / Send to Claude), and an inline editor for the list.
//!
//! Every change is a tool (`run_start`, `run_stop`, `run_edit` (save, detect, import),
//! `session_send_text`); the only
//! local state is which run is selected and the editor's unsaved rows (egui memory, per session).
//! Output is read through [`crate::Backend::run_output`] only when its generation changes, and
//! only the tail is shown.

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::icons;
use crate::model::{RunState, RunView, SessionView, Status};
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

/// The output tail the tab shows (the full run keeps up to 1 MB; Copy takes what is shown).
const OUTPUT_TAIL: usize = 64 * 1024;
/// Most output sent to Claude in one fenced block.
const SEND_MAX: usize = 32 * 1024;
/// The URL field's tooltip.
const URL_HELP: &str = "Opened in the browser once, when a local server (localhost, 127.0.0.1, [::1]) starts listening there while the command runs, or after the command finishes successfully. Remote URLs and SSH sessions: after a successful finish only. http:// or https:// only.";

/// One row of the editor (unsaved).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditRow {
    pub id: String,
    pub label: String,
    pub command_line: String,
    pub powershell: bool,
    pub post_run_url: String,
    pub is_default: bool,
}

fn editor_id(session: &str) -> Id {
    Id::new(("runs-editor", session.to_string()))
}

fn selected_id(session: &str) -> Id {
    Id::new(("runs-selected", session.to_string()))
}

/// Opens the inline editor on a copy of the session's list (`Action::EditRuns`).
pub fn open_editor(ctx: &egui::Context, s: &SessionView) {
    let rows: Vec<EditRow> = s
        .runs
        .iter()
        .map(|r| EditRow {
            id: r.id.clone(),
            label: r.label.clone(),
            command_line: r.command_line.clone(),
            powershell: r.mode == "powershell",
            post_run_url: r.post_run_url.clone().unwrap_or_default(),
            is_default: r.is_default,
        })
        .collect();
    let rows = if rows.is_empty() { vec![EditRow { is_default: true, ..EditRow::default() }] } else { rows };
    ctx.data_mut(|d| d.insert_temp(editor_id(&s.id), rows));
}

/// The `run_edit` params for the editor's rows.
pub fn edit_params(session: &str, rows: &[EditRow]) -> serde_json::Value {
    let runs: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let mut v = json!({
                "label": r.label.trim(),
                "command_line": r.command_line.trim(),
                "is_default": r.is_default,
                "mode": if r.powershell { "powershell" } else { "process" },
            });
            if !r.id.is_empty() {
                v["id"] = json!(r.id);
            }
            if !r.post_run_url.trim().is_empty() {
                v["post_run_url"] = json!(r.post_run_url.trim());
            }
            v
        })
        .collect();
    json!({"session_id": session, "runs": runs})
}

/// Why the editor can't save yet (shown instead of enabling Save).
pub fn rows_problem(rows: &[EditRow]) -> Option<String> {
    for (i, r) in rows.iter().enumerate() {
        let n = i + 1;
        if r.label.trim().is_empty() {
            return Some(format!("Command {n} needs a label."));
        }
        if r.command_line.trim().is_empty() {
            return Some(format!("{} needs a command line.", r.label.trim()));
        }
        if r.command_line.contains(['\n', '\r']) {
            return Some(format!("{}: one line only; chain commands with &&.", r.label.trim()));
        }
        let u = r.post_run_url.trim().to_ascii_lowercase();
        if !u.is_empty() && !(u.starts_with("http://") || u.starts_with("https://")) {
            return Some(format!("{}: the URL must start with http:// or https://.", r.label.trim()));
        }
    }
    None
}

/// The fenced block "Send to Claude" pastes (a fence longer than any backtick run inside).
pub fn fenced(command_line: &str, output: &str) -> String {
    let out = if output.len() > SEND_MAX {
        let mut cut = output.len() - SEND_MAX;
        while !output.is_char_boundary(cut) {
            cut += 1;
        }
        output.get(cut..).unwrap_or(output)
    } else {
        output
    };
    let longest = out.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    format!("Output of `{}`:\n{fence}\n{}\n{fence}\n", command_line.replace('`', "'"), out.trim_end())
}

/// "+ <template> (add-on)" buttons: each enabled add-on's run templates, added to this session
/// through `addon_run_apply` (the template's command line is fixed by the add-on).
fn addon_templates_row(ui: &mut Ui, app: &CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    let templates: Vec<(String, String)> = app
        .model
        .addons
        .iter()
        .filter(|a| a.state == csm_engine::addons::AddonState::Loaded)
        .flat_map(|a| a.run_templates.iter().map(|r| (r.id.clone(), r.name.clone())))
        .collect();
    if templates.is_empty() {
        return;
    }
    ui.horizontal_wrapped(|ui| {
        for (id, name) in &templates {
            let label = format!("{name} (add-on)");
            if widgets::button(ui, ButtonKind::Ghost, Some("plus"), &label, None).on_hover_text(format!("Add the add-on's run command `{id}`")).clicked() {
                actions.push(Action::Tool("addon_run_apply", json!({"session_id": s.id, "template": id})));
            }
        }
    });
}

pub fn show(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let editing: Option<Vec<EditRow>> = ui.ctx().data(|d| d.get_temp(editor_id(&s.id)));
    if let Some(rows) = editing {
        editor(ui, s, rows, actions);
        return;
    }
    if s.runs.is_empty() {
        widgets::card_frame(&t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("No run commands yet").font(semibold(text::BODY)).color(t.fg));
            ui.label(
                egui::RichText::new("Run commands start in their own background terminal, so they never type into this session.")
                    .font(theme::regular(text::CAPTION))
                    .color(t.muted),
            );
            ui.horizontal_wrapped(|ui| {
                if s.kind != crate::model::Kind::Ssh
                    && widgets::button(ui, ButtonKind::Secondary, Some("search"), "Detect from project", None).clicked()
                {
                    actions.push(Action::Tool("run_edit", json!({"session_id": s.id, "detect": true})));
                }
                if widgets::button(ui, ButtonKind::Secondary, Some("plus"), "Add a command…", None).clicked() {
                    open_editor(ui.ctx(), s);
                }
            });
            addon_templates_row(ui, app, s, actions);
        });
        return;
    }

    ui.horizontal(|ui| {
        widgets::caps(ui, "Commands");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if widgets::button(ui, ButtonKind::Ghost, Some("pencil"), "Edit", None).on_hover_text("Edit the run commands").clicked() {
                open_editor(ui.ctx(), s);
            }
        });
    });
    addon_templates_row(ui, app, s, actions);
    let sel_key = selected_id(&s.id);
    let stored: Option<String> = ui.ctx().data(|d| d.get_temp(sel_key));
    let selected = stored
        .filter(|id| s.runs.iter().any(|r| &r.id == id))
        .or_else(|| s.runs.iter().find(|r| r.state == RunState::Running).map(|r| r.id.clone()))
        .or_else(|| s.default_run().map(|r| r.id.clone()));
    // A long list (a big package.json) scrolls in the top half, so the output stays in view.
    egui::ScrollArea::vertical()
        .id_salt(("runs-list-scroll", s.id.clone()))
        .max_height((ui.available_height() / 2.0).max(size::CONTROL_LG * 3.0))
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for r in &s.runs {
                let on = selected.as_deref() == Some(r.id.as_str());
                if run_row(ui, &t, s, r, on, actions) {
                    ui.ctx().data_mut(|d| d.insert_temp(sel_key, r.id.clone()));
                }
            }
        });
    let Some(r) = selected.as_deref().and_then(|id| s.runs.iter().find(|x| x.id == id)) else { return };
    output(ui, app, &t, s, r, actions);
}

/// One command: run/stop button, label (F5 badge on the default), state chip, command line.
/// Returns true when the row was clicked (select it).
fn run_row(ui: &mut Ui, t: &Tokens, s: &SessionView, r: &RunView, on: bool, actions: &mut Vec<Action>) -> bool {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG + space::LG), Sense::click());
    if on {
        ui.painter().rect(rect, CornerRadius::same(radius::md()), t.field, Stroke::new(size::HAIRLINE, t.surface1), StrokeKind::Inside);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::md()), t.row_hover);
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Run command: {}", r.label)));
    let running = r.state == RunState::Running;
    let b = Rect::from_min_size(pos2(rect.min.x + space::SM, rect.min.y + space::SM - space::XXS), Vec2::splat(size::CONTROL_SM));
    let tip = if running { "Stop" } else { "Run" };
    if widgets::icon_button_at(
        ui,
        b,
        Id::new(("run-btn", s.id.clone(), r.id.clone())),
        if running { "stop" } else { "play" },
        false,
        &format!("{tip} {}", r.label),
    )
    .clicked()
    {
        actions.push(Action::Tool(if running { "run_stop" } else { "run_start" }, json!({"session_id": s.id, "run_id": r.id})));
    }
    let lr = ui.painter().text(pos2(b.max.x + space::SM, b.center().y), Align2::LEFT_CENTER, &r.label, semibold(text::BODY), t.fg);
    if r.is_default {
        let dr = Rect::from_min_size(
            pos2(lr.max.x + space::XS + space::XXS, b.center().y - space::SM + space::XXS),
            vec2(space::LG + space::XS + space::XXS, space::LG - space::XXS),
        );
        ui.painter().rect_stroke(dr, CornerRadius::same(radius::sm().saturating_sub(1)), Stroke::new(size::HAIRLINE, t.surface1), StrokeKind::Inside);
        ui.painter().text(dr.center(), Align2::CENTER_CENTER, "F5", theme::medium(text::CAPS), t.muted);
    }
    let (c, ic) = match r.state {
        RunState::Running => (t.blue, None),
        RunState::Passed => (t.green, Some("check")),
        RunState::Failed => (t.red, Some("x")),
        RunState::NotRun => (t.muted, None),
    };
    let dw = widgets::text_width(ui, &r.detail, theme::medium(text::TINY)) + space::LG + icon::XS;
    let chip =
        Rect::from_min_size(pos2(rect.max.x - space::SM - dw, b.center().y - space::SM - space::XXS / 2.0), vec2(dw, space::LG + size::HAIRLINE));
    if r.state != RunState::NotRun {
        ui.painter().rect_filled(chip, CornerRadius::same(radius::pill()), Tokens::tint(c, alpha::PILL));
    }
    let ir = Rect::from_min_size(pos2(chip.min.x + space::XS + space::XXS, chip.center().y - icon::XS / 2.0), Vec2::splat(icon::XS));
    if running {
        icons::paint_status(ui, ir, Status::Working, icon::XS);
        icons::request_spinner_repaint(ui);
    } else if let Some(n) = ic {
        icons::paint(ui, ir, n, icon::XS, c);
    }
    ui.painter().text(pos2(ir.max.x + space::XS, chip.center().y), Align2::LEFT_CENTER, &r.detail, theme::medium(text::TINY), t.fg);
    widgets::paint_text(
        ui,
        pos2(b.max.x + space::SM, rect.max.y - space::SM - space::XXS),
        Align2::LEFT_CENTER,
        &r.command_line,
        theme::mono(text::MONO_SM),
        t.muted,
        rect.width() - space::XXL * 2.0,
    );
    resp.clicked()
}

/// The selected run's output (tail) and its actions.
fn output(ui: &mut Ui, app: &mut CsmApp, t: &Tokens, s: &SessionView, r: &RunView, actions: &mut Vec<Action>) {
    // Read the live output only when it changed (one short lock), else reuse the cached tail.
    let text: String = match app.backend.run_output(&s.id, &r.id) {
        Some(h) => {
            let key = Id::new(("runs-output", s.id.clone(), r.id.clone()));
            let cached: Option<(u64, String)> = ui.ctx().data(|d| d.get_temp(key));
            match cached {
                Some((g, txt)) if g == h.generation() => txt,
                _ => {
                    let g = h.generation();
                    let txt = h.tail(OUTPUT_TAIL);
                    ui.ctx().data_mut(|d| d.insert_temp(key, (g, txt.clone())));
                    txt
                }
            }
        }
        None => r.output.clone(),
    };
    widgets::caps(ui, &format!("Output · {}", r.label));
    let h = (ui.available_height() - size::CONTROL - space::LG).max(size::CONTROL_LG * 2.0);
    egui::Frame::new()
        .fill(t.term_bg)
        .stroke(Stroke::new(size::HAIRLINE, t.surface0))
        .corner_radius(CornerRadius::same(radius::md()))
        .inner_margin(egui::Margin::symmetric(space::ROW_X as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.set_min_size(vec2(ui.available_width(), h));
            ui.set_max_height(h);
            egui::ScrollArea::vertical()
                .id_salt(("runs-out-scroll", s.id.clone(), r.id.clone()))
                .stick_to_bottom(true)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    let shown = if text.is_empty() {
                        if r.state == RunState::NotRun && r.exit_code.is_none() { "Not run yet." } else { "No output yet." }
                    } else {
                        text.as_str()
                    };
                    ui.add(egui::Label::new(egui::RichText::new(shown).font(theme::mono(text::MONO_SM)).color(t.term_fg)).wrap().selectable(true));
                });
        });
    ui.horizontal_wrapped(|ui| {
        let running = r.state == RunState::Running;
        if running {
            if widgets::button(ui, ButtonKind::Secondary, Some("stop"), "Stop", Some("Shift+F5")).clicked() {
                actions.push(Action::Tool("run_stop", json!({"session_id": s.id, "run_id": r.id})));
            }
        } else if widgets::button(ui, ButtonKind::Secondary, Some("play"), "Run", None).clicked() {
            actions.push(Action::Tool("run_start", json!({"session_id": s.id, "run_id": r.id})));
        }
        if widgets::button(ui, ButtonKind::Secondary, Some("copy"), "Copy", None).clicked() {
            actions.push(Action::CopyAs(text.clone(), format!("Copied the output of {}.", r.label)));
        }
        if s.is_claude() {
            if widgets::button(ui, ButtonKind::Secondary, Some("send"), "Send to Claude", None)
                .on_hover_text("Pastes the output into Claude's prompt as a fenced block, without pressing Enter")
                .clicked()
            {
                // Untrusted text: pasted with bracketed paste, never submitted.
                actions.push(Action::Tool("session_send_text", json!({"session_id": s.id, "text": fenced(&r.command_line, &text), "submit": false})));
            }
        } else if widgets::button(ui, ButtonKind::Secondary, Some("send"), "Send to terminal", None)
            .on_hover_text("Copies the output: pasting it into a shell would run each line")
            .clicked()
        {
            actions
                .push(Action::CopyAs(text.clone(), "Copied the output: a shell would run each pasted line, so paste it where you mean to.".into()));
        }
    });
}

/// The inline editor: label, command line, mode, post-run URL, default; add, delete, reorder.
fn editor(ui: &mut Ui, s: &SessionView, mut rows: Vec<EditRow>, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let key = editor_id(&s.id);
    widgets::caps(ui, "Edit run commands");
    let mut remove: Option<usize> = None;
    let mut swap: Option<(usize, usize)> = None;
    let mut make_default: Option<usize> = None;
    let n = rows.len();
    egui::ScrollArea::vertical()
        .id_salt(("runs-editor-scroll", s.id.clone()))
        .max_height((ui.available_height() - size::CONTROL_LG * 3.0).max(size::CONTROL_LG * 4.0))
        .show(ui, |ui| {
            for (i, r) in rows.iter_mut().enumerate() {
                widgets::card_frame(&t).inner_margin(egui::Margin::symmetric(space::SM as i8, space::SM as i8)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        if ui.radio(r.is_default, "").on_hover_text("The default: the Run button and F5").clicked() {
                            make_default = Some(i);
                        }
                        ui.add(
                            egui::TextEdit::singleline(&mut r.label)
                                .hint_text("Label")
                                .desired_width((ui.available_width() - size::CONTROL_SM * 3.0 - space::XL).max(size::CONTROL_LG * 2.0))
                                .id(Id::new(("run-label", s.id.clone(), i))),
                        );
                        if ui.add_enabled(i > 0, egui::Button::new("↑").small()).on_hover_text("Move up").clicked() {
                            swap = Some((i, i.saturating_sub(1)));
                        }
                        if ui.add_enabled(i + 1 < n, egui::Button::new("↓").small()).on_hover_text("Move down").clicked() {
                            swap = Some((i, i + 1));
                        }
                        let (del, _) = ui.allocate_exact_size(Vec2::splat(size::CONTROL_SM), Sense::hover());
                        if widgets::icon_button_at(ui, del, Id::new(("run-del", s.id.clone(), i)), "trash", false, "Delete").clicked() {
                            remove = Some(i);
                        }
                    });
                    ui.add(
                        egui::TextEdit::singleline(&mut r.command_line)
                            .hint_text("Command line, e.g. npm run dev")
                            .font(theme::mono(text::MONO))
                            .desired_width(f32::INFINITY)
                            .id(Id::new(("run-cmd", s.id.clone(), i))),
                    );
                    ui.horizontal(|ui| {
                        egui::ComboBox::from_id_salt(("run-mode", s.id.clone(), i))
                            .selected_text(if r.powershell { "PowerShell" } else { "Process" })
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut r.powershell, false, "Process")
                                    .on_hover_text("cmd /c on Windows, the login shell elsewhere");
                                ui.selectable_value(&mut r.powershell, true, "PowerShell").on_hover_text("pwsh: pipes, $env:, cmdlets");
                            });
                        ui.add(
                            egui::TextEdit::singleline(&mut r.post_run_url)
                                .hint_text("URL, opened once the server listens (or after success): http://localhost:5173")
                                .desired_width(f32::INFINITY)
                                .id(Id::new(("run-url", s.id.clone(), i))),
                        )
                        .on_hover_text(URL_HELP);
                    });
                });
            }
        });
    if let Some(i) = make_default {
        for (j, r) in rows.iter_mut().enumerate() {
            r.is_default = i == j;
        }
    }
    if let Some((a, b)) = swap
        && a < rows.len()
        && b < rows.len()
    {
        rows.swap(a, b);
    }
    if let Some(i) = remove
        && i < rows.len()
    {
        rows.remove(i);
    }
    let problem = rows_problem(&rows);
    let mut close = false;
    ui.horizontal_wrapped(|ui| {
        if widgets::button(ui, ButtonKind::Ghost, Some("plus"), "Add command", None).clicked() {
            let first = rows.is_empty();
            rows.push(EditRow { is_default: first, ..EditRow::default() });
        }
        if s.kind != crate::model::Kind::Ssh
            && widgets::button(ui, ButtonKind::Ghost, Some("search"), "Detect from project", None)
                .on_hover_text("Replace the list with the project type's commands")
                .clicked()
        {
            actions.push(Action::Tool("run_edit", json!({"session_id": s.id, "detect": true})));
            close = true;
        }
        if s.kind != crate::model::Kind::Ssh
            && widgets::button(ui, ButtonKind::Ghost, Some("download"), "Import from project", None)
                .on_hover_text(
                    "Add the project's commands that aren't in the list yet (a Node project: its package.json scripts), keeping yours and the default. Saves your edits first.",
                )
                .clicked()
        {
            // The import merges into the SAVED list, so valid edits are saved first (calls run in order).
            if problem.is_none() {
                actions.push(Action::Tool("run_edit", edit_params(&s.id, &rows)));
            }
            actions.push(Action::Tool("run_edit", json!({"session_id": s.id, "import": true})));
            close = true;
        }
    });
    if let Some(p) = &problem {
        ui.label(egui::RichText::new(p).font(theme::regular(text::CAPTION)).color(t.peach));
    }
    ui.horizontal(|ui| {
        let save = ui.add_enabled_ui(problem.is_none(), |ui| widgets::button(ui, ButtonKind::Primary, Some("check"), "Save", None)).inner;
        if save.clicked() {
            actions.push(Action::Tool("run_edit", edit_params(&s.id, &rows)));
            close = true;
        }
        if widgets::button(ui, ButtonKind::Secondary, None, "Cancel", None).clicked() {
            close = true;
        }
    });
    ui.label(
        egui::RichText::new("Runs start in their own background terminal in the session's folder (SSH: on the host; WSL: in the distro).")
            .font(theme::regular(text::CAPTION))
            .color(t.muted),
    );
    ui.ctx().data_mut(|d| {
        if close {
            d.remove::<Vec<EditRow>>(key);
        } else {
            d.insert_temp(key, rows);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fences_are_longer_than_any_backtick_run_and_long_output_is_tailed() {
        let f = fenced("npm test", "a ```` b\nok\n");
        assert!(f.contains("`````\na ```` b\nok\n`````"), "{f}");
        let big = "x".repeat(SEND_MAX + 10);
        assert!(fenced("c", &big).len() < SEND_MAX + 64);
        assert!(fenced("a`b", "").starts_with("Output of `a'b`"));
    }

    #[test]
    fn editor_rows_become_run_edit_params() {
        let rows = vec![
            EditRow {
                id: "a".into(),
                label: " Dev ".into(),
                command_line: "npm run dev".into(),
                powershell: false,
                post_run_url: "http://localhost:5173".into(),
                is_default: true,
            },
            EditRow { label: "Lint".into(), command_line: "npm run lint | Select -First 5".into(), powershell: true, ..EditRow::default() },
        ];
        let p = edit_params("s", &rows);
        assert_eq!(
            p["runs"][0],
            json!({"id": "a", "label": "Dev", "command_line": "npm run dev", "is_default": true, "mode": "process", "post_run_url": "http://localhost:5173"})
        );
        assert_eq!(p["runs"][1]["mode"], "powershell");
        assert!(p["runs"][1].get("id").is_none());
        assert!(rows_problem(&rows).is_none());
        assert!(rows_problem(&[EditRow { label: "x".into(), ..EditRow::default() }]).is_some());
        assert!(
            rows_problem(&[EditRow { label: "x".into(), command_line: "y".into(), post_run_url: "localhost:1".into(), ..EditRow::default() }])
                .is_some()
        );
    }
}
