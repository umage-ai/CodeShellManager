//! The inspector (`.insp`): one right-hand panel with Notes · Runs · Git · Session tabs, following
//! the active session. Notes and Session are live; Runs shows the session's run commands from the
//! model; Git is [`super::git`].

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::icons;
use crate::model::{InspectorTab, Kind, RunState, SessionView, Status};
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

/// How long a note waits after the last keystroke before it is saved (`note_set`).
const NOTE_DEBOUNCE_S: f64 = 1.0;

pub fn show(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, t.mantle);
    ui.painter().vline(rect.min.x + size::HAIRLINE / 2.0, rect.y_range(), Stroke::new(size::HAIRLINE, t.surface0));
    let model = app.model.clone();
    let Some(s) = model.active_session().cloned() else {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, "No session selected.", theme::regular(text::BODY), t.muted);
        return;
    };
    let tab = model.inspector.unwrap_or(InspectorTab::Notes);

    // Header: who, status, close.
    let head = Rect::from_min_size(rect.min + vec2(0.0, space::SM), vec2(rect.width(), size::CONTROL));
    let cy = head.center().y;
    let stripe = Rect::from_center_size(pos2(head.min.x + space::MD + size::STRIPE / 2.0, cy), vec2(size::STRIPE, space::LG));
    ui.painter().rect_filled(stripe, CornerRadius::same(radius::SM / 2), t.accent(s.accent));
    let close = Rect::from_center_size(pos2(head.max.x - space::SM - size::CONTROL_SM / 2.0, cy), Vec2::splat(size::CONTROL_SM));
    if widgets::icon_button_at(ui, close, Id::new("insp-close"), "x", false, "Hide the inspector  Ctrl+I").clicked() {
        actions.push(Action::Tool("inspector_show", json!({"tab": "hidden"})));
    }
    let label = s.status_text();
    let pill_w = icon::SM + space::XS + widgets::text_width(ui, &label, theme::medium(text::CAPTION)) + space::LG;
    let pill = Rect::from_min_size(pos2(close.min.x - space::SM - pill_w, cy - size::CHIP / 2.0), vec2(pill_w, size::CHIP));
    ui.scope_builder(UiBuilder::new().max_rect(pill), |ui| widgets::status_pill(ui, s.status, &label));
    widgets::paint_text(ui, pos2(stripe.max.x + space::SM, cy), Align2::LEFT_CENTER, &s.name, semibold(text::BODY), t.fg, pill.min.x - stripe.max.x - space::LG);

    // Tabs.
    let tabs = Rect::from_min_size(pos2(rect.min.x, head.max.y + space::XS), vec2(rect.width(), size::TAB));
    ui.painter().hline(tabs.x_range(), tabs.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));
    let mut x = tabs.min.x + space::SM;
    for tb in InspectorTab::ALL {
        let w = icon::MD + space::XS + space::XXS + widgets::text_width(ui, tb.label(), theme::medium(text::BODY)) + space::LG;
        let r = Rect::from_min_size(pos2(x, tabs.min.y), vec2(w, tabs.height()));
        x += w + space::XXS;
        let resp = ui.interact(r, Id::new(("insp-tab", tb.id())), Sense::click());
        let on = tb == tab;
        let fg = if on || resp.hovered() { t.fg } else { t.muted };
        let ir = Rect::from_min_size(pos2(r.min.x + space::SM, r.center().y - icon::MD / 2.0), Vec2::splat(icon::MD));
        icons::paint(ui, ir, tb.icon(), icon::MD, if on { t.blue } else { fg });
        ui.painter().text(pos2(ir.max.x + space::XS + space::XXS, r.center().y), Align2::LEFT_CENTER, tb.label(), theme::medium(text::BODY), fg);
        if on {
            ui.painter().hline(r.x_range(), r.max.y - size::HAIRLINE, Stroke::new(size::RING, t.blue));
        }
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Inspector: {}", tb.label())));
        if resp.clicked() && !on {
            actions.push(Action::Tool("inspector_show", json!({"tab": tb.id()})));
        }
    }

    let body = Rect::from_min_max(pos2(rect.min.x + space::MD, tabs.max.y + space::MD), pos2(rect.max.x - space::MD, rect.max.y - space::MD));
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        ui.spacing_mut().item_spacing.y = space::SM;
        match tab {
            InspectorTab::Notes => notes(ui, app, &s, actions),
            InspectorTab::Runs => runs(ui, &s, actions),
            InspectorTab::Git => git(ui, app, &s, actions),
            InspectorTab::Session => session(ui, app, &s, actions),
        }
    });
}

fn meta(ui: &mut Ui, icon_name: &str, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::XS + space::XXS;
        icons::show(ui, icon_name, icon::SM, t.muted);
        ui.label(egui::RichText::new(s).font(theme::regular(text::CAPTION)).color(t.muted));
    });
}

fn notes(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let now = ui.input(|i| i.time);
    if s.kind == Kind::Ssh {
        meta(ui, "note", "Notes are kept per local folder: an SSH session has none.");
        return;
    }
    // Ask the notes store for this session's note once per session shown (the reply fills the cache).
    if app.ui.notes_cache.asked.as_deref() != Some(s.id.as_str()) {
        app.ui.notes_cache.asked = Some(s.id.clone());
        actions.push(Action::Tool("note_get", json!({"session_id": s.id})));
    }
    if let Some(why) = app.ui.notes_cache.unavailable.get(&s.id) {
        meta(ui, "note", why);
        return;
    }
    let folder = s.folder.clone();
    let saved = app.ui.notes_cache.saved.get(&s.id).cloned().unwrap_or_else(|| app.model.note(&folder).to_string());
    let (mut buf, mut last) = app.ui.notes.get(&folder).cloned().unwrap_or((saved.clone(), 0.0));
    let h = ui.available_height() - space::XXL * 2.0 - space::LG;
    let frame = egui::Frame::new()
        .fill(t.field)
        .stroke(Stroke::new(size::HAIRLINE, t.surface0))
        .corner_radius(CornerRadius::same(radius::MD))
        .inner_margin(egui::Margin::symmetric(space::MD as i8, space::ROW_X as i8));
    let resp = frame
        .show(ui, |ui| {
            ui.add_sized(
                vec2(ui.available_width(), h.max(size::CONTROL_LG * 3.0)),
                egui::TextEdit::multiline(&mut buf).id(Id::new(("notes", folder.clone()))).frame(egui::Frame::NONE).font(theme::mono(text::SMALL)).desired_rows(12),
            )
        })
        .inner;
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Notes for this folder"));
    if resp.changed() {
        last = now;
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(NOTE_DEBOUNCE_S));
    }
    let dirty = buf != saved;
    if dirty && last > 0.0 && now - last >= NOTE_DEBOUNCE_S {
        actions.push(Action::Tool("note_set", json!({"session_id": s.id, "text": buf})));
        last = 0.0;
    } else if dirty {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(NOTE_DEBOUNCE_S));
    }
    if dirty || last > 0.0 {
        app.ui.notes.insert(folder.clone(), (buf, last));
    } else {
        app.ui.notes.remove(&folder);
    }
    let same = app.ui.notes_cache.shared.get(&s.id).copied().unwrap_or_else(|| app.model.sessions.iter().filter(|x| x.folder == s.folder).count());
    let where_ = if same > 1 { format!(" · shared by {same} sessions in this folder") } else { " · kept per folder".to_string() };
    meta(ui, if dirty { "pencil" } else { "check" }, &format!("{}{where_}", if dirty { "Editing" } else { "Saved" }));
    meta(ui, "search", "Notes show up in Ctrl+F search");
}

fn runs(ui: &mut Ui, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    if s.runs.is_empty() {
        widgets::card_frame(&t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new("No run commands yet").font(semibold(text::BODY)).color(t.fg));
            ui.label(egui::RichText::new("Run commands start in their own background terminal, so they never type into this session.").font(theme::regular(text::CAPTION)).color(t.muted));
            ui.horizontal(|ui| {
                if widgets::button(ui, ButtonKind::Secondary, Some("plus"), "Add from template", None).clicked() {
                    actions.push(Action::Tool("run_edit", json!({"session_id": s.id, "runs": []})));
                }
            });
        });
        return;
    }
    widgets::caps(ui, "Commands");
    for (i, r) in s.runs.iter().enumerate() {
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG + space::LG), Sense::hover());
        if i == 0 {
            ui.painter().rect(rect, CornerRadius::same(radius::MD), t.field, Stroke::new(size::HAIRLINE, t.surface1), StrokeKind::Inside);
        }
        let running = r.state == RunState::Running;
        let b = Rect::from_min_size(pos2(rect.min.x + space::SM, rect.min.y + space::SM - space::XXS), Vec2::splat(size::CONTROL_SM));
        let tip = if running { "Stop" } else { "Run" };
        if widgets::icon_button_at(ui, b, Id::new(("run-btn", s.id.clone(), r.id.clone())), if running { "stop" } else { "play" }, false, &format!("{tip} {}", r.label)).clicked() {
            actions.push(Action::Tool(if running { "run_stop" } else { "run_start" }, json!({"session_id": s.id, "run_id": r.id})));
        }
        let lr = ui.painter().text(pos2(b.max.x + space::SM, b.center().y), Align2::LEFT_CENTER, &r.label, semibold(text::BODY), t.fg);
        if r.is_default {
            let dr = Rect::from_min_size(pos2(lr.max.x + space::XS + space::XXS, b.center().y - space::SM + space::XXS), vec2(space::LG + space::XS + space::XXS, space::LG - space::XXS));
            ui.painter().rect_stroke(dr, CornerRadius::same(radius::SM - 1), Stroke::new(size::HAIRLINE, t.surface1), StrokeKind::Inside);
            ui.painter().text(dr.center(), Align2::CENTER_CENTER, "F5", theme::medium(text::CAPS), t.muted);
        }
        let (c, ic) = match r.state {
            RunState::Running => (t.blue, None),
            RunState::Passed => (t.green, Some("check")),
            RunState::Failed => (t.red, Some("x")),
            RunState::NotRun => (t.muted, None),
        };
        let dw = widgets::text_width(ui, &r.detail, theme::medium(text::TINY)) + space::LG + icon::XS;
        let chip = Rect::from_min_size(pos2(rect.max.x - space::SM - dw, b.center().y - space::SM - space::XXS / 2.0), vec2(dw, space::LG + size::HAIRLINE));
        if r.state != RunState::NotRun {
            ui.painter().rect_filled(chip, CornerRadius::same(radius::PILL), Tokens::tint(c, alpha::PILL));
        }
        let ir = Rect::from_min_size(pos2(chip.min.x + space::XS + space::XXS, chip.center().y - icon::XS / 2.0), Vec2::splat(icon::XS));
        if running {
            icons::paint_status(ui, ir, Status::Working, icon::XS);
            icons::request_spinner_repaint(ui);
        } else if let Some(n) = ic {
            icons::paint(ui, ir, n, icon::XS, c);
        }
        ui.painter().text(pos2(ir.max.x + space::XS, chip.center().y), Align2::LEFT_CENTER, &r.detail, theme::medium(text::TINY), t.fg);
        widgets::paint_text(ui, pos2(b.max.x + space::SM, rect.max.y - space::SM - space::XXS), Align2::LEFT_CENTER, &r.command_line, theme::mono(text::MONO_SM), t.muted, rect.width() - space::XXL * 2.0);
    }
    let first = s.default_run();
    if let Some(r) = first {
        widgets::caps(ui, &format!("Output · {}", r.label));
        let h = (ui.available_height() - size::CONTROL - space::LG).max(size::CONTROL_LG * 2.0);
        egui::Frame::new()
            .fill(t.term_bg)
            .stroke(Stroke::new(size::HAIRLINE, t.surface0))
            .corner_radius(CornerRadius::same(radius::MD))
            .inner_margin(egui::Margin::symmetric(space::ROW_X as i8, space::SM as i8))
            .show(ui, |ui| {
                ui.set_min_size(vec2(ui.available_width(), h));
                let text_ = if r.output.is_empty() { "No output yet." } else { r.output.as_str() };
                ui.label(egui::RichText::new(text_).font(theme::mono(text::MONO_SM)).color(t.term_fg));
            });
        ui.horizontal_wrapped(|ui| {
            if widgets::button(ui, ButtonKind::Secondary, Some("stop"), "Stop", None).clicked() {
                actions.push(Action::Tool("run_stop", json!({"session_id": s.id, "run_id": r.id})));
            }
            if widgets::button(ui, ButtonKind::Secondary, Some("copy"), "Copy", None).clicked() {
                actions.push(Action::CopyText(r.output.clone()));
            }
            if s.is_claude() && widgets::button(ui, ButtonKind::Secondary, Some("send"), "Send to Claude", None).clicked() {
                // Untrusted text: pasted with bracketed paste, never submitted.
                let fenced = format!("Output of `{}`:\n```\n{}\n```\n", r.command_line, r.output);
                actions.push(Action::Tool("session_send_text", json!({"session_id": s.id, "text": fenced, "submit": false})));
            }
        });
    }
}

/// The Git tab lives in [`super::git`].
fn git(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    super::git::tab(ui, app, s, actions);
}

fn session(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let group = s.group.as_deref().and_then(|g| app.model.group(g)).map_or_else(|| "Ungrouped".to_string(), |g| g.name.clone());
    let kind = match s.kind {
        Kind::Local => "Local",
        Kind::Ssh => "SSH",
        Kind::Wsl => "WSL",
    };
    egui::Grid::new("session-kv").num_columns(2).spacing(vec2(space::MD, space::XS + space::XXS)).show(ui, |ui| {
        let k = |ui: &mut Ui, s: &str| {
            ui.label(egui::RichText::new(s).font(theme::regular(text::SMALL)).color(t.muted));
        };
        k(ui, "Kind");
        ui.label(egui::RichText::new(kind).font(theme::regular(text::SMALL)));
        ui.end_row();
        k(ui, "Command");
        ui.label(egui::RichText::new(&s.command).font(theme::mono(text::MONO)));
        ui.end_row();
        k(ui, "Folder");
        ui.add(egui::Label::new(egui::RichText::new(&s.folder).font(theme::mono(text::MONO))).truncate()).on_hover_text(&s.folder);
        ui.end_row();
        k(ui, "Group");
        ui.label(egui::RichText::new(group).font(theme::regular(text::SMALL)));
        ui.end_row();
        k(ui, "Accent");
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(space::MD), Sense::hover());
            ui.painter().rect_filled(r, CornerRadius::same(radius::SM - 1), t.accent(s.accent));
            ui.label(egui::RichText::new("from folder").font(theme::regular(text::SMALL)));
        });
        ui.end_row();
        k(ui, "Started");
        ui.label(egui::RichText::new(&s.started).font(theme::regular(text::SMALL)));
        ui.end_row();
    });
    widgets::caps(ui, "Program status (OSC 7501)");
    widgets::card_frame(&t).inner_margin(egui::Margin::symmetric(space::MD as i8, space::ROW_X as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            icons::status(ui, s.status, icon::SM);
            ui.label(egui::RichText::new(s.status_text()).font(semibold(text::SMALL)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(egui::RichText::new(if s.is_claude() { "root · claude" } else { "root · heuristic" }).font(theme::regular(text::CAPTION)).color(t.muted));
            });
        });
        if s.is_claude() && s.status.is_live() {
            for (st, what, tag) in [(Status::Working, "Background: npm test", "task-2"), (Status::Done, "Explore agent", "task-1 · 2 min")] {
                ui.horizontal(|ui| {
                    ui.add_space(space::LG + space::XXS);
                    icons::status(ui, st, icon::SM);
                    ui.label(egui::RichText::new(what).font(theme::regular(text::SMALL)));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(tag).font(theme::regular(text::CAPTION)).color(t.muted));
                    });
                });
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        let sid = json!({"session_id": s.id});
        if widgets::button(ui, ButtonKind::Secondary, Some("pencil"), "Edit…", None).clicked() {
            actions.push(Action::Toast("Edit opens the New session panel, filled in (session_edit) — next round.".into()));
        }
        if widgets::button(ui, ButtonKind::Secondary, Some("restart"), "Restart", None).clicked() {
            actions.push(Action::Tool("session_restart", sid.clone()));
        }
        if s.status.is_live() {
            if widgets::button(ui, ButtonKind::Secondary, Some("moon"), "Sleep", None).clicked() {
                actions.push(Action::Tool("session_sleep", sid.clone()));
            }
        } else if widgets::button(ui, ButtonKind::Secondary, Some("power"), "Wake", None).clicked() {
            actions.push(Action::Tool("session_wake", sid.clone()));
        }
    });
    ui.label(
        egui::RichText::new("Asleep keeps the session, its group, notes and run commands. The process stops; Wake resumes the Claude conversation.")
            .font(theme::regular(text::CAPTION))
            .color(t.muted),
    );
}
