//! Add-on screens in the window (docs/addons.md, "Screens"): the view strip above the panes
//! (`Sessions | <screen> | ...`), the frame CSM draws around every screen, the placeholder states,
//! the pop-out window, and the draw of a validated view tree ([`tree`]).
//!
//! Trust. A screen is a plugin's picture: it sits inside a frame CSM draws (a header naming the
//! add-on, an "Add-on" badge and a border) and is clipped to it, so it cannot cover the frame. It
//! draws in the main panel, below every menu, dialog, consent card and toast (those are
//! `Order::Foreground` areas). What a button does is a tool call through the normal dispatch; the
//! core tools that need a question (`session_send_text`, `open_url`, `open_with`) get a confirm
//! CSM draws, with the add-on's name, the tool and the exact text and target.
//!
//! Nothing here blocks: the tree is an `Arc` the engine already validated, events are queued to
//! the plugin without waiting, and the text a person types lives in [`ScreenUi`] so a slow plugin
//! never eats keystrokes.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use csm_core::addons::screen::{FileEntry, FileRow, Kind, ScreenEvent, ScreenTree, file_rows, reveal};
use csm_engine::{ScreenInfo, ScreenPhase};
use egui::{Align2, CornerRadius, Id, Rect, Sense, Stroke, Ui, UiBuilder, ViewportBuilder, ViewportId, WidgetInfo, WidgetType, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{Action, CsmApp};
use crate::icons;
use crate::terminal::lock;
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

pub mod hl;
mod md;
mod rows;
mod view;
pub mod tree;

/// What a drawn tree asks for; turned into [`Action`]s after the draw.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Out {
    /// An event for the plugin (a click, a selection, a submit).
    Event(ScreenEvent),
    /// A tool a button is bound to. `confirm`: CSM asks first.
    Tool { tool: String, params: Value, confirm: bool },
}

/// The text of one `text_input` while it is being edited.
pub(super) struct Draft {
    pub text: String,
    /// The render revision whose `reset: true` this draft already took.
    pub reset_rev: u64,
    /// When a debounced `change` is due.
    pub change_at: Option<f64>,
}

/// A click that has had no new render yet.
struct Await {
    rev: u64,
    at: f64,
}

/// UI state of the add-on screens: the selected view, drafts, split ratios and folds. Keyed by
/// (instance, node key), so a re-render of the tree never loses it.
#[derive(Default)]
pub struct ScreenUi {
    /// The instance shown in the view strip; `None` is the sessions.
    pub active: Option<String>,
    pub(super) drafts: HashMap<(String, String), Draft>,
    pub(super) splits: HashMap<(String, String), f32>,
    pub(super) sections: HashMap<(String, String), bool>,
    pub(super) folds: HashMap<(String, String), HashSet<String>>,
    pub(super) fold_gen: u64,
    row_cache: HashMap<(String, String), RowCache>,
    /// The line selections, scroll positions, thread heights and models of code and diff views.
    pub(super) view: view::ViewState,
    /// The highlight worker (frames ask it; it never runs on the UI thread).
    pub hl: hl::Highlighter,
    awaiting: HashMap<String, Await>,
    /// Events sent to plugins, most recent last (capped), for tests and diagnostics.
    sent: Vec<(String, ScreenEvent)>,
}

/// The rows built for a `file_list`: the render and fold generation they are for, the view, the rows.
type RowCache = (u64, u64, bool, Arc<Vec<FileRow>>);
/// A button in a state bar or placeholder: label, look, what it does.
type BarButton = (&'static str, ButtonKind, Action);

/// How many sent events are remembered.
const SENT_MAX: usize = 256;
/// A click with no new render for this long shows a spinner in the frame.
const AWAIT_SECS: f64 = 0.3;

impl ScreenUi {
    /// The folder rows of a `file_list`, rebuilt only when the files or the folds changed.
    pub(super) fn rows(&mut self, id: &(String, String), rev: u64, tree: bool, files: &[FileEntry], folds: &HashSet<String>) -> Arc<Vec<FileRow>> {
        if let Some((r, g, t, rows)) = self.row_cache.get(id)
            && *r == rev
            && *g == self.fold_gen
            && *t == tree
        {
            return rows.clone();
        }
        let rows = Arc::new(file_rows(files, tree, folds));
        self.row_cache.insert(id.clone(), (rev, self.fold_gen, tree, rows.clone()));
        rows
    }

    /// The text being typed in a `text_input` (what a submit would send).
    pub fn draft(&self, instance: &str, key: &str) -> Option<&str> {
        self.drafts.get(&(instance.to_owned(), key.to_owned())).map(|d| d.text.as_str())
    }

    /// The line range selected in a code or diff view: `(side, first, last)`.
    pub fn selection(&self, instance: &str, key: &str) -> Option<(csm_core::diff::Side, u32, u32)> {
        self.view.sels.get(&(instance.to_owned(), key.to_owned())).map(|s| {
            let (a, b) = s.range();
            (s.side, a, b)
        })
    }

    /// Events sent to plugins, most recent last: (instance, event).
    pub fn sent(&self) -> &[(String, ScreenEvent)] {
        &self.sent
    }

    /// Forgets the state of instances that are gone, and a selection that points at one.
    pub fn sync(&mut self, infos: &[ScreenInfo]) {
        let alive = |i: &str| infos.iter().any(|x| x.instance == i);
        self.drafts.retain(|(i, _), _| alive(i));
        self.splits.retain(|(i, _), _| alive(i));
        self.sections.retain(|(i, _), _| alive(i));
        self.folds.retain(|(i, _), _| alive(i));
        self.row_cache.retain(|(i, _), _| alive(i));
        self.view.sync(&alive);
        self.awaiting.retain(|i, _| alive(i));
        if self.active.as_deref().is_some_and(|a| !alive(a)) {
            self.active = None;
        }
    }

    pub(super) fn record(&mut self, instance: &str, e: &ScreenEvent, rev: u64, now: f64) {
        if self.sent.len() >= SENT_MAX {
            self.sent.remove(0);
        }
        self.sent.push((instance.to_owned(), e.clone()));
        self.awaiting.insert(instance.to_owned(), Await { rev, at: now });
    }
}

/// What the tree's draw needs.
pub(super) struct Cx<'a> {
    pub info: &'a ScreenInfo,
    pub rev: u64,
    pub st: &'a mut ScreenUi,
    pub out: &'a mut Vec<Out>,
    pub t: Tokens,
}

impl Cx<'_> {
    pub(super) fn event(&mut self, key: &str, kind: csm_core::addons::screen::EventKind, value: Value) {
        self.out.push(Out::Event(ScreenEvent::new(key, kind, value)));
    }
}

fn named(resp: &egui::Response, ty: WidgetType, label: impl Into<String>) {
    let l = label.into();
    resp.widget_info(move || WidgetInfo::labeled(ty, true, l.clone()));
}

/// The instance drawn in the work area now: the selected one, if it exists and is not in its own window.
pub fn shown<'a>(infos: &'a [ScreenInfo], ui: &ScreenUi) -> Option<&'a ScreenInfo> {
    infos.iter().find(|i| Some(i.instance.as_str()) == ui.active.as_deref() && !i.popped_out)
}

/// Once per frame: fetches the open screens and drops state of the ones that are gone.
pub fn tick(app: &mut CsmApp) {
    app.ui.screen_infos = app.backend.screens();
    let infos = app.ui.screen_infos.clone();
    lock(&app.ui.screens).sync(&infos);
}

fn tab_title(i: &ScreenInfo) -> String {
    i.title.clone()
}

/// The view strip: `Sessions | <screen> ...` (a tab per open screen, with its add-on mark and a
/// close button). A screen in its own window keeps its tab; clicking that tab docks it back.
pub fn strip(ui: &mut Ui, rect: Rect, infos: &[ScreenInfo], active: Option<&str>, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(rect, 0.0, t.mantle);
    ui.painter().hline(rect.x_range(), rect.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));
    let cy = rect.center().y;
    let mut x = rect.min.x + space::SM;
    let tab_h = size::CONTROL;
    // The tabs in order (Sessions, then each open screen): the arrow keys move along them.
    let order: Vec<Id> = std::iter::once(Id::new("view-sessions")).chain(infos.iter().map(|i| Id::new(("view-screen", &i.instance)))).collect();
    for id in &order {
        if crate::list_keys::move_from(ui.ctx(), *id, &order, crate::list_keys::Axis::Horizontal).is_some() {
            break;
        }
    }
    let tab = |ui: &mut Ui, x: &mut f32, id: Id, label: &str, ic: Option<&str>, mark: bool, close: bool, on: bool, popped: bool, name: String| -> (egui::Response, Option<egui::Response>) {
        let font = if on { semibold(text::BODY) } else { theme::medium(text::BODY) };
        let lw = widgets::text_width(ui, label, font.clone()).min(size::VIEW_TAB_MAX_W - 80.0);
        let mark_w = if mark { widgets::text_width(ui, "Add-on", theme::medium(text::TINY)) + space::MD } else { 0.0 };
        let w = space::SM + ic.map_or(0.0, |_| icon::MD + space::XS) + lw + if mark { space::SM + mark_w } else { 0.0 } + if close { space::XS + icon::LG } else { 0.0 } + space::SM;
        let r = Rect::from_min_size(pos2(*x, cy - tab_h / 2.0), vec2(w, tab_h));
        *x += w + space::XXS;
        let resp = ui.interact(r, id, Sense::click());
        if on {
            ui.painter().rect_filled(r, CornerRadius::same(radius::MD), t.row_sel);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, CornerRadius::same(radius::MD), t.row_hover);
        }
        let fg = if on { t.fg } else { t.muted };
        let mut px = r.min.x + space::SM;
        if let Some(n) = ic {
            icons::paint(ui, Rect::from_min_size(pos2(px, cy - icon::MD / 2.0), vec2(icon::MD, icon::MD)), n, icon::MD, fg);
            px += icon::MD + space::XS;
        }
        let tr = widgets::paint_text(ui, pos2(px, cy), Align2::LEFT_CENTER, label, font, fg, lw);
        px = tr.max.x;
        if mark {
            let mr = Rect::from_min_size(pos2(px + space::SM, cy - size::BADGE / 2.0), vec2(mark_w, size::BADGE));
            ui.painter().rect_filled(mr, CornerRadius::same(radius::PILL), Tokens::tint(t.mauve, alpha::PILL));
            ui.painter().text(mr.center(), Align2::CENTER_CENTER, "Add-on", theme::medium(text::TINY), t.mauve);
        }
        let mut close_resp = None;
        if close {
            let cr = Rect::from_center_size(pos2(r.max.x - space::SM - icon::LG / 2.0, cy), vec2(icon::LG + space::XS, icon::LG + space::XS));
            let c = ui.interact(cr, id.with("close"), Sense::click());
            if c.hovered() {
                ui.painter().rect_filled(cr, CornerRadius::same(radius::SM), t.hover);
            }
            icons::paint(ui, cr, "x", icon::SM, if c.hovered() { t.fg } else { t.muted });
            named(&c, WidgetType::Button, format!("Close {label}"));
            close_resp = Some(c.on_hover_cursor(egui::CursorIcon::PointingHand));
        }
        let tip = if popped { format!("{label} is in its own window. Click to dock it back.") } else { label.to_owned() };
        resp.widget_info({
            let n = name;
            move || WidgetInfo::selected(WidgetType::RadioButton, true, on, n.clone())
        });
        if resp.has_focus() {
            widgets::focus_ring(ui, r, radius::MD);
        }
        (resp.on_hover_cursor(egui::CursorIcon::PointingHand).on_hover_text(tip), close_resp)
    };
    let (sessions, _) = tab(ui, &mut x, Id::new("view-sessions"), "Sessions", None, false, false, active.is_none(), false, "View: Sessions".into());
    if sessions.clicked() {
        actions.push(Action::ScreenFocus(None));
    }
    for i in infos {
        let on = active == Some(i.instance.as_str()) && !i.popped_out;
        let ic = Some(i.icon.as_str()).filter(|n| !n.is_empty() && icons::exists(n));
        let title = tab_title(i);
        let (resp, close) = tab(ui, &mut x, Id::new(("view-screen", &i.instance)), &title, ic, true, true, on, i.popped_out, format!("View: {title}"));
        if close.is_some_and(|c| c.clicked()) {
            actions.push(Action::Tool("screen_close", json!({"instance": i.instance})));
        } else if resp.clicked() {
            actions.push(if i.popped_out { Action::Tool("screen_dock", json!({"instance": i.instance})) } else { Action::ScreenFocus(Some(i.instance.clone())) });
        }
    }
}

/// Draws one screen into `rect`: the frame (header and border), then the tree or a placeholder.
/// `in_window`: the screen is in its own window, so the header offers Dock instead of Pop out.
pub fn show(ui: &mut Ui, rect: Rect, info: &ScreenInfo, tree: Option<&Arc<ScreenTree>>, st: &mut ScreenUi, in_window: bool, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let now = ui.input(|i| i.time);
    // Take the whole rect, so a window that sizes itself to its content (an embedded viewport) is as big as the frame.
    ui.allocate_rect(rect, Sense::hover());
    ui.painter().rect_filled(rect, 0.0, t.bg);
    // The header: the add-on's name and id, an "Add-on" badge, and the window controls. Drawn by
    // CSM and clipped out of the plugin's area, so a screen can neither cover nor imitate it.
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), size::PANE_HEADER));
    ui.painter().rect_filled(head, 0.0, t.mantle);
    ui.painter().hline(head.x_range(), head.max.y, Stroke::new(size::HAIRLINE, t.surface0));
    let frame_resp = ui.interact(head, Id::new(("screen-frame", &info.instance)), Sense::hover());
    named(&frame_resp, WidgetType::Label, format!("Add-on screen: {} ({})", info.title, info.addon_name));
    let cy = head.center().y;
    let mut x = head.min.x + space::MD;
    if !info.icon.is_empty() && icons::exists(&info.icon) {
        icons::paint(ui, Rect::from_min_size(pos2(x, cy - icon::LG / 2.0), vec2(icon::LG, icon::LG)), &info.icon, icon::LG, t.fg2);
        x += icon::LG + space::SM;
    }
    let tr = ui.painter().text(pos2(x, cy), Align2::LEFT_CENTER, &info.title, semibold(text::BODY), t.fg);
    let mw = widgets::text_width(ui, "Add-on", theme::medium(text::TINY)) + space::MD;
    let mr = Rect::from_min_size(pos2(tr.max.x + space::SM, cy - size::BADGE / 2.0), vec2(mw, size::BADGE));
    ui.painter().rect_filled(mr, CornerRadius::same(radius::PILL), Tokens::tint(t.mauve, alpha::PILL));
    ui.painter().text(mr.center(), Align2::CENTER_CENTER, "Add-on", theme::medium(text::TINY), t.mauve);
    let by = format!("{} \u{b7} {}", reveal_line(&info.addon_name), info.addon);
    let mut right = head.max.x - space::SM;
    let (tool, tip, ic) = if in_window { ("screen_dock", "Dock back into the main window", "dock") } else { ("screen_pop_out", "Pop out into its own window", "tear") };
    let br = Rect::from_center_size(pos2(right - size::CONTROL_SM / 2.0, cy), vec2(size::CONTROL_SM, size::CONTROL_SM));
    right = br.min.x - space::SM;
    if widgets::icon_button_at(ui, br, Id::new(("screen-pop", &info.instance)), ic, false, tip).clicked() {
        actions.push(Action::Tool(tool, json!({"instance": info.instance})));
    }
    let awaiting = st.awaiting.get(&info.instance).is_some_and(|a| a.rev == info.rev && now - a.at > AWAIT_SECS);
    if st.awaiting.get(&info.instance).is_some_and(|a| a.rev != info.rev) {
        st.awaiting.remove(&info.instance);
    }
    if awaiting {
        let sr = Rect::from_center_size(pos2(right - icon::LG / 2.0, cy), vec2(icon::LG, icon::LG));
        icons::paint_status(ui, sr, crate::model::Status::Working, icon::LG);
        icons::request_spinner_repaint(ui);
        right = sr.min.x - space::SM;
    } else if let Some(a) = st.awaiting.get(&info.instance) {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64((AWAIT_SECS - (now - a.at)).max(0.01)));
    }
    widgets::paint_text(ui, pos2(mr.max.x + space::MD, cy), Align2::LEFT_CENTER, &by, theme::regular(text::CAPTION), t.muted, (right - mr.max.x - space::MD * 2.0).max(0.0));

    let mut body = Rect::from_min_max(pos2(rect.min.x, head.max.y + size::HAIRLINE), rect.max);
    // A bar under the header for what is wrong now; the last good tree stays below it.
    let mut bar: Option<(String, Vec<BarButton>)> = None;
    match info.phase {
        ScreenPhase::Stopped if tree.is_some() => {
            bar = Some((
                format!("{} is not running.", reveal_line(&info.addon_name)),
                vec![("Restart", ButtonKind::Primary, Action::Tool("plugin_restart", json!({"id": info.addon}))), ("Close", ButtonKind::Ghost, Action::Tool("screen_close", json!({"instance": info.instance})))],
            ));
        }
        _ => {
            if let (Some(e), true) = (&info.error, tree.is_some()) {
                bar = Some((format!("{} sent a view CSM could not show: {}", reveal_line(&info.addon_name), reveal_line(e)), Vec::new()));
            }
        }
    }
    if let Some((msg, buttons)) = bar {
        let h = size::CONTROL_LG + space::XS;
        let br = Rect::from_min_size(body.min, vec2(body.width(), h));
        ui.painter().rect_filled(br, 0.0, Tokens::tint(if buttons.is_empty() { t.yellow } else { t.red }, alpha::SOFT));
        ui.painter().hline(br.x_range(), br.max.y, Stroke::new(size::HAIRLINE, t.surface0));
        let mut bx = br.max.x - space::SM;
        for (label, kind, action) in buttons.into_iter().rev() {
            let w = widgets::button_width(ui, None, label, None);
            let r = Rect::from_min_size(pos2(bx - w, br.center().y - size::CONTROL / 2.0), vec2(w, size::CONTROL));
            bx = r.min.x - space::SM;
            if ui.scope_builder(UiBuilder::new().max_rect(r), |ui| widgets::button(ui, kind, None, label, None)).inner.clicked() {
                actions.push(action);
            }
        }
        widgets::paint_text(ui, pos2(br.min.x + space::MD, br.center().y), Align2::LEFT_CENTER, &msg, theme::medium(text::BODY), t.fg, (bx - br.min.x - space::MD * 2.0).max(0.0));
        let said = ui.interact(Rect::from_min_max(br.min, pos2(bx.max(br.min.x), br.max.y)), Id::new(("screen-bar", &info.instance)), Sense::hover());
        named(&said, WidgetType::Label, msg.clone());
        body.min.y = br.max.y + size::HAIRLINE;
    }
    let inner = body.shrink(space::MD);
    match tree {
        Some(tr) => {
            let mut outs = Vec::new();
            let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)));
            child.set_clip_rect(inner.intersect(ui.clip_rect()));
            if info.phase == ScreenPhase::Stopped {
                child.disable();
                child.multiply_opacity(0.55);
            }
            {
                let mut cx = Cx { info, rev: tr.rev, st, out: &mut outs, t };
                if matches!(tr.root.kind, Kind::Split { .. } | Kind::Scroll { .. } | Kind::List { .. } | Kind::FileList { .. } | Kind::Code(_) | Kind::Diff(_)) {
                    tree::node(&mut child, &mut cx, &tr.root);
                } else {
                    egui::ScrollArea::vertical().id_salt(("screen-root", &info.instance)).auto_shrink([false, false]).show(&mut child, |ui| {
                        ui.set_width(ui.available_width());
                        tree::node(ui, &mut cx, &tr.root);
                    });
                }
            }
            for o in outs {
                match o {
                    Out::Event(e) => {
                        st.record(&info.instance, &e, tr.rev, now);
                        actions.push(Action::ScreenEvent { instance: info.instance.clone(), event: e });
                    }
                    Out::Tool { tool, params, confirm } => actions.push(Action::ScreenTool { instance: info.instance.clone(), tool, params, confirm }),
                }
            }
        }
        None => placeholder(ui, inner, info, &t, actions),
    }
    // The border is drawn last, over the plugin's area, so a view cannot paint across it.
    ui.painter().rect_stroke(rect, 0.0, Stroke::new(size::HAIRLINE, t.surface0), egui::StrokeKind::Inside);
}

/// No tree yet: opening, not answering, or not running.
fn placeholder(ui: &mut Ui, rect: Rect, info: &ScreenInfo, t: &Tokens, actions: &mut Vec<Action>) {
    let name = reveal_line(&info.addon_name);
    let (line, detail): (String, Option<String>) = match info.phase {
        ScreenPhase::Opening => (format!("Opening {}\u{2026}", reveal_line(&info.title)), None),
        ScreenPhase::NotAnswering => (format!("{name} did not answer."), info.error.as_deref().map(reveal_line)),
        ScreenPhase::Stopped => (format!("{name} is not running."), None),
        ScreenPhase::Ready => (format!("{name} has not sent a view yet."), info.error.as_deref().map(reveal_line)),
    };
    let c = rect.center();
    if info.phase == ScreenPhase::Opening {
        icons::paint_status(ui, Rect::from_center_size(pos2(c.x, c.y - space::XL), vec2(icon::HERO, icon::HERO)), crate::model::Status::Working, icon::HERO);
        icons::request_spinner_repaint(ui);
    }
    ui.painter().text(pos2(c.x, c.y), Align2::CENTER_CENTER, &line, semibold(text::EMPH), t.fg);
    let said = ui.interact(Rect::from_center_size(c, vec2(rect.width().min(560.0), size::CONTROL_LG)), Id::new(("screen-state", &info.instance)), Sense::hover());
    named(&said, WidgetType::Label, line.clone());
    if let Some(d) = detail {
        widgets::paint_text(ui, pos2(c.x - rect.width() / 2.0 + space::XL, c.y + space::XL), Align2::LEFT_CENTER, &d, theme::regular(text::BODY), t.muted, rect.width() - space::XL * 2.0);
    }
    let buttons: Vec<BarButton> = match info.phase {
        ScreenPhase::NotAnswering => vec![
            ("Retry", ButtonKind::Primary, Action::Tool("screen_open", json!({"screen": info.screen, "context": context_param(info)}))),
            ("Close", ButtonKind::Ghost, Action::Tool("screen_close", json!({"instance": info.instance}))),
        ],
        ScreenPhase::Stopped => vec![
            ("Restart", ButtonKind::Primary, Action::Tool("plugin_restart", json!({"id": info.addon}))),
            ("Close", ButtonKind::Ghost, Action::Tool("screen_close", json!({"instance": info.instance}))),
        ],
        _ => Vec::new(),
    };
    let total: f32 = buttons.iter().map(|(l, ..)| widgets::button_width(ui, None, l, None) + space::SM).sum::<f32>() - space::SM;
    let mut bx = c.x - total / 2.0;
    for (label, kind, action) in buttons {
        let w = widgets::button_width(ui, None, label, None);
        let r = Rect::from_min_size(pos2(bx, c.y + space::XL * 2.0), vec2(w, size::CONTROL));
        bx += w + space::SM;
        if ui.scope_builder(UiBuilder::new().max_rect(r), |ui| widgets::button(ui, kind, None, label, None)).inner.clicked() {
            actions.push(action);
        }
    }
}

/// The context to open the same screen again (the params of `screen_open`).
fn context_param(info: &ScreenInfo) -> Value {
    let c = &info.context;
    // The folder comes from the session; `screen_open` derives it again.
    json!({"session_id": c.session_id, "project": c.project, "path": c.path, "args": c.args})
}

/// A one-line string with invisible characters shown as markers.
fn reveal_line(s: &str) -> String {
    reveal(s).into_iter().map(|g| g.text).collect::<String>().replace('\n', " ")
}

/// The text and rows of the question CSM asks before a button runs `tool`: who asks, which tool,
/// the exact text and the target session. Invisible characters in the plugin's text are shown as
/// markers, and a long text is cut with its length.
pub fn confirm_text(addon_name: &str, addon_id: &str, tool: &str, params: &Value, session_name: Option<&str>) -> (String, String) {
    // A button's params are capped at MAX_ACTION_PARAMS bytes (compact) when the tree is parsed; the
    // pretty-printed form of the same params is at most a few times that, so nothing a person
    // approves is cut.
    const SHOWN: usize = csm_core::addons::screen::MAX_ACTION_PARAMS * 4;
    let who = reveal_line(addon_name);
    let title = match tool {
        "session_send_text" => format!("{who} wants to paste text into a session"),
        "open_url" => format!("{who} wants to open a link"),
        "open_with" => format!("{who} wants to open something in your editor"),
        _ => format!("{who} wants to run {tool}"),
    };
    let mut body = format!("The add-on {who} ({addon_id}) asked to run the tool {tool}.");
    let clip = |s: &str| -> String {
        let n = s.chars().count();
        let shown: String = s.chars().take(SHOWN).collect();
        let more = if n > SHOWN { format!("\n\u{2026} and {} more characters", n - SHOWN) } else { String::new() };
        format!("{}{more}", reveal(&shown).into_iter().map(|g| g.text).collect::<String>())
    };
    match tool {
        "session_send_text" => {
            let sid = params.get("session_id").and_then(Value::as_str).unwrap_or("");
            let target = match session_name {
                Some(n) => format!("{} ({sid})", reveal_line(n)),
                None => format!("{sid} (no such session)"),
            };
            body.push_str(&format!("\n\nTarget session: {target}"));
            body.push_str(&format!("\n\nExact text:\n{}", clip(params.get("text").and_then(Value::as_str).unwrap_or(""))));
            if params.get("submit").and_then(Value::as_bool) == Some(true) {
                body.push_str("\n\nIt will also press Enter.");
            } else {
                body.push_str("\n\nIt will be pasted; you press Enter yourself.");
            }
        }
        "open_url" => body.push_str(&format!("\n\nAddress:\n{}", clip(params.get("url").and_then(Value::as_str).unwrap_or("")))),
        _ => body.push_str(&format!("\n\nParameters:\n{}", clip(&serde_json::to_string_pretty(params).unwrap_or_default()))),
    }
    (title, body)
}

/// Where the pop-out windows read what to draw (set every main frame; the viewports run later).
#[derive(Default)]
struct PopState {
    items: Vec<(ScreenInfo, Option<Arc<ScreenTree>>)>,
    drawn: HashMap<String, (u64, ScreenPhase, Option<String>)>,
}

fn pop_state(ctx: &egui::Context) -> Arc<Mutex<PopState>> {
    ctx.data_mut(|d| d.get_temp_mut_or_insert_with(Id::new("csm-screen-pop"), || Arc::new(Mutex::new(PopState::default()))).clone())
}

/// The window an add-on screen pops out into.
pub fn pop_viewport(instance: &str) -> ViewportId {
    ViewportId::from_hash_of(("addon-screen", instance))
}

/// The screens in their own windows, as deferred viewports (like the torn-off panes).
pub fn pop_windows(ctx: &egui::Context, app: &mut CsmApp) {
    let popped: Vec<ScreenInfo> = app.ui.screen_infos.iter().filter(|i| i.popped_out).cloned().collect();
    let state = pop_state(ctx);
    {
        let mut st = lock(&state);
        st.items = popped.iter().map(|i| (i.clone(), app.backend.screen_tree(&i.instance))).collect();
        st.drawn.retain(|k, _| popped.iter().any(|i| &i.instance == k));
        for i in &popped {
            let now = (i.rev, i.phase, i.error.clone());
            if st.drawn.get(&i.instance) != Some(&now) {
                st.drawn.insert(i.instance.clone(), now);
                ctx.request_repaint_of(pop_viewport(&i.instance));
            }
        }
    }
    for info in popped {
        let vid = pop_viewport(&info.instance);
        let builder = ViewportBuilder::default().with_title(format!("{} \u{b7} {} \u{b7} CodeShellManager", reveal_line(&info.title), reveal_line(&info.addon_name))).with_inner_size(crate::theme::size::TEAR_OFF);
        let (ui_state, remote, state, instance) = (app.ui.screens.clone(), app.remote_actions.clone(), state.clone(), info.instance.clone());
        ctx.show_viewport_deferred(vid, builder, move |ui, _class| {
            let item = lock(&state).items.iter().find(|(i, _)| i.instance == instance).cloned();
            let mut out = Vec::new();
            if let Some((info, tree)) = item {
                let rect = ui.max_rect();
                show(ui, rect, &info, tree.as_ref(), &mut lock(&ui_state), true, &mut out);
            }
            if ui.ctx().input(|i| i.viewport().close_requested()) {
                out.push(Action::Tool("screen_dock", json!({"instance": instance})));
            }
            if !out.is_empty() {
                lock(&remote).extend(out);
                ui.ctx().request_repaint_of(ViewportId::ROOT);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_confirm_for_a_paste_shows_the_target_and_the_exact_text() {
        let params = json!({"session_id": "s1", "text": "rm -rf /\u{1b}[2J\u{202E}ok", "submit": true});
        let (title, body) = confirm_text("Review", "review", "session_send_text", &params, Some("Odulodo"));
        assert!(title.contains("Review") && title.contains("paste text"));
        assert!(body.contains("Odulodo (s1)") && body.contains("review"), "{body}");
        assert!(body.contains("rm -rf /\u{241B}[2J\u{27E8}U+202E\u{27E9}ok"), "invisible characters are marked: {body}");
        assert!(body.contains("press Enter"));
        let (_, body) = confirm_text("Review", "review", "session_send_text", &json!({"session_id": "zz", "text": "x"}), None);
        assert!(body.contains("no such session") && body.contains("you press Enter"));
    }

    #[test]
    fn a_long_text_is_cut_and_other_tools_show_their_params() {
        // Anything a button can carry (MAX_ACTION_PARAMS) is shown whole; only a longer text is cut.
        let shown = csm_core::addons::screen::MAX_ACTION_PARAMS * 4;
        let (_, body) = confirm_text("A", "a", "session_send_text", &json!({"session_id": "s", "text": "y".repeat(csm_core::addons::screen::MAX_ACTION_PARAMS)}), Some("N"));
        assert!(!body.contains("more characters"), "a button-sized text is not cut");
        let (_, body) = confirm_text("A", "a", "session_send_text", &json!({"session_id": "s", "text": "y".repeat(shown + 2000)}), Some("N"));
        assert!(body.contains("and 2000 more characters"));
        assert!(body.len() < shown + 600);
        let (title, body) = confirm_text("A", "a", "open_url", &json!({"url": "https://example.com"}), None);
        assert!(title.contains("open a link") && body.contains("https://example.com"));
        let (title, body) = confirm_text("A", "a", "open_with", &json!({"path": "C:/x"}), None);
        assert!(title.contains("editor") && body.contains("C:/x"));
    }

    #[test]
    fn instances_that_are_gone_lose_their_ui_state() {
        let mut st = ScreenUi::default();
        st.drafts.insert(("i1".into(), "a".into()), Draft { text: "x".into(), reset_rev: 0, change_at: None });
        st.drafts.insert(("i2".into(), "a".into()), Draft { text: "y".into(), reset_rev: 0, change_at: None });
        st.active = Some("i2".into());
        let info = |i: &str| ScreenInfo {
            instance: i.into(),
            addon: "a".into(),
            addon_name: "A".into(),
            screen: "a.s".into(),
            title: "T".into(),
            icon: String::new(),
            scope: csm_core::addons::screen::ScreenScope::Global,
            phase: ScreenPhase::Ready,
            popped_out: false,
            error: None,
            rev: 1,
            context: Default::default(),
        };
        st.sync(&[info("i1")]);
        assert_eq!(st.draft("i1", "a"), Some("x"));
        assert_eq!(st.draft("i2", "a"), None);
        assert_eq!(st.active, None, "a selection that points at a closed screen goes back to the sessions");
    }
}
