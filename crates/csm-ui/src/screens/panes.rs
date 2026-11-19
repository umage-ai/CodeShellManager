//! The work area: the group bar (`.gb`) with the five layout modes, and the panes laid out by
//! the scope's layout — Focus, Side by side, Overview, Grid, Free (split tree with draggable
//! dividers, floating panes) — plus torn-off panes and groups in their own OS windows
//! (deferred viewports, so they render and present outside the main frame).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Align2, CornerRadius, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, ViewportBuilder, ViewportId, WidgetInfo, WidgetType, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp, Menu, PaneHost};
use crate::icons;
use crate::model::{Kind, Layout, LayoutMode, Scope, SessionId, SessionView, SplitDir, SplitNode, Status, UiModel};
use crate::screens::sidebar::kind_badge;
use crate::terminal::{SourceState, TerminalSource, TerminalView, ViewOptions, lock};
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

fn named(resp: &egui::Response, ty: WidgetType, label: impl Into<String>) {
    let l = label.into();
    resp.widget_info(move || WidgetInfo::labeled(ty, true, l.clone()));
}

/// How a pane is shown.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct PaneOpts {
    pub mode: Option<LayoutMode>,
    /// Overview mini-terminal: a preview, click focuses.
    pub mini: bool,
    pub floating: bool,
    /// Shown because it is active but not pinned (Side by side).
    pub preview: bool,
    /// In its own OS window.
    pub torn: bool,
    pub active: bool,
    pub focus: bool,
    pub pinned: bool,
}

/// The tiling of the Side by side mode (the mock's `tileCss`): columns, rows, and whether the
/// first pane spans two rows.
pub fn side_tiling(n: usize) -> (usize, usize, bool) {
    match n {
        0 | 1 => (1, 1, false),
        2 => (2, 1, false),
        3 => (2, 2, true),
        4 => (2, 2, false),
        5 => (3, 2, true),
        6 => (3, 2, false),
        n => {
            let cols = (n as f32).sqrt().ceil() as usize;
            (cols, n.div_ceil(cols), false)
        }
    }
}

/// Cell rects of a `cols × rows` grid in `area` with `gap`.
pub fn grid_cells(area: Rect, cols: usize, rows: usize, gap: f32) -> Vec<Rect> {
    let (cols, rows) = (cols.max(1), rows.max(1));
    let w = (area.width() - gap * (cols as f32 - 1.0)) / cols as f32;
    let h = (area.height() - gap * (rows as f32 - 1.0)) / rows as f32;
    (0..cols * rows)
        .map(|i| {
            let (c, r) = (i % cols, i / cols);
            Rect::from_min_size(pos2(area.min.x + c as f32 * (w + gap), area.min.y + r as f32 * (h + gap)), vec2(w, h))
        })
        .collect()
}

/// Which sessions a layout shows, and where.
pub fn plan(model: &UiModel, scope: &Scope, layout: &Layout, area: Rect) -> Vec<(SessionId, Rect, PaneOpts)> {
    let gap = space::PANE_GAP;
    let live: Vec<&SessionView> = model.scope_sessions(scope).into_iter().filter(|s| s.status.is_live() && !layout.torn.contains(&s.id)).collect();
    let is_live = |id: &str| live.iter().any(|s| s.id == id);
    let active = model.active.clone().filter(|a| is_live(a));
    let mut out = Vec::new();
    match layout.mode {
        LayoutMode::Focus => {
            if let Some(a) = active.or_else(|| live.first().map(|s| s.id.clone())) {
                out.push((a, area, PaneOpts { mode: Some(LayoutMode::Focus), ..PaneOpts::default() }));
            }
        }
        LayoutMode::Side => {
            let mut ids: Vec<SessionId> = layout.pinned.iter().filter(|p| is_live(p)).cloned().collect();
            let mut preview = None;
            if let Some(a) = &active
                && !ids.contains(a)
            {
                preview = Some(a.clone());
                ids.push(a.clone());
            }
            let (cols, rows, span) = side_tiling(ids.len());
            let cells = grid_cells(area, cols, rows, gap);
            let mut slot = 0;
            for (i, id) in ids.into_iter().enumerate() {
                let mut r = cells.get(slot).copied().unwrap_or(area);
                if span && i == 0 {
                    // The first pane spans both rows of the first column.
                    if let Some(below) = cells.get(cols) {
                        r = r.union(*below);
                    }
                }
                slot += 1;
                if span && slot == cols {
                    slot += 1;
                }
                let pinned = layout.pinned.contains(&id);
                out.push((id.clone(), r, PaneOpts { mode: Some(LayoutMode::Side), preview: preview.as_deref() == Some(id.as_str()), pinned, ..PaneOpts::default() }));
            }
        }
        LayoutMode::Overview => {
            let n = live.len().max(1);
            let cols = ((n as f32 * 1.4).sqrt().ceil() as usize).max(1);
            let rows = n.div_ceil(cols);
            for (s, r) in live.iter().zip(grid_cells(area, cols, rows, gap)) {
                out.push((s.id.clone(), r, PaneOpts { mode: Some(LayoutMode::Overview), mini: true, ..PaneOpts::default() }));
            }
        }
        LayoutMode::Grid => {
            let g = layout.grid;
            let mut ids: Vec<SessionId> = live.iter().map(|s| s.id.clone()).collect();
            if let Some(a) = &active
                && let Some(i) = ids.iter().position(|x| x == a)
                && i >= g.cells()
            {
                ids.remove(i);
                ids.insert(0, a.clone());
            }
            for (id, r) in ids.into_iter().zip(grid_cells(area, usize::from(g.cols), usize::from(g.rows), gap)) {
                out.push((id, r, PaneOpts { mode: Some(LayoutMode::Grid), ..PaneOpts::default() }));
            }
        }
        LayoutMode::Free => {
            let floating: Vec<&str> = layout.floats.iter().map(|f| f.session.as_str()).collect();
            let tree = layout.tree.clone().or_else(|| {
                let ids: Vec<SessionId> = if layout.pinned.is_empty() { live.iter().map(|s| s.id.clone()).collect() } else { layout.pinned.clone() };
                SplitNode::row(&ids)
            });
            if let Some(tree) = tree {
                let mut leaves = Vec::new();
                layout_tree(&tree, area, gap, &mut Vec::new(), &mut leaves, &mut Vec::new());
                for (id, r) in leaves {
                    if is_live(&id) && !floating.contains(&id.as_str()) {
                        out.push((id, r, PaneOpts { mode: Some(LayoutMode::Free), ..PaneOpts::default() }));
                    }
                }
            }
        }
    }
    for (id, _, o) in &mut out {
        o.active = model.active.as_deref() == Some(id.as_str());
    }
    out
}

/// A divider of the split tree: where it is, which split it belongs to and which pair it moves.
#[derive(Debug, Clone)]
pub struct Divider {
    pub rect: Rect,
    pub path: Vec<usize>,
    pub index: usize,
    pub dir: SplitDir,
    /// The split's own rect (to turn a pointer position into a ratio).
    pub parent: Rect,
    /// Where the pair starts within the parent, as a fraction.
    pub before: f32,
    pub pair: f32,
}

/// Lays out a split tree: leaf rects and divider rects.
pub fn layout_tree(node: &SplitNode, rect: Rect, gap: f32, path: &mut Vec<usize>, leaves: &mut Vec<(SessionId, Rect)>, dividers: &mut Vec<Divider>) {
    match node {
        SplitNode::Leaf { session } => leaves.push((session.clone(), rect)),
        SplitNode::Split { dir, ratios, children } => {
            let n = children.len();
            if n == 0 {
                return;
            }
            let total: f32 = ratios.iter().take(n).sum::<f32>().max(f32::EPSILON);
            let len = match dir {
                SplitDir::Row => rect.width(),
                SplitDir::Col => rect.height(),
            } - gap * (n as f32 - 1.0);
            let mut at = 0.0f32;
            let mut frac_before = 0.0f32;
            for (i, child) in children.iter().enumerate() {
                let frac = ratios.get(i).copied().unwrap_or(1.0 / n as f32) / total;
                let extent = len * frac;
                let r = match dir {
                    SplitDir::Row => Rect::from_min_size(pos2(rect.min.x + at, rect.min.y), vec2(extent, rect.height())),
                    SplitDir::Col => Rect::from_min_size(pos2(rect.min.x, rect.min.y + at), vec2(rect.width(), extent)),
                };
                path.push(i);
                layout_tree(child, r, gap, path, leaves, dividers);
                path.pop();
                at += extent;
                if i + 1 < n {
                    let next = ratios.get(i + 1).copied().unwrap_or(1.0 / n as f32) / total;
                    let d = match dir {
                        SplitDir::Row => Rect::from_min_size(pos2(rect.min.x + at, rect.min.y), vec2(gap, rect.height())),
                        SplitDir::Col => Rect::from_min_size(pos2(rect.min.x, rect.min.y + at), vec2(rect.width(), gap)),
                    };
                    dividers.push(Divider { rect: d, path: path.clone(), index: i, dir: *dir, parent: rect, before: frac_before, pair: frac + next });
                    at += gap;
                }
                frac_before += frac;
            }
        }
    }
}

pub fn workspace(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    let bar = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.min.y + size::GROUP_BAR));
    ui.scope_builder(UiBuilder::new().max_rect(bar), |ui| group_bar(ui, app, actions));
    let area = Rect::from_min_max(pos2(rect.min.x, bar.max.y), rect.max);
    ui.painter().rect_filled(area, 0.0, t.crust);
    let inner = area.shrink(space::PANE_GAP);
    let model = app.model.clone();
    let scope = model.pane_scope();
    let mut layout = model.layout(&scope);
    if let Some(tree) = &app.ui.drags.tree {
        layout.tree = Some(tree.clone());
    }
    if let Some((id, r)) = &app.ui.drags.float
        && let Some(f) = layout.floats.iter_mut().find(|f| &f.session == id)
    {
        f.rect = *r;
    }
    let torn_group = matches!(&scope, Scope::Group(g) if model.group(g).is_some_and(|g| g.torn_off));
    if torn_group {
        ui.painter().text(inner.center(), Align2::CENTER_CENTER, format!("{} lives in its own window.", model.scope_name(&scope)), theme::regular(text::BODY), t.muted);
        return;
    }
    if model.sessions.is_empty() {
        first_run(ui, &model, inner, actions);
        return;
    }
    let planned = plan(&model, &scope, &layout, inner);
    if planned.is_empty() {
        let msg = if model.scope_sessions(&scope).is_empty() { "No sessions here yet. Ctrl+T starts one." } else { "Every session here is asleep. Click one in the sidebar to wake it." };
        ui.painter().text(inner.center(), Align2::CENTER_CENTER, msg, theme::regular(text::BODY), t.muted);
    }
    let focus = app.ui.focus_terminal.clone();
    let host = app.host.clone();
    {
        let mut host = lock(&host);
        for (id, r, mut o) in planned.iter().cloned() {
            o.focus = focus.as_deref() == Some(id.as_str()) && !o.mini;
            if o.focus {
                app.ui.focus_terminal = None;
            }
            let Some(s) = model.session(&id) else { continue };
            let src = app.backend.terminal(&id);
            let head = pane(ui, &mut host, &model, s, src.as_deref(), r, o, &scope, actions);
            if head.drag_started() && layout.mode == LayoutMode::Free {
                app.ui.drags.pane = Some(id.clone());
            }
        }
        // Grid: empty slots invite a pick.
        if layout.mode == LayoutMode::Grid {
            let cells = grid_cells(inner, usize::from(layout.grid.cols), usize::from(layout.grid.rows), space::PANE_GAP);
            for (i, r) in cells.iter().enumerate().skip(planned.len()) {
                let resp = ui.interact(*r, Id::new(("grid-slot", i)), Sense::click());
                let stroke = if resp.hovered() { t.blue } else { t.surface1 };
                dashed_rect(ui, *r, stroke);
                icons::paint(ui, Rect::from_center_size(r.center() - vec2(0.0, space::MD), Vec2::splat(icon::XXL)), "plus", icon::XXL, t.muted);
                ui.painter().text(r.center() + vec2(0.0, space::SM), Align2::CENTER_CENTER, "Empty slot", theme::regular(text::SMALL), if resp.hovered() { t.fg } else { t.muted });
                ui.painter().text(r.center() + vec2(0.0, space::LG + space::SM), Align2::CENTER_CENTER, "Pick a session for it", theme::regular(text::TINY), t.faint);
                named(&resp, WidgetType::Button, "Empty slot");
                if resp.clicked() {
                    actions.push(Action::OpenPalette);
                }
            }
        }
        // Free: the dividers, a header being dragged onto an edge, then the floating panes on top.
        if layout.mode == LayoutMode::Free {
            dividers(ui, app, &layout, inner, &scope, actions);
            split_drag(ui, app, &layout, &planned, &scope, actions);
            for f in &layout.floats {
                let Some(s) = model.session(&f.session) else { continue };
                if !s.status.is_live() {
                    continue;
                }
                let r = Rect::from_min_size(inner.min + vec2(f.rect[0], f.rect[1]), vec2(f.rect[2], f.rect[3]));
                let src = app.backend.terminal(&f.session);
                let o = PaneOpts { mode: Some(LayoutMode::Free), floating: true, active: model.active.as_deref() == Some(s.id.as_str()), ..PaneOpts::default() };
                let area_id = Id::new(("float", f.session.clone()));
                let mut drag = None;
                egui::Area::new(area_id).order(egui::Order::Middle).fixed_pos(r.min).show(ui.ctx(), |ui| {
                    let rr = Rect::from_min_size(r.min, r.size());
                    ui.painter().rect_filled(rr.expand(size::HAIRLINE), CornerRadius::same(radius::MD), t.surface2);
                    let shadow = egui::Shadow { offset: [0, 10], blur: 28, spread: 0, color: t.shadow };
                    ui.painter().add(shadow.as_shape(rr, CornerRadius::same(radius::MD)));
                    // Header drag moves it.
                    let head = Rect::from_min_size(rr.min, vec2(rr.width(), size::PANE_HEADER));
                    let resp = ui.interact(head.shrink2(vec2(space::XXL * 3.0, 0.0)), Id::new(("float-drag", f.session.clone())), Sense::drag());
                    if resp.dragged() {
                        drag = Some(resp.drag_delta());
                    }
                    if resp.drag_stopped() {
                        drag = Some(Vec2::ZERO);
                    }
                    pane(ui, &mut host, &model, s, src.as_deref(), rr, o, &scope, actions);
                    let _ = resp.on_hover_cursor(CursorIcon::Grab);
                });
                if let Some(d) = drag {
                    let mut nr = f.rect;
                    nr[0] = (nr[0] + d.x).clamp(0.0, (inner.width() - size::MIN_PANE).max(0.0));
                    nr[1] = (nr[1] + d.y).clamp(0.0, (inner.height() - size::PANE_HEADER).max(0.0));
                    let stopped = d == Vec2::ZERO && ui.ctx().input(|i| !i.pointer.any_down());
                    if stopped {
                        app.ui.drags.float = None;
                        let mut free = layout.free();
                        if let Some(x) = free.floats.iter_mut().find(|x| x.session == f.session) {
                            x.rect = nr;
                        }
                        actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "free": free.to_value()})));
                    } else {
                        app.ui.drags.float = Some((f.session.clone(), nr));
                    }
                }
            }
        }
    }
}

/// No sessions at all (a first run, or everything closed): say what to do, and offer the WPF
/// app's sessions when its state file exists.
fn first_run(ui: &mut Ui, model: &UiModel, area: Rect, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let card = Rect::from_center_size(area.center(), vec2(size::DIALOG_W.min(area.width()), size::DIALOG_W.min(area.height()) * 0.5));
    ui.scope_builder(UiBuilder::new().max_rect(card), |ui| {
        ui.vertical_centered(|ui| {
            ui.spacing_mut().item_spacing.y = space::SM;
            ui.label(egui::RichText::new("No sessions yet").font(semibold(text::TITLE)).color(t.fg));
            ui.label(egui::RichText::new("Open a folder and Claude (or a shell) starts there. Ctrl+T works from anywhere.").font(theme::regular(text::BODY)).color(t.fg2));
            ui.add_space(space::SM);
            let new = widgets::button(ui, ButtonKind::Primary, Some("plus"), "New session", Some("Ctrl T"));
            named(&new, WidgetType::Button, "New session");
            if new.clicked() {
                actions.push(Action::OpenQuickOpen { group: None });
            }
            if let Some(path) = &model.wpf_import {
                let import = widgets::button(ui, ButtonKind::Secondary, Some("history"), "Import from CodeShellManager (WPF)", None)
                    .on_hover_text(format!("Sessions and groups from {path}. They arrive asleep; nothing starts until you wake one."));
                named(&import, WidgetType::Button, "Import from CodeShellManager (WPF)");
                if import.clicked() {
                    actions.push(Action::Tool("state_import_wpf", json!({"path": path})));
                }
            }
        });
    });
}

fn dashed_rect(ui: &Ui, r: Rect, color: egui::Color32) {
    let st = Stroke::new(size::HAIRLINE, color);
    let dash = space::XS;
    let gapl = space::XS - size::HAIRLINE;
    let corners = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
    for w in corners.windows(2) {
        if let [a, b] = w {
            ui.painter().extend(egui::Shape::dashed_line(&[*a, *b], st, dash, gapl));
        }
    }
}

fn dividers(ui: &mut Ui, app: &mut CsmApp, layout: &Layout, area: Rect, scope: &Scope, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let Some(tree) = layout.tree.clone() else { return };
    let mut divs = Vec::new();
    layout_tree(&tree, area, space::PANE_GAP, &mut Vec::new(), &mut Vec::new(), &mut divs);
    for d in divs {
        let id = Id::new(("divider", d.path.clone(), d.index));
        let resp = ui.interact(d.rect, id, Sense::drag());
        named(&resp, WidgetType::Other, "Pane divider");
        let lit = resp.hovered() || resp.dragged();
        let line = match d.dir {
            SplitDir::Row => Rect::from_center_size(d.rect.center(), vec2(size::RING, d.rect.height() * 0.4)),
            SplitDir::Col => Rect::from_center_size(d.rect.center(), vec2(d.rect.width() * 0.4, size::RING)),
        };
        if lit {
            ui.painter().rect_filled(line, CornerRadius::same(radius::SM / 2), t.blue);
        }
        let resp = resp.on_hover_cursor(if d.dir == SplitDir::Row { CursorIcon::ResizeColumn } else { CursorIcon::ResizeRow });
        if resp.dragged()
            && let Some(p) = ui.ctx().pointer_latest_pos()
        {
            let pos = match d.dir {
                SplitDir::Row => (p.x - d.parent.min.x) / d.parent.width().max(1.0),
                SplitDir::Col => (p.y - d.parent.min.y) / d.parent.height().max(1.0),
            };
            let first = (pos - d.before).clamp(0.12 * d.pair, 0.88 * d.pair) / d.pair.max(f32::EPSILON);
            let mut next = app.ui.drags.tree.clone().unwrap_or_else(|| tree.clone());
            // Ratios are stored normalised within the split: scale the pair by its share.
            set_pair(&mut next, &d.path, d.index, first);
            app.ui.drags.tree = Some(next);
            let pct = (first * 100.0).round();
            let label = format!("{} : {}", pct, 100.0 - pct);
            let lw = widgets::text_width(ui, &label, theme::mono(text::MONO_SM)) + space::MD;
            let lr = Rect::from_center_size(d.rect.center(), vec2(lw, space::LG + space::XS));
            ui.painter().rect_filled(lr, CornerRadius::same(radius::SM), t.blue);
            ui.painter().text(lr.center(), Align2::CENTER_CENTER, label, theme::mono(text::MONO_SM), t.on_accent);
        }
        if resp.drag_stopped()
            && let Some(tree) = app.ui.drags.tree.take()
        {
            let mut free = layout.free();
            free.tree = Some(tree);
            actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "free": free.to_value()})));
        }
    }
}

/// Sets the pair (i, i+1) of the split at `path` so the first takes `first` (0–1) of the pair.
fn set_pair(node: &mut SplitNode, path: &[usize], i: usize, first: f32) {
    match node {
        SplitNode::Leaf { .. } => {}
        SplitNode::Split { ratios, children, .. } => {
            if let Some((&head, rest)) = path.split_first() {
                if let Some(c) = children.get_mut(head) {
                    set_pair(c, rest, i, first);
                }
            } else if let (Some(&a), Some(&b)) = (ratios.get(i), ratios.get(i + 1)) {
                let pair = a + b;
                if let Some(x) = ratios.get_mut(i) {
                    *x = pair * first;
                }
                if let Some(x) = ratios.get_mut(i + 1) {
                    *x = pair * (1.0 - first);
                }
            }
        }
    }
}

fn group_bar(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, t.bg);
    ui.painter().hline(rect.x_range(), rect.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));
    let model = app.model.clone();
    let scope = model.pane_scope();
    let layout = model.layout(&scope);
    let live = model.scope_sessions(&scope).iter().filter(|s| s.status.is_live()).count();
    let meta = match layout.mode {
        LayoutMode::Side => format!("{} pinned · {live} live", layout.pinned.iter().filter(|p| model.session(p).is_some_and(|s| s.status.is_live())).count()),
        LayoutMode::Overview => "every live session".into(),
        LayoutMode::Grid => format!("preset {}", layout.grid.label()),
        LayoutMode::Free => {
            let n = layout.floats.len();
            if n == 0 { "split tree".into() } else { format!("split tree · {n} floating") }
        }
        LayoutMode::Focus => "one pane".into(),
    };
    let cy = rect.center().y;
    let nr = ui.painter().text(pos2(rect.min.x + space::MD, cy), Align2::LEFT_CENTER, model.scope_name(&scope), semibold(text::EMPH), t.fg);
    let mr = ui.painter().text(pos2(nr.max.x + space::MD, cy), Align2::LEFT_CENTER, &meta, theme::regular(text::CAPTION), t.muted);
    // The segmented layout control.
    let font = theme::medium(text::SMALL);
    let mut widths = Vec::new();
    for m in LayoutMode::ALL {
        let extra = if m == LayoutMode::Grid { icon::XS + space::XS } else { 0.0 };
        widths.push(space::ROW_X + icon::MD + space::XS + space::XXS + widgets::text_width(ui, m.label(), font.clone()) + extra + space::ROW_X - space::XXS);
    }
    let seg_w: f32 = widths.iter().sum::<f32>() + space::XXS * (widths.len() as f32 + 1.0);
    let seg = Rect::from_min_size(pos2(mr.max.x + space::MD, cy - size::SEG_BUTTON / 2.0 - space::XXS - size::HAIRLINE), vec2(seg_w, size::SEG_BUTTON + space::XS + size::RING));
    ui.painter().rect(seg, CornerRadius::same(radius::MD + 1), t.crust, Stroke::new(size::HAIRLINE, t.surface0), StrokeKind::Inside);
    let mut x = seg.min.x + space::XXS + size::HAIRLINE / 2.0;
    for (m, w) in LayoutMode::ALL.into_iter().zip(widths) {
        let b = Rect::from_min_size(pos2(x, cy - size::SEG_BUTTON / 2.0), vec2(w, size::SEG_BUTTON));
        x += w + space::XXS;
        let resp = ui.interact(b, Id::new(("layout-mode", m.id())), Sense::click());
        let on = layout.mode == m;
        if on {
            ui.painter().rect_filled(b, CornerRadius::same(radius::SM + 1), t.field);
        }
        let fg = if on || resp.hovered() { t.fg } else { t.muted };
        let ir = Rect::from_min_size(pos2(b.min.x + space::ROW_X - space::XXS, cy - icon::MD / 2.0), Vec2::splat(icon::MD));
        icons::paint(ui, ir, m.icon(), icon::MD, fg);
        let lr = ui.painter().text(pos2(ir.max.x + space::XS + space::XXS, cy), Align2::LEFT_CENTER, m.label(), font.clone(), fg);
        if m == LayoutMode::Grid {
            icons::paint(ui, Rect::from_min_size(pos2(lr.max.x + space::XS, cy - icon::XS / 2.0), Vec2::splat(icon::XS)), "chevD", icon::XS, fg);
        }
        named(&resp, WidgetType::Button, format!("Layout: {}", m.label()));
        if resp.on_hover_text(m.label()).clicked() {
            if m == LayoutMode::Grid {
                if !on {
                    actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": "grid"})));
                }
                actions.push(Action::OpenMenu(Menu::Grid { at: pos2(b.min.x, seg.max.y + space::XS) }));
            } else if !on {
                actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": m.id()})));
            }
        }
    }
    let mut end = rect.max.x - space::SM;
    if model.inspector.is_none() {
        let w = widgets::text_width(ui, "Inspector", theme::medium(text::BODY)) + icon::LG + space::ROW_X * 2.0 + space::XS + space::XXS;
        let r = Rect::from_min_size(pos2(end - w, cy - size::CONTROL / 2.0), vec2(w, size::CONTROL));
        end = r.min.x - space::SM;
        let resp = ui.scope_builder(UiBuilder::new().max_rect(r), |ui| widgets::button(ui, ButtonKind::Ghost, Some("panelR"), "Inspector", None)).inner;
        if resp.clicked() {
            actions.push(Action::Tool("inspector_show", json!({"tab": "notes"})));
        }
    }
    if layout.mode == LayoutMode::Free {
        widgets::paint_text(ui, pos2(seg.max.x + space::MD, cy), Align2::LEFT_CENTER, "Drag a divider to resize. Drop a pane header on another pane's edge to split.", theme::regular(text::CAPTION), t.muted, end - seg.max.x - space::MD * 2.0);
    }
}

/// One pane: slim header and the terminal (or a preview of it).
#[allow(clippy::too_many_arguments)]
pub fn pane(
    ui: &mut Ui,
    host: &mut PaneHost,
    model: &UiModel,
    s: &SessionView,
    source: Option<&dyn TerminalSource>,
    rect: Rect,
    o: PaneOpts,
    scope: &Scope,
    actions: &mut Vec<Action>,
) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let accent = t.accent(s.accent);
    let r = CornerRadius::same(radius::MD);
    let p = ui.painter().clone();
    p.rect_filled(rect, r, t.term_bg);
    let head_h = if o.mini { size::PANE_HEADER_MINI } else { size::PANE_HEADER };
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), head_h));
    let body = Rect::from_min_max(pos2(rect.min.x, head.max.y), rect.max).shrink(size::HAIRLINE);
    let queued = model.restore.as_ref().is_some_and(|rp| {
        let live: Vec<&SessionView> = model.sessions.iter().filter(|x| x.status.is_live()).collect();
        live.iter().position(|x| x.id == s.id).is_some_and(|i| i as u32 >= rp.done)
    });

    // Terminal (drawn first so the header sits on top of the pane's clip).
    let view_key = format!("{}:{}", if o.torn { "torn" } else if o.mini { "mini" } else { "main" }, s.id);
    let mut clicked = false;
    if let Some(src) = source {
        let style = t.terminal_style();
        let opts = ViewOptions { interactive: !o.mini, request_focus: o.focus, style, ..ViewOptions::default() };
        let mut child = ui.new_child(UiBuilder::new().max_rect(body));
        child.set_clip_rect(body.intersect(ui.clip_rect()));
        let PaneHost { renderer, views, padding, .. } = host;
        let opts = ViewOptions { padding: vec2(*padding, (*padding * 0.75).round()), ..opts };
        let view = views.entry(view_key.clone()).or_insert_with(|| TerminalView::new(Id::new(("csm-pane", view_key.clone()))));
        let vr = view.show(&mut child, src, renderer, body, &opts);
        clicked = vr.clicked;
        let placeholder = if queued { Some(format!("Starting {} in {}…", s.command, s.name)) } else { TerminalView::placeholder(src) };
        if let Some(text_) = placeholder {
            p.rect_filled(body, 0.0, t.term_bg);
            let starting = queued || src.state() == SourceState::Starting;
            if starting {
                icons::paint_status(ui, Rect::from_center_size(body.center() - vec2(0.0, space::MD), Vec2::splat(icon::HERO)), Status::Working, icon::HERO);
                icons::request_spinner_repaint(ui);
            }
            p.text(body.center() + vec2(0.0, if starting { space::LG + space::XS } else { 0.0 }), Align2::CENTER_CENTER, text_, theme::regular(text::SMALL), t.muted);
        }
    } else {
        let msg = if s.status == Status::Asleep { "Asleep. Wake it from the sidebar." } else { "No terminal." };
        p.text(body.center(), Align2::CENTER_CENTER, msg, theme::regular(text::SMALL), t.muted);
    }

    // Header.
    p.rect_filled(head, CornerRadius { nw: radius::MD, ne: radius::MD, sw: 0, se: 0 }, t.mantle);
    p.hline(head.x_range(), head.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));
    p.rect_filled(Rect::from_min_size(head.min, vec2(size::STRIPE, head.height())), CornerRadius { nw: radius::MD, ..Default::default() }, accent);
    let cy = head.center().y;
    // The header itself takes clicks (activate, right-click menu); registered before its
    // controls so they sit on top of it in hit testing.
    // In Free mode a docked pane's header drags onto another pane's edge to split it.
    let splittable = o.mode == Some(LayoutMode::Free) && !o.floating && !o.torn && !o.mini;
    let head_sense = if splittable { Sense::click_and_drag() } else { Sense::click() };
    let head_resp = ui.interact(head.shrink2(vec2(space::SM, 0.0)), Id::new(("pane-head", s.id.clone(), o.torn, o.mini)), head_sense);
    named(&head_resp, WidgetType::Other, format!("Pane: {}", s.name));
    let head_resp = if splittable { head_resp.on_hover_cursor(CursorIcon::Grab) } else { head_resp };
    // Right-hand controls first (they keep their size; the name elides).
    let mut x = head.max.x - space::XS;
    let ctl = |ui: &mut Ui, name: &str, on: bool, tip: &str, x: &mut f32| -> bool {
        *x -= size::CONTROL_SM;
        let b = Rect::from_min_size(pos2(*x, cy - size::CONTROL_SM / 2.0), Vec2::splat(size::CONTROL_SM));
        *x -= space::XXS;
        widgets::icon_button_at(ui, b, Id::new(("pane-ctl", s.id.clone(), name.to_string(), o.torn)), name, on, tip).clicked()
    };
    if !o.mini {
        if ctl(ui, "more", false, "More  (Ctrl+.)", &mut x) {
            actions.push(Action::OpenMenu(Menu::Pane { session: s.id.clone(), at: pos2(x + size::CONTROL_SM, head.max.y + space::XS) }));
        }
        if o.mode == Some(LayoutMode::Free) || o.torn {
            if !o.torn && ctl(ui, "tear", false, "Tear off into its own window", &mut x) {
                actions.push(Action::Tool("pane_tear_off", json!({"session_id": s.id, "group": scope.key()})));
            }
            if o.floating || o.torn {
                if ctl(ui, "dock", false, if o.torn { "Dock back into the main window" } else { "Dock into the split tree" }, &mut x) {
                    actions.push(Action::Tool("pane_dock", json!({"session_id": s.id, "group": scope.key()})));
                }
            } else if ctl(ui, "float", false, "Float inside this window", &mut x) {
                actions.push(Action::Tool("pane_float", json!({"session_id": s.id, "group": scope.key()})));
            }
        }
        if o.mode == Some(LayoutMode::Side) {
            let tip = if o.pinned { "Pinned to Side by side. Click to unpin." } else { "Pin to keep it in Side by side" };
            if ctl(ui, "pin", o.pinned, tip, &mut x) {
                let id = if o.pinned { "layout_unpin" } else { "layout_pin" };
                actions.push(Action::Tool(id, json!({"session_id": s.id, "group": scope.key()})));
            }
        }
        // `{ }`: open the folder in the default editor; the full list is in the ⋯ menu.
        if let Some(o) = s.open_with.iter().find(|o| o.is_default).or(s.open_with.first()) {
            let tip = format!("Open in {} ({})\n{}", o.label, o.program, o.command);
            if ctl(ui, "braces", false, &tip, &mut x) {
                actions.push(Action::Tool("open_with", json!({"session_id": s.id, "entry": o.entry_id})));
            }
        }
        if let Some(run) = s.default_run() {
            // The Run split button: ▶ label | ▾.
            let label = run.label.clone();
            let lw = widgets::text_width(ui, &label, theme::medium(text::CAPTION));
            let main_w = icon::XS + space::XS + lw + space::SM + space::XS + space::XXS;
            let caret_w = icon::XS + space::SM;
            x -= space::XS;
            let whole = Rect::from_min_max(pos2(x - main_w - caret_w, cy - size::CONTROL_SM / 2.0), pos2(x, cy + size::CONTROL_SM / 2.0));
            x = whole.min.x - space::XS;
            p.rect_stroke(whole, CornerRadius::same(radius::SM), Stroke::new(size::HAIRLINE, t.surface0), StrokeKind::Inside);
            let main = Rect::from_min_size(whole.min, vec2(main_w, whole.height()));
            let caret = Rect::from_min_max(pos2(main.max.x, whole.min.y), whole.max);
            p.vline(caret.min.x, whole.y_range(), Stroke::new(size::HAIRLINE, t.surface0));
            let rm = ui.interact(main, Id::new(("run-main", s.id.clone(), o.torn)), Sense::click());
            let rc = ui.interact(caret, Id::new(("run-caret", s.id.clone(), o.torn)), Sense::click());
            if rm.hovered() {
                p.rect_filled(main.shrink(size::HAIRLINE), CornerRadius::same(radius::SM - 1), t.row_sel);
            }
            if rc.hovered() {
                p.rect_filled(caret.shrink(size::HAIRLINE), CornerRadius::same(radius::SM - 1), t.row_sel);
            }
            let ir = Rect::from_min_size(pos2(main.min.x + space::SM - size::HAIRLINE, cy - icon::XS / 2.0), Vec2::splat(icon::XS));
            icons::paint(ui, ir, "play", icon::XS, t.green);
            p.text(pos2(ir.max.x + space::XS + size::HAIRLINE, cy), Align2::LEFT_CENTER, &label, theme::medium(text::CAPTION), t.fg2);
            icons::paint(ui, caret, "chevD", icon::XS, t.fg2);
            named(&rm, WidgetType::Button, format!("Run {label} · {}", s.name));
            named(&rc, WidgetType::Button, "All run commands");
            if rm.on_hover_text("Run the default command  F5").clicked() {
                actions.push(Action::Tool("run_start", json!({"session_id": s.id})));
            }
            if rc.on_hover_text("All run commands").clicked() {
                actions.push(Action::OpenMenu(Menu::Runs { session: s.id.clone(), at: pos2(caret.max.x, head.max.y + space::XS) }));
            }
        }
    }
    // Status pill (right-aligned before the controls).
    let status = if queued { Status::Starting } else { s.status };
    let label = if queued { "Queued".to_string() } else { s.status_text() };
    let pill_w = {
        let quiet = matches!(status, Status::Idle | Status::Asleep);
        icon::SM + space::XS + widgets::text_width(ui, &label, theme::medium(text::CAPTION)) + space::SM + if quiet { space::XXS } else { space::XS + space::XXS }
    };
    // In a narrow pane the pill keeps only its glyph (the word is in its tooltip), so the name
    // keeps room to read.
    let compact = x - space::XS - pill_w - (head.min.x + size::STRIPE + space::SM) < size::PANE_NAME_MIN_W + space::XXL;
    let pill_w = if compact { size::CHIP } else { pill_w };
    let pill = Rect::from_min_size(pos2(x - space::XS - pill_w, cy - size::CHIP / 2.0), vec2(pill_w, size::CHIP));
    if compact {
        let r = ui.scope_builder(UiBuilder::new().max_rect(pill), |ui| icons::status(ui, status, icon::SM)).inner;
        let _ = r.on_hover_text(label.clone());
    } else {
        ui.scope_builder(UiBuilder::new().max_rect(pill), |ui| widgets::status_pill(ui, status, &label));
    }
    let right_limit = pill.min.x - space::SM;

    // Left: stripe, [badge] name, Preview, branch chip / group name.
    let mut lx = head.min.x + size::STRIPE + space::SM;
    let badge = match s.kind {
        Kind::Ssh => Some("SSH".to_string()),
        Kind::Wsl => Some("WSL".to_string()),
        Kind::Local if !s.is_claude() => s.command.split_whitespace().next().map(str::to_string),
        Kind::Local => None,
    };
    if let Some(b) = &badge {
        let br = kind_badge(ui, pos2(lx, cy), b);
        lx = br.max.x + space::XS + space::XXS;
    }
    let chip_room = if s.branch.is_some() && !o.mini { size::BRANCH_MAX_W + space::XXL } else { 0.0 };
    let name_max = (right_limit - lx - chip_room - if o.preview { space::XXL + space::LG } else { 0.0 }).max(size::PANE_NAME_MIN_W);
    let nr = widgets::paint_text(ui, pos2(lx, cy), Align2::LEFT_CENTER, &s.name, semibold(text::BODY), t.fg, name_max);
    lx = nr.max.x + space::SM;
    if o.preview {
        let pr = p.text(pos2(lx, cy), Align2::LEFT_CENTER, "Preview", theme::regular(text::CAPTION), t.muted);
        lx = pr.max.x + space::SM;
    }
    if o.mini {
        if let Some(g) = s.group.as_deref().and_then(|g| model.group(g)) {
            widgets::paint_text(ui, pos2(lx, cy), Align2::LEFT_CENTER, &g.name, theme::medium(text::TINY), t.faint, (right_limit - lx).max(0.0));
        }
    } else if s.branch.is_some() {
        let room = (right_limit - lx).max(0.0);
        if room > space::XXL + space::XL {
            let chip_rect = Rect::from_min_size(pos2(lx, cy - size::CHIP / 2.0), vec2(room, size::CHIP));
            let resp = ui
                .scope_builder(UiBuilder::new().max_rect(chip_rect).layout(egui::Layout::left_to_right(egui::Align::Center)), |ui| {
                    super::git::branch_chip(ui, s, room, !o.torn)
                })
                .inner;
            if resp.clicked() {
                actions.push(Action::Tool("session_focus", json!({"session_id": s.id})));
                actions.push(Action::Tool("inspector_show", json!({"tab": "git"})));
            }
        }
    }

    // Border: the active pane gets its accent ring.
    if o.active && !o.mini {
        p.rect_stroke(rect, r, Stroke::new(size::RING, accent), StrokeKind::Inside);
    } else {
        p.rect_stroke(rect, r, Stroke::new(size::HAIRLINE, if o.floating { t.surface2 } else { t.surface0 }), StrokeKind::Inside);
    }

    // Clicks: a mini pane focuses (and switches to Focus); a pane click activates.
    if o.mini && (clicked || head_resp.clicked()) {
        actions.push(Action::Tool("session_focus", json!({"session_id": s.id})));
        actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": "focus"})));
        actions.push(Action::Toast(format!("Focused {}. Overview is one click away in the group bar.", s.name)));
    } else if (clicked || head_resp.clicked()) && !o.active && !o.torn {
        actions.push(Action::Tool("session_focus", json!({"session_id": s.id})));
    }
    if head_resp.secondary_clicked() && !o.mini {
        let at = ui.ctx().pointer_latest_pos().unwrap_or(head.right_bottom());
        actions.push(Action::OpenMenu(Menu::Pane { session: s.id.clone(), at }));
    }
    head_resp
}

/// Which edge of a pane a split drop would take, for a pointer at `p` inside `r`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    Left,
    Right,
    Top,
    Bottom,
}

impl Edge {
    /// The nearest edge, when the pointer is within `size::DROP_EDGE` of it (the middle of a
    /// pane is not a drop zone).
    pub fn at(r: Rect, p: egui::Pos2) -> Option<Edge> {
        if !r.contains(p) || r.width() <= 0.0 || r.height() <= 0.0 {
            return None;
        }
        let dx = (p.x - r.min.x) / r.width();
        let dy = (p.y - r.min.y) / r.height();
        let candidates = [(dx, Edge::Left), (1.0 - dx, Edge::Right), (dy, Edge::Top), (1.0 - dy, Edge::Bottom)];
        let (d, e) = candidates.into_iter().min_by(|a, b| a.0.total_cmp(&b.0))?;
        (d <= size::DROP_EDGE).then_some(e)
    }

    /// The part of `r` the new pane would take.
    pub fn zone(self, r: Rect) -> Rect {
        let (w, h) = (r.width() / 2.0, r.height() / 2.0);
        match self {
            Edge::Left => Rect::from_min_size(r.min, vec2(w, r.height())),
            Edge::Right => Rect::from_min_size(pos2(r.min.x + w, r.min.y), vec2(w, r.height())),
            Edge::Top => Rect::from_min_size(r.min, vec2(r.width(), h)),
            Edge::Bottom => Rect::from_min_size(pos2(r.min.x, r.min.y + h), vec2(r.width(), h)),
        }
    }

    /// The split this drop makes: direction, and whether the dropped pane goes after the target.
    pub fn split(self) -> (SplitDir, bool) {
        match self {
            Edge::Left => (SplitDir::Row, false),
            Edge::Right => (SplitDir::Row, true),
            Edge::Top => (SplitDir::Col, false),
            Edge::Bottom => (SplitDir::Col, true),
        }
    }
}

/// The tree after dropping `src` on `target`'s `edge`: `src` leaves its place and splits the
/// target. `None` when nothing would change.
pub fn split_drop(tree: &SplitNode, src: &str, target: &str, edge: Edge) -> Option<SplitNode> {
    if src == target || !tree.contains(target) {
        return None;
    }
    let rest = tree.without(src)?;
    let (dir, after) = edge.split();
    Some(rest.split_leaf(target, src, dir, after))
}

/// Free mode, while a pane header is dragged: the drop zones of the pane under the pointer, and
/// the split on release (`layout_set { free }`).
fn split_drag(ui: &mut Ui, app: &mut CsmApp, layout: &Layout, planned: &[(SessionId, Rect, PaneOpts)], scope: &Scope, actions: &mut Vec<Action>) {
    let Some(src) = app.ui.drags.pane.clone() else { return };
    let Some(p) = ui.ctx().pointer_latest_pos() else { return };
    let t = Tokens::get(ui.ctx());
    ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    let target = planned.iter().find(|(id, r, _)| id != &src && r.contains(p));
    let painter = ui.painter().clone();
    let mut drop = None;
    if let Some((tid, r, _)) = target {
        // Every edge of the pane under the pointer is a zone; the one it would take is lit.
        let lit = Edge::at(*r, p);
        for e in [Edge::Left, Edge::Right, Edge::Top, Edge::Bottom] {
            let z = e.zone(*r).shrink(space::XS);
            if Some(e) == lit {
                painter.rect_filled(z, CornerRadius::same(radius::MD), Tokens::tint(t.blue, alpha::DROP * 2.0));
                painter.rect_stroke(z, CornerRadius::same(radius::MD), Stroke::new(size::RING, t.blue), StrokeKind::Inside);
            } else {
                painter.rect_stroke(z, CornerRadius::same(radius::MD), Stroke::new(size::HAIRLINE, Tokens::tint(t.blue, alpha::ASLEEP)), StrokeKind::Inside);
            }
        }
        if let Some(e) = lit {
            let z = e.zone(*r);
            let label = "Split here";
            let lw = widgets::text_width(ui, label, theme::medium(text::CAPTION)) + space::LG;
            let lr = Rect::from_center_size(z.center(), vec2(lw, size::CHIP));
            painter.rect_filled(lr, CornerRadius::same(radius::PILL), t.blue);
            painter.text(lr.center(), Align2::CENTER_CENTER, label, theme::medium(text::CAPTION), t.on_accent);
            drop = Some((tid.clone(), e));
        }
    }
    // The dragged pane's name follows the pointer.
    if let Some(s) = app.model.session(&src) {
        let w = widgets::text_width(ui, &s.name, theme::medium(text::BODY)) + space::LG + size::STRIPE;
        let gr = Rect::from_min_size(p + vec2(space::MD, space::XS), vec2(w, size::CONTROL_SM));
        painter.rect(gr, CornerRadius::same(radius::MD), t.pop, Stroke::new(size::HAIRLINE, t.surface1), StrokeKind::Inside);
        painter.rect_filled(Rect::from_min_size(gr.min + vec2(space::SM, space::SM - space::XXS), vec2(size::STRIPE, gr.height() - space::LG + space::XS)), CornerRadius::same(radius::SM / 2), t.accent(s.accent));
        painter.text(pos2(gr.min.x + space::SM + size::STRIPE + space::SM, gr.center().y), Align2::LEFT_CENTER, &s.name, theme::medium(text::BODY), t.fg);
    }
    if ui.input(|i| i.pointer.any_released()) {
        app.ui.drags.pane = None;
        let tree = layout.tree.clone().or_else(|| {
            let ids: Vec<SessionId> = planned.iter().map(|(id, ..)| id.clone()).collect();
            SplitNode::row(&ids)
        });
        if let (Some((tid, e)), Some(tree)) = (drop, tree)
            && let Some(next) = split_drop(&tree, &src, &tid, e)
        {
            let mut free = layout.free();
            free.tree = Some(next);
            actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "free": free.to_value()})));
        }
    }
}

/// Everything a torn-off window needs, refreshed by the main frame. Shared with the deferred
/// viewport callbacks (which must be `Send + Sync + 'static`).
#[derive(Default)]
pub struct TornState {
    pub model: Arc<UiModel>,
    pub sources: HashMap<SessionId, Arc<dyn TerminalSource>>,
}

/// Torn-off panes and groups as deferred viewports: each renders and presents in its own pass,
/// not inside the main frame (the spike's immediate viewports did, costing a vsync wait each).
pub fn torn_windows(ctx: &egui::Context, app: &mut CsmApp) {
    let model = app.model.clone();
    // Panes torn off from any scope's layout; groups torn off as a whole.
    let mut panes: Vec<(SessionId, String)> = Vec::new();
    for (key, l) in &model.layouts {
        for id in &l.torn {
            if model.session(id).is_some_and(|s| s.status.is_live()) {
                panes.push((id.clone(), key.clone()));
            }
        }
    }
    let groups: Vec<String> = model.groups.iter().filter(|g| g.torn_off).map(|g| g.id.clone()).collect();
    if panes.is_empty() && groups.is_empty() {
        return;
    }
    let state = torn_state(ctx);
    {
        let mut st = lock(&state);
        st.model = model.clone();
        st.sources = model.sessions.iter().filter_map(|s| app.backend.terminal(&s.id).map(|t| (s.id.clone(), t))).collect();
    }
    for (id, key) in panes {
        let name = model.session(&id).map_or_else(String::new, |s| s.name.clone());
        let vid = ViewportId::from_hash_of(("torn-pane", id.clone()));
        let builder = ViewportBuilder::default().with_title(format!("{name} · CodeShellManager")).with_inner_size(size::TEAR_OFF);
        let (host, actions, state) = (app.host.clone(), app.remote_actions.clone(), state.clone());
        ctx.show_viewport_deferred(vid, builder, move |ui, _class| {
            let st = lock(&state);
            let model = st.model.clone();
            let src = st.sources.get(&id).cloned();
            drop(st);
            let mut out = Vec::new();
            let t = Tokens::get(ui.ctx());
            let rect = ui.max_rect();
            ui.painter().rect_filled(rect, 0.0, t.crust);
            if let Some(s) = model.session(&id) {
                let o = PaneOpts { torn: true, active: true, ..PaneOpts::default() };
                pane(ui, &mut lock(&host), &model, s, src.as_deref(), rect.shrink(space::PANE_GAP), o, &Scope::from_key(&key), &mut out);
            }
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                out.push(Action::Tool("pane_dock", json!({"session_id": id, "group": key})));
            }
            if !out.is_empty() {
                lock(&actions).extend(out);
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
        });
    }
    for gid in groups {
        let name = model.group(&gid).map_or_else(String::new, |g| g.name.clone());
        let vid = ViewportId::from_hash_of(("torn-group", gid.clone()));
        let builder = ViewportBuilder::default().with_title(format!("{name} · CodeShellManager")).with_inner_size(size::TEAR_OFF);
        let (host, actions, state) = (app.host.clone(), app.remote_actions.clone(), state.clone());
        ctx.show_viewport_deferred(vid, builder, move |ui, _class| {
            let st = lock(&state);
            let model = st.model.clone();
            let sources = st.sources.clone();
            drop(st);
            let mut out = Vec::new();
            group_window(ui, &mut lock(&host), &model, &sources, &gid, &mut out);
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                out.push(Action::Tool("group_dock", json!({"group_id": gid})));
            }
            if !out.is_empty() {
                lock(&actions).extend(out);
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
        });
    }
}

fn torn_state(ctx: &egui::Context) -> Arc<Mutex<TornState>> {
    ctx.data_mut(|d| d.get_temp_mut_or_insert_with(Id::new("csm-torn-state"), || Arc::new(Mutex::new(TornState::default()))).clone())
}

/// A group in its own window: a bar with Dock back, and its live sessions side by side.
fn group_window(ui: &mut Ui, host: &mut PaneHost, model: &UiModel, sources: &HashMap<SessionId, Arc<dyn TerminalSource>>, gid: &str, out: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    ui.painter().rect_filled(rect, 0.0, t.bg);
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), size::GROUP_BAR));
    ui.painter().hline(bar.x_range(), bar.max.y, Stroke::new(size::HAIRLINE, t.surface0));
    let scope = Scope::Group(gid.to_string());
    let live: Vec<&SessionView> = model.scope_sessions(&scope).into_iter().filter(|s| s.status.is_live()).collect();
    let nr = ui.painter().text(pos2(bar.min.x + space::MD, bar.center().y), Align2::LEFT_CENTER, model.scope_name(&scope), semibold(text::EMPH), t.fg);
    ui.painter().text(pos2(nr.max.x + space::MD, bar.center().y), Align2::LEFT_CENTER, format!("{} live · own window, own layout", live.len()), theme::regular(text::CAPTION), t.muted);
    let bw = widgets::text_width(ui, "Dock back into the main window", theme::medium(text::BODY)) + icon::LG + space::ROW_X * 2.0 + space::XS + space::XXS;
    let br = Rect::from_min_size(pos2(bar.max.x - space::SM - bw, bar.center().y - size::CONTROL / 2.0), vec2(bw, size::CONTROL));
    if ui.scope_builder(UiBuilder::new().max_rect(br), |ui| widgets::button(ui, ButtonKind::Ghost, Some("dock"), "Dock back into the main window", None)).inner.clicked() {
        out.push(Action::Tool("group_dock", json!({"group_id": gid})));
    }
    let area = Rect::from_min_max(pos2(rect.min.x, bar.max.y), rect.max);
    ui.painter().rect_filled(area, 0.0, t.crust);
    let inner = area.shrink(space::PANE_GAP);
    let (cols, rows, _) = side_tiling(live.len());
    for (s, r) in live.iter().zip(grid_cells(inner, cols, rows, space::PANE_GAP)) {
        let o = PaneOpts { torn: true, active: model.active.as_deref() == Some(s.id.as_str()), ..PaneOpts::default() };
        pane(ui, host, model, s, sources.get(&s.id).map(|a| a.as_ref()), r, o, &scope, out);
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn side_tiling_follows_the_mock() {
        assert_eq!(side_tiling(1), (1, 1, false));
        assert_eq!(side_tiling(3), (2, 2, true));
        assert_eq!(side_tiling(5), (3, 2, true));
        assert_eq!(side_tiling(9), (3, 3, false));
    }

    #[test]
    fn tree_layout_gives_dividers_between_children() {
        let tree = SplitNode::row(&["a".into(), "b".into(), "c".into()]).unwrap();
        let mut leaves = Vec::new();
        let mut divs = Vec::new();
        layout_tree(&tree, Rect::from_min_size(pos2(0.0, 0.0), vec2(306.0, 100.0)), 6.0, &mut Vec::new(), &mut leaves, &mut divs);
        assert_eq!(leaves.len(), 3);
        assert_eq!(divs.len(), 2);
        assert!((leaves[0].1.width() - 98.0).abs() < 0.01);
        assert!((divs[0].rect.min.x - 98.0).abs() < 0.01);
    }

    #[test]
    fn a_header_dropped_on_an_edge_splits_that_pane() {
        let r = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 100.0));
        assert_eq!(Edge::at(r, pos2(10.0, 50.0)), Some(Edge::Left));
        assert_eq!(Edge::at(r, pos2(190.0, 50.0)), Some(Edge::Right));
        assert_eq!(Edge::at(r, pos2(100.0, 95.0)), Some(Edge::Bottom));
        assert_eq!(Edge::at(r, pos2(100.0, 50.0)), None, "the middle is not a zone");
        assert_eq!(Edge::at(r, pos2(300.0, 50.0)), None);
        let tree = SplitNode::row(&["a".into(), "b".into(), "c".into()]).unwrap();
        // c onto b's bottom: b and c stacked, a beside them.
        let next = split_drop(&tree, "c", "b", Edge::Bottom).unwrap();
        let SplitNode::Split { dir, children, .. } = &next else { panic!() };
        assert_eq!(*dir, SplitDir::Row);
        assert_eq!(children.len(), 2);
        assert_eq!(children[1], SplitNode::Split { dir: SplitDir::Col, ratios: vec![0.5, 0.5], children: vec![SplitNode::leaf("b"), SplitNode::leaf("c")] });
        assert_eq!(next.sessions(), vec!["a", "b", "c"]);
        assert!(split_drop(&tree, "a", "a", Edge::Left).is_none(), "onto itself changes nothing");
        assert_eq!(split_drop(&tree, "a", "c", Edge::Right).unwrap().sessions(), vec!["b", "c", "a"]);
    }

    #[test]
    fn set_pair_keeps_the_pair_total() {
        let mut tree = SplitNode::row(&["a".into(), "b".into()]).unwrap();
        set_pair(&mut tree, &[], 0, 0.7);
        let SplitNode::Split { ratios, .. } = tree else { panic!() };
        assert!((ratios[0] - 0.7).abs() < 1e-5 && (ratios[1] - 0.3).abs() < 1e-5);
    }
}
