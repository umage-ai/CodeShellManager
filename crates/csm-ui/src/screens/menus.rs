//! Popup menus: the pane's ⋯ menu, the sidebar row's context menu, the group menu, the grid
//! presets popover, the run list and the quick-open's group picker. Every item is an
//! [`Action`]; anything that changes state is a tool call.

use egui::{Align2, CornerRadius, Id, Order, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp, Menu};
use crate::icons;
use crate::model::{GridPreset, GroupMode, LayoutMode, Scope, Status, UiModel};
use crate::theme::{self, Tokens, alpha, icon, radius, size, space, text};
use crate::widgets;

/// One row of a menu.
#[derive(Debug, Clone)]
pub enum Entry {
    Head(String),
    Sep,
    Item { icon: &'static str, label: String, hint: Option<String>, danger: bool, action: Action },
    Sub { icon: &'static str, label: String, items: Vec<Entry> },
}

fn item(icon: &'static str, label: impl Into<String>, action: Action) -> Entry {
    Entry::Item { icon, label: label.into(), hint: None, danger: false, action }
}

fn hinted(icon: &'static str, label: impl Into<String>, hint: &str, action: Action) -> Entry {
    Entry::Item { icon, label: label.into(), hint: Some(hint.into()), danger: false, action }
}

fn tool(id: &'static str, params: serde_json::Value) -> Action {
    Action::Tool(id, params)
}

fn open_with(session: &str) -> Entry {
    let e = |label: &str, hint: &str| hinted("braces", label, hint, tool("open_with", json!({"session_id": session, "entry": label})));
    Entry::Sub {
        icon: "braces",
        label: "Open with".into(),
        items: vec![
            e("VS Code", "default · Code.exe"),
            e("Visual Studio 2026", "devenv.exe"),
            e("Rider", "rider64.exe"),
            Entry::Sep,
            item("pencil", "Edit the list…", Action::Toast("The Open with list is edited in Settings › Open with.".into())),
        ],
    }
}

fn demo_status(session: &str) -> Entry {
    Entry::Sub {
        icon: "sliders",
        label: "Demo: set status".into(),
        items: Status::ALL
            .iter()
            .map(|s| item("blank", s.label(), tool("demo_set_status", json!({"session_id": session, "status": s.id()}))))
            .collect(),
    }
}

pub fn pane_menu(model: &UiModel, id: &str) -> Vec<Entry> {
    let Some(s) = model.session(id) else { return vec![] };
    let sid = json!({"session_id": id});
    let scope = model.pane_scope().key();
    let insp = |tab: &str| tool("inspector_show", json!({"tab": tab}));
    let mut v = vec![
        Entry::Head("Panels".into()),
        hinted("note", "Notes", "Ctrl Shift N", insp("notes")),
        hinted("play", "Runs", "F5 runs default", insp("runs")),
        item("branch", "Git", insp("git")),
        Entry::Sep,
        item("folder", "Open folder", tool("open_folder", sid.clone())),
        open_with(id),
        hinted("terminal", "Open a shell here", "pwsh", tool("session_new", json!({"folder": s.folder, "command": "pwsh", "group": s.group}))),
        item("copy", "Copy path", Action::CopyText(s.folder.clone())),
        Entry::Sep,
        Entry::Head("Session".into()),
        item("pencil", "Edit session…", Action::Toast("Edit opens the New session panel, filled in (next round). The tool is session_edit.".into())),
        hinted("restart", "Restart", "Ctrl Shift R", tool("session_restart", sid.clone())),
        hinted("moon", "Sleep", "keeps it in the sidebar", tool("session_sleep", sid.clone())),
        demo_status(id),
        Entry::Sep,
        item("float", "Float in this window", tool("pane_float", json!({"session_id": id, "group": scope}))),
        item("tear", "Move to a new window", tool("pane_tear_off", json!({"session_id": id, "group": scope}))),
        Entry::Sep,
    ];
    v.push(Entry::Item { icon: "x", label: "Close".into(), hint: Some("Ctrl W".into()), danger: true, action: tool("session_close", json!({"session_id": id, "force": false})) });
    v
}

pub fn row_menu(model: &UiModel, id: &str) -> Vec<Entry> {
    let Some(s) = model.session(id) else { return vec![] };
    let sid = json!({"session_id": id});
    let scope = model.pane_scope();
    let pinned = model.layout(&scope).pinned.iter().any(|p| p == id);
    let mut groups: Vec<Entry> = model
        .groups
        .iter()
        .map(|g| {
            let here = s.group.as_deref() == Some(g.id.as_str());
            item(if here { "check" } else { "blank" }, g.name.clone(), tool("session_move_to_group", json!({"session_id": id, "group_id": g.id})))
        })
        .collect();
    groups.push(Entry::Sep);
    groups.push(item("plus", "New group…", Action::NewGroupPrompt));
    let mut v = vec![
        Entry::Head(s.name.clone()),
        item("focus", if s.status.is_live() { "Focus" } else { "Wake and focus" }, tool("session_focus", sid.clone())),
        item(
            "pin",
            if pinned { "Unpin from Side by side" } else { "Pin to Side by side" },
            tool(if pinned { "layout_unpin" } else { "layout_pin" }, json!({"session_id": id, "group": scope.key()})),
        ),
        hinted("pencil", "Rename", "F2", Action::StartRename(id.to_string())),
        Entry::Sub { icon: "layers", label: "Move to group".into(), items: groups },
        open_with(id),
        Entry::Sep,
    ];
    if s.is_claude() && s.status.is_live() {
        v.push(hinted("eraser", "Clear status", "stuck after a crash?", tool("session_status_clear", sid.clone())));
    }
    v.push(demo_status(id));
    v.push(Entry::Sep);
    v.push(item("restart", "Restart", tool("session_restart", sid.clone())));
    if s.status.is_live() {
        v.push(item("moon", "Sleep", tool("session_sleep", sid.clone())));
    } else {
        v.push(item("power", "Wake", tool("session_wake", sid.clone())));
    }
    v.push(Entry::Item { icon: "x", label: "Close".into(), hint: None, danger: true, action: tool("session_close", json!({"session_id": id, "force": false})) });
    v
}

pub fn group_menu(model: &UiModel, gid: &str) -> Vec<Entry> {
    let Some(g) = model.group(gid) else { return vec![] };
    let scope = Scope::Group(gid.to_string());
    let lay = model.layout(&scope);
    let live = model.scope_sessions(&scope).iter().filter(|s| s.status.is_live()).count();
    let gp = json!({"group_id": gid});
    let mut v = vec![
        Entry::Head(g.name.clone()),
        item("plus", format!("New session in {}", g.name), Action::OpenQuickOpen { group: Some(gid.to_string()) }),
        hinted("pencil", "Rename group", "F2", Action::StartGroupRename(gid.to_string())),
        Entry::Sub {
            icon: lay.mode.icon(),
            label: "Layout".into(),
            items: LayoutMode::ALL
                .iter()
                .map(|m| {
                    let hint = (lay.mode == *m).then(|| "current".to_string());
                    Entry::Item { icon: m.icon(), label: m.label().into(), hint, danger: false, action: tool("layout_set", json!({"group": gid, "mode": m.id()})) }
                })
                .collect(),
        },
        if g.torn_off {
            item("dock", "Dock back into the main window", tool("group_dock", gp.clone()))
        } else {
            item("tear", "Move to its own window", tool("group_tear_off", gp.clone()))
        },
    ];
    if model.group_mode == GroupMode::Headers {
        v.push(item("chevR", "Collapse or expand", Action::ToggleCollapse(gid.to_string())));
    }
    v.extend([
        Entry::Sep,
        item("restart", format!("Restart all ({live})…"), tool("group_restart_all", json!({"group_id": gid, "force": false}))),
        item("moon", "Sleep all", tool("group_sleep_all", gp.clone())),
        item("trash", "Delete group…", tool("group_delete", gp.clone())),
        Entry::Item { icon: "x", label: "Close all…".into(), hint: None, danger: true, action: tool("group_close_all", json!({"group_id": gid, "force": false})) },
    ]);
    v
}

fn runs_menu(model: &UiModel, id: &str) -> Vec<Entry> {
    let Some(s) = model.session(id) else { return vec![] };
    let mut v = vec![Entry::Head("Run commands".into())];
    for r in &s.runs {
        let hint = if r.is_default { format!("F5 · {}", r.detail) } else { r.detail.clone() };
        v.push(Entry::Item { icon: "play", label: r.label.clone(), hint: Some(hint), danger: false, action: tool("run_start", json!({"session_id": id, "run_id": r.id})) });
    }
    v.push(Entry::Sep);
    v.push(item("pencil", "Edit commands…", tool("inspector_show", json!({"tab": "runs"}))));
    v
}

/// Draws `entries` in an area at `at`; returns the area's rect.
#[allow(clippy::too_many_arguments)]
fn menu_area(ctx: &egui::Context, app: &mut CsmApp, id: Id, at: Pos2, entries: &[Entry], depth: usize, right: bool, actions: &mut Vec<Action>) -> Vec<Rect> {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut rects = Vec::new();
    let mut sub: Option<(Pos2, Vec<Entry>, String)> = None;
    // Keep the menu on screen: flip up/left when it would overflow.
    let est_h: f32 = entries
        .iter()
        .map(|e| match e {
            Entry::Head(_) => size::CONTROL_SM,
            Entry::Sep => space::SM + size::HAIRLINE,
            _ => size::MENU_ROW,
        })
        .sum::<f32>()
        + space::SM;
    let mut pos = at;
    if pos.y + est_h > screen.max.y - space::SM {
        pos.y = (screen.max.y - space::SM - est_h).max(screen.min.y + space::SM);
    }
    if !right && pos.x + size::MENU_W + space::LG > screen.max.x {
        pos.x = (screen.max.x - size::MENU_W - space::LG).max(screen.min.x);
    }
    let pivot = if right { Align2::RIGHT_TOP } else { Align2::LEFT_TOP };
    let resp = egui::Area::new(id).order(Order::Foreground).pivot(pivot).fixed_pos(pos).show(ctx, |ui| {
        widgets::pop_frame(&t).show(ui, |ui| {
            ui.set_min_width(size::MENU_W - space::SM);
            ui.spacing_mut().item_spacing.y = 0.0;
            for e in entries {
                match e {
                    Entry::Head(h) => widgets::menu_head(ui, h),
                    Entry::Sep => widgets::separator(ui),
                    Entry::Item { icon, label, hint, danger, action } => {
                        let r = widgets::menu_item(ui, icon, label, hint.as_deref(), *danger);
                        if r.hovered() && depth == 0 {
                            app.ui.submenu = None;
                        }
                        if r.clicked() {
                            actions.push(action.clone());
                            actions.push(Action::CloseMenu);
                        }
                    }
                    Entry::Sub { icon, label, items } => {
                        let open = app.ui.submenu.as_deref() == Some(label.as_str());
                        let r = widgets::menu_item(ui, icon, label, None, false);
                        if open {
                            ui.painter().rect_filled(r.rect, CornerRadius::same(radius::SM + 1), Tokens::tint(t.row_sel, alpha::ASLEEP));
                        }
                        icons::paint(ui, Rect::from_min_size(pos2(r.rect.max.x - space::ROW_X - icon::MD, r.rect.center().y - icon::MD / 2.0), Vec2::splat(icon::MD)), "chevR", icon::MD, t.muted);
                        if r.hovered() || r.clicked() {
                            app.ui.submenu = Some(label.clone());
                        }
                        if open || r.hovered() {
                            sub = Some((pos2(r.rect.max.x + space::XS + space::XXS, r.rect.min.y - space::XS - size::HAIRLINE), items.clone(), label.clone()));
                        }
                    }
                }
            }
        });
    });
    rects.push(resp.response.rect);
    if let Some((p, items, label)) = sub {
        let mut p = p;
        // A submenu that would leave the screen opens to the left of its parent instead.
        let flip = p.x + size::MENU_W + space::XXL * 2.0 > screen.max.x;
        if flip {
            p.x = resp.response.rect.min.x - space::XS;
        }
        rects.extend(menu_area(ctx, app, id.with(("sub", label)), p, &items, depth + 1, flip, actions));
    }
    rects
}

pub fn show(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(menu) = app.ui.menu.clone() else { return };
    let model = app.model.clone();
    let rects = match &menu {
        Menu::Pane { session, at } => {
            let e = pane_menu(&model, session);
            let at = pos2(at.x - size::MENU_W, at.y);
            menu_area(ctx, app, Id::new("menu-pane"), at, &e, 0, false, actions)
        }
        Menu::Row { session, at } => menu_area(ctx, app, Id::new("menu-row"), *at + vec2(space::XS + space::XXS, -space::XS), &row_menu(&model, session), 0, false, actions),
        Menu::Group { group, at } => menu_area(ctx, app, Id::new("menu-group"), *at, &group_menu(&model, group), 0, false, actions),
        Menu::Runs { session, at } => menu_area(ctx, app, Id::new("menu-runs"), pos2(at.x - size::MENU_W, at.y), &runs_menu(&model, session), 0, false, actions),
        Menu::Grid { at } => vec![grid_popover(ctx, app, *at, actions)],
        Menu::QuickGroup { at } => {
            let mut e = vec![Entry::Head("Group".into())];
            for g in &model.groups {
                e.push(item(if app.ui.quick.group.as_deref() == Some(g.id.as_str()) { "check" } else { "blank" }, g.name.clone(), Action::Toast(format!("quick-group:{}", g.id))));
            }
            e.push(item(if app.ui.quick.group.is_none() { "check" } else { "blank" }, "Ungrouped", Action::Toast("quick-group:".into())));
            menu_area(ctx, app, Id::new("menu-qgroup"), *at, &e, 0, false, actions)
        }
    };
    // The quick-open's group picker is local state, not a tool: intercept its rows.
    actions.retain(|a| {
        if let Action::Toast(t) = a
            && let Some(g) = t.strip_prefix("quick-group:")
        {
            app.ui.quick.group = (!g.is_empty()).then(|| g.to_string());
            return false;
        }
        true
    });
    // A click anywhere else closes it.
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rects.iter().any(|r| r.contains(p))));
    if clicked_outside {
        actions.push(Action::CloseMenu);
    }
}

fn grid_popover(ctx: &egui::Context, app: &mut CsmApp, at: Pos2, actions: &mut Vec<Action>) -> Rect {
    let t = Tokens::get(ctx);
    let scope = app.model.pane_scope();
    let lay = app.model.layout(&scope);
    let resp = egui::Area::new(Id::new("grid-pop")).order(Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same(space::SM as i8)).show(ui, |ui| {
            ui.label(egui::RichText::new("Grid presets").font(theme::regular(text::CAPTION)).color(t.muted));
            ui.add_space(space::XS);
            let mut tiles: Vec<Option<GridPreset>> = GridPreset::PRESETS.iter().copied().map(Some).collect();
            tiles.push(None);
            for chunk in tiles.chunks(3) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::XS;
                    for tile in chunk {
                        let (rect, r) = ui.allocate_exact_size(vec2(size::GRID_TILE_W, size::GRID_GLYPH[1] + space::XL + space::SM), Sense::click());
                        let on = lay.mode == LayoutMode::Grid && tile.is_some_and(|g| g == lay.grid);
                        if r.hovered() {
                            ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), t.row_hover);
                        }
                        if on {
                            ui.painter().rect_stroke(rect, CornerRadius::same(radius::MD), Stroke::new(size::HAIRLINE, t.blue), StrokeKind::Inside);
                        }
                        let glyph = Rect::from_min_size(pos2(rect.center().x - size::GRID_GLYPH[0] / 2.0, rect.min.y + space::SM), vec2(size::GRID_GLYPH[0], size::GRID_GLYPH[1]));
                        let label = match tile {
                            Some(g) => {
                                let cells = crate::screens::panes::grid_cells(glyph, usize::from(g.cols), usize::from(g.rows), space::XXS);
                                let fill = if on { Tokens::tint(t.blue, alpha::ASLEEP) } else { t.surface1 };
                                for c in cells {
                                    ui.painter().rect_filled(c, CornerRadius::same(radius::SM / 2), fill);
                                }
                                g.label()
                            }
                            None => {
                                ui.painter().extend(egui::Shape::dashed_line(&[glyph.left_top(), glyph.right_top(), glyph.right_bottom(), glyph.left_bottom(), glyph.left_top()], Stroke::new(size::HAIRLINE, t.surface2), space::XS, space::XS - size::HAIRLINE));
                                icons::paint(ui, glyph, "plus", icon::MD, t.muted);
                                "Custom…".into()
                            }
                        };
                        ui.painter().text(pos2(rect.center().x, glyph.max.y + space::SM + space::XS), Align2::CENTER_CENTER, &label, theme::regular(text::CAPTION), if on { t.fg } else { t.fg2 });
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Grid {label}")));
                        if r.clicked() {
                            match tile {
                                Some(g) => {
                                    actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": "grid", "grid": {"cols": g.cols, "rows": g.rows}})));
                                    actions.push(Action::CloseMenu);
                                }
                                None => app.ui.submenu = Some("custom-grid".into()),
                            }
                        }
                    }
                });
            }
            if app.ui.submenu.as_deref() == Some("custom-grid") {
                ui.add_space(space::XS);
                let key = Id::new("custom-grid-size");
                let (mut c, mut r): (u8, u8) = ui.data(|d| d.get_temp(key)).unwrap_or((lay.grid.cols, lay.grid.rows));
                ui.horizontal(|ui| {
                    ui.label("Columns");
                    ui.add(egui::DragValue::new(&mut c).range(1..=6));
                    ui.label("Rows");
                    ui.add(egui::DragValue::new(&mut r).range(1..=6));
                    if widgets::button(ui, widgets::ButtonKind::Primary, None, "Apply", None).clicked() {
                        actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": "grid", "grid": {"cols": c, "rows": r}})));
                        actions.push(Action::CloseMenu);
                    }
                });
                ui.data_mut(|d| d.insert_temp(key, (c, r)));
            }
        });
    });
    resp.response.rect
}

