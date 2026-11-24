//! The Recent menu (the title bar's clock button, Ctrl+Shift+E): the sessions used most
//! recently, live or asleep, and below them the recently closed ones.
//!
//! The order is `csm_core::recent_sessions` (the engine's `session_recent` answers the same),
//! without the active session, so the first row is the one used before it: Ctrl+Shift+E, Enter
//! flips between two sessions. Digits 1–9 and 0 pick a session directly; arrows move. Picking a
//! session is `session_focus` (it switches the rail tab to the session's group and wakes it when
//! asleep); picking a closed one is `session_reopen_closed`.

use egui::{Align2, CornerRadius, Id, Order, Pos2, Rect, Sense, Vec2, WidgetInfo, WidgetType, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::model::{ClosedSessionView, SessionView, Status, UiModel};
use crate::theme::{self, Tokens, alpha, icon, radius, size, space, text};
use crate::{icons, widgets};

/// How many recently closed sessions the menu lists (the ring keeps up to ten).
pub const MAX_CLOSED: usize = 5;

/// One row of the menu.
#[derive(Debug, Clone, Copy)]
pub enum Item<'a> {
    Session(&'a SessionView),
    /// Its position in Recently closed (`session_reopen_closed`'s `index`).
    Closed(usize, &'a ClosedSessionView),
}

impl Item<'_> {
    /// What picking it does.
    pub fn action(&self) -> Action {
        match self {
            Item::Session(s) => Action::Tool("session_focus", json!({"session_id": s.id})),
            Item::Closed(i, _) => Action::Tool("session_reopen_closed", json!({"index": i})),
        }
    }
}

/// The menu's rows, in order: recent sessions, then recently closed.
pub fn items(model: &UiModel) -> Vec<Item<'_>> {
    let recent = csm_core::recent_sessions::pick_by(
        &model.sessions,
        |s| s.id.as_str(),
        |s| s.last_used_ms,
        model.active.as_deref(),
        csm_core::recent_sessions::MAX_ITEMS,
    );
    let mut v: Vec<Item<'_>> = recent.into_iter().map(Item::Session).collect();
    v.extend(model.recently_closed.iter().take(MAX_CLOSED).enumerate().map(|(i, c)| Item::Closed(i, c)));
    v
}

/// The digit that picks the `i`th session row: 1–9, then 0 for the tenth.
pub fn digit_for(i: usize) -> Option<usize> {
    (i < 10).then_some((i + 1) % 10)
}

/// The row a digit picks (`0` is the tenth).
pub fn index_for_digit(d: usize) -> usize {
    if d == 0 { 9 } else { d - 1 }
}

/// How long ago, compact ("now", "5m", "3h", "2d"); blank for never.
fn ago(s: &SessionView, now_ms: u64) -> String {
    csm_core::recent_sessions::ago(s.last_used_ms, i64::try_from(now_ms).unwrap_or(i64::MAX))
}

/// Draws the menu with its top-right corner at `at`; returns its rect.
pub fn popup(ctx: &egui::Context, app: &mut CsmApp, at: Pos2, actions: &mut Vec<Action>) -> Rect {
    let t = Tokens::get(ctx);
    let model = app.model.clone();
    let list = items(&model);
    let sessions = list.iter().filter(|i| matches!(i, Item::Session(_))).count();
    let sel = app.ui.menu_sel.first().copied();
    let resp = egui::Area::new(Id::new("menu-recent")).order(Order::Foreground).pivot(Align2::RIGHT_TOP).fixed_pos(at).show(ctx, |ui| {
        widgets::pop_frame(&t).show(ui, |ui| {
            ui.set_width(size::RECENT_W);
            ui.spacing_mut().item_spacing.y = 0.0;
            widgets::menu_head(ui, "Recent sessions");
            if sessions == 0 {
                let msg = if model.sessions.is_empty() { "No sessions yet" } else { "No other sessions" };
                empty_row(ui, &t, msg);
            }
            for (i, item) in list.iter().enumerate() {
                if i == sessions {
                    widgets::separator(ui);
                    widgets::menu_head(ui, "Recently closed");
                }
                let r = row(ui, &t, item, i, sel == Some(i), model.now_ms);
                if r.hovered() && app.ui.menu_sel.first() != Some(&i) {
                    app.ui.menu_sel = vec![i];
                }
                if r.clicked() {
                    actions.push(item.action());
                    actions.push(Action::CloseMenu);
                }
            }
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                ui.add_space(space::ROW_X - space::SM + space::XS);
                ui.label(egui::RichText::new("Enter opens · 1–9 and 0 pick · arrows move").font(theme::regular(text::CAPTION)).color(t.muted));
            });
            ui.add_space(space::XXS);
        });
    });
    resp.response.rect
}

fn empty_row(ui: &mut egui::Ui, t: &Tokens, msg: &str) {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::MENU_ROW), Sense::hover());
    ui.painter().text(pos2(rect.min.x + space::ROW_X, rect.center().y), Align2::LEFT_CENTER, msg, theme::regular(text::BODY), t.muted);
}

fn row(ui: &mut egui::Ui, t: &Tokens, item: &Item<'_>, i: usize, selected: bool, now_ms: u64) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size::MENU_ROW), Sense::click());
    let p = ui.painter().clone();
    if selected || resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(radius::sm() + 1), t.row_sel);
    }
    if selected {
        widgets::focus_ring(ui, rect, radius::sm() + 1);
    }
    let cy = rect.center().y;
    let mut right = rect.max.x - space::ROW_X;
    let (label, name, sub) = match item {
        Item::Session(s) => {
            // The digit that picks it.
            if let Some(d) = digit_for(i) {
                let k = d.to_string();
                let kw = widgets::kbd_width(ui, &k);
                widgets::paint_kbd(ui, Rect::from_center_size(pos2(right - kw / 2.0, cy), vec2(kw, size::KBD)), &k);
                right -= kw + space::SM;
            }
            let ago = ago(s, now_ms);
            let aw = widgets::text_width(ui, "00m", theme::mono(text::TINY));
            p.text(pos2(right, cy), Align2::RIGHT_CENTER, &ago, theme::mono(text::TINY), t.muted);
            right -= aw + space::SM;
            if s.status != Status::Idle {
                let g = Rect::from_center_size(pos2(right - icon::SM / 2.0, cy), Vec2::splat(icon::SM));
                icons::paint_status(ui, g, s.status, icon::SM);
                right -= icon::SM + space::SM;
            }
            let accent = t.accent(s.accent);
            let accent = if s.status == Status::Asleep { accent.gamma_multiply(alpha::ASLEEP) } else { accent };
            p.circle_filled(pos2(rect.min.x + space::ROW_X + space::XS, cy), space::XS, accent);
            (format!("Recent: {} · {}", s.name, s.status_text()), s.name.as_str(), s.folder.as_str())
        }
        Item::Closed(_, c) => {
            let ir = Rect::from_center_size(pos2(rect.min.x + space::ROW_X + space::XS, cy), Vec2::splat(icon::MD));
            icons::paint(ui, ir, "history", icon::MD, t.muted);
            (format!("Reopen: {}", c.name), c.name.as_str(), c.subtitle.as_str())
        }
    };
    // Name, then the folder in what room is left.
    let x = rect.min.x + space::ROW_X + space::SM + space::MD;
    let room = (right - x).max(0.0);
    let nr = widgets::paint_text(ui, pos2(x, cy), Align2::LEFT_CENTER, name, theme::semibold(text::BODY), t.fg, room * 0.6);
    let left = right - nr.max.x - space::SM;
    if !sub.is_empty() && sub != name && left > space::XL {
        widgets::paint_text(ui, pos2(nr.max.x + space::SM, cy), Align2::LEFT_CENTER, sub, theme::regular(text::CAPTION), t.muted, left);
    }
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, label.clone()));
    if let Item::Session(s) = item
        && !s.folder.is_empty()
    {
        return resp.on_hover_text(s.folder.clone());
    }
    resp
}
