//! The title bar (`.tb`): brand, the Ctrl+K search field (or the restore pill while restoring),
//! and the right-hand buttons.

use egui::{Align2, CornerRadius, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::model::Status;
use crate::theme::{ThemeKind, Tokens, icon, radius, semibold, size, space, text};
use crate::widgets::{self, paint_kbd};
use crate::{icons, theme};

pub fn show(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    let p = ui.painter();
    p.rect_filled(rect, 0.0, t.mantle);
    p.hline(rect.x_range(), rect.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));

    // Brand.
    let logo = Rect::from_min_size(pos2(rect.min.x + space::MD, rect.center().y - space::LG / 2.0 - space::XXS), Vec2::splat(space::LG + space::XS));
    p.rect_filled(logo, CornerRadius::same(radius::SM + 1), t.blue);
    icons::paint(ui, logo, "terminal", icon::SM, t.on_accent);
    p.text(pos2(logo.max.x + space::SM, rect.center().y), Align2::LEFT_CENTER, "CodeShellManager", semibold(text::BODY), t.fg);

    // Middle: the search field, or the restore pill.
    let mid_w = size::SEARCH_W.min(rect.width() - 2.0 * (size::SIDEBAR - space::XL));
    let mid = Rect::from_center_size(rect.center(), vec2(mid_w.max(size::MENU_W), size::CONTROL));
    if let Some(r) = &app.model.restore {
        let label = format!("Restoring {} of {}", r.done.min(r.total), r.total);
        let tail = r.current.clone().map_or_else(|| "· done".to_string(), |c| format!("· {c}"));
        let w = widgets::text_width(ui, &label, theme::medium(text::CAPTION))
            + widgets::text_width(ui, &tail, theme::regular(text::CAPTION))
            + space::XXL
            + space::SM;
        let pill = Rect::from_center_size(rect.center(), vec2(w, size::CONTROL_SM));
        p.rect_filled(pill, CornerRadius::same(radius::PILL), Tokens::tint(t.blue, 0.14));
        let g = Rect::from_min_size(pos2(pill.min.x + space::ROW_X, pill.center().y - icon::SM / 2.0), Vec2::splat(icon::SM));
        icons::paint_status(ui, g, Status::Working, icon::SM);
        icons::request_spinner_repaint(ui);
        let lr = p.text(pos2(g.max.x + space::SM, pill.center().y), Align2::LEFT_CENTER, &label, theme::medium(text::CAPTION), t.fg);
        p.text(pos2(lr.max.x + space::XS + space::XXS, pill.center().y), Align2::LEFT_CENTER, tail, theme::regular(text::CAPTION), t.muted);
    } else {
        let resp = ui.interact(mid, ui.id().with("cmdk"), Sense::click());
        let stroke = if resp.hovered() { t.surface1 } else { t.surface0 };
        p.rect(mid, CornerRadius::same(radius::MD), t.field, Stroke::new(size::HAIRLINE, stroke), StrokeKind::Inside);
        let ir = Rect::from_min_size(pos2(mid.min.x + space::ROW_X, mid.center().y - icon::MD / 2.0), Vec2::splat(icon::MD));
        icons::paint(ui, ir, "search", icon::MD, if resp.hovered() { t.fg2 } else { t.muted });
        p.text(
            pos2(ir.max.x + space::SM, mid.center().y),
            Align2::LEFT_CENTER,
            "Search sessions and commands",
            theme::regular(text::BODY),
            if resp.hovered() { t.fg2 } else { t.muted },
        );
        let kw = widgets::kbd_width(ui, "Ctrl K");
        paint_kbd(ui, Rect::from_center_size(pos2(mid.max.x - space::XS - space::XXS - kw / 2.0, mid.center().y), vec2(kw, size::KBD)), "Ctrl K");
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, "Search sessions and commands"));
        if resp.clicked() {
            actions.push(Action::OpenPalette);
        }
    }

    // Right: recent, inspector, theme, settings, quit.
    let mut x = rect.max.x - space::SM;
    let side = size::CONTROL;
    let mut btn = |ui: &mut Ui, name: &str, on: bool, tip: &str| {
        x -= side;
        let r = Rect::from_min_size(pos2(x, rect.center().y - side / 2.0), Vec2::splat(side));
        x -= space::XXS;
        widgets::icon_button_at(ui, r, ui.id().with(("tb", name.to_string())), name, on, tip).clicked()
    };
    if btn(ui, "power", false, "Quit: sessions get time to exit") {
        actions.push(Action::AskQuit);
    }
    let settings_open = app.ui.settings.open;
    if btn(ui, "sliders", settings_open, "Settings  Ctrl+,") {
        actions.push(if settings_open { Action::CloseSettings } else { Action::OpenSettings });
    }
    if btn(ui, "help", app.ui.overlay == crate::app::Overlay::Help, "Keyboard shortcuts  F1 or ?") {
        actions.push(Action::ShowHelp);
    }
    let next = crate::engine_backend::theme_setting(if t.dark() { ThemeKind::Latte } else { ThemeKind::Mocha });
    let theme_icon = if t.dark() { "moon" } else { "sun" };
    let tip = if t.dark() {
        "Theme: dark. Click for light (Settings › Appearance has System)"
    } else {
        "Theme: light. Click for dark (Settings › Appearance has System)"
    };
    if btn(ui, theme_icon, false, tip) {
        actions.push(Action::Tool("settings_set", json!({"key": "theme", "value": next})));
    }
    let open = app.model.inspector.is_some();
    if btn(ui, "panelR", open, "Inspector  Ctrl+I") {
        actions.push(Action::Tool("inspector_show", json!({"tab": if open { "hidden" } else { "notes" }})));
    }
    let recent_open = matches!(app.ui.menu, Some(crate::app::Menu::Recent { .. }));
    if btn(ui, "history", recent_open, "Recent sessions  Ctrl+Shift+E") {
        actions.push(Action::OpenRecent);
    }
    // The Recent menu hangs under this button (also when Ctrl+Shift+E opens it); a press on the
    // button itself is not "outside the menu", so a second click closes it.
    let button = Rect::from_min_size(pos2(x + space::XXS, rect.center().y - side / 2.0), Vec2::splat(side));
    ui.ctx().data_mut(|d| {
        d.insert_temp(egui::Id::new(crate::app::RECENT_ANCHOR), pos2(button.max.x, rect.max.y + space::XS));
        d.insert_temp(egui::Id::new(crate::app::RECENT_ANCHOR).with("button"), button);
    });
}
