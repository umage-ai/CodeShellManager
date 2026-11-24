//! The inspector (`.insp`): one right-hand panel with Notes · Docs · Runs · Git · Session · Usage tabs,
//! following the active session. Notes and Session are live; Docs is [`super::docs`]; Runs shows the
//! session's run commands from the model; Git is [`super::git`].

use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::icons;
use crate::model::{InspectorTab, Kind, SessionView, Status};
use crate::theme::{self, Tokens, icon, radius, semibold, size, space, text};
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
    let usage_on = model.claude_usage_on();
    // The Usage tab belongs to the built-in Claude usage add-on: gone while it is off.
    let tab = match model.inspector.unwrap_or(InspectorTab::Notes) {
        InspectorTab::Usage if !usage_on => InspectorTab::Notes,
        t => t,
    };
    let shown: Vec<InspectorTab> = InspectorTab::ALL.into_iter().filter(|t| usage_on || *t != InspectorTab::Usage).collect();

    // Header: who, status, close.
    let head = Rect::from_min_size(rect.min + vec2(0.0, space::SM), vec2(rect.width(), size::CONTROL));
    let cy = head.center().y;
    let stripe = Rect::from_center_size(pos2(head.min.x + space::MD + size::STRIPE / 2.0, cy), vec2(size::STRIPE, space::LG));
    ui.painter().rect_filled(stripe, CornerRadius::same(radius::SM / 2), t.accent(s.accent));
    let close = Rect::from_center_size(pos2(head.max.x - space::SM - size::CONTROL_SM / 2.0, cy), Vec2::splat(size::CONTROL_SM));
    if widgets::icon_button_at(ui, close, Id::new("insp-close"), "panelRClose", false, "Hide the inspector  Ctrl+I").clicked() {
        actions.push(Action::Tool("inspector_show", json!({"tab": "hidden"})));
    }
    let label = s.status_text();
    let pill_w = icon::SM + space::XS + widgets::text_width(ui, &label, theme::medium(text::CAPTION)) + space::LG;
    let pill = Rect::from_min_size(pos2(close.min.x - space::SM - pill_w, cy - size::CHIP / 2.0), vec2(pill_w, size::CHIP));
    ui.scope_builder(UiBuilder::new().max_rect(pill), |ui| widgets::status_pill(ui, s.status, &label));
    widgets::paint_text(
        ui,
        pos2(stripe.max.x + space::SM, cy),
        Align2::LEFT_CENTER,
        &s.name,
        semibold(text::BODY),
        t.fg,
        pill.min.x - stripe.max.x - space::LG,
    );

    // Tabs.
    let tabs = Rect::from_min_size(pos2(rect.min.x, head.max.y + space::XS), vec2(rect.width(), size::TAB));
    ui.painter().hline(tabs.x_range(), tabs.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));
    let mut x = tabs.min.x + space::SM;
    // The arrow keys move along the tablist; Enter or Space selects the focused tab (a click).
    let order: Vec<Id> = shown.iter().map(|tb| Id::new(("insp-tab", tb.id()))).collect();
    let shown_tab = Id::new(("insp-tab", tab.id()));
    app.ui.region_items.extend(order.iter().map(|id| (crate::focus::FocusRegion::Inspector, *id, *id == shown_tab)));
    for id in &order {
        if crate::list_keys::move_from(ui.ctx(), *id, &order, crate::list_keys::Axis::Horizontal).is_some() {
            break;
        }
    }
    let full_w = |tb: InspectorTab| icon::MD + space::XS + space::XXS + widgets::text_width(ui, tb.label(), theme::medium(text::BODY)) + space::LG;
    // Too narrow for every label: the other tabs show their icon only (the name is the tooltip).
    let all: f32 = shown.iter().map(|tb| full_w(*tb) + space::XXS).sum();
    let compact = all > tabs.width() - space::SM * 2.0;
    for tb in shown {
        let labelled = !compact || tb == tab;
        let w = if labelled { full_w(tb) } else { icon::MD + space::LG };
        let r = Rect::from_min_size(pos2(x, tabs.min.y), vec2(w, tabs.height()));
        x += w + space::XXS;
        let resp = ui.interact(r, Id::new(("insp-tab", tb.id())), Sense::click());
        if resp.has_focus() {
            widgets::focus_ring(ui, r, radius::MD);
        }
        let on = tb == tab;
        let fg = if on || resp.hovered() { t.fg } else { t.muted };
        let ir = Rect::from_min_size(pos2(r.min.x + space::SM, r.center().y - icon::MD / 2.0), Vec2::splat(icon::MD));
        icons::paint(ui, ir, tb.icon(), icon::MD, if on { t.blue } else { fg });
        if labelled {
            ui.painter().text(pos2(ir.max.x + space::XS + space::XXS, r.center().y), Align2::LEFT_CENTER, tb.label(), theme::medium(text::BODY), fg);
        }
        if on {
            ui.painter().hline(r.x_range(), r.max.y - size::HAIRLINE, Stroke::new(size::RING, t.blue));
        }
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Inspector: {}", tb.label())));
        let resp = if labelled { resp } else { resp.on_hover_text(tb.label()) };
        if resp.clicked() && !on {
            actions.push(Action::Tool("inspector_show", json!({"tab": tb.id()})));
        }
    }

    let body = Rect::from_min_max(pos2(rect.min.x + space::MD, tabs.max.y + space::MD), pos2(rect.max.x - space::MD, rect.max.y - space::MD));
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        ui.spacing_mut().item_spacing.y = space::SM;
        match tab {
            InspectorTab::Notes => notes(ui, app, &s, actions),
            InspectorTab::Runs => super::runs::show(ui, app, &s, actions),
            InspectorTab::Git => git(ui, app, &s, actions),
            InspectorTab::Session => session(ui, app, &s, actions),
            InspectorTab::Usage => super::usage::tab(ui, app, &s, actions),
            InspectorTab::Docs => super::docs::tab(ui, app, &s, actions),
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
                egui::TextEdit::multiline(&mut buf)
                    .id(Id::new(("notes", folder.clone())))
                    .frame(egui::Frame::NONE)
                    .font(theme::mono(text::SMALL))
                    .desired_rows(12),
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
            if s.accent_overridden {
                ui.label(egui::RichText::new("set by the program").font(theme::regular(text::SMALL)));
                if ui.link(egui::RichText::new("Reset").font(theme::regular(text::SMALL))).on_hover_text("Back to the folder's colour").clicked() {
                    actions.push(Action::Tool("session_accent_reset", json!({"session_id": s.id})));
                }
            } else {
                ui.label(egui::RichText::new("from folder").font(theme::regular(text::SMALL)));
            }
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
                ui.label(
                    egui::RichText::new(if s.is_claude() { "root · claude" } else { "root · heuristic" })
                        .font(theme::regular(text::CAPTION))
                        .color(t.muted),
                );
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
            actions.push(Action::EditSession(s.id.clone()));
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
    if s.kind == Kind::Ssh {
        ports(ui, app, s, actions);
    }
}

/// The SSH session's port forwards (`session_port_forwards`): each address opens here, Stop ends
/// the forward (`session_port_unforward`). A link to a port on the host opens through one of them.
fn ports(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rows: Vec<_> = app.model.port_forwards.iter().filter(|f| f.session_id == s.id).cloned().collect();
    widgets::caps(ui, "Ports");
    if rows.is_empty() {
        ui.label(
            egui::RichText::new("No port forwards. A link to a port on this host opens through one; session_port_forward starts one.")
                .font(theme::regular(text::CAPTION))
                .color(t.muted),
        );
    }
    for f in rows {
        ui.horizontal(|ui| {
            if ui.link(egui::RichText::new(&f.url).font(theme::mono(text::MONO))).on_hover_text("Open this address on this computer").clicked() {
                actions.push(Action::Tool("open_url", json!({"url": f.url})));
            }
            ui.label(egui::RichText::new(format!("→ port {} on {}", f.remote_port, f.destination)).font(theme::regular(text::CAPTION)).color(t.muted));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, ButtonKind::Ghost, None, "Stop", None).clicked() {
                    actions.push(Action::Tool("session_port_unforward", json!({"session_id": s.id, "local_port": f.local_port})));
                }
            });
        });
    }
}
