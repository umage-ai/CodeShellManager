//! Popup menus: the pane's ⋯ menu, the sidebar row's context menu, the group menu, the grid
//! presets popover, the run list and the quick-open's group picker. Every item is an
//! [`Action`]; anything that changes state is a tool call.

use egui::{Align2, CornerRadius, Id, Order, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use serde_json::json;

use csm_core::session_sort::SortField;

use crate::app::{Action, ConfirmState, CsmApp, Menu};
use crate::icons;
use crate::model::{GridPreset, GroupMode, LayoutMode, Scope, Status, UiModel};
use crate::theme::{self, Tokens, alpha, icon, radius, size, space, text};
use crate::widgets;

/// One row of a menu.
#[derive(Debug, Clone)]
pub enum Entry {
    Head(String),
    Sep,
    Item {
        icon: &'static str,
        label: String,
        hint: Option<String>,
        danger: bool,
        action: Action,
    },
    Sub {
        icon: &'static str,
        label: String,
        items: Vec<Entry>,
    },
    /// An item with a hover tooltip (Open with: the exact command it runs).
    ItemTip {
        icon: &'static str,
        label: String,
        hint: Option<String>,
        tip: String,
        action: Action,
    },
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

/// The Open with submenu: every entry for the session's kind, each showing its program's file
/// name and, as its tooltip, the exact command it runs (a label can't hide what runs).
pub fn open_with(s: &crate::model::SessionView) -> Entry {
    let mut items: Vec<Entry> = s
        .open_with
        .iter()
        .map(|o| Entry::ItemTip {
            icon: "braces",
            label: o.label.clone(),
            hint: Some(if o.is_default { format!("default · {}", o.program) } else { o.program.clone() }),
            tip: o.command.clone(),
            action: tool("open_with", json!({"session_id": s.id, "entry": o.entry_id})),
        })
        .collect();
    if items.is_empty() {
        items.push(item("search", "No editors yet: detect installed…", tool("open_with_detect", json!({}))));
    }
    items.push(Entry::Sep);
    items.push(item("pencil", "Edit…", Action::OpenWithEditor));
    Entry::Sub { icon: "braces", label: "Open with".into(), items }
}

/// Label of the sibling-worktree submenu (row and pane menus).
pub const SIBLING_WORKTREE: &str = "New session in sibling worktree";
/// Label of the item that opens the Git tab's New worktree input.
pub const NEW_WORKTREE: &str = "New worktree from this branch…";

/// A session whose folder git can read (local or WSL, on a branch): the worktree items apply.
fn in_repo(s: &crate::model::SessionView) -> bool {
    s.branch.is_some() && s.kind != crate::model::Kind::Ssh
}

/// The worktree items of a session's menu (WPF's "New session in sibling worktree ▸" and "New
/// worktree from this branch…"). The submenu lists the repository's other worktrees that have
/// no session, from the last `worktree_list` reply (asked when the menu opens, see [`show`]);
/// picking one is `session_new` with the params the engine gave for it: same kind, command,
/// args and group as this session, placed after it.
fn worktree_entries(model: &UiModel, s: &crate::model::SessionView) -> Vec<Entry> {
    if !in_repo(s) {
        return Vec::new();
    }
    let list = model.git.get(&s.id).and_then(|d| d.worktrees.as_ref());
    let items = match list {
        None => vec![Entry::Head("Reading worktrees…".into())],
        Some(list) => {
            let free: Vec<Entry> = list
                .iter()
                .filter(|w| !w.current && !w.prunable && w.sessions.is_empty() && w.open.is_object())
                .map(|w| Entry::Item {
                    icon: if w.main { "branch" } else { "fork" },
                    label: w.name.clone(),
                    hint: w.branch.clone(),
                    danger: false,
                    action: tool("session_new", w.open.clone()),
                })
                .collect();
            if free.is_empty() { vec![Entry::Head("No other worktree without a session".into())] } else { free }
        }
    };
    vec![Entry::Sub { icon: "fork", label: SIBLING_WORKTREE.into(), items }, item("plus", NEW_WORKTREE, Action::NewWorktree(s.id.clone()))]
}

/// The Claude slash commands the person configured (`ClaudeCommands`, as `settings_get` last
/// told the app; the defaults until it answered), cleaned like the engine cleans them.
pub fn claude_commands(app: &CsmApp) -> Vec<csm_core::models::SlashCommand> {
    use csm_core::models::SlashCommand;
    let list = app
        .ui
        .settings
        .values
        .get("ClaudeCommands")
        .and_then(|v| serde_json::from_value::<Vec<SlashCommand>>(v.clone()).ok())
        .unwrap_or_else(SlashCommand::defaults);
    SlashCommand::clean_list(&list)
}

/// "Claude commands ▸" (the WPF ⚡ Commands panel): each one is typed into the session and
/// entered (`session_send_command`). Only for a live Claude session.
pub fn claude_commands_entry(s: &crate::model::SessionView, presets: &[csm_core::models::SlashCommand]) -> Option<Entry> {
    if !s.is_claude() || !s.status.is_live() || presets.is_empty() {
        return None;
    }
    let items = presets
        .iter()
        .map(|c| Entry::Item {
            icon: "send",
            label: c.command.clone(),
            hint: (!c.description.is_empty()).then(|| c.description.clone()),
            danger: false,
            // The person picked it: like typing, it goes in even mid-turn.
            action: tool("session_send_command", json!({"session_id": s.id, "command": c.command, "force": true})),
        })
        .collect();
    Some(Entry::Sub { icon: "send", label: "Claude commands".into(), items })
}

const DEMO_STATUS: &str = "Demo: set status";

fn demo_status(session: &str) -> Entry {
    Entry::Sub {
        icon: "sliders",
        label: DEMO_STATUS.into(),
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
        open_with(s),
        hinted("terminal", "Open a shell here", "pwsh", tool("session_new", json!({"folder": s.folder, "command": "pwsh", "group": s.group}))),
        item("copy", "Copy path", Action::CopyText(s.folder.clone())),
    ];
    v.extend(worktree_entries(model, s));
    v.extend([
        Entry::Sep,
        Entry::Head("Session".into()),
        hinted("pencil", "Edit session…", "folder, command", Action::EditSession(id.to_string())),
        hinted("tag", "Rename", "F2", Action::StartRename(id.to_string())),
        hinted("copy", "Duplicate", "Ctrl Alt T", tool("session_duplicate", sid.clone())),
        hinted("restart", "Restart", "Ctrl Shift R", tool("session_restart", sid.clone())),
        hinted("moon", "Sleep", "keeps it in the sidebar", tool("session_sleep", sid.clone())),
        demo_status(id),
    ]);
    if s.accent_overridden {
        v.push(item("palette", "Reset accent colour", tool("session_accent_reset", sid.clone())));
    }
    // The window bar's actions, and the header controls a narrow pane leaves out.
    let lay = model.layout(&model.pane_scope());
    let pane = json!({"session_id": id, "group": scope});
    let maxed = lay.maximized.as_deref() == Some(id);
    // A torn-off pane has its OS window's own minimize and maximize; in Focus the one pane
    // already fills the canvas.
    let torn = lay.torn.iter().any(|t| t == id);
    v.push(Entry::Sep);
    if !torn {
        v.push(item("min", "Minimize", tool("pane_minimize", pane.clone())));
        if maxed {
            v.push(item("unmax", "Restore", tool("pane_restore", pane.clone())));
        } else if lay.mode != crate::model::LayoutMode::Focus {
            v.push(item("max", "Maximize", tool("pane_maximize", pane.clone())));
        }
    }
    if torn || lay.floats.iter().any(|f| f.session == id) {
        v.push(item("dock", "Dock", tool("pane_dock", pane.clone())));
    } else {
        v.push(item("float", "Float in this window", tool("pane_float", pane.clone())));
    }
    if !torn {
        v.push(item("tear", "Move to a new window", tool("pane_tear_off", pane.clone())));
    }
    if lay.mode == crate::model::LayoutMode::Side {
        let pinned = lay.pinned.iter().any(|p| p == id);
        let (label, tid) = if pinned { ("Unpin from Side by side", "layout_unpin") } else { ("Pin to Side by side", "layout_pin") };
        v.push(item("pin", label, tool(tid, pane.clone())));
    }
    if let Some(run) = s.default_run() {
        v.push(hinted("play", format!("Run {}", run.label), "F5", tool("run_start", sid.clone())));
    }
    v.push(Entry::Sep);
    v.push(Entry::Item { icon: "x", label: "Close".into(), hint: Some("Ctrl W".into()), danger: true, action: Action::CloseSession(id.to_string()) });
    v
}

/// The sidebar row's menu. `targets` is what an action applies to (C# `ResolveActionTargets`):
/// the whole selection when the row is part of it, else the row alone.
pub fn row_menu(model: &UiModel, id: &str, targets: &[String]) -> Vec<Entry> {
    if targets.len() > 1 {
        return bulk_menu(model, targets);
    }
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
        hinted("tag", "Rename", "F2", Action::StartRename(id.to_string())),
        item("pencil", "Edit session…", Action::EditSession(id.to_string())),
        Entry::Sub { icon: "layers", label: "Move to group".into(), items: groups },
        item("folder", "Open folder", tool("open_folder", sid.clone())),
        open_with(s),
        hinted("copy", "Duplicate session", "Ctrl Alt T", tool("session_duplicate", sid.clone())),
    ];
    v.extend(worktree_entries(model, s));
    v.push(Entry::Sep);
    if s.is_claude() && s.status.is_live() {
        v.push(hinted("eraser", "Clear status", "stuck after a crash?", tool("session_status_clear", sid.clone())));
    }
    if s.accent_overridden {
        v.push(hinted("palette", "Reset accent colour", "set by the program", tool("session_accent_reset", sid.clone())));
    }
    v.push(demo_status(id));
    v.push(Entry::Sep);
    v.push(item("restart", "Restart", tool("session_restart", sid.clone())));
    if s.status.is_live() {
        v.push(item("moon", "Sleep", tool("session_sleep", sid.clone())));
    } else {
        v.push(item("power", "Wake", tool("session_wake", sid.clone())));
    }
    v.push(Entry::Item { icon: "x", label: "Close".into(), hint: None, danger: true, action: Action::CloseSession(id.to_string()) });
    v
}

/// The row menu with several rows selected: each action names its count and applies to all of
/// them (the `*_many` tools). Restart and Close ask first, like the group menu's "… all".
pub fn bulk_menu(model: &UiModel, targets: &[String]) -> Vec<Entry> {
    let n = targets.len();
    let ids = json!(targets);
    let sessions: Vec<&crate::model::SessionView> = targets.iter().filter_map(|id| model.session(id)).collect();
    let live: Vec<String> = sessions.iter().filter(|s| s.status.is_live()).map(|s| s.id.clone()).collect();
    let asleep: Vec<String> = sessions.iter().filter(|s| !s.status.is_live()).map(|s| s.id.clone()).collect();
    let busy = sessions.iter().filter(|s| matches!(s.status, Status::Working | Status::Approval | Status::Question)).count();
    let mut groups: Vec<Entry> = model
        .groups
        .iter()
        .map(|g| {
            let all_here = sessions.iter().all(|s| s.group.as_deref() == Some(g.id.as_str()));
            item(if all_here { "check" } else { "blank" }, g.name.clone(), tool("session_move_many", json!({"session_ids": ids, "group_id": g.id})))
        })
        .collect();
    groups.push(Entry::Sep);
    groups.push(item("blank", "Ungrouped", tool("session_move_many", json!({"session_ids": ids, "group_id": null}))));
    let mut v = vec![
        Entry::Head(format!("{n} sessions selected")),
        Entry::Sub { icon: "layers", label: format!("Move to group ({n})"), items: groups },
        Entry::Sep,
    ];
    if !asleep.is_empty() {
        v.push(item("power", format!("Wake ({})", asleep.len()), tool("session_wake_many", json!({"session_ids": asleep}))));
    }
    if !live.is_empty() {
        let k = live.len();
        let restart = tool("session_restart_many", json!({"session_ids": live, "force": true}));
        v.push(item(
            "restart",
            format!("Restart ({k})…"),
            if k > 1 {
                Action::Confirm(ConfirmState {
                    title: format!("Restart {k} sessions?"),
                    body: "They restart one after another; Claude sessions resume their conversations. Running run commands stop.".into(),
                    yes_label: "Restart".into(),
                    no_label: "Cancel".into(),
                    yes: Box::new(restart),
                })
            } else {
                restart
            },
        ));
        v.push(item("moon", format!("Sleep ({k})"), tool("session_sleep_many", json!({"session_ids": live}))));
    }
    let body = match busy {
        0 => "They go to Recently closed (Ctrl+Shift+T reopens them one by one).".to_string(),
        b => format!(
            "{b} of them {} working or waiting for you; closing cuts that off. They go to Recently closed.",
            if b == 1 { "is" } else { "are" }
        ),
    };
    v.push(Entry::Item {
        icon: "x",
        label: format!("Close ({n})…"),
        hint: None,
        danger: true,
        action: Action::Confirm(ConfirmState {
            title: format!("Close {n} sessions?"),
            body,
            yes_label: "Close".into(),
            no_label: "Cancel".into(),
            yes: Box::new(tool("session_close_many", json!({"session_ids": ids, "force": true}))),
        }),
    });
    v.push(Entry::Sep);
    v.push(hinted("blank", "Clear selection", "Esc", Action::ClearSelection));
    v
}

/// The sort control's menu (C# `BuildSortSessionsMenu`): one-shot sorts of the sidebar scope.
/// Each item's arrow is the direction picking it applies: the field sorted last flips, the
/// others start in their natural direction.
pub fn sort_menu(model: &UiModel, last: Option<(SortField, bool)>) -> Vec<Entry> {
    let scope = match model.group_mode {
        GroupMode::Rail => model.rail.clone(),
        _ => Scope::All,
    };
    let group = match &scope {
        Scope::All => "all".to_string(),
        Scope::Group(g) => g.clone(),
    };
    let entry = |f: SortField| {
        let current = last.filter(|(lf, _)| *lf == f);
        let desc = current.map_or_else(|| f.default_descending(), |(_, d)| !d);
        let words = match (f, desc) {
            (SortField::LastActive, true) => "newest first",
            (SortField::LastActive, false) => "oldest first",
            (SortField::Dirty, true) => "dirty first",
            (SortField::Dirty, false) => "clean first",
            (_, false) => "A to Z",
            (_, true) => "Z to A",
        };
        Entry::Item {
            icon: if desc { "arrowDown" } else { "arrowUp" },
            label: f.label().into(),
            hint: Some(if current.is_some() { format!("again: {words}") } else { words.into() }),
            danger: false,
            action: tool("session_sort", json!({"by": f.id(), "descending": desc, "group": group})),
        }
    };
    let mut v = vec![Entry::Head(format!("Sort {}", if scope == Scope::All { "all sessions".into() } else { model.scope_name(&scope) }))];
    v.extend([SortField::Name, SortField::Folder, SortField::LastActive].map(entry));
    v.push(Entry::Sub { icon: "branch", label: "Git".into(), items: [SortField::Branch, SortField::Dirty, SortField::Repo].map(entry).to_vec() });
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
                    Entry::Item {
                        icon: m.icon(),
                        label: m.label().into(),
                        hint,
                        danger: false,
                        action: tool("layout_set", json!({"group": gid, "mode": m.id()})),
                    }
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
        Entry::Item {
            icon: "x",
            label: "Close all…".into(),
            hint: None,
            danger: true,
            action: tool("group_close_all", json!({"group_id": gid, "force": false})),
        },
    ]);
    v
}

fn runs_menu(model: &UiModel, id: &str) -> Vec<Entry> {
    let Some(s) = model.session(id) else { return vec![] };
    let mut v = vec![Entry::Head("Run commands".into())];
    for r in &s.runs {
        let hint = if r.is_default { format!("F5 · {}", r.detail) } else { r.detail.clone() };
        v.push(Entry::Item {
            icon: "play",
            label: r.label.clone(),
            hint: Some(hint),
            danger: false,
            action: tool("run_start", json!({"session_id": id, "run_id": r.id})),
        });
    }
    v.push(Entry::Sep);
    v.push(item("pencil", "Edit commands…", Action::EditRuns(id.to_string())));
    v
}

/// Draws `entries` in an area at `at`; returns the area's rect.
#[allow(clippy::too_many_arguments)]
fn menu_area(
    ctx: &egui::Context,
    app: &mut CsmApp,
    id: Id,
    at: Pos2,
    entries: &[Entry],
    depth: usize,
    right: bool,
    actions: &mut Vec<Action>,
) -> Vec<Rect> {
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
            // As wide as the widest row, never narrower than the menu width.
            let width = entries.iter().map(|e| row_width(ui, e)).fold(size::MENU_W - space::SM, f32::max);
            ui.set_width(width);
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
                    Entry::ItemTip { icon, label, hint, tip, action } => {
                        let r = widgets::menu_item(ui, icon, label, hint.as_deref(), false).on_hover_text(tip.as_str());
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
                        icons::paint(
                            ui,
                            Rect::from_min_size(
                                pos2(r.rect.max.x - space::ROW_X - icon::MD, r.rect.center().y - icon::MD / 2.0),
                                Vec2::splat(icon::MD),
                            ),
                            "chevR",
                            icon::MD,
                            t.muted,
                        );
                        if r.hovered() || r.clicked() {
                            app.ui.submenu = Some(label.clone());
                        }
                        if open || r.hovered() {
                            sub = Some((
                                pos2(r.rect.max.x + space::XS + space::XXS, r.rect.min.y - space::XS - size::HAIRLINE),
                                items.clone(),
                                label.clone(),
                            ));
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

/// Once per opening of a session's menu, when the sibling submenu opens (not on every right
/// click, as WPF): read its worktrees again (`worktree_list`, git on an engine worker; the reply
/// lands in `model.git` and fills the submenu). Submitted directly rather than as an
/// [`Action::Tool`], which would close the menu.
fn ask_worktrees(ctx: &egui::Context, app: &mut CsmApp, menu: &Menu, session: &str) {
    if app.ui.submenu.as_deref() != Some(SIBLING_WORKTREE) || !app.model.session(session).is_some_and(in_repo) {
        return;
    }
    let key = Id::new("menu-worktrees-asked");
    let tag = format!("{menu:?}");
    if ctx.data(|d| d.get_temp::<String>(key)).as_deref() == Some(tag.as_str()) {
        return;
    }
    ctx.data_mut(|d| d.insert_temp(key, tag));
    app.dispatch.submit("worktree_list", json!({"session_id": session}));
}

/// The review's status switcher only where `demo_set_status` exists (the demo engine).
fn with_demo(mut v: Vec<Entry>, demo: bool) -> Vec<Entry> {
    if !demo {
        v.retain(|e| !matches!(e, Entry::Sub { label, .. } if label == DEMO_STATUS));
    }
    v
}

pub fn show(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(menu) = app.ui.menu.clone() else {
        // Closed: the next opening reads the worktrees again.
        ctx.data_mut(|d| d.remove_temp::<String>(Id::new("menu-worktrees-asked")));
        return;
    };
    let model = app.model.clone();
    let demo = app.dispatch.registry().get("demo_set_status").is_some();
    if let Menu::Pane { session, .. } | Menu::Row { session, .. } = &menu {
        ask_worktrees(ctx, app, &menu, session);
    }
    let rects = match &menu {
        Menu::Pane { session, at } => {
            let mut e = with_demo(pane_menu(&model, session), demo);
            if let Some(sub) = model.session(session).and_then(|s| claude_commands_entry(s, &claude_commands(app))) {
                // At the end of the first block, before the Session section.
                let at = e.iter().position(|x| matches!(x, Entry::Head(h) if h == "Session")).map_or(e.len(), |i| i.saturating_sub(1));
                e.insert(at, sub);
            }
            let at = pos2(at.x - size::MENU_W, at.y);
            menu_area(ctx, app, Id::new("menu-pane"), at, &e, 0, false, actions)
        }
        Menu::Row { session, at } => {
            let order: Vec<&str> = model.sessions.iter().map(|s| s.id.as_str()).collect();
            let targets = app.ui.selection.targets(session, &order);
            menu_area(
                ctx,
                app,
                Id::new("menu-row"),
                *at + vec2(space::XS + space::XXS, -space::XS),
                &with_demo(row_menu(&model, session, &targets), demo),
                0,
                false,
                actions,
            )
        }
        Menu::Sort { at } => {
            let e = sort_menu(&model, app.ui.sort);
            menu_area(ctx, app, Id::new("menu-sort"), *at, &e, 0, false, actions)
        }
        Menu::Group { group, at } => menu_area(ctx, app, Id::new("menu-group"), *at, &group_menu(&model, group), 0, false, actions),
        Menu::Runs { session, at } => {
            menu_area(ctx, app, Id::new("menu-runs"), pos2(at.x - size::MENU_W, at.y), &runs_menu(&model, session), 0, false, actions)
        }
        Menu::Grid { at } => vec![grid_popover(ctx, app, *at, actions)],
        Menu::QuickGroup { at } => {
            let mut e = vec![Entry::Head("Group".into())];
            for g in &model.groups {
                e.push(item(
                    if app.ui.quick.group.as_deref() == Some(g.id.as_str()) { "check" } else { "blank" },
                    g.name.clone(),
                    Action::Toast(format!("quick-group:{}", g.id)),
                ));
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
                        let glyph = Rect::from_min_size(
                            pos2(rect.center().x - size::GRID_GLYPH[0] / 2.0, rect.min.y + space::SM),
                            vec2(size::GRID_GLYPH[0], size::GRID_GLYPH[1]),
                        );
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
                                ui.painter().extend(egui::Shape::dashed_line(
                                    &[glyph.left_top(), glyph.right_top(), glyph.right_bottom(), glyph.left_bottom(), glyph.left_top()],
                                    Stroke::new(size::HAIRLINE, t.surface2),
                                    space::XS,
                                    space::XS - size::HAIRLINE,
                                ));
                                icons::paint(ui, glyph, "plus", icon::MD, t.muted);
                                "Custom…".into()
                            }
                        };
                        ui.painter().text(
                            pos2(rect.center().x, glyph.max.y + space::SM + space::XS),
                            Align2::CENTER_CENTER,
                            &label,
                            theme::regular(text::CAPTION),
                            if on { t.fg } else { t.fg2 },
                        );
                        r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Grid {label}")));
                        if r.clicked() {
                            match tile {
                                Some(g) => {
                                    actions.push(Action::Tool(
                                        "layout_set",
                                        json!({"group": scope.key(), "mode": "grid", "cols": g.cols, "rows": g.rows}),
                                    ));
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
                        actions.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": "grid", "cols": c, "rows": r})));
                        actions.push(Action::CloseMenu);
                    }
                });
                ui.data_mut(|d| d.insert_temp(key, (c, r)));
            }
        });
    });
    resp.response.rect
}

/// The width a menu row needs: icon, label, hint or submenu chevron, padding.
fn row_width(ui: &egui::Ui, e: &Entry) -> f32 {
    let label = |s: &str| widgets::text_width(ui, s, theme::regular(text::BODY));
    let base = space::SM + icon::LG + space::ROW_X * 2.0;
    match e {
        Entry::Item { label: l, hint, .. } | Entry::ItemTip { label: l, hint, .. } => {
            base + label(l) + hint.as_ref().map_or(0.0, |h| widgets::text_width(ui, h, theme::mono(text::TINY)) + space::LG)
        }
        Entry::Sub { label: l, .. } => base + label(l) + icon::MD + space::LG,
        Entry::Head(_) | Entry::Sep => 0.0,
    }
}
