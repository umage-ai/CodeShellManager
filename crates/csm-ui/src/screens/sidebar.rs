//! The group rail (`.grail`) and the sidebar (`.sb`): the "Needs you" inbox with its drag
//! divider, the section header with the group-view switch, and the session rows in the rail,
//! headers or flat view.
//!
//! Drag and drop: a live row drags onto a rail tab or a group header (`session_move_to_group`
//! into that group), or between rows, where an insertion line shows the slot. The slot is an
//! anchor ("before X" / "after Y", `csm_core::sidebar_drop`), never an index, so it means the
//! same place in the sidebar, the panes and `state.json`. Renaming (F2), renaming a group and
//! naming a new one happen in place.

use csm_core::sidebar_drop::{self, SidebarRow, SidebarRowKind};
use egui::{Align2, CornerRadius, CursorIcon, Id, Order, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp, InlineTarget, Menu};
use crate::icons;
use crate::model::{GroupMode, Kind, Scope, SessionView, Status, UiModel, initials};
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

fn named(resp: &egui::Response, ty: WidgetType, label: impl Into<String>) {
    let l = label.into();
    resp.widget_info(move || WidgetInfo::labeled(ty, true, l.clone()));
}

/// Needs-you count and the done/error dot for a scope.
fn badge_for(model: &UiModel, scope: &Scope) -> (usize, Option<Status>) {
    let ss = model.scope_sessions(scope);
    let n = ss.iter().filter(|s| s.status.needs_you()).count();
    let dot = if ss.iter().any(|s| s.status == Status::Error) {
        Some(Status::Error)
    } else if ss.iter().any(|s| s.status == Status::Done) {
        Some(Status::Done)
    } else {
        None
    };
    (n, dot)
}

/// The group rail: All, one tab per group (initials), new group.
pub fn rail(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, t.crust);
    ui.painter().vline(rect.max.x - size::HAIRLINE / 2.0, rect.y_range(), Stroke::new(size::HAIRLINE, t.surface0));
    let model = app.model.clone();
    let mut y = rect.min.y + space::SM;
    let cx = rect.center().x;
    let dragging = app.ui.drags.row.clone();
    let released = ui.input(|i| i.pointer.any_released());
    let pointer = ui.ctx().pointer_latest_pos();
    let mut tab = |ui: &mut Ui, scope: Scope, label: String, y: &mut f32| {
        let r = Rect::from_center_size(pos2(cx, *y + size::RAIL_TAB / 2.0), Vec2::splat(size::RAIL_TAB));
        *y += size::RAIL_TAB + space::XS;
        let resp = ui.interact(r, Id::new(("rail-tab", scope.key())), Sense::click());
        let on = model.rail == scope;
        let p = ui.painter();
        // A dragged session over a group's tab: it moves there on release.
        let drop_here = matches!(&scope, Scope::Group(_)) && dragging.is_some() && pointer.is_some_and(|p| r.contains(p));
        if drop_here {
            p.rect_filled(r, CornerRadius::same(radius::MD), Tokens::tint(t.blue, alpha::DROP * 2.0));
            p.rect_stroke(r, CornerRadius::same(radius::MD), Stroke::new(size::RING, t.blue), StrokeKind::Inside);
            if released
                && let (Some(sid), Scope::Group(g)) = (&dragging, &scope)
                && model.session(sid).is_some_and(|s| s.group.as_deref() != Some(g.as_str()))
            {
                actions.push(Action::Tool("session_move_to_group", json!({"session_id": sid, "group_id": g})));
            }
        } else if on || resp.hovered() {
            p.rect_filled(r, CornerRadius::same(radius::MD), if on { t.row_sel } else { t.row_hover });
        }
        if on {
            let bar = Rect::from_min_max(
                pos2(r.min.x - space::XS - space::XXS, r.min.y + space::SM),
                pos2(r.min.x - space::XS - space::XXS + size::STRIPE, r.max.y - space::SM),
            );
            p.rect_filled(bar, CornerRadius::same(radius::SM / 2), t.blue);
        }
        let fg = if on || resp.hovered() { t.fg } else { t.muted };
        match &scope {
            Scope::All => icons::paint(ui, r, "overview", icon::LG, fg),
            Scope::Group(_) => {
                p.text(r.center(), Align2::CENTER_CENTER, &label, semibold(text::SMALL), fg);
            }
        }
        let (n, dot) = badge_for(&model, &scope);
        if n > 0 {
            let c = pos2(r.max.x + space::XXS, r.min.y + space::XS + space::XXS / 2.0);
            let b = widgets::paint_count(ui, c, n, false, size::BADGE);
            ui.painter().rect_stroke(
                b.expand(size::RING / 2.0),
                CornerRadius::same(radius::PILL),
                Stroke::new(size::RING, t.crust),
                StrokeKind::Middle,
            );
        } else if let Some(d) = dot {
            let c = pos2(r.max.x - space::XS - size::HAIRLINE, r.min.y + space::XS + size::HAIRLINE);
            ui.painter().circle_filled(c, space::XS + size::RING / 2.0, t.crust);
            ui.painter().circle_filled(c, space::XS, icons::status_color(&t, d));
        }
        let torn = match &scope {
            Scope::Group(g) => model.group(g).is_some_and(|g| g.torn_off),
            Scope::All => false,
        };
        if torn {
            let ir = Rect::from_min_size(r.max - Vec2::splat(icon::XS), Vec2::splat(icon::XS));
            icons::paint(ui, ir, "monitor", icon::XS - size::RING, t.blue);
        }
        let name = model.scope_name(&scope);
        let tip = match (&scope, n) {
            (Scope::All, _) => name.clone(),
            (_, 0) => format!("{name}. Right-click for group actions."),
            (_, n) => format!("{name} · {n} need you. Right-click for group actions."),
        };
        named(&resp, WidgetType::Button, format!("Group tab: {name}"));
        let resp = resp.on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand);
        if resp.clicked() {
            actions.push(Action::Tool("view_set", json!({"rail_tab": scope.key()})));
        }
        if let Scope::Group(g) = &scope
            && resp.secondary_clicked()
        {
            actions.push(Action::OpenMenu(Menu::Group { group: g.clone(), at: pos2(r.max.x + space::SM, r.min.y) }));
        }
    };
    tab(ui, Scope::All, String::new(), &mut y);
    ui.painter().hline((cx - space::MD)..=(cx + space::MD), y, Stroke::new(size::HAIRLINE, t.surface0));
    y += space::XS + space::XXS;
    for g in &model.groups {
        tab(ui, Scope::Group(g.id.clone()), initials(&g.name), &mut y);
    }
    let add = Rect::from_center_size(pos2(cx, y + size::CONTROL / 2.0), vec2(size::RAIL_TAB, size::CONTROL));
    if widgets::icon_button_at(ui, add, Id::new("rail-add"), "plus", false, "New group").clicked() {
        actions.push(Action::NewGroupPrompt);
    }
    if app.ui.compact {
        // A narrow window: the sidebar opens over the panes from here.
        let b = Rect::from_center_size(pos2(cx, rect.max.y - space::SM - size::CONTROL / 2.0), vec2(size::RAIL_TAB, size::CONTROL));
        let peek = app.ui.peek_sidebar;
        if widgets::icon_button_at(
            ui,
            b,
            Id::new("rail-peek"),
            "panelL",
            peek,
            if peek { "Hide the sessions list" } else { "Show the sessions list" },
        )
        .clicked()
        {
            app.ui.peek_sidebar = !peek;
        }
        let nb = Rect::from_center_size(pos2(cx, b.min.y - space::XS - size::CONTROL / 2.0), vec2(size::RAIL_TAB, size::CONTROL));
        if widgets::icon_button_at(ui, nb, Id::new("rail-new"), "terminal", false, "New session  Ctrl+T").clicked() {
            actions.push(Action::OpenQuickOpen { group: None });
        }
    }
}

/// Compact window: the sidebar over the panes, closed by a click outside it or by picking a
/// session.
pub fn peek(ctx: &egui::Context, app: &mut CsmApp, rect: Rect, actions: &mut Vec<Action>) {
    let t = Tokens::get(ctx);
    let before = actions.len();
    let resp = egui::Area::new(Id::new("sidebar-peek")).order(Order::Foreground).fixed_pos(rect.min).show(ctx, |ui| {
        ui.set_min_size(rect.size());
        let shadow = egui::Shadow { offset: [8, 0], blur: 28, spread: 0, color: t.shadow };
        ui.painter().add(shadow.as_shape(rect, CornerRadius::ZERO));
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
        show(&mut child, app, actions);
    });
    let picked = actions[before..].iter().any(|a| matches!(a, Action::Tool("session_focus", _)));
    let outside =
        ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !resp.response.rect.contains(p) && p.x > rect.max.x));
    if picked || outside {
        app.ui.peek_sidebar = false;
    }
}

/// The in-place text field (F2 rename, group rename, new group). Enter or a click elsewhere
/// saves; Esc cancels.
fn inline_field(ui: &mut Ui, app: &mut CsmApp, rect: Rect, label: &str, hint: &str, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let Some(e) = app.ui.inline.as_mut() else { return };
    e.unseen_frames = 0;
    ui.painter().rect(rect, CornerRadius::same(radius::SM), t.field, Stroke::new(size::HAIRLINE, t.blue), StrokeKind::Inside);
    let edit = egui::TextEdit::singleline(&mut e.text)
        .id(Id::new("inline-edit"))
        .frame(egui::Frame::NONE)
        .font(theme::regular(text::BODY))
        .hint_text(egui::RichText::new(hint).color(t.faint))
        .vertical_align(egui::Align::Center)
        .desired_width(rect.width() - space::SM * 2.0);
    // A child, not `put`: the field must not move the rows below it.
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect.shrink2(vec2(space::SM, size::HAIRLINE))));
    let r = child.add(edit);
    let label = label.to_string();
    r.widget_info(move || WidgetInfo::labeled(WidgetType::TextEdit, true, label.clone()));
    if e.focus {
        r.request_focus();
        e.focus = false;
    } else if r.lost_focus() {
        let esc = ui.input(|i| i.key_pressed(egui::Key::Escape));
        actions.push(if esc { Action::CancelInline } else { Action::CommitInline });
    }
}

fn inline_is(app: &CsmApp, target: &InlineTarget) -> bool {
    app.ui.inline.as_ref().is_some_and(|e| &e.target == target)
}

pub fn show(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, t.mantle);
    ui.painter().vline(rect.max.x - size::HAIRLINE / 2.0, rect.y_range(), Stroke::new(size::HAIRLINE, t.surface0));
    let model = app.model.clone();
    let mut top = rect.min.y;

    if model.inbox_panel.open {
        let h = app
            .ui
            .drags
            .inbox_h
            .unwrap_or(model.inbox_panel.height)
            .clamp(size::INBOX_MIN_H, (rect.height() - size::INBOX_MIN_H * 2.0).max(size::INBOX_MIN_H));
        let pane = Rect::from_min_max(pos2(rect.min.x + space::SM, top + space::SM), pos2(rect.max.x - space::SM, top + h));
        inbox(ui, app, pane, actions);
        top += h;
        // The divider between the inbox and the session list.
        let grip = Rect::from_min_max(pos2(rect.min.x, top), pos2(rect.max.x, top + size::SPLIT_GRIP));
        let resp = ui.interact(grip, Id::new("inbox-split"), Sense::click_and_drag());
        named(&resp, WidgetType::Other, "Inbox divider");
        let dragging = resp.dragged();
        let lit = dragging || resp.hovered();
        let line = Rect::from_center_size(grip.center(), vec2(grip.width() * 0.3, space::XS - size::HAIRLINE));
        ui.painter().rect_filled(line, CornerRadius::same(radius::SM / 2), if lit { t.blue } else { t.surface0 });
        let resp = resp.on_hover_cursor(CursorIcon::ResizeRow).on_hover_text("Drag to resize the inbox. Double-click resets.");
        // A triple click (a double click right after another click) resets it too.
        if resp.double_clicked() || resp.triple_clicked() {
            app.ui.drags.inbox_h = None;
            actions.push(Action::Tool("inbox_panel_set", json!({"height": size::INBOX_DEFAULT_H})));
        } else if dragging {
            let base = app.ui.drags.inbox_h.unwrap_or(model.inbox_panel.height);
            app.ui.drags.inbox_h = Some((base + resp.drag_delta().y).clamp(size::INBOX_MIN_H, size::INBOX_MAX_H));
        } else if resp.drag_stopped()
            && let Some(h) = app.ui.drags.inbox_h.take()
        {
            actions.push(Action::Tool("inbox_panel_set", json!({"height": h})));
        }
        top = grip.max.y;
    } else {
        top += space::SM;
    }

    // Footer.
    let foot_h = size::CONTROL + space::LG;
    let foot = Rect::from_min_max(pos2(rect.min.x, rect.max.y - foot_h), rect.max);
    ui.painter().hline(foot.x_range(), foot.min.y, Stroke::new(size::HAIRLINE, t.surface0));
    ui.scope_builder(UiBuilder::new().max_rect(foot.shrink(space::SM)), |ui| {
        ui.horizontal_centered(|ui| {
            if widgets::button(ui, ButtonKind::Ghost, Some("plus"), "New session", Some("Ctrl T")).clicked() {
                actions.push(Action::OpenQuickOpen { group: None });
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if widgets::icon_button(ui, "layers", size::CONTROL, false, "New group").clicked() {
                    actions.push(Action::NewGroupPrompt);
                }
            });
        });
    });

    let list = Rect::from_min_max(pos2(rect.min.x + space::SM, top), pos2(rect.max.x - space::SM, foot.min.y - space::XS));
    let mut rows: Vec<SidebarRow> = Vec::new();
    ui.scope_builder(UiBuilder::new().max_rect(list), |ui| {
        section_header(ui, app, actions);
        if inline_is(app, &InlineTarget::NewGroup) {
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL), Sense::hover());
            inline_field(ui, app, r.shrink2(vec2(space::XXS, space::XXS)), "New group name", "New group, e.g. Client X", actions);
            ui.add_space(space::XS);
        }
        let list_top = ui.cursor().min.y;
        egui::ScrollArea::vertical().id_salt("session-list").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            match model.group_mode {
                GroupMode::Headers => headers_view(ui, app, &mut rows, actions),
                GroupMode::Rail | GroupMode::Flat => {
                    let scope = if model.group_mode == GroupMode::Rail { model.rail.clone() } else { Scope::All };
                    if model.group_mode == GroupMode::Rail {
                        scope_header(ui, app, &scope, actions);
                    }
                    let ss = model.scope_sessions(&scope);
                    section_rows(ui, app, &ss, &mut rows, actions);
                    if ss.is_empty() {
                        empty_scope(ui, &model, &scope);
                    }
                }
            }
        });
        // A dragged row: the insertion line (or the group header it lands under) and the drop.
        let list_rect = Rect::from_min_max(pos2(list.min.x, list_top), list.max);
        drop_between_rows(ui, app, &rows, list_rect, actions);
    });
    drag_ghost(ui.ctx(), app);
}

/// The row kind `sidebar_drop` resolves against: sleeping rows are never anchors.
fn row_kind(s: &SessionView) -> SidebarRowKind {
    if s.status.is_live() { SidebarRowKind::Session } else { SidebarRowKind::Dormant }
}

/// An empty group (or no sessions at all): say so, and how to fill it.
fn empty_scope(ui: &mut Ui, model: &UiModel, scope: &Scope) {
    let t = Tokens::get(ui.ctx());
    ui.add_space(space::SM);
    let msg = match scope {
        Scope::Group(_) => "No sessions in this group yet. Drag one onto its tab, or press Ctrl+T.",
        Scope::All if model.sessions.is_empty() => "No sessions yet. Ctrl+T opens a folder.",
        Scope::All => "No sessions here yet.",
    };
    ui.horizontal(|ui| {
        ui.add_space(space::XS + space::XXS);
        ui.add(egui::Label::new(egui::RichText::new(msg).color(t.muted).size(text::CAPTION)).wrap());
    });
}

/// While a session row is dragged over the list: where it would land, and the move on release.
fn drop_between_rows(ui: &mut Ui, app: &mut CsmApp, rows: &[SidebarRow], list: Rect, actions: &mut Vec<Action>) {
    let Some(dragged) = app.ui.drags.row.clone() else { return };
    let Some(p) = ui.ctx().pointer_latest_pos().filter(|p| list.contains(*p)) else { return };
    let Some(target) = sidebar_drop::resolve(rows, f64::from(p.y)) else { return };
    let t = Tokens::get(ui.ctx());
    let model = app.model.clone();
    // Over a group header: the header lights up (the session lands at the top of that group).
    let header = rows.iter().find(|r| r.kind == SidebarRowKind::GroupHeader && f64::from(p.y) >= r.top && f64::from(p.y) < r.bottom());
    let painter = ui.painter().with_clip_rect(list);
    if let Some(h) = header {
        let hr = Rect::from_min_size(pos2(list.min.x, h.top as f32), vec2(list.width(), h.height as f32));
        painter.rect(
            hr,
            CornerRadius::same(radius::SM),
            Tokens::tint(t.blue, alpha::DROP * 2.0),
            Stroke::new(size::RING, t.blue),
            StrokeKind::Inside,
        );
    } else {
        let y = target.indicator_y as f32;
        painter.hline((list.min.x + space::XS)..=(list.max.x - space::XS), y, Stroke::new(size::RING, t.blue));
        painter.circle_filled(pos2(list.min.x + space::XS, y), space::XS - size::HAIRLINE, t.blue);
    }
    if !ui.input(|i| i.pointer.any_released()) {
        return;
    }
    let anchor = target.before_session_id.as_ref().or(target.after_session_id.as_ref());
    if anchor == Some(&dragged) {
        return;
    }
    let group = if target.assigns_group {
        target.group_id.clone()
    } else {
        anchor.and_then(|a| model.session(a)).map_or_else(|| model.session(&dragged).and_then(|s| s.group.clone()), |s| s.group.clone())
    };
    let mut params = json!({"session_id": dragged, "group_id": group});
    if let Some(b) = &target.before_session_id {
        params["before"] = json!(b);
    } else if let Some(a) = &target.after_session_id {
        params["after"] = json!(a);
    }
    actions.push(Action::Tool("session_move_to_group", params));
}

/// The dragged row, following the pointer.
fn drag_ghost(ctx: &egui::Context, app: &CsmApp) {
    let Some(id) = &app.ui.drags.row else { return };
    let (Some(s), Some(p)) = (app.model.session(id), ctx.pointer_latest_pos()) else { return };
    let t = Tokens::get(ctx);
    ctx.set_cursor_icon(CursorIcon::Grabbing);
    egui::Area::new(Id::new("row-ghost")).order(Order::Tooltip).interactable(false).fixed_pos(p + vec2(space::MD, space::XS)).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::symmetric(space::ROW_X as i8, space::XS as i8)).show(ui, |ui| {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(size::STRIPE, icon::MD), Sense::hover());
                ui.painter().rect_filled(r, CornerRadius::same(radius::SM / 2), t.accent(s.accent));
                ui.label(egui::RichText::new(&s.name).font(theme::medium(text::BODY)).color(t.fg));
            });
        });
    });
}

/// Live sessions first, then asleep, each in model order.
fn ordered<'a>(ss: &[&'a SessionView]) -> Vec<&'a SessionView> {
    ss.iter().filter(|s| s.status.is_live()).chain(ss.iter().filter(|s| !s.status.is_live())).copied().collect()
}

/// One line of a sidebar section once worktrees are nested.
#[derive(Debug, Clone, PartialEq)]
pub enum SideItem<'a> {
    /// A session row; `nested` ones are indented under their main checkout's row or a repo header.
    Session { s: &'a SessionView, nested: bool },
    /// A light `repo (N)` header over linked-worktree sessions whose main checkout has no
    /// session in this section (none at all, or one in another group).
    Repo { name: String, count: usize },
}

/// A section's rows with worktrees nested (WPF's worktree clusters, as plan §4 has them: shown
/// under their repo automatically). Live sessions first, then asleep, as [`ordered`]; a session
/// in a linked worktree follows the row of its main checkout's session (`parent`, or another
/// session of the main checkout when the parent's row is in another section) when that is in the
/// section, else it sits under a [`SideItem::Repo`] header with the other worktrees of that
/// repository, placed where the first of them would be. Every session appears exactly once.
pub fn nest<'a>(ss: &[&'a SessionView]) -> Vec<SideItem<'a>> {
    use crate::git_model::folder_key;
    let list = ordered(ss);
    let here = |id: &str| list.iter().find(|x| x.id == id).copied();
    // A row others may nest under: not nested itself (no parent in this section).
    let top = |p: &SessionView| p.parent.as_deref().is_none_or(|pp| pp == p.id || here(pp).is_none());
    let repo_key = |s: &SessionView| -> Option<String> {
        s.worktree_of.as_ref()?;
        s.repo_root.as_deref().map(folder_key).or_else(|| s.worktree_of.as_deref().map(str::to_lowercase))
    };
    // The row a session nests under: its parent when that is here and a top row; for a session
    // in a linked worktree, else the first top row of a session in the repository's main checkout.
    let parent_of = |s: &SessionView| -> Option<String> {
        let parent = s.parent.as_deref().filter(|p| *p != s.id).and_then(&here).filter(|p| top(p));
        let main = || {
            s.worktree_of.as_ref()?;
            let root = folder_key(s.repo_root.as_deref()?);
            list.iter()
                .find(|m| m.id != s.id && m.worktree_of.is_none() && m.repo_root.as_deref().map(folder_key) == Some(root.clone()) && top(m))
                .copied()
        };
        parent.or_else(main).map(|p| p.id.clone())
    };
    let orphan = |s: &SessionView| parent_of(s).is_none() && repo_key(s).is_some();
    let mut out = Vec::new();
    let mut done: std::collections::HashSet<&'a str> = std::collections::HashSet::new();
    // A row and, depth first, every row nested under it.
    let emit = |out: &mut Vec<SideItem<'a>>, done: &mut std::collections::HashSet<&'a str>, s: &'a SessionView, nested: bool| {
        let mut stack = vec![(s, nested)];
        while let Some((x, n)) = stack.pop() {
            if !done.insert(x.id.as_str()) {
                continue;
            }
            out.push(SideItem::Session { s: x, nested: n });
            let kids: Vec<&'a SessionView> =
                list.iter().copied().filter(|c| !done.contains(c.id.as_str()) && parent_of(c).as_deref() == Some(x.id.as_str())).collect();
            stack.extend(kids.into_iter().rev().map(|c| (c, true)));
        }
    };
    for &s in &list {
        if done.contains(s.id.as_str()) || parent_of(s).is_some() {
            continue;
        }
        if orphan(s) {
            let key = repo_key(s);
            let members: Vec<&'a SessionView> = list.iter().copied().filter(|m| orphan(m) && repo_key(m) == key).collect();
            let name = s.worktree_of.clone().unwrap_or_default();
            out.push(SideItem::Repo { name, count: members.len() });
            for m in members {
                emit(&mut out, &mut done, m, true);
            }
            continue;
        }
        emit(&mut out, &mut done, s, false);
    }
    // Whatever a parent chain left out (never expected): a plain row rather than a lost one.
    for &s in &list {
        if !done.contains(s.id.as_str()) {
            emit(&mut out, &mut done, s, false);
        }
    }
    out
}

/// A section's rows: sessions (worktrees nested) and the light repo headers, each recorded for
/// the drop resolver.
fn section_rows(ui: &mut Ui, app: &mut CsmApp, ss: &[&SessionView], rows: &mut Vec<SidebarRow>, actions: &mut Vec<Action>) {
    for item in nest(ss) {
        match item {
            SideItem::Session { s, nested } => {
                let r = row_at(ui, app, s, nested, actions);
                rows.push(SidebarRow::new(row_kind(s), Some(&s.id), f64::from(r.rect.top()), f64::from(r.rect.height())));
            }
            SideItem::Repo { name, count } => {
                let r = repo_header(ui, &name, count);
                rows.push(SidebarRow::new(SidebarRowKind::ClusterHeader, None, f64::from(r.top()), f64::from(r.height())));
            }
        }
    }
}

/// The light `repo (N)` line over worktree sessions whose main checkout has no row here.
fn repo_header(ui: &mut Ui, name: &str, count: usize) -> Rect {
    let t = Tokens::get(ui.ctx());
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_SM), Sense::hover());
    let cy = rect.center().y;
    let ir = Rect::from_min_size(pos2(rect.min.x + space::SM, cy - icon::XS / 2.0), Vec2::splat(icon::XS));
    icons::paint(ui, ir, "folder", icon::XS, t.faint);
    let label = format!("{name} ({count})");
    widgets::paint_text(
        ui,
        pos2(ir.max.x + space::XS, cy),
        Align2::LEFT_CENTER,
        &label,
        theme::medium(text::CAPTION),
        t.muted,
        rect.width() - space::XXL,
    );
    named(&resp, WidgetType::Label, format!("Repo: {label}"));
    resp.on_hover_text(format!("Worktrees of {name}; its main checkout has no session here"));
    rect
}

fn inbox(ui: &mut Ui, app: &mut CsmApp, pane: Rect, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let model = app.model.clone();
    ui.painter().rect(pane, CornerRadius::same(radius::LG), t.bg, Stroke::new(size::HAIRLINE, t.surface0), StrokeKind::Inside);
    let needs = model.needs_you();
    let unseen = model.unseen();
    // Header.
    let head = Rect::from_min_max(pane.min, pos2(pane.max.x, pane.min.y + size::CONTROL + space::SM));
    let cy = head.center().y + space::XXS;
    let ir = Rect::from_min_size(pos2(head.min.x + space::ROW_X, cy - icon::LG / 2.0), Vec2::splat(icon::LG));
    icons::paint(ui, ir, "inbox", icon::LG, t.fg);
    let tr = ui.painter().text(pos2(ir.max.x + space::SM, cy), Align2::LEFT_CENTER, "Needs you", semibold(text::BODY), t.fg);
    let cw = widgets::paint_count(ui, pos2(tr.max.x + space::SM + space::XS + space::XXS, cy), needs.len(), needs.is_empty(), size::COUNT);
    let _ = cw;
    let close = Rect::from_center_size(pos2(head.max.x - space::SM - size::CONTROL_SM / 2.0, cy), Vec2::splat(size::CONTROL_SM));
    if widgets::icon_button_at(
        ui,
        close,
        Id::new("inbox-close"),
        "x",
        false,
        "Hide the inbox. Ctrl+J still works; the badge next to Sessions brings it back.",
    )
    .clicked()
    {
        actions.push(Action::Tool("inbox_panel_set", json!({"open": false})));
    }
    let kw = widgets::kbd_width(ui, "Ctrl J");
    let next_w = widgets::text_width(ui, "Next", theme::regular(text::CAPTION)) + kw + space::SM + space::SM;
    let next = Rect::from_min_max(
        pos2(close.min.x - space::XS - next_w, cy - size::KBD / 2.0 - space::XXS),
        pos2(close.min.x - space::XS, cy + size::KBD / 2.0 + space::XXS),
    );
    let nr = ui.interact(next, Id::new("inbox-next"), Sense::click());
    if nr.hovered() {
        ui.painter().rect_filled(next, CornerRadius::same(radius::SM), t.row_sel);
    }
    ui.painter().text(
        pos2(next.min.x + space::XS + space::XXS, cy),
        Align2::LEFT_CENTER,
        "Next",
        theme::regular(text::CAPTION),
        if nr.hovered() { t.fg } else { t.muted },
    );
    widgets::paint_kbd(ui, Rect::from_min_size(pos2(next.max.x - space::XS - kw, cy - size::KBD / 2.0), vec2(kw, size::KBD)), "Ctrl J");
    named(&nr, WidgetType::Button, "Jump to the next session that needs you");
    if nr.on_hover_cursor(CursorIcon::PointingHand).clicked() {
        actions.push(Action::Tool("inbox_next", json!({})));
    }

    let body = Rect::from_min_max(pos2(pane.min.x, head.max.y), pos2(pane.max.x, pane.max.y - space::XS));
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        egui::ScrollArea::vertical().id_salt("inbox-list").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            if needs.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(space::ROW_X);
                    ui.label(egui::RichText::new("Nothing is waiting on you.").color(t.muted).size(text::CAPTION));
                });
            }
            for s in &needs {
                inbox_row(ui, &model, s, actions);
            }
            if !unseen.is_empty() {
                ui.add_space(space::XS + space::XXS);
                ui.horizontal(|ui| {
                    ui.add_space(space::ROW_X);
                    ui.label(egui::RichText::new("UNSEEN").font(semibold(text::CAPS)).color(t.faint));
                });
                for s in &unseen {
                    inbox_row(ui, &model, s, actions);
                }
            }
        });
    });
}

fn inbox_row(ui: &mut Ui, model: &UiModel, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    let (outer, _) = ui.allocate_exact_size(vec2(w, size::INBOX_ROW), Sense::hover());
    let rect = outer.shrink2(vec2(space::XS, space::XXS / 2.0));
    let resp = ui.interact(rect, Id::new(("inbox-row", s.id.clone())), Sense::click());
    let active = model.active.as_deref() == Some(s.id.as_str());
    if active || resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), if active { t.row_sel } else { t.row_hover });
    }
    let line1 = rect.min.y + space::SM + space::XXS + space::XS;
    let g = Rect::from_center_size(pos2(rect.min.x + space::SM + icon::MD / 2.0, line1), Vec2::splat(icon::MD));
    icons::paint_status(ui, g, s.status, icon::MD);
    let age = model.age_text(s);
    let ar = ui.painter().text(pos2(rect.max.x - space::SM, line1), Align2::RIGHT_CENTER, &age, theme::mono(text::MONO_SM), t.faint);
    widgets::paint_text(
        ui,
        pos2(g.max.x + space::SM, line1),
        Align2::LEFT_CENTER,
        &s.name,
        theme::medium(text::BODY),
        t.fg,
        ar.min.x - g.max.x - space::MD,
    );
    widgets::paint_text(
        ui,
        pos2(g.max.x + space::SM, line1 + space::LG + space::XXS),
        Align2::LEFT_CENTER,
        &s.detail,
        theme::regular(text::CAPTION),
        t.muted,
        rect.max.x - g.max.x - space::LG,
    );
    named(&resp, WidgetType::Button, format!("Inbox: {}", s.name));
    if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(s.status_text()).clicked() {
        actions.push(Action::Tool("session_focus", json!({"session_id": s.id})));
    }
}

fn section_header(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let model = app.model.clone();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_SM), Sense::hover());
    let cy = rect.center().y;
    let r = ui.painter().text(pos2(rect.min.x + space::XS + space::XXS, cy), Align2::LEFT_CENTER, "SESSIONS", semibold(text::CAPS), t.muted);
    let mut x = r.max.x + space::XS + space::XXS;
    if !model.inbox_panel.open {
        // The reopen badge: the Needs-you count next to "Sessions".
        let n = model.needs_you().len();
        let label = n.to_string();
        let w = icon::XS + widgets::text_width(ui, &label, semibold(text::TINY)) + space::SM + space::XS;
        let br = Rect::from_min_size(pos2(x, cy - size::COUNT / 2.0), vec2(w, size::COUNT));
        let resp = ui.interact(br, Id::new("inbox-reopen"), Sense::click());
        let (bg, fg) = if n > 0 { (t.st_approval, t.on_accent) } else { (t.surface0, t.fg2) };
        ui.painter().rect_filled(br, CornerRadius::same(radius::PILL), bg);
        icons::paint(
            ui,
            Rect::from_min_size(pos2(br.min.x + space::XS + space::XXS / 2.0, cy - icon::XS / 2.0), Vec2::splat(icon::XS)),
            "inbox",
            icon::XS - space::XXS / 2.0,
            fg,
        );
        ui.painter().text(pos2(br.max.x - space::XS - space::XXS / 2.0, cy), Align2::RIGHT_CENTER, label, semibold(text::TINY), fg);
        named(&resp, WidgetType::Button, "Show the Needs you inbox");
        if resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text("Show the Needs you inbox").clicked() {
            actions.push(Action::Tool("inbox_panel_set", json!({"open": true})));
        }
        x = br.max.x + space::XS;
    }
    let _ = x;
    // The group-view switch.
    let views = [
        (GroupMode::Rail, "rail", "Group rail: one group at a time, panes follow it"),
        (GroupMode::Headers, "layers", "Group headers: every group, collapsible"),
        (GroupMode::Flat, "list", "Flat list: no groups"),
    ];
    let bw = space::XL - space::XXS;
    let bh = space::LG + space::XXS;
    let seg = Rect::from_min_max(
        pos2(rect.max.x - space::XS - 3.0 * bw - space::XS, cy - bh / 2.0 - space::XXS),
        pos2(rect.max.x - space::XS, cy + bh / 2.0 + space::XXS),
    );
    ui.painter().rect(seg, CornerRadius::same(radius::SM + 1), t.crust, Stroke::new(size::HAIRLINE, t.surface0), StrokeKind::Inside);
    for (i, (mode, name, tip)) in views.iter().enumerate() {
        let b = Rect::from_min_size(
            pos2(seg.min.x + space::XXS / 2.0 + size::HAIRLINE + i as f32 * (bw + size::HAIRLINE), seg.min.y + space::XXS / 2.0 + size::HAIRLINE),
            vec2(bw, bh),
        );
        let resp = ui.interact(b, Id::new(("gview", *name)), Sense::click());
        let on = model.group_mode == *mode;
        if on {
            ui.painter().rect_filled(b, CornerRadius::same(radius::SM - 1), t.field);
        }
        icons::paint(ui, b, name, icon::SM, if on || resp.hovered() { t.fg } else { t.faint });
        named(&resp, WidgetType::RadioButton, format!("Group view: {}", name));
        if resp.on_hover_text(*tip).clicked() && !on {
            actions.push(Action::Tool(
                "view_set",
                json!({"view": match mode { GroupMode::Rail => "rail", GroupMode::Headers => "headers", GroupMode::Flat => "flat" }}),
            ));
        }
    }
    if model.group_mode != GroupMode::Rail {
        let live = model.sessions.iter().filter(|s| s.status.is_live()).count();
        let asleep = model.sessions.len() - live;
        ui.painter().text(
            pos2(seg.min.x - space::SM, cy),
            Align2::RIGHT_CENTER,
            format!("{live} live · {asleep} asleep"),
            theme::mono(text::MONO_SM),
            t.muted,
        );
    }
    ui.add_space(space::XXS);
}

fn scope_header(ui: &mut Ui, app: &mut CsmApp, scope: &Scope, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let model = app.model.clone();
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL), Sense::hover());
    let cy = rect.center().y;
    let ss = model.scope_sessions(scope);
    let live = ss.iter().filter(|s| s.status.is_live()).count();
    let renaming = matches!(scope, Scope::Group(g) if inline_is(app, &InlineTarget::Group(g.clone())));
    if renaming {
        let fr =
            Rect::from_min_max(pos2(rect.min.x, rect.min.y + space::XXS), pos2(rect.max.x - size::CONTROL_SM - space::XS, rect.max.y - space::XXS));
        inline_field(ui, app, fr, "Rename group", "Group name", actions);
    } else {
        let name = model.scope_name(scope);
        let nr = widgets::paint_text(
            ui,
            pos2(rect.min.x + space::XS + space::XXS, cy),
            Align2::LEFT_CENTER,
            &name,
            semibold(text::BODY),
            t.fg,
            rect.width() - size::CONTROL_SM - space::XXL * 2.0,
        );
        ui.painter().text(
            pos2(nr.max.x + space::XS + space::XXS, cy),
            Align2::LEFT_CENTER,
            format!("{live}/{}", ss.len()),
            theme::mono(text::MONO_SM),
            t.faint,
        );
        if let Scope::Group(g) = scope {
            // Double-click the name to rename it in place.
            let hr = ui.interact(nr, Id::new(("scope-name", g.clone())), Sense::click());
            if hr.on_hover_text("Double-click to rename (F2 renames the active session)").double_clicked() {
                actions.push(Action::StartGroupRename(g.clone()));
            }
        }
    }
    if let Scope::Group(g) = scope {
        let b = Rect::from_center_size(pos2(rect.max.x - space::XXS - size::CONTROL_SM / 2.0, cy), Vec2::splat(size::CONTROL_SM));
        if widgets::icon_button_at(ui, b, Id::new(("scope-more", g.clone())), "more", false, "Group actions (or right-click its tab)").clicked() {
            actions.push(Action::OpenMenu(Menu::Group { group: g.clone(), at: pos2(b.max.x + space::XS, b.min.y) }));
        }
    }
}

fn headers_view(ui: &mut Ui, app: &mut CsmApp, rows: &mut Vec<SidebarRow>, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let model = app.model.clone();
    let mut scopes: Vec<(Scope, String)> = model.groups.iter().map(|g| (Scope::Group(g.id.clone()), g.name.clone())).collect();
    if model.sessions.iter().any(|s| s.group.as_ref().is_none_or(|g| model.group(g).is_none())) {
        scopes.push((Scope::All, "Ungrouped".into()));
    }
    for (scope, name) in scopes {
        let ss: Vec<&SessionView> = match &scope {
            Scope::Group(g) => model.scope_sessions(&scope).into_iter().filter(|s| s.group.as_deref() == Some(g.as_str())).collect(),
            Scope::All => model.sessions.iter().filter(|s| s.group.as_ref().is_none_or(|g| model.group(g).is_none())).collect(),
        };
        let gid = match &scope {
            Scope::Group(g) => g.clone(),
            Scope::All => "ungrouped".into(),
        };
        let collapsed = app.model.group(&gid).is_some_and(|g| g.collapsed);
        let (gap, _) = ui.allocate_exact_size(vec2(ui.available_width(), space::XS), Sense::hover());
        let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::ROW), Sense::hover());
        // The header's slot (with the gap above it) is a drop target for its group.
        let header_id = match &scope {
            Scope::Group(g) => Some(g.as_str()),
            Scope::All => None,
        };
        rows.push(SidebarRow::new(SidebarRowKind::GroupHeader, header_id, f64::from(gap.top()), f64::from(rect.bottom() - gap.top())));
        let resp = ui.interact(rect, Id::new(("group-header", gid.clone())), Sense::click());
        if resp.hovered() {
            ui.painter().rect_filled(rect, CornerRadius::same(radius::SM), t.row_hover);
        }
        let cy = rect.center().y;
        let ch = Rect::from_min_size(pos2(rect.min.x + space::XXS, cy - icon::MD / 2.0), Vec2::splat(icon::MD));
        icons::paint(ui, ch, if collapsed { "chevR" } else { "chevD" }, icon::MD, t.fg2);
        let renaming = matches!(&scope, Scope::Group(g) if inline_is(app, &InlineTarget::Group(g.clone())));
        let nr = if renaming {
            let fr = Rect::from_min_max(pos2(ch.max.x + space::XS, rect.min.y + space::XXS), pos2(rect.max.x - space::XL, rect.max.y - space::XXS));
            inline_field(ui, app, fr, "Rename group", "Group name", actions);
            fr
        } else {
            widgets::paint_text(
                ui,
                pos2(ch.max.x + space::XS + space::XXS, cy),
                Align2::LEFT_CENTER,
                &name,
                semibold(text::SMALL),
                t.fg2,
                rect.width() - space::XXL * 3.0,
            )
        };
        let live = ss.iter().filter(|s| s.status.is_live()).count();
        let cr = ui.painter().text(
            pos2(nr.max.x + space::XS + space::XXS, cy),
            Align2::LEFT_CENTER,
            format!("{live}/{}", ss.len()),
            theme::mono(text::MONO_SM),
            t.faint,
        );
        if collapsed {
            // A collapsed group still shows the glyph of any session in it that needs you.
            let mut x = cr.max.x + space::XS + space::XXS;
            for s in ss.iter().filter(|s| s.status.needs_you() || s.status.unseen()) {
                icons::paint_status(ui, Rect::from_min_size(pos2(x, cy - icon::XS / 2.0), Vec2::splat(icon::XS)), s.status, icon::XS);
                x += icon::XS + space::XXS;
            }
        }
        let torn = model.group(&gid).is_some_and(|g| g.torn_off);
        if torn {
            let label = "Window 2";
            let w = widgets::text_width(ui, label, theme::medium(text::TINY)) + icon::XS + space::SM + space::XS;
            let wr = Rect::from_min_size(pos2(rect.max.x - space::XS - w, cy - size::COUNT / 2.0), vec2(w, size::COUNT));
            ui.painter().rect_filled(wr, CornerRadius::same(radius::PILL), Tokens::tint(t.blue, alpha::SOFT));
            icons::paint(
                ui,
                Rect::from_min_size(pos2(wr.min.x + space::XS + space::XXS, cy - icon::XS / 2.0), Vec2::splat(icon::XS)),
                "monitor",
                icon::XS - size::HAIRLINE,
                t.blue,
            );
            ui.painter().text(pos2(wr.max.x - space::XS - space::XXS, cy), Align2::RIGHT_CENTER, label, theme::medium(text::TINY), t.blue);
        } else if let Scope::Group(_) = &scope {
            let mode = model.layout(&scope).mode;
            icons::paint(
                ui,
                Rect::from_min_size(pos2(rect.max.x - space::XS - icon::SM, cy - icon::SM / 2.0), Vec2::splat(icon::SM)),
                mode.icon(),
                icon::SM,
                t.faint,
            );
        }
        named(&resp, WidgetType::Button, format!("Group: {name}"));
        let resp = resp.on_hover_text(if collapsed {
            "Click to expand. Right-click for group actions."
        } else {
            "Click to collapse. Right-click for group actions."
        });
        if resp.clicked() {
            actions.push(Action::ToggleCollapse(gid.clone()));
        }
        if resp.secondary_clicked()
            && let Scope::Group(g) = &scope
        {
            let at = ui.ctx().pointer_latest_pos().unwrap_or(rect.right_top());
            actions.push(Action::OpenMenu(Menu::Group { group: g.clone(), at }));
        }
        if resp.double_clicked()
            && let Scope::Group(g) = &scope
        {
            actions.push(Action::StartGroupRename(g.clone()));
        }
        if !collapsed {
            section_rows(ui, app, &ss, rows, actions);
            if ss.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(space::LG + space::XS);
                    ui.label(egui::RichText::new("Empty. Drag a session here.").color(t.faint).size(text::CAPTION));
                });
            }
        }
    }
}

/// A compact session row (`.row`): accent stripe, name (with kind badge), branch, status glyph;
/// the selected row gets a second line with the status word and folder.
pub fn row(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) -> egui::Response {
    row_at(ui, app, s, s.parent.is_some(), actions)
}

/// [`row`], indented (with the worktree mark) when `nested` under its repo.
fn row_at(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, nested: bool, actions: &mut Vec<Action>) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let model = app.model.clone();
    let model = model.as_ref();
    let selected = model.active.as_deref() == Some(s.id.as_str());
    let queued = model.restore.as_ref().is_some_and(|r| {
        let live: Vec<&SessionView> = model.sessions.iter().filter(|x| x.status.is_live()).collect();
        live.iter().position(|x| x.id == s.id).is_some_and(|i| i as u32 >= r.done)
    });
    let status = if queued { Status::Starting } else { s.status };
    let h = if selected { size::ROW + space::LG + space::XXS } else { size::ROW };
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::hover());
    let asleep = s.status == Status::Asleep;
    // Live rows drag (onto a rail tab, a group header, or between rows); sleeping ones don't.
    let sense = if asleep { Sense::click() } else { Sense::click_and_drag() };
    let resp = ui.interact(rect, Id::new(("session-row", s.id.clone())), sense);
    if resp.drag_started() {
        app.ui.drags.row = Some(s.id.clone());
    }
    let being_dragged = app.ui.drags.row.as_deref() == Some(s.id.as_str());
    let mut child = ui.new_child(UiBuilder::new().max_rect(rect));
    if asleep {
        child.set_opacity(alpha::ASLEEP);
    }
    if being_dragged {
        child.set_opacity(alpha::ASLEEP);
    }
    let p = child.painter().clone();
    if selected || resp.hovered() {
        p.rect_filled(rect, CornerRadius::same(radius::SM), if selected { t.row_sel } else { t.row_hover });
    }
    if resp.has_focus() {
        p.rect_stroke(rect, CornerRadius::same(radius::SM), Stroke::new(size::RING, t.blue), StrokeKind::Inside);
    }
    let indent = if nested { space::LG - space::XXS } else { 0.0 };
    let cy = rect.min.y + size::ROW / 2.0;
    let stripe = Rect::from_center_size(pos2(rect.min.x + space::SM + indent + size::STRIPE / 2.0, cy), vec2(size::STRIPE, icon::MD));
    p.rect_filled(stripe, CornerRadius::same(radius::SM / 2), t.accent(s.accent));
    let gx = rect.max.x - space::XS - space::XXS - icon::MD;
    icons::paint_status(&child, Rect::from_min_size(pos2(gx, cy - icon::MD / 2.0), Vec2::splat(icon::MD)), status, icon::MD);
    if status == Status::Working {
        icons::request_spinner_repaint(&child);
    }
    let mut right = gx - space::SM;
    if let Some(b) = &s.branch {
        let br = widgets::paint_text(&child, pos2(right, cy), Align2::RIGHT_CENTER, b, theme::mono(text::MONO_SM), t.muted, size::BRANCH_MAX_W);
        right = br.min.x - space::SM;
    }
    let mut x = stripe.max.x + space::SM;
    if nested || s.worktree_of.is_some() {
        icons::paint(&child, Rect::from_min_size(pos2(x, cy - icon::XS / 2.0), Vec2::splat(icon::XS)), "fork", icon::XS, t.faint);
        x += icon::XS + space::XS;
    }
    let name_color = if queued { t.muted } else { t.fg };
    let badge = match s.kind {
        Kind::Ssh => Some("SSH"),
        Kind::Wsl => Some("WSL"),
        Kind::Local => None,
    };
    let bw = badge.map_or(0.0, |b| widgets::text_width(&child, b, semibold(text::CAPS)) + space::SM + space::XS);
    if inline_is(app, &InlineTarget::Session(s.id.clone())) {
        let fr = Rect::from_min_max(pos2(x - space::XS, cy - size::ROW / 2.0 + space::XXS), pos2(gx - space::XS, cy + size::ROW / 2.0 - space::XXS));
        inline_field(ui, app, fr, "Rename session", "Session name", actions);
    } else {
        let nr = widgets::paint_text(&child, pos2(x, cy), Align2::LEFT_CENTER, &s.name, theme::regular(text::BODY), name_color, right - x - bw);
        if let Some(b) = badge {
            kind_badge(&child, pos2(nr.max.x + space::XS, cy), b);
        }
    }
    if selected {
        let y2 = rect.max.y - space::SM - space::XXS;
        let label = if queued { "Queued".to_string() } else { s.status_text() };
        let lr =
            p.text(pos2(stripe.max.x + space::SM, y2), Align2::LEFT_CENTER, &label, theme::medium(text::CAPTION), icons::status_color(&t, status));
        widgets::paint_text(
            &child,
            pos2(lr.max.x, y2),
            Align2::LEFT_CENTER,
            &format!(" · {}", s.folder),
            theme::regular(text::CAPTION),
            t.muted,
            rect.max.x - lr.max.x - space::SM,
        );
    }
    named(&resp, WidgetType::Button, format!("Session: {}", s.name));
    let tip = if asleep {
        format!("{} · Asleep keeps the session, its group, notes and run commands. Click to wake.", s.name)
    } else {
        format!("{} · {}\n{}\nDrag onto a group tab or header to move it; F2 renames.", s.name, s.status_text(), s.folder)
    };
    let resp = if app.ui.drags.row.is_some() { resp } else { resp.on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand) };
    if resp.clicked() || (resp.has_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
        actions.push(Action::Tool("session_focus", json!({"session_id": s.id})));
    }
    if resp.secondary_clicked() {
        let at = ui.ctx().pointer_latest_pos().unwrap_or(rect.right_top());
        actions.push(Action::OpenMenu(Menu::Row { session: s.id.clone(), at }));
    }
    resp
}

/// The small `SSH` / `WSL` / command badge (`.kb`).
pub fn kind_badge(ui: &Ui, left_center: egui::Pos2, label: &str) -> Rect {
    let t = Tokens::get(ui.ctx());
    let font = semibold(text::CAPS);
    let w = widgets::text_width(ui, label, font.clone()) + space::SM;
    let r = Rect::from_min_size(pos2(left_center.x, left_center.y - space::SM + size::HAIRLINE), vec2(w, space::LG - space::XXS));
    ui.painter().rect_stroke(r, CornerRadius::same(radius::SM - 1), Stroke::new(size::HAIRLINE, t.surface1), StrokeKind::Inside);
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, theme::mono(text::CAPS), t.muted);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, parent: Option<&str>, worktree_of: Option<&str>, root: Option<&str>, status: Status) -> SessionView {
        SessionView {
            id: id.into(),
            name: id.into(),
            parent: parent.map(Into::into),
            worktree_of: worktree_of.map(Into::into),
            repo_root: root.map(Into::into),
            status,
            ..SessionView::default()
        }
    }

    fn shape(items: &[SideItem<'_>]) -> Vec<String> {
        items
            .iter()
            .map(|i| match i {
                SideItem::Session { s, nested: true } => format!("  {}", s.id),
                SideItem::Session { s, nested: false } => s.id.clone(),
                SideItem::Repo { name, count } => format!("[{name} ({count})]"),
            })
            .collect()
    }

    #[test]
    fn worktrees_follow_their_main_checkout_or_sit_under_a_repo_header() {
        use Status::*;
        let all = [
            s("wt1", Some("main"), Some("Repo"), Some("C:\\r"), Working),
            s("other", None, None, None, Idle),
            s("main", None, None, Some("C:\\r"), Idle),
            s("wt2", Some("main"), Some("Repo"), Some("C:\\r"), Asleep),
            // A worktree of another repo whose main checkout has no session (the root spelled
            // two ways: case and a trailing separator).
            s("x1", None, Some("Lib"), Some("C:\\lib"), Idle),
            s("x2", None, Some("Lib"), Some("C:\\LIB\\"), Working),
        ];
        let refs: Vec<&SessionView> = all.iter().collect();
        assert_eq!(shape(&nest(&refs)), ["other", "main", "  wt1", "  wt2", "[Lib (2)]", "  x1", "  x2"]);

        // Without the main checkout in the section (another group): a header instead.
        let refs: Vec<&SessionView> = all.iter().filter(|x| x.id != "main").collect();
        assert_eq!(shape(&nest(&refs)), ["[Repo (2)]", "  wt1", "  wt2", "other", "[Lib (2)]", "  x1", "  x2"]);
    }

    #[test]
    fn a_worktree_nests_under_a_main_checkout_session_in_its_own_section() {
        use Status::*;
        // The engine picked `main` (another group) as the parent; `main2` is the same checkout here.
        let all = [s("wt", Some("main"), Some("Repo"), Some("C:\\r"), Idle), s("main2", None, None, Some("c:\\R\\"), Idle)];
        let refs: Vec<&SessionView> = all.iter().collect();
        assert_eq!(shape(&nest(&refs)), ["main2", "  wt"]);
    }

    #[test]
    fn rows_under_a_nested_row_are_never_lost() {
        use Status::*;
        // `child` has the worktree session as its parent: under a repo header, and under a
        // worktree that nests by repo root, it still shows (once).
        let all = [s("wt", None, Some("Repo"), Some("C:\\r"), Idle), s("child", Some("wt"), None, None, Idle)];
        let refs: Vec<&SessionView> = all.iter().collect();
        assert_eq!(shape(&nest(&refs)), ["[Repo (1)]", "  wt", "  child"]);
        let all =
            [s("wt", None, Some("Repo"), Some("C:\\r"), Idle), s("child", Some("wt"), None, None, Idle), s("main", None, None, Some("C:\\r"), Idle)];
        let refs: Vec<&SessionView> = all.iter().collect();
        assert_eq!(shape(&nest(&refs)), ["main", "  wt", "  child"]);
        // A parent cycle: both still appear.
        let all = [s("a", Some("b"), None, None, Idle), s("b", Some("a"), None, None, Idle)];
        let refs: Vec<&SessionView> = all.iter().collect();
        assert_eq!(nest(&refs).len(), 2);
    }
}
