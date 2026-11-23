//! Git in the window: the branch chip in the pane header (`⎇ main ● ↑2 ↓5`) and the Inspector's
//! Git tab. Every action is a git tool (`git_fetch`, `git_pull`, `git_switch_branch`,
//! `git_branches`, `worktree_list`, `worktree_new`, `worktree_remove`, `worktree_prune`); the
//! tab reads the snapshot's git fields and the details the backend kept from tool replies
//! ([`crate::git_model`]).
//!
//! Read-mostly, as in the WPF app: no commit, push or merge here, because Claude is editing
//! this tree.

use egui::{Align2, CornerRadius, Id, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetType, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{Action, CsmApp};
use crate::git_model::{GitDetail, signature};
use crate::icons;
use crate::model::{Kind, SessionView, Status};
use crate::theme::{self, Tokens, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

/// Changed files listed before "and N more".
const MAX_CHANGES: usize = 60;

/// The pane header's chip: branch, a peach dirty dot, blue ahead/behind, ⟳ while a git
/// operation runs; the tooltip is the git header line (with "worktree of X").
pub fn branch_chip(ui: &mut Ui, s: &SessionView, max_w: f32, clickable: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let branch = s.branch.as_deref().unwrap_or("detached");
    let font = theme::mono(text::MONO_SM);
    let mut sync = String::new();
    if s.ahead > 0 {
        sync.push_str(&format!("↑{}", s.ahead));
    }
    if s.behind > 0 {
        if !sync.is_empty() {
            sync.push(' ');
        }
        sync.push_str(&format!("↓{}", s.behind));
    }
    if s.git_busy {
        if !sync.is_empty() {
            sync.push(' ');
        }
        sync.push('⟳');
    }
    let sync_w = if sync.is_empty() { 0.0 } else { widgets::text_width(ui, &sync, font.clone()) + space::XS };
    let dot_w = if s.dirty { space::SM } else { 0.0 };
    let extra = icon::XS + space::XS + dot_w + sync_w;
    let tw = widgets::text_width(ui, branch, font.clone());
    let w = (space::SM + space::XXS + extra + tw).min(max_w.max(extra + space::XXL));
    let sense = if clickable { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::CHIP), sense);
    let stroke = if clickable && resp.hovered() { t.surface2 } else { t.surface0 };
    ui.painter().rect(rect, CornerRadius::same(radius::SM), t.crust, Stroke::new(size::HAIRLINE, stroke), StrokeKind::Inside);
    let cy = rect.center().y;
    let mut x = rect.min.x + space::XS + space::XXS / 2.0;
    icons::paint(ui, Rect::from_min_size(pos2(x, cy - icon::XS / 2.0), Vec2::splat(icon::XS)), "branch", icon::XS, t.fg2);
    x += icon::XS + space::XS;
    let name_w = (rect.max.x - space::XS - dot_w - sync_w - x).max(space::SM);
    let r = widgets::paint_text(ui, pos2(x, cy), Align2::LEFT_CENTER, branch, font.clone(), t.fg2, name_w);
    x = r.max.x + space::XS;
    if s.dirty {
        ui.painter().circle_filled(pos2(x + space::XXS, cy), space::XS - size::HAIRLINE, t.peach);
        x += space::SM;
    }
    if !sync.is_empty() {
        ui.painter().text(pos2(x, cy), Align2::LEFT_CENTER, &sync, font, t.blue);
    }
    let label = format!("Git: {branch}");
    resp.widget_info(|| egui::WidgetInfo::labeled(if clickable { WidgetType::Button } else { WidgetType::Label }, true, &label));
    let tip = s.git_summary.clone().unwrap_or_else(|| format!("⎇ {branch}"));
    resp.on_hover_text(if s.git_busy { format!("{tip}\nA git operation is running.") } else { tip })
}

/// Ephemeral state of the Git tab (egui memory; nothing here changes the app's state).
#[derive(Clone, Default)]
struct TabMem {
    /// Per session: the signature and generation the lists were last requested for.
    requested: std::collections::HashMap<String, (String, u64)>,
    switch_open: bool,
    /// The New worktree input, while open.
    new_worktree: Option<String>,
    /// Put the keyboard in the New worktree input on its next frame.
    focus_new: bool,
    /// The worktree path whose removal is being confirmed.
    confirm_remove: Option<String>,
    delete_branch: bool,
    /// The failure seq the person dismissed.
    dismissed: u64,
}

fn mem_id() -> Id {
    Id::new("csm-git-tab")
}

fn load(ui: &Ui) -> TabMem {
    ui.ctx().data_mut(|d| d.get_temp::<TabMem>(mem_id()).unwrap_or_default())
}

fn store(ui: &Ui, m: TabMem) {
    ui.ctx().data_mut(|d| d.insert_temp(mem_id(), m));
}

/// "New worktree from this branch…" (the session menus): the tab opens with its New worktree
/// input showing and focused. The caller focuses the session and shows the tab.
pub fn open_new_worktree(ctx: &egui::Context) {
    ctx.data_mut(|d| {
        let mut m = d.get_temp::<TabMem>(mem_id()).unwrap_or_default();
        m.new_worktree.get_or_insert_with(String::new);
        m.focus_new = true;
        d.insert_temp(mem_id(), m);
    });
}

fn caption(ui: &mut Ui, t: &Tokens, s: &str) {
    ui.label(egui::RichText::new(s).font(theme::regular(text::CAPTION)).color(t.muted));
}

/// The Inspector's Git tab.
pub fn tab(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    if s.kind == Kind::Ssh {
        widgets::card_frame(&t).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let head = match &s.branch {
                Some(b) => format!("⎇ {b}{}", if s.dirty { " ●" } else { "" }),
                None => "No git info for this session".into(),
            };
            ui.label(egui::RichText::new(head).font(semibold(text::BODY)).color(t.fg));
            caption(ui, &t, "An SSH session's branch is what it pushes with OSC 9001; git actions need a local or WSL folder.");
        });
        return;
    }
    let sid = s.id.clone();
    let detail = app.model.git.get(&sid).cloned().unwrap_or_default();
    let mut mem = load(ui);
    request_if_stale(&mut mem, s, &detail, actions);

    egui::ScrollArea::vertical().id_salt(("git-tab", sid.clone())).auto_shrink([false, false]).show(ui, |ui| {
        ui.spacing_mut().item_spacing.y = space::SM;
        let repo = s.branch.is_some() || s.git_summary.is_some() || detail.summary.is_some();
        if !repo {
            widgets::card_frame(&t).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let (h, c) = if detail.not_repo {
                    ("Not a git repository", "This session's folder is not inside a git repository.")
                } else {
                    ("Reading git…", "git status runs in the background.")
                };
                ui.label(egui::RichText::new(h).font(semibold(text::BODY)).color(t.fg));
                caption(ui, &t, c);
            });

            return;
        }
        header(ui, &t, s, &detail);
        actions_row(ui, s, &detail, &mut mem, actions);
        failure(ui, &t, &detail, &mut mem, actions);
        if mem.switch_open {
            branches(ui, &t, s, &detail, actions);
        }
        changes(ui, &t, &detail);
        worktrees(ui, &t, s, &detail, &mut mem, actions);
        caption(ui, &t, "No commit, push or merge here: Claude is editing this tree.");
    });
    store(ui, mem);
}

/// Reads status and worktrees again when the tab first shows a session, after an operation, or
/// when the poll moved the snapshot's git fields. Once per change, never every frame.
fn request_if_stale(mem: &mut TabMem, s: &SessionView, detail: &GitDetail, actions: &mut Vec<Action>) {
    let key = (signature(s), detail.generation);
    let have = detail.summary.is_some() || detail.not_repo || detail.worktrees.is_some();
    match mem.requested.get(&s.id) {
        Some(k) if *k == key => return,
        // A fixture (the demo) or a reply already present for this state: nothing to read.
        None if have => {
            mem.requested.insert(s.id.clone(), key);
            return;
        }
        _ => {}
    }
    mem.requested.insert(s.id.clone(), key);
    let p = json!({"session_id": s.id});
    actions.push(Action::Tool("git_status", p.clone()));
    actions.push(Action::Tool("worktree_list", p));
}

fn header(ui: &mut Ui, t: &Tokens, s: &SessionView, d: &GitDetail) {
    widgets::card_frame(t).inner_margin(egui::Margin::symmetric(space::MD as i8, space::ROW_X as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            icons::show(ui, "branch", icon::MD, t.fg);
            let b = s.branch.clone().unwrap_or_else(|| "detached HEAD".into());
            ui.label(egui::RichText::new(b).font(theme::mono(text::BODY)).color(t.fg).strong());
            if s.dirty {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(space::SM), Sense::hover());
                ui.painter().circle_filled(r.center(), space::XS - size::HAIRLINE, t.peach);
            }
            if let Some(u) = &s.upstream {
                ui.label(egui::RichText::new("→").color(t.muted));
                ui.label(egui::RichText::new(u).font(theme::mono(text::MONO)).color(t.fg2));
            }
        });
        let line = s.git_summary.clone().or_else(|| d.summary.clone()).unwrap_or_default();
        let resp = ui.add(egui::Label::new(egui::RichText::new(&line).font(theme::mono(text::MONO)).color(t.muted)).wrap());
        resp.widget_info(|| egui::WidgetInfo::labeled(WidgetType::Label, true, format!("Git status: {line}")));
        if s.git_busy {
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(Vec2::splat(icon::SM), Sense::hover());
                icons::paint_status(ui, r, Status::Working, icon::SM);
                icons::request_spinner_repaint(ui);
                caption(ui, t, "A git operation is running on this repository…");
            });
        }
    });
}

fn actions_row(ui: &mut Ui, s: &SessionView, d: &GitDetail, mem: &mut TabMem, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        let sid = json!({"session_id": s.id});
        if widgets::button(ui, ButtonKind::Secondary, Some("fetch"), "Fetch", None)
            .on_hover_text("git fetch (never prompts for a password)")
            .clicked()
        {
            actions.push(Action::Tool("git_fetch", sid.clone()));
        }
        if widgets::button(ui, ButtonKind::Secondary, Some("download"), "Pull", None)
            .on_hover_text("git pull --ff-only: moves the branch only when that loses nothing")
            .clicked()
        {
            actions.push(Action::Tool("git_pull", sid.clone()));
        }
        let label = if mem.switch_open { "Switch branch ▾" } else { "Switch branch ▸" };
        if widgets::button(ui, ButtonKind::Secondary, Some("branch"), label, None).clicked() {
            mem.switch_open = !mem.switch_open;
            if mem.switch_open {
                // The list is loaded when it opens.
                actions.push(Action::Tool("git_branches", sid.clone()));
            }
        }
        // A github.com remote: its branch page (▾: the pull requests too), through `open_url`.
        if let Some(gh) = &d.github {
            if widgets::button(ui, ButtonKind::Secondary, None, "Open on GitHub", None).on_hover_text(gh.branch_url.as_str()).clicked() {
                actions.push(Action::Tool("open_url", json!({"url": gh.branch_url})));
            }
            let more = widgets::icon_button(ui, "chevD", size::CONTROL, false, "More GitHub links");
            if more.clicked() {
                let at = pos2(more.rect.max.x, more.rect.max.y + space::XS);
                actions.push(Action::OpenMenu(crate::app::Menu::GitHub { session: s.id.clone(), at }));
            }
        }
    });
}

fn failure(ui: &mut Ui, t: &Tokens, d: &GitDetail, mem: &mut TabMem, actions: &mut Vec<Action>) {
    let Some(f) = &d.failure else { return };
    if f.seq <= mem.dismissed {
        return;
    }
    egui::Frame::new()
        .fill(t.term_bg)
        .stroke(Stroke::new(size::HAIRLINE, t.red))
        .corner_radius(CornerRadius::same(radius::MD))
        .inner_margin(egui::Margin::symmetric(space::ROW_X as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                icons::show(ui, "x", icon::SM, t.red);
                ui.label(egui::RichText::new("git said").font(semibold(text::SMALL)).color(t.red));
            });
            let r = ui.add(egui::Label::new(egui::RichText::new(&f.text).font(theme::mono(text::MONO_SM)).color(t.term_fg)).wrap());
            r.widget_info(|| egui::WidgetInfo::labeled(WidgetType::Label, true, format!("Git failure: {}", f.text)));
            ui.horizontal(|ui| {
                if widgets::button(ui, ButtonKind::Ghost, Some("copy"), "Copy", None).clicked() {
                    actions.push(Action::CopyText(f.text.clone()));
                }
                if widgets::button(ui, ButtonKind::Ghost, Some("x"), "Dismiss", None).clicked() {
                    mem.dismissed = f.seq;
                }
            });
        });
}

fn row(ui: &mut Ui, enabled: bool) -> (Rect, Response) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG), if enabled { Sense::click() } else { Sense::hover() });
    let t = Tokens::get(ui.ctx());
    if enabled && resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), t.row_hover);
    }
    (rect, resp)
}

fn branches(ui: &mut Ui, t: &Tokens, s: &SessionView, d: &GitDetail, actions: &mut Vec<Action>) {
    widgets::caps(ui, "Switch branch");
    let Some(list) = &d.branches else {
        caption(ui, t, "Loading branches…");
        return;
    };
    if list.is_empty() {
        caption(ui, t, "No local branches.");
        return;
    }
    for b in list {
        let (rect, resp) = row(ui, !b.current);
        let cy = rect.center().y;
        let ir = Rect::from_min_size(pos2(rect.min.x + space::SM, cy - icon::SM / 2.0), Vec2::splat(icon::SM));
        icons::paint(ui, ir, if b.current { "check" } else { "branch" }, icon::SM, if b.current { t.green } else { t.muted });
        widgets::paint_text(ui, pos2(ir.max.x + space::SM, cy), Align2::LEFT_CENTER, &b.name, theme::mono(text::MONO), t.fg, rect.width() * 0.6);
        let tag = match (&b.elsewhere, b.current) {
            (_, true) => Some("current".to_string()),
            (Some(e), _) => Some(format!("in {}", e.name)),
            _ => None,
        };
        if let Some(tag) = &tag {
            ui.painter().text(pos2(rect.max.x - space::SM, cy), Align2::RIGHT_CENTER, tag, theme::regular(text::CAPTION), t.muted);
        }
        let label = match &tag {
            Some(tg) => format!("Branch: {} ({tg})", b.name),
            None => format!("Branch: {}", b.name),
        };
        resp.widget_info(|| egui::WidgetInfo::labeled(WidgetType::Button, !b.current, &label));
        if resp.clicked() {
            match &b.elsewhere {
                // git allows a branch in one worktree at a time: go there instead.
                Some(e) => match &e.session_id {
                    Some(id) => actions.push(Action::Tool("session_focus", json!({"session_id": id}))),
                    None => actions.push(Action::Tool("session_new", e.open.clone())),
                },
                None => actions.push(Action::Tool("git_switch_branch", json!({"session_id": s.id, "name": b.name}))),
            }
        }
    }
}

fn changes(ui: &mut Ui, t: &Tokens, d: &GitDetail) {
    if d.changes.is_empty() {
        return;
    }
    widgets::caps(ui, &format!("Changes ({})", d.changes.len()));
    for c in d.changes.iter().take(MAX_CHANGES) {
        let code = c.code.replace('.', " ");
        let color = match code.trim() {
            "??" | "A" => t.green,
            "D" => t.red,
            x if x.contains('U') || x == "AA" || x == "DD" => t.red,
            _ => t.peach,
        };
        let r = ui
            .horizontal(|ui| {
                ui.label(egui::RichText::new(&code).font(theme::mono(text::MONO)).color(color).strong());
                ui.add(egui::Label::new(egui::RichText::new(&c.path).font(theme::mono(text::MONO)).color(t.fg)).truncate());
            })
            .response;
        r.widget_info(|| egui::WidgetInfo::labeled(WidgetType::Label, true, format!("Changed: {} {}", code.trim(), c.path)));
    }
    if d.changes.len() > MAX_CHANGES {
        caption(ui, t, &format!("and {} more", d.changes.len() - MAX_CHANGES));
    }
}

fn worktrees(ui: &mut Ui, t: &Tokens, s: &SessionView, d: &GitDetail, mem: &mut TabMem, actions: &mut Vec<Action>) {
    widgets::caps(ui, "Worktrees");
    let Some(list) = &d.worktrees else {
        caption(ui, t, "Loading worktrees…");
        return;
    };
    for w in list {
        let (rect, resp) = row(ui, !w.current && !w.prunable);
        let y1 = rect.min.y + space::MD;
        let ir = Rect::from_min_size(pos2(rect.min.x + space::SM, y1 - icon::SM / 2.0), Vec2::splat(icon::SM));
        icons::paint(ui, ir, if w.main { "branch" } else { "fork" }, icon::SM, if w.current { t.blue } else { t.muted });
        widgets::paint_text(
            ui,
            pos2(ir.max.x + space::SM, y1),
            Align2::LEFT_CENTER,
            &w.label,
            theme::mono(text::MONO_SM),
            if w.prunable { t.muted } else { t.fg },
            rect.width() - space::XXL * 2.0,
        );
        let who = match (w.current, w.sessions.len(), w.sessions.iter().all(|x| x.asleep)) {
            (true, _, _) => "this session".to_string(),
            (_, 0, _) => "no session · click to open one".to_string(),
            (_, n, true) => format!("{n} asleep · click to wake"),
            (_, 1, _) => "1 session · click to focus".to_string(),
            (_, n, _) => format!("{n} sessions · click to focus"),
        };
        widgets::paint_text(
            ui,
            pos2(ir.max.x + space::SM, rect.max.y - space::SM),
            Align2::LEFT_CENTER,
            &who,
            theme::regular(text::CAPTION),
            t.muted,
            rect.width() - space::XXL,
        );
        let label = format!("Worktree: {}", w.name);
        resp.widget_info(|| egui::WidgetInfo::labeled(WidgetType::Button, !w.current, &label));
        if resp.clicked() {
            let (id, p) = w.target();
            actions.push(Action::Tool(id, p));
        }
    }

    // New worktree: a small inline input.
    let sid = s.id.clone();
    match mem.new_worktree.take() {
        Some(mut name) => {
            let mut keep = true;
            ui.horizontal(|ui| {
                let r = ui
                    .add(egui::TextEdit::singleline(&mut name).hint_text("new-branch").desired_width(ui.available_width() - size::CONTROL_LG * 3.0));
                r.widget_info(|| egui::WidgetInfo::labeled(WidgetType::TextEdit, true, "New worktree branch"));
                if std::mem::take(&mut mem.focus_new) {
                    r.request_focus();
                }
                let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                let create = widgets::button(ui, ButtonKind::Primary, Some("plus"), "Create", None).clicked();
                if (create || enter) && !name.trim().is_empty() {
                    actions.push(Action::Tool("worktree_new", json!({"session_id": sid, "branch": name.trim()})));
                    keep = false;
                }
                if widgets::button(ui, ButtonKind::Ghost, Some("x"), "Cancel", None).clicked() {
                    keep = false;
                }
            });
            let from = s.branch.as_deref().map_or_else(|| "HEAD".to_string(), |b| format!("this branch, {b},"));
            caption(
                ui,
                t,
                &format!(
                    "Checks out the branch (created from {from} when new) beside the main checkout and opens a session there with this one's command."
                ),
            );
            if keep {
                mem.new_worktree = Some(name);
            }
        }
        None => {
            ui.horizontal_wrapped(|ui| {
                if widgets::button(ui, ButtonKind::Ghost, Some("plus"), "New worktree…", None).clicked() {
                    mem.new_worktree = Some(String::new());
                }
                if d.prunable > 0 {
                    let l = if d.prunable == 1 { "Prune 1 missing worktree".to_string() } else { format!("Prune {} missing worktrees", d.prunable) };
                    if widgets::button(ui, ButtonKind::Ghost, Some("eraser"), &l, None)
                        .on_hover_text("Only git's bookkeeping for folders already deleted")
                        .clicked()
                    {
                        actions.push(Action::Tool("worktree_prune", json!({"session_id": sid})));
                    }
                }
                let current = list.iter().find(|w| w.current && !w.main);
                if let Some(cur) = current
                    && widgets::button(ui, ButtonKind::Danger, Some("trash"), "Remove…", None).clicked()
                {
                    mem.confirm_remove = Some(cur.path.clone());
                    mem.delete_branch = false;
                }
            });
        }
    }
    remove_confirm(ui, t, s, list, mem, actions);
}

/// The confirmation names the folder and the sessions it will close (and the sleeping ones it
/// will delete); git still refuses a worktree with changes (no `--force`).
fn remove_confirm(ui: &mut Ui, t: &Tokens, s: &SessionView, list: &[crate::git_model::WorktreeRow], mem: &mut TabMem, actions: &mut Vec<Action>) {
    let Some(path) = mem.confirm_remove.clone() else { return };
    let Some(w) = list.iter().find(|w| w.path == path) else {
        mem.confirm_remove = None;
        return;
    };
    let live: Vec<&str> = w.sessions.iter().filter(|x| !x.asleep).map(|x| x.name.as_str()).collect();
    let asleep: Vec<&str> = w.sessions.iter().filter(|x| x.asleep).map(|x| x.name.as_str()).collect();
    egui::Frame::new()
        .fill(t.field)
        .stroke(Stroke::new(size::HAIRLINE, t.red))
        .corner_radius(CornerRadius::same(radius::MD))
        .inner_margin(egui::Margin::symmetric(space::ROW_X as i8, space::SM as i8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(format!("Remove the worktree {}?", w.name)).font(semibold(text::BODY)).color(t.fg));
            ui.add(egui::Label::new(egui::RichText::new(&w.path).font(theme::mono(text::MONO_SM)).color(t.fg2)).wrap());
            if !live.is_empty() {
                caption(ui, t, &format!("Closes: {}.", live.join(", ")));
            }
            if !asleep.is_empty() {
                caption(ui, t, &format!("Deletes the sleeping session(s): {}.", asleep.join(", ")));
            }
            caption(ui, t, "git refuses if it has changes or another repository inside it; nothing is forced.");
            if let Some(b) = &w.branch {
                let r = ui.checkbox(&mut mem.delete_branch, format!("Then delete the branch {b} (git branch -d keeps it if unmerged)"));
                r.widget_info(|| egui::WidgetInfo::selected(WidgetType::Checkbox, true, mem.delete_branch, "Delete the branch too"));
            }
            ui.horizontal(|ui| {
                if widgets::button(ui, ButtonKind::Danger, Some("trash"), "Remove", None).clicked() {
                    let p: Value = json!({"session_id": s.id, "path": w.path, "force": true, "delete_branch": mem.delete_branch});
                    actions.push(Action::Tool("worktree_remove", p));
                    mem.confirm_remove = None;
                }
                if widgets::button(ui, ButtonKind::Ghost, Some("x"), "Cancel", None).clicked() {
                    mem.confirm_remove = None;
                }
            });
        });
}
