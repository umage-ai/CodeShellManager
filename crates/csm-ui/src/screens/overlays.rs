//! Things drawn over everything: toasts (`Event::Toast`), consent cards
//! (`Event::ConsentRequested` → `consent_answer`), the busy-close dialog, the shutdown board and
//! the restore rail. A native renderer can draw these over live panes; WebView2 never could.

use egui::{Align2, CornerRadius, Id, Order, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{Action, CsmApp, Overlay};
use crate::icons;
use crate::model::{RestoreProgress, ShutdownRowState, Status};
use crate::theme::{self, Tokens, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

/// The 2px determinate rail under the title bar while sessions restore.
pub fn restore_rail(ui: &Ui, rect: Rect, r: &RestoreProgress) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(rect, 0.0, t.surface0);
    let mut fill = rect;
    let f = if r.total == 0 { 1.0 } else { r.done.min(r.total) as f32 / r.total as f32 };
    fill.set_width(rect.width() * f);
    ui.painter().rect_filled(fill, 0.0, t.blue);
}

pub fn show(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    toasts(ctx, app);
    consent_cards(ctx, app, actions);
    if app.ui.overlay == Overlay::Busy {
        busy_dialog(ctx, app, actions);
    }
    if app.ui.overlay == Overlay::Confirm {
        confirm_dialog(ctx, app, actions);
    }
    if app.ui.overlay == Overlay::Help {
        help(ctx, actions);
    }
    if app.model.shutdown.is_some() {
        shutdown_board(ctx, app, actions);
    }
}

/// A yes/no question ("Restart now?"). Enter answers yes; Esc and the scrim answer no.
fn confirm_dialog(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(c) = app.ui.confirm.clone() else {
        actions.push(Action::CloseOverlays);
        return;
    };
    let t = Tokens::get(ctx);
    scrim(ctx, "confirm-scrim");
    let yes = |actions: &mut Vec<Action>| {
        actions.push(Action::CloseOverlays);
        actions.push((*c.yes).clone());
    };
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        yes(actions);
        return;
    }
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("confirm")).order(Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same((space::LG + space::XS) as i8)).show(ui, |ui| {
            ui.set_width(size::DIALOG_W);
            ui.spacing_mut().item_spacing.y = space::MD;
            ui.label(egui::RichText::new(&c.title).font(semibold(text::TITLE)).color(t.fg));
            ui.add(egui::Label::new(egui::RichText::new(&c.body).font(theme::regular(text::BODY)).color(t.fg2)).wrap());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::button(ui, ButtonKind::Primary, None, &c.yes_label, Some("Enter")).clicked() {
                    yes(actions);
                }
                if widgets::button(ui, ButtonKind::Ghost, None, &c.no_label, Some("Esc")).clicked() {
                    actions.push(Action::CloseOverlays);
                }
            });
        });
    });
}

/// Every keyboard shortcut, in one place (`?`, F1, Ctrl+/).
pub const SHORTCUTS: [(&str, &str); 18] = [
    ("Ctrl K", "Search sessions and commands"),
    ("Ctrl T", "New session"),
    ("Ctrl Shift T", "Reopen the last closed session"),
    ("Ctrl J", "Next session that needs you"),
    ("Ctrl Tab", "Next session in this group"),
    ("Ctrl Shift Tab", "Previous session"),
    ("Ctrl 1…9", "Rail tabs (1 is All)"),
    ("Ctrl I", "Show or hide the inspector"),
    ("Ctrl W", "Close the active session"),
    ("F2", "Rename the active session"),
    ("F5", "Run the default run command"),
    ("Shift F5", "Stop it"),
    ("Ctrl ,", "Settings"),
    ("F1  ?  Ctrl /", "This list"),
    ("Esc", "Close a menu, panel or dialog"),
    ("Enter", "Open or run the selected row"),
    ("Tab", "New session: the next command"),
    ("Ctrl Enter", "New session: start and keep the panel open"),
];

fn help(ctx: &egui::Context, actions: &mut Vec<Action>) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("help-scrim")).order(Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(rect, 0.0, t.scrim);
        if resp.clicked() {
            actions.push(Action::CloseOverlays);
        }
    });
    egui::Area::new(Id::new("help")).order(Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same((space::LG + space::XS) as i8)).show(ui, |ui| {
            ui.set_width(size::HELP_W);
            ui.horizontal(|ui| {
                icons::show(ui, "keyboard", icon::XL, t.fg);
                ui.label(egui::RichText::new("Keyboard shortcuts").font(semibold(text::TITLE)).color(t.fg));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::icon_button(ui, "x", size::CONTROL_SM, false, "Close  Esc").clicked() {
                        actions.push(Action::CloseOverlays);
                    }
                });
            });
            ui.add_space(space::SM);
            egui::Grid::new("help-keys").num_columns(2).spacing(vec2(space::LG, space::XS + space::XXS)).show(ui, |ui| {
                for (keys, what) in SHORTCUTS {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::XS;
                        for k in keys.split("  ") {
                            widgets::kbd(ui, k);
                        }
                    });
                    let r = ui.label(egui::RichText::new(what).font(theme::regular(text::BODY)).color(t.fg2));
                    let label = format!("Shortcut: {keys}: {what}");
                    r.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label.clone()));
                    ui.end_row();
                }
            });
            ui.add_space(space::SM);
            ui.label(egui::RichText::new("Every command is also in Ctrl+K, by name.").font(theme::regular(text::CAPTION)).color(t.muted));
        });
    });
}

fn toasts(ctx: &egui::Context, app: &mut CsmApp) {
    let now = ctx.input(|i| i.time);
    app.ui.toasts.retain(|t| t.until > now);
    if app.ui.toasts.is_empty() {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut y = screen.max.y - space::LG - space::XS;
    for (i, toast) in app.ui.toasts.iter().enumerate().rev() {
        let resp = egui::Area::new(Id::new(("toast", i)))
            .order(Order::Tooltip)
            .interactable(false)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(pos2(screen.center().x, y))
            .show(ctx, |ui| {
                widgets::pop_frame(&t).inner_margin(egui::Margin::symmetric((space::MD + space::XXS) as i8, (space::SM + size::HAIRLINE) as i8)).show(ui, |ui| {
                    ui.set_max_width(size::TOAST_MAX_W);
                    ui.horizontal(|ui| {
                        icons::show(ui, "check", icon::LG, t.blue);
                        ui.add(egui::Label::new(egui::RichText::new(&toast.text).font(theme::regular(text::BODY)).color(t.fg)).wrap());
                    });
                });
            });
        y = resp.response.rect.min.y - space::SM;
    }
}

fn consent_cards(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    if app.ui.consents.is_empty() {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut y = screen.max.y - space::LG;
    let model = app.model.clone();
    for req in app.ui.consents.clone().iter().rev() {
        let id = req["request_id"].as_u64().unwrap_or(0);
        let client = req["client"].as_str().unwrap_or("An MCP client").to_string();
        let from = req["from_session_id"].as_str().and_then(|s| model.session(s)).map(|s| s.name.clone());
        let title = req["title"].as_str().unwrap_or("a tool").to_string();
        let tool = req["tool"].as_str().unwrap_or("").to_string();
        let summary = req["summary"].as_str().unwrap_or("").to_string();
        let params = match &req["params"] {
            Value::Null => String::new(),
            v => serde_json::to_string_pretty(v).unwrap_or_default(),
        };
        let who = match &from {
            Some(f) => format!("{client} in {f}"),
            None => client.clone(),
        };
        let resp = egui::Area::new(Id::new(("consent", id))).order(Order::Foreground).pivot(Align2::RIGHT_BOTTOM).fixed_pos(pos2(screen.max.x - space::LG, y)).show(ctx, |ui| {
            widgets::pop_frame(&t).inner_margin(egui::Margin::same(space::LG as i8)).show(ui, |ui| {
                ui.set_width(size::CONSENT_W);
                ui.horizontal(|ui| {
                    icons::status(ui, Status::Approval, icon::XL);
                    ui.label(egui::RichText::new(format!("{who} wants to run “{title}”")).font(semibold(text::BODY)).color(t.fg));
                });
                ui.label(egui::RichText::new(summary).font(theme::regular(text::CAPTION)).color(t.muted));
                ui.label(egui::RichText::new(&tool).font(theme::mono(text::MONO_SM)).color(t.fg2));
                if !params.is_empty() {
                    egui::Frame::new().fill(t.crust).corner_radius(CornerRadius::same(radius::MD)).inner_margin(egui::Margin::same(space::SM as i8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(egui::Label::new(egui::RichText::new(&params).font(theme::mono(text::MONO_SM)).color(t.fg2)).wrap());
                    });
                }
                ui.add_space(space::XS);
                ui.horizontal(|ui| {
                    if widgets::button(ui, ButtonKind::Primary, None, "Allow once", None).clicked() {
                        actions.push(Action::Consent { request_id: id, answer: "allow_once" });
                    }
                    if widgets::button(ui, ButtonKind::Secondary, None, "Always for this client", None).clicked() {
                        actions.push(Action::Consent { request_id: id, answer: "allow_always" });
                    }
                    if widgets::button(ui, ButtonKind::Ghost, None, "Don't allow", None).clicked() {
                        actions.push(Action::Consent { request_id: id, answer: "deny" });
                    }
                });
            });
        });
        y = resp.response.rect.min.y - space::SM;
    }
}

fn scrim(ctx: &egui::Context, id: &str) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(Id::new(id)).order(Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let (rect, _) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(rect, 0.0, t.scrim);
    });
}

fn busy_dialog(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ctx);
    scrim(ctx, "busy-scrim");
    // Enter and Esc both keep running.
    if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        actions.push(Action::CloseOverlays);
    }
    let busy: Vec<_> = app.model.sessions.iter().filter(|s| matches!(s.status, Status::Working | Status::Approval | Status::Question)).cloned().collect();
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("busy")).order(Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same((space::LG + space::XS) as i8)).show(ui, |ui| {
            ui.set_width(size::DIALOG_W);
            ui.spacing_mut().item_spacing.y = space::MD;
            ui.label(egui::RichText::new(format!("{} sessions are still busy", busy.len())).font(semibold(text::TITLE)).color(t.fg));
            ui.label(
                egui::RichText::new("Closing the app stops them. A Claude session that is mid-turn loses that turn; the conversation itself resumes next time.")
                    .font(theme::regular(text::BODY))
                    .color(t.fg2),
            );
            widgets::card_frame(&t).inner_margin(egui::Margin::ZERO).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.spacing_mut().item_spacing.y = 0.0;
                for (i, s) in busy.iter().enumerate() {
                    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG + space::XS), Sense::hover());
                    if i > 0 {
                        ui.painter().hline(r.x_range(), r.min.y, Stroke::new(size::HAIRLINE, t.surface0));
                    }
                    let cy = r.center().y;
                    ui.painter().rect_filled(Rect::from_center_size(pos2(r.min.x + space::MD + size::STRIPE / 2.0, cy), vec2(size::STRIPE, space::LG)), CornerRadius::same(radius::SM / 2), t.accent(s.accent));
                    ui.painter().text(pos2(r.min.x + space::MD + space::MD + size::STRIPE, cy), Align2::LEFT_CENTER, &s.name, theme::medium(text::BODY), t.fg);
                    let why = if s.status == Status::Approval { "Needs you: approval, mid-turn".to_string() } else { s.status_text() };
                    let wr = ui.painter().text(pos2(r.max.x - space::MD, cy), Align2::RIGHT_CENTER, why, theme::regular(text::CAPTION), t.muted);
                    icons::paint_status(ui, Rect::from_min_size(pos2(wr.min.x - space::XS - space::XXS - icon::SM, cy - icon::SM / 2.0), Vec2::splat(icon::SM)), s.status, icon::SM);
                }
            });
            ui.horizontal(|ui| {
                let cr = ui.add(egui::Checkbox::new(&mut app.ui.dont_warn, egui::RichText::new("Don't warn me again").font(theme::regular(text::BODY)).color(t.fg2)));
                cr.on_hover_text("Settings › General › Warn when closing busy sessions turns it back on.");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if widgets::button(ui, ButtonKind::Primary, None, "Keep running", None).clicked() {
                        actions.push(Action::CloseOverlays);
                    }
                    if widgets::button(ui, ButtonKind::Danger, None, "Close anyway", None).clicked() {
                        actions.push(Action::CloseOverlays);
                        if app.ui.dont_warn {
                            actions.push(Action::Tool("settings_set", json!({"key": "WarnOnCloseWhenBusy", "value": false})));
                        }
                        actions.push(Action::Tool("app_quit", json!({})));
                    }
                });
            });
            ui.label(egui::RichText::new("Enter and Esc both keep running.").font(theme::regular(text::CAPTION)).color(t.muted));
        });
    });
}

fn shutdown_board(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(board) = app.model.shutdown.clone() else { return };
    let t = Tokens::get(ctx);
    scrim(ctx, "shutdown-scrim");
    let screen = ctx.content_rect();
    let done = board.rows.iter().filter(|r| matches!(r.state, ShutdownRowState::Clean | ShutdownRowState::Forced)).count();
    let any_forced = board.rows.iter().any(|r| r.state == ShutdownRowState::Forced);
    let hint = if any_forced {
        "One session did not answer. It was closed without waiting; nothing else was lost."
    } else if board.budget_used_s < 6.0 {
        "Claude sessions get time to exit on their own so they can save their config."
    } else {
        "Busy Claude sessions take 2 to 5 seconds each to exit."
    };
    egui::Area::new(Id::new("shutdown")).order(Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same((space::LG + space::XS) as i8)).show(ui, |ui| {
            ui.set_width(size::BOARD_W);
            ui.spacing_mut().item_spacing.y = space::XS;
            ui.label(egui::RichText::new("Closing CodeShellManager").font(semibold(text::TITLE)).color(t.fg));
            ui.label(egui::RichText::new("Waiting for sessions to exit, one at a time.").font(theme::regular(text::BODY)).color(t.fg2));
            ui.add_space(space::SM);
            for row in &board.rows {
                let Some(s) = app.model.session(&row.session) else { continue };
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::ROW), Sense::hover());
                let cy = r.center().y;
                let g = Rect::from_min_size(pos2(r.min.x, cy - icon::MD / 2.0), Vec2::splat(icon::MD));
                match row.state {
                    ShutdownRowState::Pending => {
                        ui.painter().text(g.center(), Align2::CENTER_CENTER, "·", theme::regular(text::BODY), t.faint);
                    }
                    ShutdownRowState::Closing => {
                        icons::paint_status(ui, g, Status::Working, icon::MD);
                        icons::request_spinner_repaint(ui);
                    }
                    ShutdownRowState::Clean => icons::paint_status(ui, g, Status::Done, icon::MD),
                    ShutdownRowState::Forced => icons::paint_status(ui, g, Status::Error, icon::MD),
                }
                ui.painter().rect_filled(Rect::from_center_size(pos2(g.max.x + space::ROW_X, cy), vec2(size::STRIPE, icon::MD)), CornerRadius::same(radius::SM / 2), t.accent(s.accent));
                widgets::paint_text(ui, pos2(g.max.x + space::LG + space::XS, cy), Align2::LEFT_CENTER, &s.name, theme::regular(text::BODY), t.fg, size::MENU_W);
                let word = match row.state {
                    ShutdownRowState::Pending => "waiting",
                    ShutdownRowState::Closing if s.is_claude() => "closing, saving config",
                    ShutdownRowState::Closing => "closing",
                    ShutdownRowState::Clean => "exited cleanly",
                    ShutdownRowState::Forced => "force-closed after 10 s",
                };
                let wc = if row.state == ShutdownRowState::Forced { t.red } else { t.muted };
                ui.painter().text(pos2(r.max.x - size::GRID_TILE_W * 2.0, cy), Align2::LEFT_CENTER, word, theme::regular(text::CAPTION), wc);
                if row.state != ShutdownRowState::Pending {
                    ui.painter().text(pos2(r.max.x, cy), Align2::RIGHT_CENTER, format!("{:.1}s", row.elapsed_s), theme::mono(text::MONO), t.muted);
                }
            }
            ui.add_space(space::SM);
            let lab = |ui: &mut Ui, a: &str, b: String| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(a).font(theme::regular(text::CAPTION)).color(t.muted));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(egui::RichText::new(b).font(theme::regular(text::CAPTION)).color(t.muted));
                    });
                });
            };
            let total = board.rows.len().max(1);
            lab(ui, "Closed", format!("{done} / {}", board.rows.len()));
            widgets::bar(ui, done as f32 / total as f32, t.blue, size::BAR);
            ui.add_space(space::SM);
            lab(ui, "Wait budget", format!("{:.1} s of {:.0} s", board.budget_used_s, board.budget_s));
            widgets::bar(ui, board.budget_used_s / board.budget_s.max(1.0), t.peach, size::BAR);
            ui.add_space(space::SM);
            let waiting = done < board.rows.len();
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // `app_quit { force }`: the engine stops waiting for the remaining exits.
                    if waiting
                        && widgets::button(ui, ButtonKind::Secondary, None, "Force close now", None)
                            .on_hover_text("Everything still running is stopped now, without waiting for it to exit on its own.")
                            .clicked()
                    {
                        actions.push(Action::Tool("app_quit", json!({"force": true})));
                    }
                    ui.add(egui::Label::new(egui::RichText::new(hint).font(theme::regular(text::CAPTION)).color(t.muted)).wrap());
                });
            });
        });
    });
}
