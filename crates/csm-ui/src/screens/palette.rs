//! The command palette (Ctrl+K: sessions, live or asleep, and every registered tool by title,
//! fuzzy), the new-session quick-open (Ctrl+T: recent folders, Tab cycles the command, Remote…
//! for SSH and WSL) and the one-line prompt (rename, new group).

use egui::{Align2, CornerRadius, Id, Key, Modifiers, Order, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use serde_json::json;

use crate::app::{Action, CsmApp, Menu, Overlay, PromptKind, RemoteKind};
use crate::icons;
use crate::model::{SessionView, Status, fuzzy};
use crate::theme::{self, Tokens, icon, radius, size, space, text};
use crate::widgets::{self, ButtonKind};

/// One row of the palette.
#[derive(Debug, Clone, PartialEq)]
pub enum PalRow {
    Session { id: String, idx: Vec<usize> },
    Search(String),
    Tool { id: &'static str, title: &'static str, idx: Vec<usize>, hint: Option<&'static str> },
}

/// Keyboard hints for tools that have a shortcut.
fn shortcut_for(id: &str) -> Option<&'static str> {
    match id {
        "inbox_next" => Some("Ctrl J"),
        "session_new" => Some("Ctrl T"),
        "inspector_show" => Some("Ctrl I"),
        "session_reopen_closed" => Some("Ctrl Shift T"),
        "session_close" => Some("Ctrl W"),
        "run_start" => Some("F5"),
        "search_output" => Some("Ctrl F"),
        _ => None,
    }
}

/// Tools shown before anything is typed (the mock's command list), in this order.
const SUGGESTED: [&str; 6] = ["inbox_next", "session_new", "group_restart_all", "open_with", "layout_set", "inspector_show"];

/// The palette's rows for a query: sessions, then output search, then tools.
pub fn rows(app: &CsmApp, query: &str) -> Vec<PalRow> {
    let model = &app.model;
    let commands_only = query.starts_with('>');
    let q = query.trim_start_matches('>').trim();
    let mut out = Vec::new();
    if !commands_only {
        let mut ss: Vec<(f32, &SessionView, Vec<usize>)> = model
            .sessions
            .iter()
            .filter_map(|s| {
                if q.is_empty() {
                    return s.status.is_live().then(|| (0.0, s, Vec::new()));
                }
                fuzzy(q, &s.name).map(|(sc, idx)| (sc, s, idx)).or_else(|| fuzzy(q, &s.folder).map(|(sc, _)| (sc - 1.0, s, Vec::new())))
            })
            .collect();
        ss.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, s, idx) in ss.into_iter().take(if q.is_empty() { 5 } else { 6 }) {
            out.push(PalRow::Session { id: s.id.clone(), idx });
        }
        if !q.is_empty() {
            out.push(PalRow::Search(q.to_string()));
        }
    }
    let reg = app.dispatch.registry();
    if q.is_empty() {
        for id in SUGGESTED {
            if let Some(s) = reg.get(id) {
                out.push(PalRow::Tool { id: s.id, title: s.title, idx: Vec::new(), hint: shortcut_for(s.id) });
            }
        }
        if commands_only {
            for s in reg.all() {
                if !SUGGESTED.contains(&s.id) && s.id != "consent_answer" {
                    out.push(PalRow::Tool { id: s.id, title: s.title, idx: Vec::new(), hint: shortcut_for(s.id) });
                }
            }
        }
    } else {
        let mut tools: Vec<(f32, PalRow)> = reg
            .all()
            .iter()
            .filter(|s| s.id != "consent_answer")
            .filter_map(|s| fuzzy(q, s.title).map(|(sc, idx)| (sc, PalRow::Tool { id: s.id, title: s.title, idx, hint: shortcut_for(s.id) })))
            .collect();
        tools.sort_by(|a, b| b.0.total_cmp(&a.0));
        out.extend(tools.into_iter().take(if commands_only { 40 } else { 8 }).map(|(_, r)| r));
    }
    out
}

fn scrim(ctx: &egui::Context, id: &str, actions: &mut Vec<Action>) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(Id::new(id)).order(Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(rect, 0.0, t.scrim);
        if resp.clicked() {
            actions.push(Action::CloseOverlays);
        }
    });
}

fn highlighted(ui: &Ui, s: &str, idx: &[usize], base: egui::Color32) -> egui::text::LayoutJob {
    let t = Tokens::get(ui.ctx());
    let mut job = egui::text::LayoutJob::default();
    for (i, c) in s.chars().enumerate() {
        let hit = idx.contains(&i);
        let font = if hit { theme::semibold(text::BODY) } else { theme::regular(text::BODY) };
        job.append(&c.to_string(), 0.0, egui::TextFormat { font_id: font, color: if hit { t.blue } else { base }, ..Default::default() });
    }
    job
}

pub fn show(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    match app.ui.overlay {
        Overlay::Palette => palette(ctx, app, actions),
        Overlay::QuickOpen => quick_open(ctx, app, actions),
        Overlay::Prompt => prompt(ctx, app, actions),
        _ => {}
    }
}

fn card(ctx: &egui::Context, id: &str, w: f32, body: impl FnOnce(&mut Ui)) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let pos = pos2(screen.center().x - w / 2.0, screen.min.y + size::PALETTE_TOP);
    egui::Area::new(Id::new(id)).order(Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::new()
            .fill(t.pop)
            .stroke(Stroke::new(size::HAIRLINE, t.surface1))
            .corner_radius(CornerRadius::same(radius::XL))
            .shadow(egui::Shadow { offset: [0, 16], blur: 40, spread: 0, color: t.shadow })
            .show(ui, |ui| {
                ui.set_width(w);
                ui.spacing_mut().item_spacing.y = 0.0;
                body(ui);
            });
    });
}

fn input_row(ui: &mut Ui, icon_name: &str, buf: &mut String, hint: &str, focus: &mut bool, id: &str) -> egui::Response {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::PALETTE_INPUT), Sense::hover());
    ui.painter().hline(rect.x_range(), rect.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));
    icons::paint(
        ui,
        Rect::from_min_size(pos2(rect.min.x + space::MD + space::XXS, rect.center().y - icon::XXL / 2.0), Vec2::splat(icon::XXL)),
        icon_name,
        icon::XXL,
        t.muted,
    );
    let kw = widgets::kbd_width(ui, "Esc");
    widgets::paint_kbd(ui, Rect::from_center_size(pos2(rect.max.x - space::MD - kw / 2.0, rect.center().y), vec2(kw, size::KBD)), "Esc");
    let er = Rect::from_min_max(pos2(rect.min.x + space::XXL + space::LG, rect.min.y), pos2(rect.max.x - space::XL - kw, rect.max.y));
    let edit = egui::TextEdit::singleline(buf)
        .id(Id::new(id))
        .frame(egui::Frame::NONE)
        .hint_text(egui::RichText::new(hint).color(t.faint))
        .font(theme::regular(text::INPUT))
        .vertical_align(egui::Align::Center)
        .desired_width(er.width());
    let resp = ui.put(er, edit);
    if *focus {
        resp.request_focus();
        *focus = false;
    }
    resp
}

fn palette(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    scrim(ctx, "palette-scrim", actions);
    // Keys first (the text field must not see arrows/enter).
    let n = rows(app, &app.ui.palette.query).len();
    let (down, up, enter) = ctx.input_mut(|i| {
        (i.consume_key(Modifiers::NONE, Key::ArrowDown), i.consume_key(Modifiers::NONE, Key::ArrowUp), i.consume_key(Modifiers::NONE, Key::Enter))
    });
    if n > 0 {
        if down {
            app.ui.palette.sel = (app.ui.palette.sel + 1) % n;
            app.ui.palette.scroll_to_sel = true;
        }
        if up {
            app.ui.palette.sel = (app.ui.palette.sel + n - 1) % n;
            app.ui.palette.scroll_to_sel = true;
        }
    }
    let list = rows(app, &app.ui.palette.query);
    if enter && let Some(r) = list.get(app.ui.palette.sel.min(list.len().saturating_sub(1))) {
        actions.extend(activate(app, r));
        return;
    }
    let model = app.model.clone();
    let mut query = app.ui.palette.query.clone();
    let mut focus = app.ui.palette.focus_input;
    let sel = app.ui.palette.sel;
    let scroll = std::mem::take(&mut app.ui.palette.scroll_to_sel);
    let mut picked = None;
    let mut changed = false;
    card(ctx, "palette", size::PALETTE_W, |ui| {
        let t = Tokens::get(ui.ctx());
        let resp = input_row(ui, "search", &mut query, "Search sessions and commands", &mut focus, "palette-input");
        changed = resp.changed();
        egui::ScrollArea::vertical().max_height(size::PALETTE_LIST_H).auto_shrink([false, true]).show(ui, |ui| {
            ui.add_space(space::XS);
            if list.is_empty() {
                ui.add_space(space::XL);
                ui.vertical_centered(|ui| ui.label(egui::RichText::new(format!("No session or command matches “{query}”.")).color(t.muted)));
                ui.add_space(space::XL);
            }
            let mut last_section = "";
            for (i, row) in list.iter().enumerate() {
                let section = match row {
                    PalRow::Session { .. } if query.trim().is_empty() => "Recent sessions",
                    PalRow::Session { .. } => "Sessions",
                    PalRow::Search(_) => "Search",
                    PalRow::Tool { .. } => "Commands",
                };
                if section != last_section {
                    last_section = section;
                    ui.add_space(space::XS);
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD + space::XS);
                        widgets::caps(ui, section);
                    });
                    ui.add_space(space::XS);
                }
                let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::PALETTE_ROW), Sense::hover());
                let rect = outer.shrink2(vec2(space::XS + space::XXS, 0.0));
                let r = ui.interact(rect, Id::new(("pal-row", i)), Sense::click());
                if i == sel || r.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), t.row_sel);
                }
                if i == sel && scroll {
                    ui.scroll_to_rect(rect, None);
                }
                let cy = rect.center().y;
                let mut x = rect.min.x + space::SM;
                let label = match row {
                    PalRow::Session { id, idx } => {
                        let Some(s) = model.session(id) else { continue };
                        ui.painter().rect_filled(
                            Rect::from_center_size(pos2(x + size::STRIPE / 2.0, cy), vec2(size::STRIPE, space::LG)),
                            CornerRadius::same(radius::SM / 2),
                            t.accent(s.accent),
                        );
                        x += size::STRIPE + space::ROW_X;
                        let job = highlighted(ui, &s.name, idx, t.fg);
                        let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                        let gw = g.size().x;
                        ui.painter().galley(pos2(x, cy - g.size().y / 2.0), g, t.fg);
                        let right_text = if s.status == Status::Asleep { "Asleep · Enter wakes".to_string() } else { s.status_text() };
                        let rw = widgets::text_width(ui, &right_text, theme::regular(text::CAPTION));
                        let rr = ui.painter().text(
                            pos2(rect.max.x - space::SM, cy),
                            Align2::RIGHT_CENTER,
                            &right_text,
                            theme::regular(text::CAPTION),
                            t.muted,
                        );
                        let gr = Rect::from_min_size(pos2(rr.min.x - space::SM - icon::SM, cy - icon::SM / 2.0), Vec2::splat(icon::SM));
                        icons::paint_status(ui, gr, s.status, icon::SM);
                        widgets::paint_text(
                            ui,
                            pos2(x + gw + space::ROW_X, cy),
                            Align2::LEFT_CENTER,
                            &s.folder,
                            theme::regular(text::CAPTION),
                            t.muted,
                            (gr.min.x - x - gw - space::LG - rw * 0.0).max(0.0),
                        );
                        s.name.clone()
                    }
                    PalRow::Search(q) => {
                        icons::paint(ui, Rect::from_min_size(pos2(x, cy - icon::LG / 2.0), Vec2::splat(icon::LG)), "search", icon::LG, t.muted);
                        let l = format!("Search all output and notes for “{q}”");
                        ui.painter().text(pos2(x + icon::LG + space::ROW_X, cy), Align2::LEFT_CENTER, &l, theme::regular(text::BODY), t.fg);
                        let kw = widgets::kbd_width(ui, "Ctrl F");
                        widgets::paint_kbd(ui, Rect::from_center_size(pos2(rect.max.x - space::SM - kw / 2.0, cy), vec2(kw, size::KBD)), "Ctrl F");
                        l
                    }
                    PalRow::Tool { id, title, idx, hint } => {
                        let spec = app.dispatch.registry().get(id).copied();
                        let ic = tool_icon(id);
                        icons::paint(ui, Rect::from_min_size(pos2(x, cy - icon::LG / 2.0), Vec2::splat(icon::LG)), ic, icon::LG, t.muted);
                        x += icon::LG + space::ROW_X;
                        let job = highlighted(ui, title, idx, t.fg);
                        let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                        let gw = g.size().x;
                        ui.painter().galley(pos2(x, cy - g.size().y / 2.0), g, t.fg);
                        let mut right = rect.max.x - space::SM;
                        if let Some(h) = hint {
                            let kw = widgets::kbd_width(ui, h);
                            widgets::paint_kbd(ui, Rect::from_center_size(pos2(right - kw / 2.0, cy), vec2(kw, size::KBD)), h);
                            right -= kw + space::SM;
                        }
                        if let Some(spec) = spec {
                            widgets::paint_text(
                                ui,
                                pos2(x + gw + space::ROW_X, cy),
                                Align2::LEFT_CENTER,
                                spec.summary,
                                theme::regular(text::CAPTION),
                                t.faint,
                                (right - x - gw - space::LG).max(0.0),
                            );
                        }
                        title.to_string()
                    }
                };
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Palette: {label}")));
                if r.clicked() {
                    picked = Some(row.clone());
                }
            }
            ui.add_space(space::XS + space::XXS);
        });
        // Footer.
        let (frect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL + space::SM), Sense::hover());
        ui.painter().hline(frect.x_range(), frect.min.y, Stroke::new(size::HAIRLINE, t.surface0));
        let mut fx = frect.min.x + space::MD + space::XXS;
        let fy = frect.center().y;
        for (keys, what) in [(&["↑", "↓"][..], "move"), (&["Enter"][..], "open or run"), (&[">"][..], "commands only")] {
            for k in keys {
                let kw = widgets::kbd_width(ui, k);
                widgets::paint_kbd(ui, Rect::from_min_size(pos2(fx, fy - size::KBD / 2.0), vec2(kw, size::KBD)), k);
                fx += kw + space::XS;
            }
            let r = ui.painter().text(pos2(fx + space::XXS, fy), Align2::LEFT_CENTER, what, theme::regular(text::CAPTION), t.muted);
            fx = r.max.x + space::LG;
        }
        ui.painter().text(
            pos2(frect.max.x - space::MD, fy),
            Align2::RIGHT_CENTER,
            "Asleep sessions wake when you pick them",
            theme::regular(text::CAPTION),
            t.muted,
        );
    });
    app.ui.palette.focus_input = focus;
    if changed {
        app.ui.palette.query = query;
        app.ui.palette.sel = 0;
    }
    if let Some(r) = picked {
        actions.extend(activate(app, &r));
    }
}

fn tool_icon(id: &str) -> &'static str {
    let area = id.split('_').next().unwrap_or("");
    match id {
        "inbox_next" | "inbox_list" | "inbox_panel_set" => "inbox",
        "session_new" | "group_new" => "plus",
        "inspector_show" => "panelR",
        "session_reopen_closed" | "recently_closed_list" => "history",
        "session_sleep" | "group_sleep_all" => "moon",
        "session_restart" | "group_restart_all" => "restart",
        "open_with" => "braces",
        "open_folder" => "folder",
        "session_status_clear" => "eraser",
        "group_tear_off" | "pane_tear_off" => "tear",
        "session_close" | "group_close_all" => "x",
        "app_quit" => "power",
        _ => match area {
            "layout" | "view" | "pane" => "overview",
            "git" | "worktree" => "branch",
            "run" => "play",
            "note" => "note",
            "search" => "search",
            "group" => "layers",
            "settings" | "app" | "demo" => "sliders",
            _ => "terminal",
        },
    }
}

fn activate(app: &CsmApp, row: &PalRow) -> Vec<Action> {
    let mut out = vec![Action::CloseOverlays];
    match row {
        PalRow::Session { id, .. } => out.push(Action::Tool("session_focus", json!({"session_id": id}))),
        PalRow::Search(q) => out.push(Action::OpenSearch(q.clone())),
        PalRow::Tool { id, .. } => {
            // A layout tool from the palette: cycle to the next mode.
            if *id == "layout_set" {
                let scope = app.model.pane_scope();
                let cur = app.model.layout(&scope).mode;
                let modes = crate::model::LayoutMode::ALL;
                let next = modes.iter().position(|m| *m == cur).map_or(modes[0], |i| modes[(i + 1) % modes.len()]);
                out.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": next.id()})));
            } else {
                out.push(Action::PaletteTool(id));
            }
        }
    }
    out
}

// ---- quick-open ------------------------------------------------------------------------------

/// Folders for the quick-open's query: recent first when empty, fuzzy otherwise.
pub fn folder_rows(app: &CsmApp, query: &str) -> Vec<(usize, Vec<usize>)> {
    let q = query.trim();
    let mut v: Vec<(f32, usize, Vec<usize>)> = app
        .model
        .folders
        .iter()
        .enumerate()
        .filter_map(|(i, f)| {
            if q.is_empty() {
                return Some((if f.open > 0 { 1.0 } else { 0.0 }, i, Vec::new()));
            }
            fuzzy(q, &f.name).map(|(s, idx)| (s, i, idx)).or_else(|| fuzzy(q, &f.path).map(|(s, _)| (s - 1.0, i, Vec::new())))
        })
        .collect();
    v.sort_by(|a, b| b.0.total_cmp(&a.0));
    v.into_iter().take(7).map(|(_, i, idx)| (i, idx)).collect()
}

/// The quick-open's commands: the launch commands, then the session's own command when it
/// isn't one of them (edit mode).
pub fn quick_commands(app: &CsmApp) -> Vec<String> {
    let mut v = app.model.launch_commands.clone();
    if let Some(c) = &app.ui.quick.custom_cmd {
        v.push(c.clone());
    }
    v
}

/// `ssh` params from the fields: `user@host`, an optional port, the remote folder.
fn ssh_params(q: &crate::app::QuickOpenState) -> serde_json::Value {
    let (user, host) = match q.host.trim().split_once('@') {
        Some((u, h)) => (Some(u.to_string()), h.to_string()),
        None => (None, q.host.trim().to_string()),
    };
    let port = q.ssh_port.trim().parse::<i32>().ok();
    json!({"user": user, "host": host, "port": port, "remote_folder": q.remote_folder.trim()})
}

fn wsl_params(q: &crate::app::QuickOpenState) -> serde_json::Value {
    let user = (!q.wsl_user.trim().is_empty()).then(|| q.wsl_user.trim().to_string());
    json!({"distro": q.distro.trim(), "user": user, "folder": q.linux_folder.trim()})
}

fn start_quick(app: &CsmApp, keep_open: bool) -> Vec<Action> {
    let q = &app.ui.quick;
    let cmds = quick_commands(app);
    let cmd = cmds.get(q.cmd).cloned().unwrap_or_else(|| "claude".into());
    let mut out = Vec::new();
    if !keep_open || q.edit.is_some() {
        out.push(Action::CloseOverlays);
    }
    if let Some(id) = &q.edit {
        // Edit: every field as the panel shows it; the engine decides whether it needs a restart.
        let (program, args) = split_command(&cmd);
        let mut p = json!({"session_id": id, "command": program, "args": args.unwrap_or_default()});
        if !q.name.trim().is_empty() {
            p["name"] = json!(q.name.trim());
        }
        match (q.remote, q.rkind) {
            (false, _) => {
                p["kind"] = json!("local");
                p["folder"] = json!(q.query.trim());
            }
            (true, RemoteKind::Ssh) => {
                p["kind"] = json!("ssh");
                p["ssh"] = ssh_params(q);
            }
            (true, RemoteKind::Wsl) => {
                p["kind"] = json!("wsl");
                p["wsl"] = wsl_params(q);
            }
        }
        out.push(Action::Tool("session_edit", p));
        return out;
    }
    if q.remote {
        let params = match q.rkind {
            RemoteKind::Ssh => {
                let (program, args) = split_command(&cmd);
                let program = if program == "claude" { "bash".to_string() } else { program };
                json!({"command": program, "args": args, "kind": "ssh", "group": q.group, "ssh": ssh_params(q)})
            }
            RemoteKind::Wsl => {
                json!({"command": split_command(&cmd).0, "args": split_command(&cmd).1, "kind": "wsl", "group": q.group, "wsl": wsl_params(q)})
            }
        };
        out.push(Action::Tool("session_new", params));
        return out;
    }
    let rows = folder_rows(app, &q.query);
    let folder = match rows.get(q.sel.min(rows.len().saturating_sub(1))).and_then(|(i, _)| app.model.folders.get(*i)) {
        Some(f) => f.path.clone(),
        None if !q.query.trim().is_empty() => q.query.trim().to_string(),
        None => return out,
    };
    let (program, args) = split_command(&cmd);
    out.push(Action::Tool("session_new", json!({"folder": folder, "command": program, "args": args, "group": q.group})));
    out
}

/// A launch command as `session_new` takes it: the program, then the rest as typed arguments
/// (`claude --continue` → `claude`, `--continue`).
pub fn split_command(cmd: &str) -> (String, Option<String>) {
    let cmd = cmd.trim();
    match cmd.split_once(char::is_whitespace) {
        Some((p, a)) if !a.trim().is_empty() => (p.to_string(), Some(a.trim().to_string())),
        _ => (cmd.to_string(), None),
    }
}

fn quick_open(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    scrim(ctx, "quick-scrim", actions);
    let editing = app.ui.quick.edit.clone();
    let commands = quick_commands(app);
    let n = folder_rows(app, &app.ui.quick.query).len();
    let ncmd = commands.len().max(1);
    let (down, up, enter, ctrl_enter, tab, shift_tab) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::COMMAND, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Tab),
            i.consume_key(Modifiers::SHIFT, Key::Tab),
        )
    });
    if n > 0 && down {
        app.ui.quick.sel = (app.ui.quick.sel + 1) % n;
        app.ui.quick.scroll_to_sel = true;
    }
    if n > 0 && up {
        app.ui.quick.sel = (app.ui.quick.sel + n - 1) % n;
        app.ui.quick.scroll_to_sel = true;
    }
    if tab {
        app.ui.quick.cmd = (app.ui.quick.cmd + 1) % ncmd;
    }
    if shift_tab {
        app.ui.quick.cmd = (app.ui.quick.cmd + ncmd - 1) % ncmd;
    }
    if editing.is_some() && (up || down) {
        // Edit: arrows pick a folder into the field (Enter saves, it doesn't start anything).
        let rows = folder_rows(app, &app.ui.quick.query);
        if let Some(f) = rows.get(app.ui.quick.sel).and_then(|(i, _)| app.model.folders.get(*i)) {
            app.ui.quick.query = f.path.clone();
        }
    }
    if enter || ctrl_enter {
        actions.extend(start_quick(app, ctrl_enter));
        if !ctrl_enter || editing.is_some() {
            return;
        }
    }
    let rows = folder_rows(app, &app.ui.quick.query);
    let model = app.model.clone();
    let mut q = app.ui.quick.clone();
    let scroll = std::mem::take(&mut app.ui.quick.scroll_to_sel);
    let mut picked = None;
    let mut changed = false;
    let mut open_group_menu = None;
    let mut save_clicked = false;
    let mut ask_distros = false;
    card(ctx, "quick-open", size::PALETTE_W, |ui| {
        let t = Tokens::get(ui.ctx());
        if let Some(id) = &q.edit {
            // Edit mode: what is being edited, and its name.
            let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG + space::SM), Sense::hover());
            ui.painter().rect_filled(hr, CornerRadius { nw: radius::XL, ne: radius::XL, sw: 0, se: 0 }, t.mantle);
            ui.painter().hline(hr.x_range(), hr.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));
            let accent = model.session(id).map_or(t.blue, |s| t.accent(s.accent));
            let stripe = Rect::from_center_size(pos2(hr.min.x + space::LG + size::STRIPE / 2.0, hr.center().y), vec2(size::STRIPE, space::LG));
            ui.painter().rect_filled(stripe, CornerRadius::same(radius::SM / 2), accent);
            let tr = ui.painter().text(
                pos2(stripe.max.x + space::SM, hr.center().y),
                Align2::LEFT_CENTER,
                "Edit session",
                theme::semibold(text::BODY),
                t.fg,
            );
            let nr = Rect::from_min_max(
                pos2(tr.max.x + space::LG, hr.min.y + space::XS + space::XXS),
                pos2(hr.max.x - space::LG, hr.max.y - space::XS - space::XXS),
            );
            let name = egui::TextEdit::singleline(&mut q.name)
                .id(Id::new("quick-name"))
                .hint_text(egui::RichText::new("Name").color(t.faint))
                .font(theme::regular(text::BODY))
                .vertical_align(egui::Align::Center);
            let r = ui.put(nr, name);
            r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Session name"));
        }
        let (icon_name, hint) = if q.edit.is_some() { ("folder", "Folder: a path") } else { ("plus", "Folder to start in: type a name or a path") };
        let resp = if q.remote && q.edit.is_some() {
            // An SSH or WSL session's folder is in the connection fields below.
            ui.allocate_response(vec2(ui.available_width(), space::XS), Sense::hover())
        } else {
            input_row(ui, icon_name, &mut q.query, hint, &mut q.focus_input, "quick-input")
        };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Folder"));
        changed = resp.changed();
        let list_h = if q.edit.is_some() { size::PALETTE_LIST_H / 2.0 } else { size::PALETTE_LIST_H };
        let show_list = !(q.remote && q.edit.is_some());
        egui::ScrollArea::vertical().max_height(if show_list { list_h } else { 0.0 }).auto_shrink([false, true]).show(ui, |ui| {
            if !show_list {
                return;
            }
            ui.add_space(space::XS);
            ui.horizontal(|ui| {
                ui.add_space(space::MD + space::XS);
                widgets::caps(ui, if q.query.trim().is_empty() { "Recent folders" } else { "Folders" });
            });
            ui.add_space(space::XS);
            if rows.is_empty() {
                ui.add_space(space::LG);
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new(format!("No folder matches. Enter starts in “{}” if it exists.", q.query.trim())).color(t.muted))
                });
                ui.add_space(space::LG);
            }
            for (i, (fi, idx)) in rows.iter().enumerate() {
                let Some(f) = model.folders.get(*fi) else { continue };
                let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::PALETTE_ROW), Sense::hover());
                let rect = outer.shrink2(vec2(space::XS + space::XXS, 0.0));
                let r = ui.interact(rect, Id::new(("qo-row", i)), Sense::click());
                if i == q.sel || r.hovered() {
                    ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), t.row_sel);
                }
                if i == q.sel && scroll {
                    ui.scroll_to_rect(rect, None);
                }
                let cy = rect.center().y;
                let x = rect.min.x + space::SM;
                icons::paint(
                    ui,
                    Rect::from_min_size(pos2(x, cy - icon::LG / 2.0), Vec2::splat(icon::LG)),
                    if f.recent { "history" } else { "folder" },
                    icon::LG,
                    t.muted,
                );
                let job = highlighted(ui, &f.name, idx, t.fg);
                let g = ui.ctx().fonts_mut(|fo| fo.layout_job(job));
                let gw = g.size().x;
                let nx = x + icon::LG + space::ROW_X;
                ui.painter().galley(pos2(nx, cy - g.size().y / 2.0), g, t.fg);
                let tag = if f.open > 0 {
                    format!("{} open", f.open)
                } else if f.tag.starts_with("Repo under") {
                    "repo".into()
                } else {
                    f.tag.clone()
                };
                let tr = ui.painter().text(pos2(rect.max.x - space::SM, cy), Align2::RIGHT_CENTER, &tag, theme::regular(text::CAPTION), t.muted);
                let mut right = tr.min.x - space::SM;
                if let Some(b) = &f.branch {
                    let cw = (widgets::text_width(ui, b, theme::mono(text::MONO_SM)) + icon::XS + space::SM + space::XS)
                        .min(size::BRANCH_MAX_W + space::XXL);
                    let cr = Rect::from_min_size(pos2(right - cw, cy - size::CHIP / 2.0), vec2(cw, size::CHIP));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(cr), |ui| widgets::branch_chip(ui, b, false, 0, cw, false));
                    right = cr.min.x - space::SM;
                }
                widgets::paint_text(
                    ui,
                    pos2(nx + gw + space::ROW_X, cy),
                    Align2::LEFT_CENTER,
                    &f.path,
                    theme::mono(text::MONO_SM),
                    t.muted,
                    (right - nx - gw - space::ROW_X).max(0.0),
                );
                r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Folder: {}", f.name)));
                if r.clicked() {
                    if q.edit.is_some() {
                        // Edit: a row fills the field; Save is separate.
                        q.query = f.path.clone();
                        q.sel = i;
                    } else {
                        picked = Some(i);
                    }
                }
            }
            ui.add_space(space::XS);
        });
        // The command row: Tab cycles.
        let t = Tokens::get(ui.ctx());
        let (crect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL + space::LG + space::XS), Sense::hover());
        ui.painter().hline(crect.x_range(), crect.min.y, Stroke::new(size::HAIRLINE, t.surface0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(crect.shrink2(vec2(space::MD + space::XXS, space::SM))), |ui| {
            ui.horizontal_centered(|ui| {
                ui.label(egui::RichText::new("Command").font(theme::regular(text::CAPTION)).color(t.muted));
                // The cycle control (`.cyc`).
                let font = theme::mono(text::MONO);
                let widths: Vec<f32> = commands.iter().map(|c| widgets::text_width(ui, c, font.clone()) + space::MD).collect();
                let total: f32 = widths.iter().sum::<f32>() + space::XXS * (widths.len() as f32 + 1.0);
                let (seg, _) = ui.allocate_exact_size(vec2(total, size::CONTROL), Sense::hover());
                ui.painter().rect(seg, CornerRadius::same(radius::MD + 1), t.crust, Stroke::new(size::HAIRLINE, t.surface0), StrokeKind::Inside);
                let mut x = seg.min.x + space::XXS;
                for (i, (c, w)) in commands.iter().zip(widths).enumerate() {
                    let b = Rect::from_min_size(pos2(x, seg.min.y + space::XXS), vec2(w, size::CONTROL_SM));
                    x += w + space::XXS;
                    let r = ui.interact(b, Id::new(("qo-cmd", i)), Sense::click());
                    let on = i == q.cmd;
                    if on {
                        ui.painter().rect_filled(b, CornerRadius::same(radius::SM + 1), t.field);
                    }
                    ui.painter().text(b.center(), Align2::CENTER_CENTER, c, font.clone(), if on { t.fg } else { t.muted });
                    r.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::RadioButton, true, format!("Command: {c}")));
                    if r.clicked() {
                        q.cmd = i;
                    }
                }
                widgets::kbd(ui, "Tab").on_hover_text("Tab cycles the command, Shift+Tab goes back");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let kind = if q.remote { ButtonKind::Primary } else { ButtonKind::Secondary };
                    let tip = if q.remote { "Back to a local folder" } else { "Connect over SSH or WSL instead" };
                    if widgets::button(ui, kind, Some("server"), "Remote…", None).on_hover_text(tip).clicked() {
                        q.remote = !q.remote;
                    }
                    if q.edit.is_none() {
                        let gname = q.group.as_deref().and_then(|g| model.group(g)).map_or_else(|| "Ungrouped".to_string(), |g| g.name.clone());
                        let gr = widgets::button(ui, ButtonKind::Secondary, Some("layers"), &gname, None)
                            .on_hover_text("Group: suggested from the scope; pick another");
                        if gr.clicked() {
                            open_group_menu = Some(gr.rect.left_bottom());
                        }
                    }
                });
            });
        });
        if q.remote {
            let picker = q.rkind == RemoteKind::Wsl && q.distros.as_ref().is_some_and(|d| !d.is_empty());
            let extra = if picker { size::CONTROL + space::SM } else { 0.0 };
            let (rr, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL_LG * 2.0 + space::LG + extra), Sense::hover());
            ui.painter().rect_filled(rr, 0.0, t.mantle);
            ui.painter().hline(rr.x_range(), rr.min.y, Stroke::new(size::HAIRLINE, t.surface0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(rr.shrink2(vec2(space::MD + space::XXS, space::MD))), |ui| {
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new("Connect over").font(theme::regular(text::CAPTION)).color(t.muted));
                        ui.horizontal(|ui| {
                            for (k, l) in [(RemoteKind::Ssh, "SSH"), (RemoteKind::Wsl, "WSL")] {
                                let kind = if q.rkind == k { ButtonKind::Primary } else { ButtonKind::Secondary };
                                if widgets::button(ui, kind, None, l, None).clicked() {
                                    q.rkind = k;
                                }
                            }
                        });
                    });
                    ui.add_space(space::MD);
                    let field = |ui: &mut Ui, label: &str, buf: &mut String, hint: &str| {
                        ui.vertical(|ui| {
                            ui.label(egui::RichText::new(label).font(theme::regular(text::CAPTION)).color(t.muted));
                            let edit = egui::TextEdit::singleline(buf).hint_text(egui::RichText::new(hint).color(t.faint));
                            let w = if label == "Port" || label == "User" { size::GRID_TILE_W } else { size::MENU_W - space::XXL * 2.0 };
                            let r = ui.add(edit.font(theme::mono(text::MONO)).desired_width(w));
                            let l = label.to_string();
                            r.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, l.clone()));
                        });
                    };
                    match q.rkind {
                        RemoteKind::Ssh => {
                            field(ui, "Host", &mut q.host, "user@host");
                            field(ui, "Port", &mut q.ssh_port, "22");
                            field(ui, "Remote folder", &mut q.remote_folder, "/srv/app");
                        }
                        RemoteKind::Wsl => {
                            if q.distros.is_none() {
                                // Asked once, off the engine thread (`wsl.exe` can boot a VM).
                                q.distros = Some(Vec::new());
                                ask_distros = true;
                            }
                            field(ui, "Distro", &mut q.distro, "Ubuntu");
                            field(ui, "User", &mut q.wsl_user, "default");
                            field(ui, "Linux folder", &mut q.linux_folder, "~/src (blank: home)");
                        }
                    }
                });
                // The installed distributions, one click each.
                let installed: Vec<String> = q.distros.clone().unwrap_or_default();
                if q.rkind == RemoteKind::Wsl && !installed.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new("Installed").font(theme::regular(text::CAPTION)).color(t.muted));
                        for d in &installed {
                            let kind = if q.distro == *d { ButtonKind::Primary } else { ButtonKind::Ghost };
                            if widgets::button(ui, kind, None, d, None).clicked() {
                                q.distro = d.clone();
                            }
                        }
                    });
                }
            });
        }
        // Footer.
        let (frect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL + space::SM), Sense::hover());
        ui.painter().hline(frect.x_range(), frect.min.y, Stroke::new(size::HAIRLINE, t.surface0));
        let fy = frect.center().y;
        let cmd = commands.get(q.cmd).cloned().unwrap_or_default();
        let mut fx = frect.min.x + space::MD + space::XXS;
        let keys: Vec<(&str, String)> = if q.edit.is_some() {
            vec![("Enter", "save".to_string()), ("Esc", "cancel".to_string())]
        } else {
            vec![("Enter", format!("start {cmd}")), ("Ctrl Enter", "start and keep this open".to_string())]
        };
        for (k, what) in keys {
            let kw = widgets::kbd_width(ui, k);
            widgets::paint_kbd(ui, Rect::from_min_size(pos2(fx, fy - size::KBD / 2.0), vec2(kw, size::KBD)), k);
            let r = ui.painter().text(pos2(fx + kw + space::XS + space::XXS, fy), Align2::LEFT_CENTER, what, theme::regular(text::CAPTION), t.muted);
            fx = r.max.x + space::LG;
        }
        let tail = if q.edit.is_some() { "A new folder, command or connection needs a restart" } else { "Repos are found under your roots" };
        let save_w = if q.edit.is_some() { widgets::text_width(ui, "Save", theme::medium(text::BODY)) + space::ROW_X * 2.0 } else { 0.0 };
        widgets::paint_text(
            ui,
            pos2(frect.max.x - space::MD - save_w - if save_w > 0.0 { space::SM } else { 0.0 }, fy),
            Align2::RIGHT_CENTER,
            tail,
            theme::regular(text::CAPTION),
            t.muted,
            (frect.max.x - fx - save_w - space::XL).max(0.0),
        );
        if q.edit.is_some() {
            let br = Rect::from_min_size(pos2(frect.max.x - space::MD - save_w, fy - size::CONTROL / 2.0), vec2(save_w, size::CONTROL));
            if ui
                .scope_builder(egui::UiBuilder::new().max_rect(br), |ui| widgets::button(ui, ButtonKind::Primary, None, "Save", None))
                .inner
                .clicked()
            {
                save_clicked = true;
            }
        }
    });
    if changed {
        q.sel = 0;
    }
    if ask_distros {
        actions.push(Action::Tool("wsl_distros", json!({})));
    }
    if save_clicked {
        app.ui.quick = q;
        actions.extend(start_quick(app, false));
        return;
    }
    if let Some(i) = picked {
        q.sel = i;
        app.ui.quick = q;
        actions.extend(start_quick(app, false));
        return;
    }
    app.ui.quick = q;
    if let Some(at) = open_group_menu {
        actions.push(Action::OpenMenu(Menu::QuickGroup { at }));
    }
}

// ---- prompt ------------------------------------------------------------------------------------

fn prompt(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(mut p) = app.ui.prompt.clone() else { return };
    scrim(ctx, "prompt-scrim", actions);
    let (title, hint) = match &p.kind {
        PromptKind::RenameSession(_) => ("Rename session", "Name"),
        PromptKind::RenameGroup(_) => ("Rename group", "Name"),
        PromptKind::NewGroup => ("New group", "Group name, e.g. Client X"),
    };
    let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let mut submit = enter;
    card(ctx, "prompt", size::DIALOG_W, |ui| {
        let t = Tokens::get(ui.ctx());
        ui.add_space(space::MD);
        ui.horizontal(|ui| {
            ui.add_space(space::LG);
            ui.label(egui::RichText::new(title).font(theme::semibold(text::TITLE)).color(t.fg));
        });
        input_row(ui, "pencil", &mut p.text, hint, &mut p.focus, "prompt-input");
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            ui.add_space(space::LG);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(space::LG);
                if widgets::button(ui, ButtonKind::Primary, None, "Save", Some("Enter")).clicked() {
                    submit = true;
                }
                if widgets::button(ui, ButtonKind::Ghost, None, "Cancel", None).clicked() {
                    actions.push(Action::CloseOverlays);
                }
            });
        });
        ui.add_space(space::MD);
    });
    let text_ = p.text.trim().to_string();
    if submit && !text_.is_empty() {
        actions.push(Action::CloseOverlays);
        actions.push(match &p.kind {
            PromptKind::RenameSession(id) => Action::Tool("session_rename", json!({"session_id": id, "name": text_})),
            PromptKind::RenameGroup(id) => Action::Tool("group_rename", json!({"group_id": id, "name": text_})),
            PromptKind::NewGroup => Action::Tool("group_new", json!({"name": text_})),
        });
    }
    app.ui.prompt = Some(p);
}
