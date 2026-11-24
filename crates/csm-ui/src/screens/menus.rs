//! Popup menus: the pane's ⋯ menu, the sidebar row's context menu, the group menu, the grid
//! presets popover, the run list and the quick-open's group picker. Every item is an
//! [`Action`]; anything that changes state is a tool call.

use egui::{Align2, CornerRadius, Id, Key, Modifiers, Order, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2, pos2, vec2};
use serde_json::json;

use csm_core::keymap::Keymap;
use csm_core::session_sort::SortField;

use crate::app::{Action, ConfirmState, CsmApp, Menu, consume_exact};
use crate::icons;
use crate::menu_nav::{self, Keys, Nav, Row};
use crate::model::{GridPreset, GroupMode, LayoutMode, Scope, Status, UiModel};
use crate::theme::{self, Tokens, alpha, icon, radius, size, space, text};
use crate::widgets;

/// One row of a menu.
/// Whether a remote window hides this menu entry: what runs on this laptop (open folder, open with,
/// backup) is not offered there. Submenus that are only local (Open with) go with their parent.
pub fn hidden_in_remote(entry: &Entry) -> bool {
    // "Open a shell here" starts a shell in this laptop's view of a folder: a laptop-only item (the
    // office's shells are the sessions the window already shows).
    if let Entry::Item { label, .. } = entry
        && label == "Open a shell here"
    {
        return true;
    }
    match entry {
        Entry::Item { action, .. } | Entry::ItemTip { action, .. } => match action {
            Action::Tool(id, _) => crate::remote_window::is_local_only(id),
            Action::ToolNamed(id, _) | Action::PaletteTool(id) => crate::remote_window::is_local_only(id),
            Action::OpenWithEditor => true,
            _ => false,
        },
        Entry::Sub { label, .. } => label.eq_ignore_ascii_case("Open with"),
        Entry::Head(_) | Entry::Sep | Entry::Off { .. } => false,
    }
}

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
    /// A row shown but not offered now (a tool that needs an active session, with none): muted,
    /// and the keys skip it.
    Off {
        icon: &'static str,
        label: String,
        hint: Option<String>,
    },
}

fn item(icon: &'static str, label: impl Into<String>, action: Action) -> Entry {
    Entry::Item { icon, label: label.into(), hint: None, danger: false, action }
}

fn hinted(icon: &'static str, label: impl Into<String>, hint: &str, action: Action) -> Entry {
    Entry::Item { icon, label: label.into(), hint: Some(hint.into()), danger: false, action }
}

/// The first chord of an action in the keyboard map, as a menu hint ("" when it has none).
fn chord_hint(keys: &Keymap, id: &str) -> String {
    keys.label(id).unwrap_or_default()
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

/// A session whose folder is on this machine (local or WSL): Open folder, Open with and a shell there apply.
fn is_local(s: &crate::model::SessionView) -> bool {
    s.kind != crate::model::Kind::Ssh
}

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
    if !app.model.slash_presets.is_empty() {
        return app.model.slash_presets.clone();
    }
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

/// "Copy last reply" and "Copy reply…" on a Claude pane: Claude's replies from its transcript
/// (`claude_reply_get`, `claude_reply_list`). A WSL or SSH session's answer says it is not
/// available there.
fn claude_copy_entries(s: &crate::model::SessionView, keys: &Keymap) -> Vec<Entry> {
    if !s.is_claude() || !s.status.is_live() {
        return Vec::new();
    }
    vec![
        hinted("copy", "Copy last reply", &chord_hint(keys, "claude_reply_get"), Action::CopyClaude("claude_reply_get", json!({"session_id": s.id}))),
        item("copy", "Copy reply…", Action::OpenClaudeReplies(s.id.clone())),
    ]
}

/// The longest preview in the reply picker (characters).
const PICK_PREVIEW: usize = 48;

fn short(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

/// The "Copy reply…" picker: the recent replies (newest first), each followed by its code blocks
/// as entries of their own. Each copies through `claude_reply_get` / `claude_code_block_get` by
/// the reply's id, so a reply written meanwhile doesn't shift what is copied.
pub fn claude_replies_menu(state: Option<&crate::app::ClaudeReplies>, session: &str) -> Vec<Entry> {
    let mut v = vec![Entry::Head("Copy Claude's reply".into())];
    let rows = match state.filter(|r| r.session == session).map(|r| &r.rows) {
        None | Some(None) => {
            v.push(Entry::Head("Reading replies…".into()));
            return v;
        }
        Some(Some(Err(e))) => {
            v.push(Entry::Head(short(e, 60)));
            return v;
        }
        Some(Some(Ok(rows))) => rows,
    };
    if rows.is_empty() {
        v.push(Entry::Head("Claude has not replied yet".into()));
        return v;
    }
    for (i, r) in rows.iter().enumerate() {
        if i > 0 {
            v.push(Entry::Sep);
        }
        let id = r["id"].as_str().unwrap_or_default();
        let lines = r["lines"].as_u64().unwrap_or(0);
        let lines = if lines == 1 { "1 line".to_string() } else { format!("{lines} lines") };
        let preview = r["preview"].as_str().unwrap_or_default();
        let label = if preview.is_empty() { "(no text)".to_string() } else { short(preview, PICK_PREVIEW) };
        v.push(Entry::Item {
            icon: "copy",
            label,
            hint: Some(if i == 0 { format!("latest · {lines}") } else { lines }),
            danger: false,
            action: Action::CopyClaude("claude_reply_get", json!({"session_id": session, "reply_id": id})),
        });
        for b in r["code_blocks"].as_array().into_iter().flatten() {
            let k = b["index"].as_u64().unwrap_or(0);
            let lang = b["lang"].as_str().map(|l| format!("{l} ")).unwrap_or_default();
            let preview = b["preview"].as_str().unwrap_or_default();
            v.push(item(
                "braces",
                format!("Copy code block {}: {lang}{}", k + 1, short(preview, PICK_PREVIEW / 2)),
                Action::CopyClaude("claude_code_block_get", json!({"session_id": session, "reply_id": id, "block_index": k})),
            ));
        }
    }
    v
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

pub fn pane_menu(model: &UiModel, id: &str, keys: &Keymap) -> Vec<Entry> {
    let Some(s) = model.session(id) else { return vec![] };
    let sid = json!({"session_id": id});
    let scope = model.pane_scope().key();
    let insp = |tab: &str| tool("inspector_show", json!({"tab": tab}));
    let mut v = vec![
        Entry::Head("Panels".into()),
        hinted("note", "Notes", &chord_hint(keys, "ui.inspector_notes"), insp("notes")),
        hinted("play", "Runs", &format!("{} runs default", chord_hint(keys, "run_start")), insp("runs")),
        item("branch", "Git", insp("git")),
        Entry::Sep,
    ];
    // An SSH (or NEXUS) session's folder is on another machine: nothing here can open it, so
    // only its path is offered.
    if is_local(s) {
        v.extend([
            item("folder", "Open folder", tool("open_folder", sid.clone())),
            open_with(s),
            hinted("terminal", "Open a shell here", "pwsh", tool("session_new", json!({"folder": s.folder, "command": "pwsh", "group": s.group}))),
            item("copy", "Copy path", Action::CopyText(s.folder.clone())),
        ]);
    } else {
        v.push(item("copy", "Copy remote path", Action::CopyText(s.folder.clone())));
    }
    v.extend(claude_copy_entries(s, keys));
    v.extend(worktree_entries(model, s));
    v.extend([
        Entry::Sep,
        Entry::Head("Session".into()),
        hinted("pencil", "Edit session…", "folder, command", Action::EditSession(id.to_string())),
        hinted("tag", "Rename", &chord_hint(keys, "session_rename"), Action::StartRename(id.to_string())),
        hinted("copy", "Duplicate", &chord_hint(keys, "session_duplicate"), tool("session_duplicate", sid.clone())),
        hinted("restart", "Restart", &chord_hint(keys, "session_restart"), tool("session_restart", sid.clone())),
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
        v.push(hinted("play", format!("Run {}", run.label), &chord_hint(keys, "run_start"), tool("run_start", sid.clone())));
    }
    v.push(Entry::Sep);
    v.push(Entry::Item { icon: "x", label: "Close".into(), hint: Some(chord_hint(keys, "session_close")), danger: true, action: Action::CloseSession(id.to_string()) });
    v
}

/// The sidebar row's menu. `targets` is what an action applies to (C# `ResolveActionTargets`):
/// the whole selection when the row is part of it, else the row alone.
pub fn row_menu(model: &UiModel, id: &str, targets: &[String], keys: &Keymap) -> Vec<Entry> {
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
        hinted("tag", "Rename", &chord_hint(keys, "session_rename"), Action::StartRename(id.to_string())),
        item("pencil", "Edit session…", Action::EditSession(id.to_string())),
        Entry::Sub { icon: "layers", label: "Move to group".into(), items: groups },
        hinted("copy", "Duplicate session", &chord_hint(keys, "session_duplicate"), tool("session_duplicate", sid.clone())),
    ];
    if is_local(s) {
        // After "Move to group", as before; an SSH session's folder is on another machine.
        let at = v.len() - 1;
        v.insert(at, open_with(s));
        v.insert(at, item("folder", "Open folder", tool("open_folder", sid.clone())));
    }
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
        also: None,
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
        also: None,
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

pub fn group_menu(model: &UiModel, gid: &str, keys: &Keymap) -> Vec<Entry> {
    let Some(g) = model.group(gid) else { return vec![] };
    let scope = Scope::Group(gid.to_string());
    let lay = model.layout(&scope);
    let live = model.scope_sessions(&scope).iter().filter(|s| s.status.is_live()).count();
    let gp = json!({"group_id": gid});
    let mut v = vec![
        Entry::Head(g.name.clone()),
        item("plus", format!("New session in {}", g.name), Action::OpenQuickOpen { group: Some(gid.to_string()) }),
        hinted("pencil", "Rename group", &chord_hint(keys, "session_rename"), Action::StartGroupRename(gid.to_string())),
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
        item("send", "Send to all sessions…", Action::BroadcastPrompt(gid.to_string())),
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

fn runs_menu(model: &UiModel, id: &str, keys: &Keymap) -> Vec<Entry> {
    let Some(s) = model.session(id) else { return vec![] };
    let mut v = vec![Entry::Head("Run commands".into())];
    for r in &s.runs {
        let hint = if r.is_default { format!("{} · {}", chord_hint(keys, "run_start"), r.detail) } else { r.detail.clone() };
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

/// The Git tab's GitHub links: the branch's page and the pull requests, through `open_url`.
fn github_menu(model: &UiModel, id: &str) -> Vec<Entry> {
    let Some(gh) = model.git.get(id).and_then(|d| d.github.as_ref()) else { return vec![] };
    vec![
        Entry::Head("GitHub".into()),
        item(
            "branch",
            if model.session(id).is_some_and(|s| s.branch.is_some()) { "Open branch on GitHub" } else { "Open repository on GitHub" },
            tool("open_url", json!({"url": gh.branch_url})),
        ),
        item("history", "Open pull requests", tool("open_url", json!({"url": gh.pulls_url}))),
    ]
}

/// Draws `entries` in an area at `at`; returns the area's rect. The row highlighted at this depth
/// (`menu_sel`, by the keyboard or the pointer) gets the keyboard highlight, and the pointer over a
/// row moves the highlight there.
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
    // A remote window offers nothing that runs on this laptop.
    let offered: Vec<Entry>;
    let entries = if app.remote.is_some() {
        offered = entries.iter().filter(|e| !hidden_in_remote(e)).cloned().collect();
        offered.as_slice()
    } else {
        entries
    };
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let mut rects = Vec::new();
    let mut sub: Option<(Pos2, Vec<Entry>, String)> = None;
    let acc = access_of(entries);
    let lit = app.ui.menu_sel.get(depth).copied();
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
    // The height the menu had last frame (its content, measured below) is the better guess; the estimate
    // is for the first frame only.
    let height_key = Id::new(("menu-height", id));
    let last_h = ctx.data(|d| d.get_temp::<f32>(height_key)).unwrap_or(0.0);
    let guess_h = if last_h > 0.0 { last_h } else { est_h };
    let mut pos = at;
    if pos.y + guess_h > screen.max.y - space::SM {
        pos.y = (screen.max.y - space::SM - guess_h).max(screen.min.y + space::SM);
    }
    if !right && pos.x + size::MENU_W + space::LG > screen.max.x {
        pos.x = (screen.max.x - size::MENU_W - space::LG).max(screen.min.x);
    }
    let pivot = if right { Align2::RIGHT_TOP } else { Align2::LEFT_TOP };
    // A menu taller than the room below it scrolls (the main menu's long areas); a key that moves
    // the highlight brings its row into view (`scroll_to_me` below).
    let room = (screen.max.y - space::SM - pos.y).max(size::MENU_ROW);
    let scroll = guess_h > room;
    // Only a key brings the highlighted row into view: the pointer over a row moves the highlight
    // too, and scrolling to it would move the menu under the pointer.
    let by_key = ctx.data(|d| d.get_temp::<bool>(menu_keys_id())).unwrap_or(false);
    let resp = egui::Area::new(id).order(Order::Foreground).pivot(pivot).fixed_pos(pos).show(ctx, |ui| {
        let mut frame = |ui: &mut egui::Ui| {
        let body = widgets::pop_frame(&t).show(ui, |ui| {
            // As wide as the widest row, never narrower than the menu width.
            let width = entries.iter().map(|e| row_width(ui, e)).fold(size::MENU_W - space::SM, f32::max);
            ui.set_width(width);
            ui.spacing_mut().item_spacing.y = 0.0;
            for (idx, e) in entries.iter().enumerate() {
                let on = lit == Some(idx);
                let under = acc.get(idx).copied().flatten().map(|(c, _)| c);
                match e {
                    Entry::Head(h) => widgets::menu_head(ui, h),
                    Entry::Sep => widgets::separator(ui),
                    Entry::Item { icon, label, hint, danger, action } => {
                        let r = widgets::menu_item_at(ui, icon, label, hint.as_deref(), *danger, on, under);
                        if on && scroll && by_key {
                            r.scroll_to_me(Some(egui::Align::Center));
                        }
                        if r.hovered() {
                            if depth == 0 {
                                app.ui.submenu = None;
                            }
                            highlight(&mut app.ui.menu_sel, depth, idx);
                        }
                        if r.clicked() {
                            actions.push(action.clone());
                            // An item that opens another menu in this one's place keeps it open.
                            if !matches!(action, Action::OpenClaudeReplies(_)) {
                                actions.push(Action::CloseMenu);
                            }
                        }
                    }
                    Entry::ItemTip { icon, label, hint, tip, action } => {
                        let r = widgets::menu_item_at(ui, icon, label, hint.as_deref(), false, on, under).on_hover_text(tip.as_str());
                        if r.hovered() {
                            if depth == 0 {
                                app.ui.submenu = None;
                            }
                            highlight(&mut app.ui.menu_sel, depth, idx);
                        }
                        if r.clicked() {
                            actions.push(action.clone());
                            actions.push(Action::CloseMenu);
                        }
                    }
                    Entry::Off { icon, label, hint } => {
                        widgets::menu_item_off(ui, icon, label, hint.as_deref());
                    }
                    Entry::Sub { icon, label, items } => {
                        let open = app.ui.submenu.as_deref() == Some(label.as_str());
                        let r = widgets::menu_item_at(ui, icon, label, None, false, on, under);
                        if open {
                            ui.painter().rect_filled(r.rect, CornerRadius::same(radius::sm() + 1), Tokens::tint(t.row_sel, alpha::ASLEEP));
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
                            highlight(&mut app.ui.menu_sel, depth, idx);
                        }
                        if open || r.hovered() {
                            sub = Some((
                                pos2(r.rect.max.x + space::XS + space::XXS, r.rect.min.y - space::XS - size::HAIRLINE),
                                items.clone(),
                                label.clone(),
                            ));
                        }
                        if on && scroll && by_key {
                            r.scroll_to_me(Some(egui::Align::Center));
                        }
                    }
                }
            }
        });
        ctx.data_mut(|d| d.insert_temp(height_key, body.response.rect.height()));
        };
        if scroll {
            egui::ScrollArea::vertical().max_height(room).min_scrolled_height(room).auto_shrink([false, false]).show(ui, frame);
        } else {
            frame(ui);
        }
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

/// A list menu as it is drawn: its area's id and place, and its entries as offered here.
struct Listing {
    id: &'static str,
    at: Pos2,
    entries: Vec<Entry>,
}

/// The list a menu shows, or `None` for the menus that are not lists (the grid, Recent).
fn listing(app: &CsmApp, menu: &Menu) -> Option<Listing> {
    let model = &app.model;
    let keys = &app.prefs.keymap;
    let demo = app.dispatch.has_tool("demo_set_status");
    let (id, at, entries) = match menu {
        Menu::Pane { session, at } => {
            let mut e = with_demo(pane_menu(model, session, keys), demo);
            if let Some(sub) = model.session(session).and_then(|s| claude_commands_entry(s, &claude_commands(app))) {
                // At the end of the first block, before the Session section.
                let idx = e.iter().position(|x| matches!(x, Entry::Head(h) if h == "Session")).map_or(e.len(), |i| i.saturating_sub(1));
                e.insert(idx, sub);
            }
            ("menu-pane", pos2(at.x - size::MENU_W, at.y), e)
        }
        Menu::Row { session, at } => {
            let order: Vec<&str> = model.sessions.iter().map(|s| s.id.as_str()).collect();
            let targets = app.ui.selection.targets(session, &order);
            ("menu-row", *at + vec2(space::XS + space::XXS, -space::XS), with_demo(row_menu(model, session, &targets, keys), demo))
        }
        Menu::Sort { at } => ("menu-sort", *at, sort_menu(model, app.ui.sort)),
        Menu::Group { group, at } => ("menu-group", *at, group_menu(model, group, keys)),
        Menu::Runs { session, at } => ("menu-runs", pos2(at.x - size::MENU_W, at.y), runs_menu(model, session, keys)),
        Menu::GitHub { session, at } => ("menu-github", pos2(at.x - size::MENU_W, at.y), github_menu(model, session)),
        Menu::ClaudeReplies { session, at } => {
            ("menu-claude-replies", pos2(at.x - size::MENU_W, at.y), claude_replies_menu(app.ui.claude_replies.as_ref(), session))
        }
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
            ("menu-qgroup", *at, e)
        }
        Menu::Main { at } => ("menu-main", *at, main_entries(app)),
        Menu::Grid { .. } | Menu::Recent { .. } => return None,
    };
    Some(Listing { id, at, entries: offered_here(entries, app.remote.is_some()) })
}

/// The main menu's areas, in the order they are drawn.
pub const MAIN_AREAS: [&str; 8] = ["Session", "View / Layout", "Groups", "Run", "Git", "Add-ons", "Settings", "Help"];

/// The main menu's area for a tool, by its toolset. Board tasks and the layout tools are the
/// view's; notes, search and the sessions themselves are the session's.
pub fn main_area(toolset: csm_engine::Toolset) -> &'static str {
    use csm_engine::Toolset;
    match toolset {
        Toolset::Core | Toolset::Search | Toolset::Notes => "Session",
        Toolset::Layout | Toolset::Tasks => "View / Layout",
        Toolset::Groups => "Groups",
        Toolset::Runs => "Run",
        Toolset::Git => "Git",
        Toolset::Addons => "Add-ons",
        Toolset::App => "Settings",
    }
}

/// Whether a tool acts on one session, so it is off while no session is active.
fn needs_session(schema: &serde_json::Value) -> bool {
    schema.get("required").and_then(serde_json::Value::as_array).is_some_and(|r| r.iter().any(|v| v.as_str() == Some("session_id")))
}

/// The main menu: every action the app offers, in areas (`MAIN_AREAS`). The tools come from the
/// registry, so a new tool appears here without UI work (the palette's list, minus what the
/// palette hides); the UI's own actions and the layout modes are added by hand. Tools that act on
/// a session are shown muted while none is active.
pub fn main_entries(app: &CsmApp) -> Vec<Entry> {
    let model = &app.model;
    let keys = &app.prefs.keymap;
    let has_session = model.active.as_deref().is_some_and(|a| model.session(a).is_some());
    let mut areas: Vec<Vec<Entry>> = MAIN_AREAS.iter().map(|_| Vec::new()).collect();
    let mut put = |area: &str, e: Entry| {
        if let Some(slot) = MAIN_AREAS.iter().position(|a| *a == area).and_then(|i| areas.get_mut(i)) {
            slot.push(e);
        }
    };
    let hint = |s: &str| keys.label(s);
    let own = |label: &str, hint: &str, action: Action| Entry::Item {
        icon: "blank",
        label: label.into(),
        hint: Some(hint.into()),
        danger: false,
        action,
    };
    put("Settings", own("Settings…", &chord_hint(keys, "ui.settings"), Action::OpenSettings));
    put("Help", own("Keyboard shortcuts…", &chord_hint(keys, "ui.help"), Action::ShowHelp));
    put("Help", own("Command palette…", &chord_hint(keys, "ui.palette"), Action::OpenPalette));
    let scope = model.pane_scope().key();
    for mode in LayoutMode::ALL {
        put(
            "View / Layout",
            Entry::Item {
                icon: "blank",
                label: format!("Layout: {}", mode.label()),
                hint: None,
                danger: false,
                action: Action::Tool("layout_set", json!({"group": scope, "mode": mode.id()})),
            },
        );
    }
    for t in app.dispatch.tools().iter() {
        // Layout modes have their rows above (the generic layout_set cycles, which a menu does not
        // need); a consent answer is the consent card's, not a command.
        if t.id == "consent_answer" || t.id == "layout_set" || crate::screens::palette::tool_hidden(app, &t.id) {
            continue;
        }
        let hint = hint(&t.id);
        let area = main_area(t.toolset);
        if needs_session(&t.input_schema) && !has_session {
            put(area, Entry::Off { icon: "blank", label: t.title.clone(), hint });
        } else {
            put(area, Entry::Item { icon: "blank", label: t.title.clone(), hint, danger: false, action: Action::PaletteTool(t.id.clone()) });
        }
    }
    let mut out = Vec::new();
    for (name, mut items) in MAIN_AREAS.iter().zip(areas) {
        if items.is_empty() {
            continue;
        }
        // Each area reads alphabetically, so a row is found by its name.
        items.sort_by(|a, b| sort_label(a).cmp(sort_label(b)));
        out.push(Entry::Sub { icon: "blank", label: (*name).to_string(), items });
    }
    out
}

/// The title a row sorts by (its label; "" for a row without one).
fn sort_label(e: &Entry) -> &str {
    match e {
        Entry::Item { label, .. } | Entry::ItemTip { label, .. } | Entry::Sub { label, .. } | Entry::Off { label, .. } => label.as_str(),
        Entry::Head(_) | Entry::Sep => "",
    }
}

/// Where the main menu opens: under the title bar's button (right-aligned to it) when that is drawn,
/// else under the title bar at the left (F10).
pub fn main_menu_at(ctx: &egui::Context) -> Pos2 {
    let screen = ctx.content_rect();
    match ctx.data(|d| d.get_temp::<Rect>(Id::new(crate::app::MAIN_ANCHOR))) {
        Some(r) => pos2((r.max.x - size::MENU_W).max(screen.min.x), r.max.y + space::XS),
        None => screen.min + vec2(space::MD, size::TITLE_BAR),
    }
}

/// The entries a remote window offers (recursively into submenus).
fn offered_here(entries: Vec<Entry>, remote: bool) -> Vec<Entry> {
    if !remote {
        return entries;
    }
    entries
        .into_iter()
        .filter(|e| !hidden_in_remote(e))
        .map(|e| match e {
            Entry::Sub { icon, label, items } => Entry::Sub { icon, label, items: offered_here(items, true) },
            other => other,
        })
        .collect()
}

fn label_of(e: &Entry) -> Option<&str> {
    match e {
        Entry::Item { label, .. } | Entry::ItemTip { label, .. } | Entry::Sub { label, .. } => Some(label.as_str()),
        Entry::Head(_) | Entry::Sep | Entry::Off { .. } => None,
    }
}

/// The access letter of each row of a level (see [`menu_nav::access`]).
fn access_of(entries: &[Entry]) -> Vec<Option<(usize, char)>> {
    let labels: Vec<Option<&str>> = entries.iter().map(label_of).collect();
    menu_nav::access(&labels)
}

/// The rows of a level as the keys see them: what the highlight can rest on, and each access letter.
fn nav_rows(entries: &[Entry]) -> Vec<Row> {
    access_of(entries)
        .into_iter()
        .zip(entries)
        .map(|(a, e)| Row { selectable: label_of(e).is_some(), letter: a.map(|(_, c)| c) })
        .collect()
}

/// The entries of the level at the end of `chain`: the submenus opened, by their row index.
fn level<'a>(entries: &'a [Entry], chain: &[usize]) -> Option<&'a [Entry]> {
    chain.iter().try_fold(entries, |lvl, i| match lvl.get(*i)? {
        Entry::Sub { items, .. } => Some(items.as_slice()),
        _ => None,
    })
}

/// Highlights row `idx` of the level at `depth`, closing the levels below it (a key or a hover).
fn highlight(sel: &mut Vec<usize>, depth: usize, idx: usize) {
    if sel.get(depth) != Some(&idx) {
        sel.truncate(depth);
        sel.push(idx);
    }
}

/// The highlight a menu opened from the keyboard starts on: its first row (the Recent menu's
/// first row). Empty when no row can take it.
pub fn first_highlight(app: &CsmApp, menu: &Menu) -> Vec<usize> {
    if matches!(menu, Menu::Recent { .. }) {
        return if crate::screens::recent::items(&app.model).is_empty() { Vec::new() } else { vec![0] };
    }
    listing(app, menu).and_then(|l| menu_nav::first(&nav_rows(&l.entries))).map(|i| vec![i]).unwrap_or_default()
}

/// Every letter key and the letter it types: the access keys.
const LETTERS: [(Key, char); 26] = [
    (Key::A, 'a'),
    (Key::B, 'b'),
    (Key::C, 'c'),
    (Key::D, 'd'),
    (Key::E, 'e'),
    (Key::F, 'f'),
    (Key::G, 'g'),
    (Key::H, 'h'),
    (Key::I, 'i'),
    (Key::J, 'j'),
    (Key::K, 'k'),
    (Key::L, 'l'),
    (Key::M, 'm'),
    (Key::N, 'n'),
    (Key::O, 'o'),
    (Key::P, 'p'),
    (Key::Q, 'q'),
    (Key::R, 'r'),
    (Key::S, 's'),
    (Key::T, 't'),
    (Key::U, 'u'),
    (Key::V, 'v'),
    (Key::W, 'w'),
    (Key::X, 'x'),
    (Key::Y, 'y'),
    (Key::Z, 'z'),
];

/// The digit keys and the digit they type (the Recent menu's access keys).
const DIGITS: [(Key, char); 10] = [
    (Key::Num0, '0'),
    (Key::Num1, '1'),
    (Key::Num2, '2'),
    (Key::Num3, '3'),
    (Key::Num4, '4'),
    (Key::Num5, '5'),
    (Key::Num6, '6'),
    (Key::Num7, '7'),
    (Key::Num8, '8'),
    (Key::Num9, '9'),
];

/// Reads the menu's keys this frame and consumes them, so neither the terminal nor the shortcuts
/// take them. Typed text is dropped too: an open menu has the keyboard. `digits`: the digit keys
/// are access keys as well (Recent).
fn take_keys(ctx: &egui::Context, digits: bool) -> Keys {
    ctx.input_mut(|i| {
        let mut k = Keys {
            up: consume_exact(i, Modifiers::NONE, Key::ArrowUp) || consume_exact(i, Modifiers::SHIFT, Key::Tab),
            down: consume_exact(i, Modifiers::NONE, Key::ArrowDown) || consume_exact(i, Modifiers::NONE, Key::Tab),
            home: consume_exact(i, Modifiers::NONE, Key::Home),
            end: consume_exact(i, Modifiers::NONE, Key::End),
            enter: consume_exact(i, Modifiers::NONE, Key::Enter) || consume_exact(i, Modifiers::NONE, Key::Space),
            forward: consume_exact(i, Modifiers::NONE, Key::ArrowRight),
            back: consume_exact(i, Modifiers::NONE, Key::ArrowLeft),
            escape: consume_exact(i, Modifiers::NONE, Key::Escape),
            letter: None,
        };
        k.letter = LETTERS.iter().chain(DIGITS.iter().filter(|_| digits)).find_map(|(key, c)| consume_exact(i, Modifiers::NONE, *key).then_some(*c));
        i.events.retain(|e| !matches!(e, egui::Event::Text(_)));
        k
    })
}

/// An open menu is modal for the plain keys it does not read (Backspace, Delete, the F-keys): they
/// reach neither the shortcuts nor the focused terminal. Escape (the grid closes with it), and the
/// Ctrl and Alt chords, go on. Called by the app after `menu_keys` took the keys it reads.
pub fn drop_plain_keys(ctx: &egui::Context) {
    ctx.input_mut(|i| {
        i.events.retain(|e| {
            !matches!(e, egui::Event::Key { key, modifiers, .. } if *key != Key::Escape && !modifiers.command && !modifiers.ctrl && !modifiers.alt)
        });
    });
}

/// The temporary flag that a menu key was pressed this frame (`menu_keys` sets it, the menus read it).
fn menu_keys_id() -> Id {
    Id::new("menu-keys-pressed")
}

/// The open menu's keys, when it reads them (every list menu and the Recent menu; the grid does
/// not, its Escape is the app's). `Some(actions)` when the menu took keys this frame: the caller
/// runs the actions and skips the frame's other shortcuts. `None` when the keys go on.
pub fn menu_keys(ctx: &egui::Context, app: &mut CsmApp) -> Option<Vec<Action>> {
    let menu = app.ui.menu.clone()?;
    if matches!(menu, Menu::Recent { .. }) {
        return recent_keys(ctx, app);
    }
    let list = listing(app, &menu)?;
    let keys = take_keys(ctx, false);
    if keys == Keys::default() {
        return None;
    }
    // A key moved the highlight: this frame the menu brings the highlighted row into view.
    ctx.data_mut(|d| d.insert_temp(menu_keys_id(), true));
    // The highlight path: one row index per open level; the last is the level the keys act on.
    let mut path = app.ui.menu_sel.clone();
    let depth = path.len().saturating_sub(1);
    let sel = path.last().copied();
    let chain: Vec<usize> = path.iter().take(depth).copied().collect();
    let Some(lvl) = level(&list.entries, &chain) else {
        app.ui.menu_sel.clear();
        return Some(Vec::new());
    };
    let nav = menu_nav::navigate(keys, sel, &nav_rows(lvl), !chain.is_empty());
    let activates = matches!(nav, Nav::Activate(_));
    let mut acts = Vec::new();
    match nav {
        Nav::Nothing => {}
        Nav::Highlight(i) => highlight(&mut path, depth, i),
        Nav::Activate(i) | Nav::Forward(i) => match lvl.get(i) {
            Some(Entry::Sub { label, items, .. }) => {
                highlight(&mut path, depth, i);
                path.push(menu_nav::first(&nav_rows(items)).unwrap_or(0));
                app.ui.submenu = Some(label.clone());
            }
            Some(Entry::Item { action, .. }) if activates => {
                acts.push(action.clone());
                if !matches!(action, Action::OpenClaudeReplies(_)) {
                    acts.push(Action::CloseMenu);
                }
            }
            Some(Entry::ItemTip { action, .. }) if activates => {
                acts.push(action.clone());
                acts.push(Action::CloseMenu);
            }
            _ => {}
        },
        Nav::Back => {
            // Left or Escape in a submenu: close it and go back to its row.
            path.pop();
            app.ui.submenu = None;
        }
        Nav::Close => acts.push(Action::CloseMenu),
    }
    app.ui.menu_sel = path;
    intercept_quick_groups(app, &mut acts);
    Some(acts)
}

/// The Recent menu's keys: arrows and Tab move, Enter picks the highlighted row, a digit picks its
/// session, Escape or Ctrl+Shift+E closes it.
fn recent_keys(ctx: &egui::Context, app: &mut CsmApp) -> Option<Vec<Action>> {
    let close = ctx.input_mut(|i| consume_exact(i, Modifiers::COMMAND | Modifiers::SHIFT, Key::E));
    let keys = take_keys(ctx, true);
    if close {
        app.ui.menu_sel.clear();
        return Some(vec![Action::CloseMenu]);
    }
    if keys == Keys::default() {
        return None;
    }
    let list = crate::screens::recent::items(&app.model);
    let sessions = list.iter().filter(|i| matches!(i, crate::screens::recent::Item::Session(_))).count();
    let rows: Vec<Row> = (0..list.len())
        .map(|i| Row {
            selectable: true,
            letter: (i < sessions)
                .then(|| crate::screens::recent::digit_for(i).and_then(|d| u32::try_from(d).ok()).and_then(|d| char::from_digit(d, 10)))
                .flatten(),
        })
        .collect();
    let sel = app.ui.menu_sel.first().copied();
    let mut acts = Vec::new();
    match menu_nav::navigate(keys, sel, &rows, false) {
        Nav::Highlight(i) => app.ui.menu_sel = vec![i],
        Nav::Activate(i) => {
            if let Some(item) = list.get(i) {
                acts.push(item.action());
                acts.push(Action::CloseMenu);
            }
        }
        Nav::Close => acts.push(Action::CloseMenu),
        Nav::Nothing | Nav::Forward(_) | Nav::Back => {}
    }
    Some(acts)
}

/// The quick-open's group picker is local state, not a tool: its rows are taken here.
fn intercept_quick_groups(app: &mut CsmApp, actions: &mut Vec<Action>) {
    actions.retain(|a| {
        if let Action::Toast(t) = a
            && let Some(g) = t.strip_prefix("quick-group:")
        {
            app.ui.quick.group = (!g.is_empty()).then(|| g.to_string());
            return false;
        }
        true
    });
}

pub fn show(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(menu) = app.ui.menu.clone() else {
        // Closed: the next opening reads the worktrees again.
        ctx.data_mut(|d| d.remove_temp::<String>(Id::new("menu-worktrees-asked")));
        return;
    };
    if let Menu::Pane { session, .. } | Menu::Row { session, .. } = &menu {
        ask_worktrees(ctx, app, &menu, session);
    }
    let rects = match &menu {
        Menu::Grid { at } => vec![grid_popover(ctx, app, *at, actions)],
        Menu::Recent { at } => {
            let mut v = vec![crate::screens::recent::popup(ctx, app, *at, actions)];
            v.extend(ctx.data(|d| d.get_temp::<Rect>(Id::new(crate::app::RECENT_ANCHOR).with("button"))));
            v
        }
        _ => {
            let mut v = match listing(app, &menu) {
                Some(l) => menu_area(ctx, app, Id::new(l.id), l.at, &l.entries, 0, false, actions),
                None => Vec::new(),
            };
            // The title bar's button is not "outside" the main menu (see `MAIN_ANCHOR`).
            if matches!(menu, Menu::Main { .. }) {
                v.extend(ctx.data(|d| d.get_temp::<Rect>(Id::new(crate::app::MAIN_ANCHOR))));
            }
            v
        }
    };
    ctx.data_mut(|d| d.remove::<bool>(menu_keys_id()));
    intercept_quick_groups(app, actions);
    // A click anywhere else closes it.
    let clicked_outside = ctx.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !rects.iter().any(|r| r.contains(p))));
    if clicked_outside {
        actions.push(Action::CloseMenu);
    }
}

/// Tiles per row in the Grid menu: the one-row presets, then the multi-row ones.
const GRID_MENU_COLUMNS: usize = 4;

/// One Grid menu tile: a preset or remembered custom (`Some`), or Custom… (`None`). With
/// `forgettable`, an × in its corner (drawn while the tile is hovered) forgets the size; its
/// response is the second value.
fn grid_tile(ui: &mut egui::Ui, t: &Tokens, tile: Option<GridPreset>, on: bool, forgettable: bool) -> (egui::Response, Option<egui::Response>) {
    let (rect, r) = ui.allocate_exact_size(vec2(size::GRID_TILE_W, size::GRID_GLYPH[1] + space::XL + space::SM), Sense::click());
    // Interacted after the tile, so it sits on top and takes its own clicks.
    let forget = tile.filter(|_| forgettable).map(|g| {
        let x = Rect::from_min_size(pos2(rect.max.x - icon::LG - space::XXS, rect.min.y + space::XXS), Vec2::splat(icon::LG));
        let f = ui.interact(x, ui.id().with(("grid-forget", g.cols, g.rows)), Sense::click());
        f.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Forget grid {}", g.label())));
        (x, f)
    });
    let hovered = r.hovered() || forget.as_ref().is_some_and(|(_, f)| f.hovered());
    if hovered {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::md()), t.row_hover);
    }
    if on {
        ui.painter().rect_stroke(rect, CornerRadius::same(radius::md()), Stroke::new(size::HAIRLINE, t.blue), StrokeKind::Inside);
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
                ui.painter().rect_filled(c, CornerRadius::same(radius::sm() / 2), fill);
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
    let forget = forget.map(|(x, f)| {
        if hovered {
            let over = f.hovered();
            ui.painter().rect_filled(x, CornerRadius::same(radius::sm()), if over { t.surface1 } else { t.mantle });
            icons::paint(ui, x, "x", icon::SM, if over { t.fg } else { t.muted });
        }
        f.on_hover_text("Forget this grid")
    });
    (r, forget)
}

fn grid_popover(ctx: &egui::Context, app: &mut CsmApp, at: Pos2, actions: &mut Vec<Action>) -> Rect {
    let t = Tokens::get(ctx);
    let scope = app.model.pane_scope();
    let lay = app.model.layout(&scope);
    let resp = egui::Area::new(Id::new("grid-pop")).order(Order::Foreground).fixed_pos(at).show(ctx, |ui| {
        widgets::pop_frame(&t).inner_margin(egui::Margin::same(space::SM as i8)).show(ui, |ui| {
            let caption = |ui: &mut egui::Ui, s: &str| {
                ui.label(egui::RichText::new(s).font(theme::regular(text::CAPTION)).color(t.muted));
                ui.add_space(space::XS);
            };
            caption(ui, "Grid presets");
            let presets: Vec<Option<GridPreset>> = GridPreset::PRESETS.iter().copied().map(Some).collect();
            // The presets, then the remembered customs (newest first, each with a forget ×) and
            // the Custom… tile.
            let mut customs: Vec<Option<GridPreset>> = app.model.custom_grids.iter().copied().filter(|g| !g.is_preset()).map(Some).collect();
            customs.push(None);
            for (section, tiles) in [(None, presets), (Some("Custom"), customs)] {
                if let Some(s) = section {
                    ui.add_space(space::XS);
                    caption(ui, s);
                }
                for chunk in tiles.chunks(GRID_MENU_COLUMNS) {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = space::XS;
                        for tile in chunk {
                            let on = lay.mode == LayoutMode::Grid && tile.is_some_and(|g| g == lay.grid);
                            let forgettable = section.is_some() && tile.is_some();
                            let (r, forget) = grid_tile(ui, &t, *tile, on, forgettable);
                            if let (Some(g), Some(f)) = (tile, forget)
                                && f.clicked()
                            {
                                actions.push(Action::Tool("grid_preset_forget", json!({"cols": g.cols, "rows": g.rows})));
                                // A tool call closes the menu: open it again, without that size.
                                actions.push(Action::OpenMenu(Menu::Grid { at }));
                                continue;
                            }
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
        Entry::Item { label: l, hint, .. } | Entry::ItemTip { label: l, hint, .. } | Entry::Off { label: l, hint, .. } => {
            base + label(l) + hint.as_ref().map_or(0.0, |h| widgets::text_width(ui, h, theme::mono(text::TINY)) + space::LG)
        }
        Entry::Sub { label: l, .. } => base + label(l) + icon::MD + space::LG,
        Entry::Head(_) | Entry::Sep => 0.0,
    }
}
