//! The Board layout (`layout_set { mode: "board" }`): the scope's live sessions as cards in
//! status columns, beside their projects' tasks.
//!
//! Columns: **Backlog | Working | Needs you | Review | Idle | Done**.
//!
//! - Agent cards are placed by status: working and starting → Working; needs you → Needs you,
//!   in the inbox's order; done-unseen and error → Review; idle → Idle, with asleep sessions
//!   folded into one row there. A card shows the name, the project, branch and dirty mark, a
//!   "doing now" line (the program's OSC 7501 message, else the screen's last line of text), the
//!   task it works on, and how long it has been in its column. Click focuses the session; a right
//!   click opens the sidebar row's menu; an idle card has "+ task".
//! - Task cards: Backlog (drag one onto an agent card: a confirm shows the exact prompt, then
//!   `task_assign`), Review (Accept / Reopen), Done. A task assigned to a session the board
//!   doesn't show sits in Working.
//!
//! Every action is a tool call (`task_*`, `session_focus`, `layout_set`); the drag is egui's own
//! drag-and-drop payload, so it also works in a torn-off group's window.

use egui::{Align2, CornerRadius, CursorIcon, Id, Rect, Sense, Stroke, StrokeKind, Ui, UiBuilder, Vec2, WidgetInfo, WidgetType, pos2, vec2};
use serde_json::json;

use crate::app::{Action, ConfirmState, Menu};
use crate::icons;
use crate::model::{Scope, SessionView, Status, UiModel};
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets;
use csm_core::tasks::{Task, TaskState};

/// The Board's columns, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Backlog,
    Working,
    NeedsYou,
    Review,
    Idle,
    Done,
}

impl Column {
    pub const ALL: [Column; 6] = [Column::Backlog, Column::Working, Column::NeedsYou, Column::Review, Column::Idle, Column::Done];

    pub fn label(self) -> &'static str {
        match self {
            Column::Backlog => "Backlog",
            Column::Working => "Working",
            Column::NeedsYou => "Needs you",
            Column::Review => "Review",
            Column::Idle => "Idle",
            Column::Done => "Done",
        }
    }
}

/// Most Done tasks a column lists (newest first).
pub const DONE_MAX: usize = 30;
/// Longest "doing now" line read from a screen.
const LINE_MAX: usize = 200;

/// The column an agent card goes in.
pub fn column_of(s: &SessionView) -> Column {
    match s.status {
        Status::Starting | Status::Working => Column::Working,
        Status::Approval | Status::Question => Column::NeedsYou,
        Status::Done | Status::Error => Column::Review,
        Status::Idle | Status::Asleep => Column::Idle,
    }
}

/// One card.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Card<'a> {
    Agent(&'a SessionView),
    Task(&'a Task),
}

/// What the board shows for a scope: the cards of each column (in [`Column::ALL`] order) and the
/// asleep sessions folded under Idle. Pure, so it is tested without drawing.
#[derive(Debug, Default)]
pub struct Plan<'a> {
    pub columns: [Vec<Card<'a>>; 6],
    pub asleep: Vec<&'a SessionView>,
    /// Done tasks not listed (past [`DONE_MAX`]).
    pub done_more: usize,
}

impl Plan<'_> {
    pub fn column(&self, c: Column) -> &[Card<'_>] {
        let i = Column::ALL.iter().position(|x| *x == c).unwrap_or(0);
        self.columns.get(i).map_or(&[], Vec::as_slice)
    }
}

/// The projects whose tasks a scope's board shows: every project of a session in the group,
/// and the group itself (`None` for All: every task).
pub fn scope_projects(model: &UiModel, scope: &Scope) -> Option<Vec<String>> {
    match scope {
        Scope::All => None,
        Scope::Group(g) => {
            let mut v: Vec<String> = model.scope_sessions(scope).iter().map(|s| s.project.clone()).filter(|p| !p.is_empty()).collect();
            v.push(format!("group:{g}"));
            v.sort();
            v.dedup();
            Some(v)
        }
    }
}

/// The project a new task on this board goes to: the active session's when it is in scope, else
/// the first session's, else the group's.
pub fn default_project(model: &UiModel, scope: &Scope) -> String {
    let sessions = model.scope_sessions(scope);
    let active = model.active.as_deref().and_then(|a| sessions.iter().find(|s| s.id == a));
    match active.or(sessions.first()) {
        Some(s) if !s.project.is_empty() => s.project.clone(),
        _ => match scope {
            Scope::Group(g) => format!("group:{g}"),
            Scope::All => "group:".to_string(),
        },
    }
}

pub fn plan<'a>(model: &'a UiModel, scope: &Scope) -> Plan<'a> {
    let mut p = Plan::default();
    let sessions = model.scope_sessions(scope);
    let projects = scope_projects(model, scope);
    let in_scope = |t: &&Task| projects.as_ref().is_none_or(|ps| ps.iter().any(|x| x == &t.project));
    let tasks: Vec<&Task> = model.tasks.iter().filter(in_scope).collect();
    let on_board = |id: &str| sessions.iter().any(|s| s.id == id && s.status.is_live());
    let mut push = |c: Column, card: Card<'a>| {
        if let Some(col) = Column::ALL.iter().position(|x| *x == c).and_then(|i| p.columns.get_mut(i)) {
            col.push(card);
        }
    };

    // Backlog: by order, then age.
    let mut backlog: Vec<&Task> = tasks.iter().copied().filter(|t| t.state == TaskState::Backlog).collect();
    backlog.sort_by_key(|t| (t.order, t.created_ms));
    for t in backlog {
        push(Column::Backlog, Card::Task(t));
    }
    // Working: sessions in sidebar order, then tasks whose assignee isn't on the board.
    for s in sessions.iter().filter(|s| column_of(s) == Column::Working) {
        push(Column::Working, Card::Agent(s));
    }
    for t in tasks.iter().filter(|t| t.state == TaskState::Assigned && !t.assignee.as_deref().is_some_and(&on_board)) {
        push(Column::Working, Card::Task(t));
    }
    // Needs you, in the inbox's order (oldest first).
    for s in model.needs_you().into_iter().filter(|s| sessions.iter().any(|x| x.id == s.id)) {
        push(Column::NeedsYou, Card::Agent(s));
    }
    // Review: unseen results (inbox order), then tasks waiting for the person, newest first.
    for s in model.unseen().into_iter().filter(|s| sessions.iter().any(|x| x.id == s.id)) {
        push(Column::Review, Card::Agent(s));
    }
    let mut review: Vec<&Task> = tasks.iter().copied().filter(|t| t.state == TaskState::Review).collect();
    review.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
    for t in review {
        push(Column::Review, Card::Task(t));
    }
    // Idle; the asleep ones fold into one row.
    for s in sessions.iter().filter(|s| s.status == Status::Idle) {
        push(Column::Idle, Card::Agent(s));
    }
    // Done, newest first, capped.
    let mut done: Vec<&Task> = tasks.iter().copied().filter(|t| t.state == TaskState::Done).collect();
    done.sort_by_key(|t| std::cmp::Reverse(t.updated_ms));
    p.done_more = done.len().saturating_sub(DONE_MAX);
    for t in done.into_iter().take(DONE_MAX) {
        push(Column::Done, Card::Task(t));
    }
    p.asleep = sessions.into_iter().filter(|s| s.status == Status::Asleep).collect();
    p
}

/// Who made a task, as the assignment prompt names them (the engine's wording).
pub fn creator_name(model: &UiModel, t: &Task) -> String {
    if t.created_by == csm_core::tasks::CREATED_BY_PERSON {
        return "the user".into();
    }
    if let Some(sid) = t.creator_session() {
        return model.session(sid).map_or_else(|| "a closed session".into(), |s| s.name.clone());
    }
    t.created_by.strip_prefix("mcp:").map_or_else(|| t.created_by.clone(), str::to_owned)
}

/// The confirm shown when a backlog task is dropped on an agent card: the exact prompt
/// `task_assign` pastes.
pub fn assign_confirm(model: &UiModel, t: &Task, s: &SessionView) -> Action {
    let prompt = csm_core::tasks::assignment_prompt(t, &creator_name(model, t));
    let shown: String = prompt.chars().take(600).collect();
    let more = if prompt.chars().count() > 600 { "…" } else { "" };
    let mut body = format!("This is pasted into {} and Enter is pressed:\n\n{shown}{more}", s.name);
    if s.status == Status::Working {
        body.push_str("\n\nIt is working mid-turn: the text lands in the middle of its work.");
    }
    Action::Confirm(ConfirmState {
        title: format!("Assign {} to {}?", t.id, s.name),
        body,
        yes_label: "Assign and send".into(),
        no_label: "Cancel".into(),
        yes: Box::new(Action::Tool(
            "task_assign",
            json!({"task_id": t.id, "session_id": s.id, "send": true, "force": s.status == Status::Working}),
        )),
    })
}

/// "4m", "2h", "3d".
pub fn age(now_ms: u64, since_ms: Option<u64>) -> String {
    let Some(t) = since_ms else { return String::new() };
    let min = now_ms.saturating_sub(t) / 60_000;
    match min {
        0..60 => format!("{min}m"),
        60..1440 => format!("{}h", min / 60),
        _ => format!("{}d", min / 1440),
    }
}

/// The screen's last line of text, for the "doing now" line. Never waits: `None` while the
/// terminal is busy (its reader holds it) or has no such line.
pub fn screen_line(src: &dyn crate::terminal::TerminalSource) -> Option<String> {
    let term = src.terminal()?;
    let t = term.try_lock().ok()?;
    t.last_text_line(LINE_MAX)
}

fn named(resp: &egui::Response, ty: WidgetType, label: impl Into<String>) {
    let l = label.into();
    resp.widget_info(move || WidgetInfo::labeled(ty, true, l.clone()));
}

/// What is being dragged: a backlog task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDrag(pub String);

/// Draws the board for `scope` in `area`. `last_line` reads a session's screen (non-blocking;
/// `None` while its terminal is busy). `torn`: in a torn-off group's window.
pub fn show(ui: &mut Ui, area: Rect, model: &UiModel, scope: &Scope, last_line: &dyn Fn(&str) -> Option<String>, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let p = plan(model, scope);
    let n = Column::ALL.len() as f32;
    let gap = space::PANE_GAP;
    let col_w = ((area.width() - gap * (n - 1.0)) / n).max(size::BOARD_COLUMN_MIN);
    let total_w = col_w * n + gap * (n - 1.0);
    let dragging = egui::DragAndDrop::has_payload_of_type::<TaskDrag>(ui.ctx());
    ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
        egui::ScrollArea::horizontal().id_salt(("board-h", scope.key())).auto_shrink([false, false]).show(ui, |ui| {
            let (row, _) = ui.allocate_exact_size(vec2(total_w, area.height()), Sense::hover());
            for (i, c) in Column::ALL.into_iter().enumerate() {
                let x = row.min.x + i as f32 * (col_w + gap);
                let r = Rect::from_min_size(pos2(x, row.min.y), vec2(col_w, row.height()));
                column(ui, r, c, &p, model, scope, last_line, dragging, actions);
            }
        });
    });
    // The dragged task follows the pointer.
    if let (Some(drag), Some(pos)) = (egui::DragAndDrop::payload::<TaskDrag>(ui.ctx()), ui.ctx().pointer_latest_pos())
        && let Some(task) = model.tasks.iter().find(|x| x.id == drag.0)
    {
        let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Tooltip, Id::new("board-drag")));
        let r = Rect::from_min_size(pos + vec2(space::SM, space::SM), vec2(col_w - space::LG, size::BOARD_LINE + space::SM * 2.0));
        painter.rect(r, CornerRadius::same(radius::MD), t.pop, Stroke::new(size::HAIRLINE, t.blue), StrokeKind::Inside);
        let g = ui.ctx().fonts_mut(|f| {
            let mut job = egui::text::LayoutJob::simple_singleline(task.title.clone(), theme::medium(text::SMALL), t.fg);
            job.wrap = egui::text::TextWrapping::truncate_at_width(r.width() - space::MD * 2.0);
            f.layout_job(job)
        });
        painter.galley(pos2(r.min.x + space::MD, r.center().y - g.size().y / 2.0), g, t.fg);
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    }
}

#[allow(clippy::too_many_arguments)]
fn column(
    ui: &mut Ui,
    r: Rect,
    c: Column,
    p: &Plan<'_>,
    model: &UiModel,
    scope: &Scope,
    last_line: &dyn Fn(&str) -> Option<String>,
    dragging: bool,
    actions: &mut Vec<Action>,
) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_filled(r, CornerRadius::same(radius::LG), t.mantle);
    let cards = p.column(c);
    let count = cards.len() + if c == Column::Idle { p.asleep.len() } else { 0 } + if c == Column::Done { p.done_more } else { 0 };
    let head = Rect::from_min_size(r.min, vec2(r.width(), size::CONTROL + space::SM));
    let cy = head.center().y;
    let lr = ui.painter().text(pos2(head.min.x + space::MD, cy), Align2::LEFT_CENTER, c.label().to_uppercase(), semibold(text::CAPS), t.muted);
    widgets::paint_count(ui, pos2(lr.max.x + space::SM + size::COUNT / 2.0, cy), count, true, size::COUNT);
    let hr = ui.interact(head, Id::new(("board-col", c.label(), scope.key())), Sense::hover());
    named(&hr, WidgetType::Label, format!("Board column: {} ({count})", c.label()));
    if c == Column::Backlog {
        let br = Rect::from_center_size(pos2(head.max.x - space::SM - size::CONTROL_SM / 2.0, cy), Vec2::splat(size::CONTROL_SM));
        let resp = widgets::icon_button_at(ui, br, Id::new(("board-new-task", scope.key())), "plus", false, "New task");
        named(&resp, WidgetType::Button, "New task");
        if resp.clicked() {
            actions.push(Action::NewTaskPrompt { project: default_project(model, scope), assign_to: None });
        }
    }
    let body = Rect::from_min_max(pos2(r.min.x, head.max.y), r.max).shrink2(vec2(space::XS + space::XXS, space::XS));
    ui.scope_builder(UiBuilder::new().max_rect(body), |ui| {
        egui::ScrollArea::vertical().id_salt(("board-v", c.label(), scope.key())).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::XS + space::XXS;
            let w = ui.available_width();
            for card in cards {
                match card {
                    Card::Agent(s) => agent_card(ui, w, model, s, scope, last_line, dragging, actions),
                    Card::Task(task) => task_card(ui, w, model, task, c, actions),
                }
            }
            if c == Column::Idle && !p.asleep.is_empty() {
                asleep_row(ui, w, &p.asleep, scope, actions);
            }
            if c == Column::Done && p.done_more > 0 {
                ui.label(egui::RichText::new(format!("{} more done", p.done_more)).font(theme::regular(text::CAPTION)).color(t.faint));
            }
            if cards.is_empty() && !(c == Column::Idle && !p.asleep.is_empty()) {
                let hint = match c {
                    Column::Backlog => "No tasks. + adds one.",
                    Column::Working => "Nothing running.",
                    Column::NeedsYou => "Nobody waits for you.",
                    Column::Review => "Nothing to review.",
                    Column::Idle => "No idle sessions.",
                    Column::Done => "Nothing done yet.",
                };
                ui.add_space(space::SM);
                ui.vertical_centered(|ui| ui.label(egui::RichText::new(hint).font(theme::regular(text::CAPTION)).color(t.faint)));
            }
        });
    });
}

/// The task a session works on (assigned to it), if any.
fn task_of<'a>(model: &'a UiModel, s: &SessionView) -> Option<&'a Task> {
    model.tasks.iter().find(|t| t.state == TaskState::Assigned && t.assignee.as_deref() == Some(s.id.as_str()))
}

#[allow(clippy::too_many_arguments)]
fn agent_card(
    ui: &mut Ui,
    w: f32,
    model: &UiModel,
    s: &SessionView,
    scope: &Scope,
    last_line: &dyn Fn(&str) -> Option<String>,
    dragging: bool,
    actions: &mut Vec<Action>,
) {
    let t = Tokens::get(ui.ctx());
    let task = task_of(model, s);
    let idle = s.status == Status::Idle;
    let lines = 3.0 + if task.is_some() { 1.0 } else { 0.0 };
    let h = size::BOARD_LINE * lines + space::SM * 2.0;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click());
    named(&resp, WidgetType::Button, format!("Agent: {}", s.name));
    let hover_drop = dragging && resp.dnd_hover_payload::<TaskDrag>().is_some();
    let stroke = if hover_drop {
        Stroke::new(size::RING, t.blue)
    } else if model.active.as_deref() == Some(s.id.as_str()) || resp.hovered() {
        Stroke::new(size::HAIRLINE, t.surface2)
    } else {
        Stroke::new(size::HAIRLINE, t.surface0)
    };
    let cr = CornerRadius::same(radius::MD);
    ui.painter().rect(rect, cr, t.bg, stroke, StrokeKind::Inside);
    let stripe = Rect::from_min_size(rect.min, vec2(size::STRIPE, rect.height()));
    ui.painter().rect_filled(stripe, CornerRadius { nw: radius::MD, sw: radius::MD, ne: 0, se: 0 }, t.accent(s.accent));
    let x0 = rect.min.x + space::SM + size::STRIPE;
    let x1 = rect.max.x - space::SM;
    let line = |i: f32| rect.min.y + space::SM + size::BOARD_LINE * (i + 0.5);
    // 1: status, name, time in column.
    let ir = Rect::from_center_size(pos2(x0 + icon::SM / 2.0, line(0.0)), Vec2::splat(icon::SM));
    icons::paint_status(ui, ir, s.status, icon::SM);
    let age = age(model.now_ms, s.status_since_ms);
    let aw = widgets::text_width(ui, &age, theme::regular(text::CAPTION));
    ui.painter().text(pos2(x1, line(0.0)), Align2::RIGHT_CENTER, &age, theme::regular(text::CAPTION), t.muted);
    let nx = ir.max.x + space::XS + space::XXS;
    widgets::paint_text(ui, pos2(nx, line(0.0)), Align2::LEFT_CENTER, &s.name, semibold(text::BODY), t.fg, x1 - aw - space::SM - nx);
    // 2: project · branch (dirty), and "+ task" on an idle card.
    let mut right = x1;
    if idle {
        let label = "+ task";
        let bw = widgets::text_width(ui, label, theme::medium(text::CAPTION)) + space::SM * 2.0;
        let br = Rect::from_min_size(pos2(x1 - bw, line(1.0) - size::BOARD_LINE / 2.0), vec2(bw, size::BOARD_LINE));
        let b = ui.interact(br, Id::new(("board-add-task", s.id.as_str())), Sense::click());
        named(&b, WidgetType::Button, format!("New task for {}", s.name));
        ui.painter().rect_filled(br, CornerRadius::same(radius::SM), if b.hovered() { t.row_sel } else { t.field });
        ui.painter().text(br.center(), Align2::CENTER_CENTER, label, theme::medium(text::CAPTION), t.fg2);
        if b.clicked() {
            actions.push(Action::NewTaskPrompt { project: s.project.clone(), assign_to: Some(s.id.clone()) });
        }
        right = br.min.x - space::XS;
    }
    let mut meta = s.project_name.clone();
    if let Some(b) = &s.branch {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(b);
        if s.dirty {
            meta.push_str(" ●");
        }
    }
    widgets::paint_text(ui, pos2(x0, line(1.0)), Align2::LEFT_CENTER, &meta, theme::regular(text::CAPTION), t.muted, right - x0);
    // 3: doing now.
    let now = if s.detail.trim().is_empty() { last_line(&s.id).unwrap_or_default() } else { s.detail.clone() };
    let now = if now.is_empty() { s.status_text() } else { now };
    widgets::paint_text(ui, pos2(x0, line(2.0)), Align2::LEFT_CENTER, &now, theme::regular(text::SMALL), t.fg2, x1 - x0);
    // 4: its task.
    if let Some(task) = task {
        let txt = format!("{} · {}", task.id, task.title);
        icons::paint(ui, Rect::from_center_size(pos2(x0 + icon::XS / 2.0, line(3.0)), Vec2::splat(icon::XS)), "check", icon::XS, t.blue);
        widgets::paint_text(ui, pos2(x0 + icon::XS + space::XS, line(3.0)), Align2::LEFT_CENTER, &txt, theme::medium(text::CAPTION), t.blue, x1 - x0 - icon::XS);
    }
    if let Some(drop) = resp.dnd_release_payload::<TaskDrag>()
        && let Some(task) = model.tasks.iter().find(|x| x.id == drop.0)
    {
        actions.push(assign_confirm(model, task, s));
    } else if resp.clicked() {
        actions.push(Action::Tool("session_focus", json!({"session_id": s.id})));
        actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": "focus"})));
        actions.push(Action::Toast(format!("Focused {}. The Board is one click away in the group bar.", s.name)));
    }
    if resp.secondary_clicked() {
        let at = ui.ctx().pointer_latest_pos().unwrap_or(rect.right_bottom());
        actions.push(Action::OpenMenu(Menu::Row { session: s.id.clone(), at }));
    }
}

fn task_card(ui: &mut Ui, w: f32, model: &UiModel, task: &Task, c: Column, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let review = c == Column::Review;
    let h = size::BOARD_LINE * 2.0 + space::SM * 2.0 + if review { size::CONTROL_SM + space::XS } else { 0.0 };
    let sense = if task.state == TaskState::Backlog { Sense::click_and_drag() } else { Sense::click() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), sense);
    named(&resp, WidgetType::Button, format!("Task: {}", task.title));
    if task.state == TaskState::Backlog {
        resp.dnd_set_drag_payload(TaskDrag(task.id.clone()));
    }
    let being_dragged = egui::DragAndDrop::payload::<TaskDrag>(ui.ctx()).is_some_and(|d| d.0 == task.id);
    let fill = if being_dragged { Tokens::tint(t.bg, alpha::ASLEEP) } else { t.bg };
    let stroke = if resp.hovered() { t.surface2 } else { t.surface0 };
    ui.painter().rect(rect, CornerRadius::same(radius::MD), fill, Stroke::new(size::HAIRLINE, stroke), StrokeKind::Inside);
    let x0 = rect.min.x + space::SM;
    let x1 = rect.max.x - space::SM;
    let line = |i: f32| rect.min.y + space::SM + size::BOARD_LINE * (i + 0.5);
    let idr = ui.painter().text(pos2(x0, line(0.0)), Align2::LEFT_CENTER, &task.id, theme::mono(text::MONO_SM), t.faint);
    let tx = idr.max.x + space::XS + space::XXS;
    widgets::paint_text(ui, pos2(tx, line(0.0)), Align2::LEFT_CENTER, &task.title, theme::medium(text::SMALL), t.fg, x1 - tx);
    let who = task.assignee.as_deref().map(|a| model.session(a).map_or_else(|| "a closed session".to_string(), |s| s.name.clone()));
    let mut meta = csm_core::tasks::project_label(&task.project, |g| model.group(g).map(|x| x.name.clone()));
    if let Some(w) = who {
        meta.push_str(" · ");
        meta.push_str(&w);
    }
    if let Some(n) = &task.progress {
        meta.push_str(" · ");
        meta.push_str(n);
    }
    let age = age(model.now_ms, u64::try_from(task.updated_ms).ok());
    let aw = widgets::text_width(ui, &age, theme::regular(text::CAPTION));
    ui.painter().text(pos2(x1, line(1.0)), Align2::RIGHT_CENTER, &age, theme::regular(text::CAPTION), t.faint);
    widgets::paint_text(ui, pos2(x0, line(1.0)), Align2::LEFT_CENTER, &meta, theme::regular(text::CAPTION), t.muted, x1 - x0 - aw - space::SM);
    if review {
        let y = rect.min.y + space::SM + size::BOARD_LINE * 2.0 + space::XS;
        let mut x = x0;
        for (label, tool, primary) in [("Accept", "task_accept", true), ("Reopen", "task_reopen", false)] {
            let bw = widgets::text_width(ui, label, theme::medium(text::SMALL)) + space::ROW_X * 2.0;
            let br = Rect::from_min_size(pos2(x, y), vec2(bw, size::CONTROL_SM));
            x = br.max.x + space::XS;
            let b = ui.interact(br, Id::new((label, task.id.as_str())), Sense::click());
            named(&b, WidgetType::Button, format!("{label} {}", task.id));
            let (bg, fg) = if primary { (t.blue, t.on_accent) } else { (if b.hovered() { t.row_sel } else { t.field }, t.fg) };
            ui.painter().rect(br, CornerRadius::same(radius::MD), bg, Stroke::new(size::HAIRLINE, t.surface0), StrokeKind::Inside);
            ui.painter().text(br.center(), Align2::CENTER_CENTER, label, theme::medium(text::SMALL), fg);
            if b.clicked() {
                actions.push(Action::Tool(tool, json!({"task_id": task.id})));
            }
        }
    }
    let id = task.id.clone();
    let title = task.title.clone();
    let open = task.state.is_open();
    resp.context_menu(|ui| {
        if open && widgets::menu_item(ui, "x", "Cancel task", None, false).clicked() {
            actions.push(Action::Tool("task_update", json!({"task_id": id, "cancel": true})));
            ui.close();
        }
        if widgets::menu_item(ui, "trash", "Delete task…", None, true).clicked() {
            actions.push(Action::Confirm(ConfirmState {
                title: format!("Delete {id}?"),
                body: format!("“{title}” is deleted for good."),
                yes_label: "Delete".into(),
                no_label: "Cancel".into(),
                yes: Box::new(Action::Tool("task_delete", json!({"task_id": id}))),
            }));
            ui.close();
        }
    });
}

/// The asleep sessions, folded into one row under Idle; a click unfolds them (widget state).
fn asleep_row(ui: &mut Ui, w: f32, asleep: &[&SessionView], scope: &Scope, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let key = Id::new(("board-asleep-open", scope.key()));
    let open = ui.ctx().data(|d| d.get_temp::<bool>(key)).unwrap_or(false);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::BOARD_LINE + space::SM), Sense::click());
    named(&resp, WidgetType::Button, format!("{} asleep", asleep.len()));
    if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::SM), t.row_hover);
    }
    icons::paint(ui, Rect::from_center_size(pos2(rect.min.x + space::SM, rect.center().y), Vec2::splat(icon::XS)), if open { "chevD" } else { "chevR" }, icon::XS, t.muted);
    ui.painter().text(pos2(rect.min.x + space::SM + icon::XS, rect.center().y), Align2::LEFT_CENTER, format!("{} asleep", asleep.len()), theme::regular(text::CAPTION), t.muted);
    if resp.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(key, !open));
    }
    if open {
        for s in asleep {
            let (r, row) = ui.allocate_exact_size(vec2(w, size::BOARD_LINE), Sense::click());
            named(&row, WidgetType::Button, format!("Agent: {}", s.name));
            if row.hovered() {
                ui.painter().rect_filled(r, CornerRadius::same(radius::SM), t.row_hover);
            }
            icons::paint_status(ui, Rect::from_center_size(pos2(r.min.x + space::SM + icon::XS / 2.0, r.center().y), Vec2::splat(icon::XS)), s.status, icon::XS);
            widgets::paint_text(ui, pos2(r.min.x + space::LG + space::XS, r.center().y), Align2::LEFT_CENTER, &s.name, theme::regular(text::CAPTION), t.fg2, w - space::XL);
            if row.clicked() {
                actions.push(Action::Tool("session_focus", json!({"session_id": s.id})));
            }
            if row.secondary_clicked() {
                let at = ui.ctx().pointer_latest_pos().unwrap_or(r.right_bottom());
                actions.push(Action::OpenMenu(Menu::Row { session: s.id.clone(), at }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str, group: &str, status: Status, project: &str) -> SessionView {
        SessionView { id: id.into(), name: id.into(), group: Some(group.into()), status, project: project.into(), ..SessionView::default() }
    }

    fn task(id: &str, project: &str, state: TaskState, assignee: Option<&str>, order: i64, updated: i64) -> Task {
        Task {
            id: id.into(),
            project: project.into(),
            title: id.into(),
            body: String::new(),
            state,
            assignee: assignee.map(str::to_owned),
            created_by: csm_core::tasks::CREATED_BY_PERSON.into(),
            source: "ui".into(),
            progress: None,
            order,
            created_ms: 0,
            updated_ms: updated,
        }
    }

    fn ids(p: &Plan<'_>, c: Column) -> Vec<String> {
        p.column(c)
            .iter()
            .map(|card| match card {
                Card::Agent(s) => format!("@{}", s.id),
                Card::Task(t) => t.id.clone(),
            })
            .collect()
    }

    #[test]
    fn cards_land_in_their_columns() {
        let now = 1_000_000_000u64;
        let mut m = UiModel { now_ms: now, ..UiModel::default() };
        m.sessions = vec![
            session("w", "g", Status::Working, "repo:/a"),
            session("s", "g", Status::Starting, "repo:/a"),
            session("q", "g", Status::Question, "repo:/a"),
            session("p", "g", Status::Approval, "repo:/a"),
            session("d", "g", Status::Done, "repo:/a"),
            session("e", "g", Status::Error, "repo:/a"),
            session("i", "g", Status::Idle, "repo:/a"),
            session("z", "g", Status::Asleep, "repo:/a"),
            session("other", "h", Status::Working, "repo:/b"),
        ];
        // Needs you: oldest in the inbox first (p waited longer than q).
        m.sessions[2].inbox_since_ms = Some(now - 1_000);
        m.sessions[3].inbox_since_ms = Some(now - 5_000);
        m.tasks = vec![
            task("t2", "repo:/a", TaskState::Backlog, None, 2, 0),
            task("t1", "repo:/a", TaskState::Backlog, None, 1, 0),
            task("t3", "repo:/a", TaskState::Review, Some("w"), 0, 5),
            task("t4", "repo:/a", TaskState::Done, Some("w"), 0, 7),
            task("t5", "repo:/a", TaskState::Assigned, Some("gone"), 0, 0),
            task("t6", "repo:/a", TaskState::Assigned, Some("w"), 0, 0),
            task("t7", "repo:/b", TaskState::Backlog, None, 0, 0),
            task("t8", "group:g", TaskState::Backlog, None, 9, 0),
            task("t9", "repo:/a", TaskState::Cancelled, None, 0, 0),
        ];
        let g = Scope::Group("g".into());
        let p = plan(&m, &g);
        assert_eq!(ids(&p, Column::Backlog), ["t1", "t2", "t8"], "by order; another repo's task is not here");
        assert_eq!(ids(&p, Column::Working), ["@w", "@s", "t5"], "a task whose session is gone waits in Working");
        assert_eq!(ids(&p, Column::NeedsYou), ["@p", "@q"]);
        assert_eq!(ids(&p, Column::Review), ["@d", "@e", "t3"]);
        assert_eq!(ids(&p, Column::Idle), ["@i"]);
        assert_eq!(p.asleep.len(), 1);
        assert_eq!(ids(&p, Column::Done), ["t4"]);
        // All: every session and every task.
        let all = plan(&m, &Scope::All);
        assert!(ids(&all, Column::Working).contains(&"@other".to_string()));
        assert!(ids(&all, Column::Backlog).contains(&"t7".to_string()));
    }

    #[test]
    fn the_assign_confirm_shows_the_exact_prompt() {
        let m = UiModel { sessions: vec![session("a", "g", Status::Idle, "repo:/a")], ..UiModel::default() };
        let mut t = task("t1", "repo:/a", TaskState::Backlog, None, 0, 0);
        t.title = "Fix the login".into();
        t.body = "It 500s".into();
        let Action::Confirm(c) = assign_confirm(&m, &t, &m.sessions[0]) else { panic!() };
        assert!(c.body.contains("Task t1 from the user: Fix the login\nIt 500s"), "{}", c.body);
        assert_eq!(*c.yes, Action::Tool("task_assign", json!({"task_id": "t1", "session_id": "a", "send": true, "force": false})));
    }

    #[test]
    fn ages_read_short() {
        assert_eq!(age(10 * 60_000, Some(0)), "10m");
        assert_eq!(age(3 * 3_600_000, Some(0)), "3h");
        assert_eq!(age(50 * 3_600_000, Some(0)), "2d");
        assert_eq!(age(0, Some(5)), "0m", "a clock behind the stamp is not negative");
        assert_eq!(age(0, None), "");
    }
}
