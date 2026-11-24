//! Things drawn over everything: toasts (`Event::Toast`), consent cards
//! (`Event::ConsentRequested` → `consent_answer`), the close and busy-close dialogs, the shutdown board and
//! the restore rail. A native renderer can draw these over live panes; WebView2 never could.

use egui::{Align2, CornerRadius, Id, Order, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use csm_core::keymap::Keymap;
use serde_json::{Value, json};

use crate::app::{Action, CsmApp, Overlay};
use crate::dialog_keys::{self, Controls, Field, Pressed, Slot};
use crate::icons;
use crate::model::{RestoreProgress, SessionId, ShutdownRowState, Status};
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
    let above = open_with_offer(ctx, app, actions);
    consent_cards(ctx, app, actions, above);
    if app.ui.overlay == Overlay::Busy {
        busy_dialog(ctx, app, actions);
    }
    if app.ui.overlay == Overlay::Confirm {
        confirm_dialog(ctx, app, actions);
    }
    if app.ui.overlay == Overlay::CloseAsk {
        close_dialog(ctx, app, actions);
    }
    if app.ui.overlay == Overlay::Help {
        help(ctx, &app.prefs.keymap, actions);
    }
    if app.ui.overlay == Overlay::ConnectRemote {
        crate::screens::connect_remote::show(ctx, app, actions);
    }
    if app.model.shutdown.is_some() {
        shutdown_board(ctx, app, actions);
    }
}

/// The keys of the confirm, close and prompt dialogs (`dialog_keys`), kept under these ids.
pub const CONFIRM_ID: &str = "dlg-confirm";
pub const CLOSE_ID: &str = "dlg-close";
pub const PROMPT_ID: &str = "dlg-prompt";

/// The controls of the open modal dialog (`Overlay::Confirm`, `CloseAsk` or `Prompt`), with the
/// id its keys are kept under. Buttons are numbered in the order the dialog draws them.
pub fn dialog_controls(app: &CsmApp) -> Option<(Id, Controls)> {
    match app.ui.overlay {
        Overlay::Confirm => {
            let c = app.ui.confirm.as_ref()?;
            Some((Id::new(CONFIRM_ID), Controls { field: Field::None, buttons: 2 + usize::from(c.also.is_some()), default: Some(0) }))
        }
        Overlay::CloseAsk => Some((Id::new(CLOSE_ID), Controls { field: Field::Checkbox, buttons: 2, default: Some(0) })),
        Overlay::Prompt => app.ui.prompt.as_ref().map(|_| (Id::new(PROMPT_ID), Controls { field: Field::Text, buttons: 2, default: None })),
        _ => None,
    }
}

/// Reads the keys of the open modal dialog before the shortcuts and the terminal see them, and
/// forgets the focus of the dialogs that are not open. True when Escape cancels the dialog.
pub fn take_keys(ctx: &egui::Context, app: &CsmApp) -> bool {
    let open = dialog_controls(app);
    for id in [CONFIRM_ID, CLOSE_ID, PROMPT_ID] {
        if open.as_ref().is_none_or(|(o, _)| *o != Id::new(id)) {
            dialog_keys::forget(ctx, Id::new(id));
        }
    }
    if app.ui.overlay != Overlay::QuickOpen {
        crate::screens::palette::release_quick(ctx);
    }
    let Some((id, c)) = open else { return false };
    dialog_keys::take(ctx, id, &c) == Some(Pressed::Cancel)
}

/// Forgets the presses of the dialogs for this frame. An open menu takes the keys, so a press a
/// dialog stored earlier must not reach the dialog.
pub fn clear_presses(ctx: &egui::Context) {
    for id in [CONFIRM_ID, CLOSE_ID, PROMPT_ID] {
        dialog_keys::clear_pressed(ctx, Id::new(id));
    }
}

/// A dialog button. It takes the focus ring while it has the keyboard focus, and the default
/// button keeps the ring while no control has it. The ring sits outside the button, so it shows
/// on a primary (blue) button too.
pub fn dialog_button(ui: &mut Ui, id: Id, idx: usize, default: Option<usize>, kind: ButtonKind, label: &str, kbd: Option<&str>) -> egui::Response {
    let resp = widgets::button(ui, kind, None, label, kbd);
    let focus = dialog_keys::current(ui.ctx(), id);
    // `widgets::button` draws the ring once egui gives the button the focus; this covers the
    // dialog's own focus and the default button at rest.
    if !resp.has_focus() && (focus == Some(Slot::Button(idx)) || (focus.is_none() && default == Some(idx))) {
        widgets::focus_ring(ui, resp.rect.expand(space::XS), radius::md());
    }
    resp
}

/// The button a dialog's keys pressed this frame, if any.
pub fn pressed_button(ctx: &egui::Context, id: &str) -> Option<usize> {
    match dialog_keys::pressed(ctx, Id::new(id)) {
        Some(Pressed::Button(b)) => Some(b),
        _ => None,
    }
}

/// A yes/no question ("Restart now?"). Enter answers yes; Esc and the scrim answer no. Tab moves
/// between the buttons, and the yes button is the default.
fn confirm_dialog(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(c) = app.ui.confirm.clone() else {
        actions.push(Action::CloseOverlays);
        return;
    };
    let t = Tokens::get(ctx);
    scrim(ctx, "confirm-scrim");
    let id = Id::new(CONFIRM_ID);
    let no = 1 + usize::from(c.also.is_some());
    let mut picked = pressed_button(ctx, CONFIRM_ID);
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("confirm")).order(Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same((space::LG + space::XS) as i8)).show(ui, |ui| {
            ui.set_width(size::DIALOG_W);
            ui.spacing_mut().item_spacing.y = space::MD;
            ui.label(egui::RichText::new(&c.title).font(semibold(text::TITLE)).color(t.fg));
            ui.add(egui::Label::new(egui::RichText::new(&c.body).font(theme::regular(text::BODY)).color(t.fg2)).wrap());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if dialog_button(ui, id, 0, Some(0), ButtonKind::Primary, &c.yes_label, Some("Enter")).clicked() {
                    picked = Some(0);
                }
                if let Some((label, _)) = &c.also
                    && dialog_button(ui, id, 1, Some(0), ButtonKind::Secondary, label, None).clicked()
                {
                    picked = Some(1);
                }
                if dialog_button(ui, id, no, Some(0), ButtonKind::Ghost, &c.no_label, Some("Esc")).clicked() {
                    picked = Some(no);
                }
            });
        });
    });
    match picked {
        Some(0) => {
            actions.push(Action::CloseOverlays);
            actions.push((*c.yes).clone());
        }
        Some(b) if b == no => actions.push(Action::CloseOverlays),
        Some(_) => {
            if let Some((_, act)) = &c.also {
                actions.push(Action::CloseOverlays);
                actions.push((**act).clone());
            }
        }
        None => {}
    }
}

/// Closing the window with `CloseAction` Ask: keep running in the tray (Enter) or quit. Esc
/// cancels (the window stays). "Remember my choice" makes the answer the setting; Space on it
/// toggles it.
fn close_dialog(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ctx);
    scrim(ctx, "close-scrim");
    let id = Id::new(CLOSE_ID);
    let mut picked = pressed_button(ctx, CLOSE_ID);
    if dialog_keys::pressed(ctx, id) == Some(Pressed::Toggle) {
        app.ui.close_remember = !app.ui.close_remember;
    }
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("close-ask")).order(Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same((space::LG + space::XS) as i8)).show(ui, |ui| {
            ui.set_width(size::DIALOG_W);
            ui.spacing_mut().item_spacing.y = space::MD;
            ui.label(egui::RichText::new("Keep CodeShellManager running?").font(semibold(text::TITLE)).color(t.fg));
            ui.add(
                egui::Label::new(
                    egui::RichText::new("Your sessions keep running in the tray. Quit stops them.").font(theme::regular(text::BODY)).color(t.fg2),
                )
                .wrap(),
            );
            ui.horizontal(|ui| {
                let cr = ui.add(egui::Checkbox::new(
                    &mut app.ui.close_remember,
                    egui::RichText::new("Remember my choice").font(theme::regular(text::BODY)).color(t.fg2),
                ));
                if dialog_keys::current(ctx, id) == Some(Slot::Field) {
                    widgets::focus_ring(ui, cr.rect, radius::sm());
                }
                cr.on_hover_text("Settings › General › When I close the window changes it later.");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if dialog_button(ui, id, 0, Some(0), ButtonKind::Primary, "Keep running in tray", Some("Enter")).clicked() {
                        picked = Some(0);
                    }
                    if dialog_button(ui, id, 1, Some(0), ButtonKind::Secondary, "Quit", None).clicked() {
                        picked = Some(1);
                    }
                });
            });
            ui.label(egui::RichText::new("Esc keeps the window open.").font(theme::regular(text::CAPTION)).color(t.muted));
        });
    });
    match picked {
        Some(0) => actions.push(Action::CloseAnswer { tray: true, remember: app.ui.close_remember }),
        Some(1) => actions.push(Action::CloseAnswer { tray: false, remember: app.ui.close_remember }),
        _ => {}
    }
}

/// Keys the keyboard map does not hold: the mouse, the dialogs, the lists and the terminal. Tokens
/// are separated by two spaces, one key cap each.
pub const FIXED_SHORTCUTS: [(&str, &str); 12] = [
    ("Ctrl click", "Select sessions in the sidebar"),
    ("Shift click", "Select a range of sessions"),
    ("Ctrl click", "Open a link in a terminal"),
    ("Ctrl Alt C", "Copy a selection from Claude as shown"),
    ("Arrows  Home  End", "Move through a list, a menu or a tab strip"),
    ("Space", "Select the focused session row"),
    ("Left  Right", "On the quick-open command row: the previous or next command"),
    ("Tab  Shift Tab", "Next or previous control in a dialog or the quick-open"),
    ("Ctrl Enter", "New session: start and keep the panel open"),
    ("Esc", "Close a menu, panel or dialog, or return to the terminal"),
    ("Enter", "Open or run the selected row"),
    ("?", "This list, when nothing takes typing"),
];

/// The help list's rows: every action of the keyboard map with its chords (the rail's nine tab
/// chords are one row), then the fixed keys. Each row is (key caps, what it does).
pub fn shortcut_rows(map: &Keymap) -> Vec<(String, String)> {
    let mut rows: Vec<(String, String)> = Vec::new();
    for e in map.entries() {
        let chords: Vec<String> = e.chords().iter().map(ToString::to_string).collect();
        if chords.is_empty() {
            continue;
        }
        if e.id().starts_with("ui.rail_tab_") && e.id() != "ui.rail_tab_1" {
            continue;
        }
        let keys = if e.id() == "ui.rail_tab_1" {
            match (map.label("ui.rail_tab_1"), map.label("ui.rail_tab_9")) {
                (Some(first), Some(last)) => format!("{first} to {last}"),
                _ => chords.join("  "),
            }
        } else {
            chords.join("  ")
        };
        let what = if e.id() == "ui.rail_tab_1" { "Rail tabs (1 is All)".to_owned() } else { e.name().to_owned() };
        rows.push((keys, what));
    }
    rows.extend(FIXED_SHORTCUTS.iter().map(|(k, w)| ((*k).to_owned(), (*w).to_owned())));
    rows
}

fn help(ctx: &egui::Context, keymap: &Keymap, actions: &mut Vec<Action>) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("help-scrim")).order(Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
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
            let max_h = (ctx.content_rect().height() - size::PALETTE_TOP * 2.0).max(size::MENU_W);
            egui::ScrollArea::vertical().max_height(max_h).show(ui, |ui| {
                egui::Grid::new("help-keys").num_columns(2).spacing(vec2(space::LG, space::XS + space::XXS)).show(ui, |ui| {
                    for (keys, what) in shortcut_rows(keymap) {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = space::XS;
                            for k in keys.split("  ") {
                                widgets::kbd(ui, k);
                            }
                        });
                        let r = ui.label(egui::RichText::new(&what).font(theme::regular(text::BODY)).color(t.fg2));
                        let label = format!("Shortcut: {keys}: {what}");
                        r.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Label, true, label.clone()));
                        ui.end_row();
                    }
                });
            });
            ui.add_space(space::SM);
            ui.label(
                egui::RichText::new("Every command is also in Ctrl+K, by name. Change a chord in Settings, Keyboard.")
                    .font(theme::regular(text::CAPTION))
                    .color(t.muted),
            );
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
                widgets::pop_frame(&t)
                    .inner_margin(egui::Margin::symmetric((space::MD + space::XXS) as i8, (space::SM + size::HAIRLINE) as i8))
                    .show(ui, |ui| {
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

/// The one-time question after a CodeShellManager 0.x import: its Open with list runs programs,
/// so it is never adopted without a yes (`open_with_pending_answer`). Bottom right, like the
/// consent cards, which stack above it. Returns the top of the card (or the screen's bottom
/// margin when there is no card).
fn open_with_offer(ctx: &egui::Context, app: &CsmApp, actions: &mut Vec<Action>) -> f32 {
    let screen = ctx.content_rect();
    let bottom = screen.max.y - space::LG;
    let pending = &app.model.pending_open_with;
    if pending.is_empty() {
        return bottom;
    }
    let t = Tokens::get(ctx);
    let resp = egui::Area::new(Id::new("open-with-offer"))
        .order(Order::Foreground)
        .pivot(Align2::RIGHT_BOTTOM)
        .fixed_pos(pos2(screen.max.x - space::LG, bottom))
        .show(ctx, |ui| {
            widgets::pop_frame(&t).inner_margin(egui::Margin::same(space::LG as i8)).show(ui, |ui| {
                ui.set_width(size::CONSENT_W);
                ui.horizontal(|ui| {
                    icons::status(ui, Status::Approval, icon::XL);
                    ui.label(egui::RichText::new(OPEN_WITH_OFFER_TITLE).font(semibold(text::BODY)).color(t.fg));
                });
                ui.add(
                    egui::Label::new(
                        egui::RichText::new("Your Open with list from CodeShellManager 0.x. Each one runs the program shown when you open a folder with it.")
                            .font(theme::regular(text::CAPTION))
                            .color(t.muted),
                    )
                    .wrap(),
                );
                egui::Frame::new().fill(t.crust).corner_radius(CornerRadius::same(radius::md())).inner_margin(egui::Margin::same(space::SM as i8)).show(
                    ui,
                    |ui| {
                        ui.set_width(ui.available_width());
                        for e in pending {
                            let name = if e.is_default { format!("{} (default)", e.label) } else { e.label.clone() };
                            ui.label(egui::RichText::new(name).font(theme::medium(text::BODY)).color(t.fg));
                            for (kind, args) in [("local", &e.local_args), ("WSL", &e.wsl_args), ("SSH", &e.ssh_args)] {
                                if let Some(a) = args {
                                    let line = format!("{kind}: {} {a}", e.executable);
                                    ui.add(egui::Label::new(egui::RichText::new(line).font(theme::mono(text::MONO_SM)).color(t.fg2)).wrap());
                                }
                            }
                        }
                    },
                );
                ui.add_space(space::XS);
                ui.horizontal(|ui| {
                    if widgets::button(ui, ButtonKind::Primary, None, "Allow", None).clicked() {
                        actions.push(Action::Tool("open_with_pending_answer", json!({"adopt_open_with": true})));
                    }
                    if widgets::button(ui, ButtonKind::Ghost, None, "Don't allow", None).clicked() {
                        actions.push(Action::Tool("open_with_pending_answer", json!({"adopt_open_with": false})));
                    }
                });
            });
        });
    resp.response.rect.min.y - space::SM
}

/// The offer card's title (tests find the card by it).
pub const OPEN_WITH_OFFER_TITLE: &str = "Use these editors from CodeShellManager 0.x?";

fn consent_cards(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>, start_y: f32) {
    if app.ui.consents.is_empty() {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut y = start_y;
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
        let target = Some(&req["target"]).filter(|t| !t.is_null()).cloned();
        let who = match &from {
            Some(f) => format!("{client} in {f}"),
            None => client.clone(),
        };
        let resp = egui::Area::new(Id::new(("consent", id)))
            .order(Order::Foreground)
            .pivot(Align2::RIGHT_BOTTOM)
            .fixed_pos(pos2(screen.max.x - space::LG, y))
            .show(ctx, |ui| {
                widgets::pop_frame(&t).inner_margin(egui::Margin::same(space::LG as i8)).show(ui, |ui| {
                    ui.set_width(size::CONSENT_W);
                    ui.horizontal(|ui| {
                        icons::status(ui, Status::Approval, icon::XL);
                        ui.label(egui::RichText::new(format!("{who} wants to run “{title}”")).font(semibold(text::BODY)).color(t.fg));
                    });
                    ui.label(egui::RichText::new(summary).font(theme::regular(text::CAPTION)).color(t.muted));
                    ui.label(egui::RichText::new(&tool).font(theme::mono(text::MONO_SM)).color(t.fg2));
                    // A New session form runs an add-on tool: the card names it and its effects.
                    if let Some(target) = target.as_ref() {
                        let line = format!("runs {} ({})", target["tool"].as_str().unwrap_or("?"), target["effects"].as_str().unwrap_or("unknown effects"));
                        ui.label(egui::RichText::new(line).font(theme::regular(text::CAPTION)).color(t.muted));
                    }
                    if !params.is_empty() {
                        egui::Frame::new()
                            .fill(t.crust)
                            .corner_radius(CornerRadius::same(radius::md()))
                            .inner_margin(egui::Margin::same(space::SM as i8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.add(egui::Label::new(egui::RichText::new(&params).font(theme::mono(text::MONO_SM)).color(t.fg2)).wrap());
                            });
                    }
                    ui.add_space(space::XS);
                    ui.horizontal(|ui| {
                        if widgets::button(ui, ButtonKind::Primary, None, "Allow once", None).clicked() {
                            actions.push(Action::Consent { request_id: id, answer: "allow_once" });
                        }
                        // Never offered for a form's add-on tool: the engine does not keep such a grant.
                        if target.is_none() && widgets::button(ui, ButtonKind::Secondary, None, "Always for this client", None).clicked() {
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
    egui::Area::new(Id::new(id)).order(Order::Middle).fixed_pos(screen.min).show(ctx, |ui| {
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
    let busy: Vec<_> =
        app.model.sessions.iter().filter(|s| matches!(s.status, Status::Working | Status::Approval | Status::Question)).cloned().collect();
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("busy")).order(Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same((space::LG + space::XS) as i8)).show(ui, |ui| {
            ui.set_width(size::DIALOG_W);
            ui.spacing_mut().item_spacing.y = space::MD;
            ui.label(egui::RichText::new(format!("{} sessions are still busy", busy.len())).font(semibold(text::TITLE)).color(t.fg));
            ui.label(
                egui::RichText::new(
                    "Closing the app stops them. A Claude session that is mid-turn loses that turn; the conversation itself resumes next time.",
                )
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
                    ui.painter().rect_filled(
                        Rect::from_center_size(pos2(r.min.x + space::MD + size::STRIPE / 2.0, cy), vec2(size::STRIPE, space::LG)),
                        CornerRadius::same(radius::sm() / 2),
                        t.accent(s.accent),
                    );
                    ui.painter().text(
                        pos2(r.min.x + space::MD + space::MD + size::STRIPE, cy),
                        Align2::LEFT_CENTER,
                        &s.name,
                        theme::medium(text::BODY),
                        t.fg,
                    );
                    let why = if s.status == Status::Approval { "Needs you: approval, mid-turn".to_string() } else { s.status_text() };
                    let wr = ui.painter().text(pos2(r.max.x - space::MD, cy), Align2::RIGHT_CENTER, why, theme::regular(text::CAPTION), t.muted);
                    icons::paint_status(
                        ui,
                        Rect::from_min_size(pos2(wr.min.x - space::XS - space::XXS - icon::SM, cy - icon::SM / 2.0), Vec2::splat(icon::SM)),
                        s.status,
                        icon::SM,
                    );
                }
            });
            ui.horizontal(|ui| {
                let cr = ui.add(egui::Checkbox::new(
                    &mut app.ui.dont_warn,
                    egui::RichText::new("Don't warn me again").font(theme::regular(text::BODY)).color(t.fg2),
                ));
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

/// The shutdown board. It always fits the window: the header (title, the count and the bar) and
/// the footer (Force close now) stay put, and the rows scroll between them, the one closing now
/// first, then the queue in the engine's order, then the ones that are done.
fn shutdown_board(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(board) = app.model.shutdown.clone() else { return };
    let t = Tokens::get(ctx);
    scrim(ctx, "shutdown-scrim");
    let screen = ctx.content_rect();
    let total = board.rows.len();
    let done = board.done();
    let closing = board.rows.iter().filter(|r| r.state == ShutdownRowState::Closing).count();
    let queued = total.saturating_sub(done + closing);
    let any_forced = board.rows.iter().any(|r| r.state == ShutdownRowState::Forced);
    let hint = if any_forced {
        "A session did not answer. It was closed without waiting; nothing else was lost."
    } else if board.budget_used_s < 6.0 {
        "Claude sessions get time to exit on their own so they can save their config."
    } else {
        "Busy Claude sessions take 2 to 5 seconds each to exit."
    };
    let summary = match (done, closing) {
        _ if done == total => format!("Closed all {total} sessions"),
        (_, 0) => format!("Closing {total} sessions: {done} done, {queued} queued"),
        _ => format!("Closing {total} sessions: {done} done, {closing} closing, {queued} queued"),
    };
    let is_claude = |id: &String| app.model.session(id).is_some_and(|s| s.is_claude());
    let rows: Vec<_> = board.ordered(is_claude).into_iter().filter_map(|r| app.model.session(&r.session).map(|s| (r, s))).collect();
    let next_claude: Vec<SessionId> = board.next_up(is_claude).into_iter().cloned().collect();

    let pad = space::LG + space::XS;
    let width = size::BOARD_W.min(screen.width() - 2.0 * (space::LG + pad)).max(size::MENU_W);
    let max_h = (screen.height() - 2.0 * (space::LG + pad)).max(size::ROW * 2.0);
    // The footer's height from the last frame, so the list leaves room for it.
    let footer_id = Id::new("shutdown-footer-h");
    let footer_h = ctx.data(|d| d.get_temp::<f32>(footer_id)).unwrap_or(size::CONTROL_LG + size::ROW);
    egui::Area::new(Id::new("shutdown")).order(Order::Foreground).pivot(Align2::CENTER_CENTER).fixed_pos(screen.center()).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same(pad as i8)).show(ui, |ui| {
            ui.set_width(width);
            ui.set_max_height(max_h);
            ui.spacing_mut().item_spacing.y = space::XS;
            let top = ui.cursor().min.y;
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new("Closing CodeShellManager").font(semibold(text::TITLE)).color(t.fg));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(egui::RichText::new(format!("{:.0} s", board.budget_used_s)).font(theme::regular(text::CAPTION)).color(t.muted))
                        .on_hover_text("Time since closing began.");
                });
            });
            ui.label(egui::RichText::new(summary).font(theme::medium(text::BODY)).color(t.fg2));
            widgets::bar(ui, done as f32 / total.max(1) as f32, t.blue, size::BAR);
            ui.add(
                egui::Label::new(
                    egui::RichText::new(board_explanation(board.slots()))
                    .font(theme::regular(text::CAPTION))
                    .color(t.muted),
                )
                .wrap(),
            );
            ui.add_space(space::SM);
            let header_h = ui.cursor().min.y - top;
            let list_h = (max_h - header_h - footer_h - space::SM).max(size::ROW * 2.0);
            // show_rows spaces rows by the item spacing too: none, so the list is exactly its rows.
            ui.spacing_mut().item_spacing.y = 0.0;
            egui::ScrollArea::vertical().id_salt("shutdown-rows").max_height(list_h).auto_shrink([false, true]).show_rows(
                ui,
                size::ROW,
                rows.len(),
                |ui, range| {
                    for (row, s) in rows.iter().skip(range.start).take(range.len()) {
                        shutdown_row(ui, &t, row, s, next_claude.contains(&row.session));
                    }
                },
            );
            ui.add_space(space::SM);
            let foot = ui.cursor().min.y;
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // `app_quit { force }`: the engine stops waiting for the remaining exits.
                    if done < total
                        && widgets::button(ui, ButtonKind::Secondary, None, "Force close now", None)
                            .on_hover_text("Everything still running is stopped now, without waiting for it to exit on its own.")
                            .clicked()
                    {
                        actions.push(Action::Tool("app_quit", json!({"force": true})));
                    }
                    ui.add(egui::Label::new(egui::RichText::new(hint).font(theme::regular(text::CAPTION)).color(t.muted)).wrap());
                });
            });
            let fh = ui.cursor().min.y - foot;
            ctx.data_mut(|d| d.insert_temp(footer_id, fh));
        });
    });
}

/// The board's explanation line: how Claude sessions exit, and what keeps their config safe.
pub(crate) fn board_explanation(slots: usize) -> String {
    if slots <= 1 {
        "Claude sessions exit one at a time, so each saves its config before the next starts. Everything else stops at once.".into()
    } else {
        format!(
            "Claude sessions exit {slots} at a time, a moment apart, so each can save its config; ~/.claude.json is backed up first and checked after. Everything else stops at once."
        )
    }
}

/// One row of the shutdown board: state glyph, accent, name, what it is doing or waiting for, and
/// for a Claude session that finished, how long its exit took.
fn shutdown_row(ui: &mut Ui, t: &Tokens, row: &crate::model::ShutdownRow, s: &crate::model::SessionView, next: bool) {
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
    ui.painter().rect_filled(
        Rect::from_center_size(pos2(g.max.x + space::ROW_X, cy), vec2(size::STRIPE, icon::MD)),
        CornerRadius::same(radius::sm() / 2),
        t.accent(s.accent),
    );
    let claude = s.is_claude();
    // An exit measured at zero was no wait: the process was already gone when its turn came.
    let instant = row.elapsed_s < 0.05;
    let word = match row.state {
        ShutdownRowState::Pending if claude && next => "next",
        ShutdownRowState::Pending => "queued",
        ShutdownRowState::Closing if claude => "waiting for Claude to exit",
        ShutdownRowState::Closing => "stopping",
        ShutdownRowState::Clean if !claude => "stopped",
        ShutdownRowState::Clean if instant => "had already exited",
        ShutdownRowState::Clean => "exited cleanly",
        ShutdownRowState::Forced => "force-closed",
    };
    let wc = match row.state {
        ShutdownRowState::Forced => t.red,
        ShutdownRowState::Closing => t.fg2,
        _ => t.muted,
    };
    let word_x = r.max.x - size::GRID_TILE_W * 2.0;
    let name_x = g.max.x + space::LG + space::XS;
    widgets::paint_text(ui, pos2(name_x, cy), Align2::LEFT_CENTER, &s.name, theme::regular(text::BODY), t.fg, word_x - name_x - space::SM);
    ui.painter().text(pos2(word_x, cy), Align2::LEFT_CENTER, word, theme::regular(text::CAPTION), wc);
    if claude && matches!(row.state, ShutdownRowState::Clean | ShutdownRowState::Forced) && !instant {
        ui.painter().text(pos2(r.max.x, cy), Align2::RIGHT_CENTER, format!("{:.1}s", row.elapsed_s), theme::mono(text::MONO), t.muted);
    }
}

#[cfg(test)]
mod tests {
    use super::board_explanation;

    #[test]
    fn the_board_explains_how_many_claude_sessions_exit_at_once() {
        assert!(board_explanation(1).contains("one at a time"));
        assert!(board_explanation(0).contains("one at a time"), "an unknown count reads as one");
        let four = board_explanation(4);
        assert!(four.contains("exit 4 at a time"), "{four}");
        assert!(four.contains("~/.claude.json"), "{four}");
        assert!(!four.contains("one at a time"));
    }
}
