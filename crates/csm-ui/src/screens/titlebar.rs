//! The title bar (`.tb`): brand, the Ctrl+K search field (or the restore pill while restoring),
//! and the right-hand buttons.
//!
//! On Windows the main window has no OS title bar (`CsmApp::window_controls`): this row is the
//! window's only one. It then also carries minimize, maximize/restore and close at its right end,
//! and [`resize_edges`] lets the window's edges resize it. Its empty space drags the window and a
//! double-click maximizes on every platform. Torn-off pane and group windows keep the OS frame.

use egui::viewport::ResizeDirection;
use egui::{Align2, Color32, CornerRadius, CursorIcon, Id, Order, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, ViewportCommand, pos2, vec2};
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
    // A theme's own title bar colours (win98's navy bar), dimmed while the window is not focused.
    let focused = ui.ctx().input(|i| i.viewport().focused.unwrap_or(true));
    let (bar_fg, own_fg) = (t.title_fg(focused), t.custom_title().then(|| t.title_fg(focused)));
    p.rect_filled(rect, 0.0, t.title_bg(focused));
    let line = if t.style.bevel { t.bevel_sh() } else { t.surface0 };
    p.hline(rect.x_range(), rect.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, line));

    // The bar's empty space behaves like a window title bar: drag moves the window, double-click
    // maximizes or restores. Registered first, so the search field and buttons drawn on top of it
    // take their own clicks.
    let bg = ui.interact(rect, ui.id().with("tb-drag"), Sense::click_and_drag());
    if bg.double_clicked() {
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!maximized));
    } else if bg.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }

    // Brand.
    let logo = Rect::from_min_size(pos2(rect.min.x + space::MD, rect.center().y - space::LG / 2.0 - space::XXS), Vec2::splat(space::LG + space::XS));
    p.rect_filled(logo, CornerRadius::same(radius::sm() + 1), t.blue);
    icons::paint(ui, logo, "terminal", icon::SM, t.on_accent);
    let brand = p.text(pos2(logo.max.x + space::SM, rect.center().y), Align2::LEFT_CENTER, "CodeShellManager", semibold(text::BODY), bar_fg);

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
        p.rect_filled(pill, CornerRadius::same(radius::pill()), Tokens::tint(t.blue, 0.14));
        let g = Rect::from_min_size(pos2(pill.min.x + space::ROW_X, pill.center().y - icon::SM / 2.0), Vec2::splat(icon::SM));
        icons::paint_status(ui, g, Status::Working, icon::SM);
        icons::request_spinner_repaint(ui);
        let lr = p.text(pos2(g.max.x + space::SM, pill.center().y), Align2::LEFT_CENTER, &label, theme::medium(text::CAPTION), t.fg);
        p.text(pos2(lr.max.x + space::XS + space::XXS, pill.center().y), Align2::LEFT_CENTER, tail, theme::regular(text::CAPTION), t.muted);
    } else {
        let resp = ui.interact(mid, ui.id().with("cmdk"), Sense::click());
        let stroke = if resp.hovered() { t.surface1 } else { t.surface0 };
        p.rect(mid, CornerRadius::same(radius::md()), t.field, Stroke::new(size::HAIRLINE, stroke), StrokeKind::Inside);
        if t.style.bevel {
            widgets::paint_bevel(p, mid, widgets::Edge::Sunken, &t);
        }
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

    // The add-on status dot (the worst `csm/status` of the running add-ons), just after the brand.
    // Remote access, while it is on, follows it between the brand and the search field.
    let addon_status = crate::screens::addons::worst_status(&app.model.addons);
    let dot_room = if let Some((tone, text)) = &addon_status {
        crate::screens::addons::title_dot(ui, *tone, text, brand.max.x + space::SM, rect.center().y, actions);
        size::STATUS_DOT + space::SM
    } else {
        0.0
    };
    crate::screens::remote::indicator(ui, app, brand.max.x + space::LG + dot_room, mid.min.x - space::MD, rect.center().y, actions);

    // Right: the window's own buttons (frameless only), then recent, inspector, theme, settings,
    // quit, set apart from them.
    let mut x = if app.window_controls { window_controls(ui, rect, bar_fg) - space::MD } else { rect.max.x - space::SM };
    let side = size::CONTROL;
    // The buttons drawn, for the keyboard (`region_items`): F6 enters at the main menu button.
    let mut ids: Vec<egui::Id> = Vec::new();
    let mut btn = |ui: &mut Ui, name: &str, on: bool, tip: &str| {
        x -= side;
        let r = Rect::from_min_size(pos2(x, rect.center().y - side / 2.0), Vec2::splat(side));
        x -= space::XXS;
        let resp = widgets::icon_button_at_in(ui, r, ui.id().with(("tb", name.to_string())), name, on, tip, own_fg);
        ids.push(resp.id);
        resp.clicked()
    };
    // A remote window's button disconnects; the office keeps running (see `Action::AskQuit`).
    let power_tip = match &app.remote {
        Some(r) => format!("Disconnect from {}: the office keeps running", r.host_name),
        None => "Quit: sessions get time to exit".to_owned(),
    };
    if btn(ui, "power", false, &power_tip) {
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
    // The main menu, for the mouse too (F10: `app::shortcuts`), left of the
    // others. Its button is the menu's anchor, so a second click closes the menu rather than
    // reopening it.
    let main_open = matches!(app.ui.menu, Some(crate::app::Menu::Main { .. }));
    let main_button = Rect::from_min_size(pos2(x - side, rect.center().y - side / 2.0), Vec2::splat(side));
    ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new(crate::app::MAIN_ANCHOR), main_button));
    let main_id = ui.id().with("tb-main");
    app.ui.region_items.extend(ids.iter().map(|id| (crate::focus::FocusRegion::TitleBar, *id, false)));
    app.ui.region_items.push((crate::focus::FocusRegion::TitleBar, main_id, true));
    let main_tip = match app.prefs.keymap.label("ui.main_menu") {
        Some(chord) => format!("Main menu  {chord}"),
        None => "Main menu".to_owned(),
    };
    if widgets::icon_button_at(ui, main_button, main_id, "more", main_open, &main_tip).clicked() {
        actions.push(if main_open {
            Action::CloseMenu
        } else {
            Action::OpenMenu(crate::app::Menu::Main { at: crate::screens::menus::main_menu_at(ui.ctx()) })
        });
    }
    // Left and Right move the focus along the buttons, left to right: the buttons were drawn right
    // to left, and the main menu button is the leftmost.
    let mut order: Vec<egui::Id> = vec![main_id];
    order.extend(ids.iter().rev().copied());
    for id in &order {
        if crate::list_keys::move_from(ui.ctx(), *id, &order, crate::list_keys::Axis::Horizontal).is_some() {
            break;
        }
    }
    x -= side + space::XXS;
    update_badge(ui, app, rect, mid, x, actions);
}

/// The accessible name of the update badge (tests find it by this prefix).
pub const UPDATE_BADGE_PREFIX: &str = "Update available: ";

/// "Update available: 2.1.0", left of the buttons (`update_check` found a newer release): a click
/// opens its release page (`open_url`, nothing is downloaded), its × hides it (`update_dismiss`).
/// Left out while the bar is too narrow to keep it clear of the search field.
fn update_badge(ui: &mut Ui, app: &CsmApp, bar: Rect, mid: Rect, right: f32, actions: &mut Vec<Action>) {
    let Some(u) = &app.model.update else { return };
    let t = Tokens::get(ui.ctx());
    let label = format!("{UPDATE_BADGE_PREFIX}{}", u.version);
    let font = theme::medium(text::CAPTION);
    let close = size::CONTROL_SM;
    let w = space::ROW_X + icon::SM + space::XS + widgets::text_width(ui, &label, font.clone()) + space::XS + close;
    let right = right - space::SM;
    if right - w < mid.max.x + space::SM {
        return;
    }
    let pill = Rect::from_min_max(pos2(right - w, bar.center().y - size::CONTROL_SM / 2.0), pos2(right, bar.center().y + size::CONTROL_SM / 2.0));
    let body = Rect::from_min_max(pill.min, pos2(pill.max.x - close, pill.max.y));
    let resp = ui.interact(body, ui.id().with("tb-update"), Sense::click());
    let p = ui.painter();
    p.rect_filled(pill, CornerRadius::same(radius::pill()), Tokens::tint(t.green, if resp.hovered() { 0.24 } else { 0.14 }));
    let g = Rect::from_min_size(pos2(pill.min.x + space::ROW_X, pill.center().y - icon::SM / 2.0), Vec2::splat(icon::SM));
    icons::paint(ui, g, "arrowUp", icon::SM, t.green);
    ui.painter().text(pos2(g.max.x + space::XS, pill.center().y), Align2::LEFT_CENTER, &label, font, t.fg);
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label.clone()));
    if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Open the release page (nothing is downloaded)").clicked() {
        actions.push(Action::Tool("open_url", json!({"url": u.url})));
    }
    let x_rect = Rect::from_center_size(pos2(pill.max.x - close / 2.0, pill.center().y), Vec2::splat(close));
    if widgets::icon_button_at(ui, x_rect, ui.id().with("tb-update-x"), "x", false, "Dismiss the update notice").clicked() {
        actions.push(Action::Tool("update_dismiss", json!({"version": u.version})));
    }
}

/// Minimize, maximize/restore and close at the bar's right end, Windows-sized; returns their left
/// edge. Close is the OS close request ([`ViewportCommand::Close`]), so it does what the OS
/// button did: `CloseAction` decides (see [`crate::background`]).
fn window_controls(ui: &mut Ui, bar: Rect, bar_fg: Color32) -> f32 {
    let t = Tokens::get(ui.ctx());
    let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
    let w = size::CAPTION_BUTTON_W;
    let (max_icon, max_label) = if maximized { ("unmax", "Restore") } else { ("max", "Maximize") };
    let buttons = [
        ("x", "Close", ViewportCommand::Close),
        (max_icon, max_label, ViewportCommand::Maximized(!maximized)),
        ("min", "Minimize", ViewportCommand::Minimized(true)),
    ];
    let mut x = bar.max.x;
    for (icon_name, label, command) in buttons {
        x -= w;
        // Above the bar's hairline, so the line runs on under the buttons.
        let r = Rect::from_min_max(pos2(x, bar.min.y), pos2(x + w, bar.max.y - size::HAIRLINE));
        let resp = ui.interact(r, ui.id().with(("caption", label)), Sense::click());
        let close = label == "Close";
        let (bg, fg) = match (close, resp.hovered(), resp.is_pointer_button_down_on()) {
            (true, true, _) => (t.close_hover, t.on_close_hover),
            (false, _, true) => (t.hover, t.fg),
            (false, true, false) => (t.row_sel, t.fg),
            _ => (Color32::TRANSPARENT, t.fg2),
        };
        if t.style.bevel {
            // 3D caption buttons: small raised squares (sunken while pressed), the bar's own colours around.
            let b = Rect::from_center_size(r.center(), vec2(w - space::SM, size::CONTROL_SM - space::SM));
            let down = resp.is_pointer_button_down_on();
            ui.painter().rect_filled(b, 0.0, if close && resp.hovered() { t.close_hover } else { t.bg });
            widgets::paint_bevel(ui.painter(), b, if down { widgets::Edge::Sunken } else { widgets::Edge::Raised }, &t);
            let ink = if close && resp.hovered() { t.on_close_hover } else { t.fg };
            icons::paint(ui, b.translate(if down { vec2(size::HAIRLINE, size::HAIRLINE) } else { Vec2::ZERO }), icon_name, icon::SM, ink);
        } else {
            ui.painter().rect_filled(r, 0.0, bg);
            icons::paint(ui, r, icon_name, icon::SM, if bar_fg == t.fg { fg } else { bar_fg });
        }
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, label));
        if resp.on_hover_text(label).clicked() {
            ui.ctx().send_viewport_cmd(command);
        }
    }
    x
}

/// The frameless window's resize zones: a thin band along each edge, diagonal near the corners.
/// A press there hands the resize to the OS ([`ViewportCommand::BeginResize`]), which also snaps.
/// Drawn last and kept on top, so the panes and dialogs under the edges never take those presses.
/// None while maximized or full screen.
pub fn resize_edges(ctx: &egui::Context) {
    let (maximized, fullscreen) = ctx.input(|i| (i.viewport().maximized.unwrap_or(false), i.viewport().fullscreen.unwrap_or(false)));
    if maximized || fullscreen {
        return;
    }
    let full = ctx.content_rect();
    let e = size::WINDOW_RESIZE_EDGE;
    let strips = [
        ("n", Rect::from_min_max(full.min, pos2(full.max.x, full.min.y + e))),
        ("s", Rect::from_min_max(pos2(full.min.x, full.max.y - e), full.max)),
        ("w", Rect::from_min_max(pos2(full.min.x, full.min.y + e), pos2(full.min.x + e, full.max.y - e))),
        ("e", Rect::from_min_max(pos2(full.max.x - e, full.min.y + e), pos2(full.max.x, full.max.y - e))),
    ];
    for (name, strip) in strips {
        let area =
            egui::Area::new(Id::new(("resize-edge", name))).order(Order::Foreground).fixed_pos(strip.min).movable(false).constrain(false).show(
                ctx,
                |ui| {
                    let (r, resp) = ui.allocate_exact_size(strip.size(), Sense::click_and_drag());
                    let Some(p) = resp.hover_pos() else { return };
                    let dir = resize_direction(full, r, p);
                    resp.on_hover_cursor(resize_cursor(dir));
                    if ui.input(|i| i.pointer.primary_pressed()) {
                        ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(dir));
                    }
                },
            );
        ctx.move_to_top(area.response.layer_id);
    }
}

/// Which way a press at `p` in an edge band resizes: the band's own edge, or a corner's diagonal
/// within [`size::WINDOW_RESIZE_CORNER`] of one.
fn resize_direction(full: Rect, band: Rect, p: egui::Pos2) -> ResizeDirection {
    let c = size::WINDOW_RESIZE_CORNER;
    let north = p.y < full.min.y + c;
    let south = p.y > full.max.y - c;
    let west = p.x < full.min.x + c;
    let east = p.x > full.max.x - c;
    let top = band.min.y <= full.min.y;
    let bottom = band.max.y >= full.max.y;
    let left = !top && !bottom && band.min.x <= full.min.x;
    match () {
        () if north && west => ResizeDirection::NorthWest,
        () if north && east => ResizeDirection::NorthEast,
        () if south && west => ResizeDirection::SouthWest,
        () if south && east => ResizeDirection::SouthEast,
        () if top => ResizeDirection::North,
        () if bottom => ResizeDirection::South,
        () if left => ResizeDirection::West,
        () => ResizeDirection::East,
    }
}

fn resize_cursor(dir: ResizeDirection) -> CursorIcon {
    match dir {
        ResizeDirection::North | ResizeDirection::South => CursorIcon::ResizeVertical,
        ResizeDirection::East | ResizeDirection::West => CursorIcon::ResizeHorizontal,
        ResizeDirection::NorthWest | ResizeDirection::SouthEast => CursorIcon::ResizeNwSe,
        ResizeDirection::NorthEast | ResizeDirection::SouthWest => CursorIcon::ResizeNeSw,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edges_resize_their_own_way_and_corners_diagonally() {
        let full = Rect::from_min_max(pos2(0.0, 0.0), pos2(800.0, 600.0));
        let e = size::WINDOW_RESIZE_EDGE;
        let top = Rect::from_min_max(full.min, pos2(800.0, e));
        let bottom = Rect::from_min_max(pos2(0.0, 600.0 - e), full.max);
        let left = Rect::from_min_max(pos2(0.0, e), pos2(e, 600.0 - e));
        let right = Rect::from_min_max(pos2(800.0 - e, e), pos2(800.0, 600.0 - e));
        assert_eq!(resize_direction(full, top, pos2(400.0, 1.0)), ResizeDirection::North);
        assert_eq!(resize_direction(full, top, pos2(2.0, 1.0)), ResizeDirection::NorthWest);
        assert_eq!(resize_direction(full, top, pos2(798.0, 1.0)), ResizeDirection::NorthEast);
        assert_eq!(resize_direction(full, bottom, pos2(400.0, 599.0)), ResizeDirection::South);
        assert_eq!(resize_direction(full, bottom, pos2(798.0, 599.0)), ResizeDirection::SouthEast);
        assert_eq!(resize_direction(full, left, pos2(1.0, 300.0)), ResizeDirection::West);
        assert_eq!(resize_direction(full, left, pos2(1.0, 595.0)), ResizeDirection::SouthWest, "the corner reaches up the side");
        assert_eq!(resize_direction(full, right, pos2(799.0, 300.0)), ResizeDirection::East);
        assert_eq!(resize_direction(full, right, pos2(799.0, 8.0)), ResizeDirection::NorthEast);
    }
}
