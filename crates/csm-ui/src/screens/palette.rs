//! The command palette (Ctrl+K: sessions, live or asleep, and every registered tool by title,
//! fuzzy), the new-session quick-open (Ctrl+T: recent folders, a typed path or Browse… for the
//! system's folder dialog, Tab cycles the command, Remote… for SSH and WSL) and the one-line
//! prompt (rename, new group).

use egui::{Align2, CornerRadius, Id, Key, Modifiers, Order, Rect, Sense, Stroke, StrokeKind, Ui, Vec2, pos2, vec2};
use serde_json::json;

use csm_core::keymap::Keymap;

use crate::app::{Action, CsmApp, Menu, Overlay, ProfilePick, PromptKind, QuickOpenState, RemoteKind};
use crate::backend::{ToolInfo, ToolPort};
use crate::dialog_keys::{self, Controls, Field, Pressed, Slot};
use crate::git_model::{WorktreeRow, folder_key};
use crate::icons;
use crate::model::{SessionView, Status, UiModel, fuzzy};
use crate::theme::{self, Tokens, icon, radius, size, space, text};
use crate::widgets::{self, ButtonKind};

/// The quick-open's folder-dialog button.
const BROWSE: &str = "Browse…";

/// One row of the palette.
#[derive(Debug, Clone, PartialEq)]
pub enum PalRow {
    Session {
        id: String,
        idx: Vec<usize>,
    },
    Search(String),
    Tool {
        id: String,
        title: String,
        summary: String,
        idx: Vec<usize>,
        hint: Option<String>,
    },
    /// A palette command from an enabled add-on: calls `tool` with its fixed params (`addons.rs`).
    Addon {
        title: String,
        tool: String,
        params: serde_json::Value,
        idx: Vec<usize>,
    },
    /// A place an enabled process add-on offers to open a session on (`remote_open`).
    Remote {
        addon: String,
        remote_id: String,
        title: String,
        detail: String,
        idx: Vec<usize>,
    },
    /// An add-on's screen: "Open <title>" (`screen_open`, on the active session).
    Screen {
        id: String,
        title: String,
        addon_name: String,
        icon: String,
        idx: Vec<usize>,
    },
    /// A Claude slash command for the active Claude session (`session_send_command`).
    Slash {
        command: String,
        description: String,
        idx: Vec<usize>,
    },
}

/// The palette title of a remote: `Remote… › add-on › label`.
fn remote_title(r: &csm_engine::RemoteView) -> String {
    format!("Remote… › {} › {}", r.addon_name, r.label)
}

/// Whether the palette and the main menu leave a tool out now: the Claude usage tools (the dashboard
/// among them) while that add-on is off, and what is on this laptop (or Quit) in a remote window.
pub fn tool_hidden(app: &CsmApp, id: &str) -> bool {
    let usage_off = !app.model.claude_usage_on() && USAGE_ADDON_TOOLS.contains(&id);
    // A remote window hides what is on this laptop, and Quit becomes Disconnect (plus the office's
    // own quit, asked for first).
    let remote = app.remote.is_some() && (crate::remote_window::is_local_only(id) || id == "app_quit" || id == "peer_connect_open");
    usage_off || remote
}

/// Tools shown before anything is typed (the mock's command list), in this order.
const SUGGESTED: [&str; 6] = ["inbox_next", "session_new", "group_restart_all", "open_with", "layout_set", "inspector_show"];

/// One palette row per layout mode, so "board" finds the Board directly (the generic layout_set
/// row only cycles to the next mode). Ids are `layout:<mode id>`, handled in `activate` (not tools
/// in the registry); the titles are static because rows borrow them.
const LAYOUT_ROWS: [(&str, &str); 6] = [
    ("layout:focus", "Layout: Focus"),
    ("layout:side", "Layout: Side by side"),
    ("layout:overview", "Layout: Overview"),
    ("layout:board", "Layout: Board"),
    ("layout:grid", "Layout: Grid"),
    ("layout:free", "Layout: Free"),
];

/// The palette's tool rows for a query. Empty query: the suggested tools, and after `>` every
/// tool. Otherwise the tools whose titles match, best first, with the layout rows. The
/// `consent_answer` tool is never listed (a consent prompt answers it, not the palette).
/// `hidden` drops tools that are offered but should not be listed now (the Claude usage tools
/// while that add-on is off).
fn tool_rows(port: &dyn ToolPort, query: &str, commands_only: bool, keymap: &Keymap, hidden: &dyn Fn(&str) -> bool) -> Vec<PalRow> {
    let tools: Vec<ToolInfo> = port.tools().iter().filter(|t| !hidden(&t.id)).cloned().collect();
    let row = |t: &ToolInfo, idx: Vec<usize>| PalRow::Tool {
        id: t.id.clone(),
        title: t.title.clone(),
        summary: t.summary.clone(),
        idx,
        hint: keymap.label(&t.id),
    };
    let mut out = Vec::new();
    if query.is_empty() {
        for id in SUGGESTED {
            if let Some(t) = tools.iter().find(|t| t.id == id) {
                out.push(row(t, Vec::new()));
            }
        }
        if commands_only {
            for t in tools.iter() {
                if !SUGGESTED.contains(&t.id.as_str()) && t.id != "consent_answer" {
                    out.push(row(t, Vec::new()));
                }
            }
        }
    } else {
        let mut hits: Vec<(f32, PalRow)> = tools
            .iter()
            .filter(|t| t.id != "consent_answer")
            .filter_map(|t| fuzzy(query, &t.title).map(|(sc, idx)| (sc, row(t, idx))))
            .collect();
        for (id, title) in LAYOUT_ROWS {
            if let Some((sc, idx)) = fuzzy(query, title) {
                hits.push((sc, PalRow::Tool { id: id.to_string(), title: title.to_string(), summary: String::new(), idx, hint: None }));
            }
        }
        hits.sort_by(|a, b| b.0.total_cmp(&a.0));
        out.extend(hits.into_iter().take(if commands_only { 40 } else { 8 }).map(|(_, r)| r));
    }
    out
}

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
    // The active Claude session's slash commands: typed `/c…`, or listed with ">".
    if model.active.as_deref().and_then(|a| model.session(a)).is_some_and(|s| s.is_claude() && s.status.is_live()) {
        for c in crate::screens::menus::claude_commands(app) {
            let hit = if q.is_empty() { commands_only.then(Vec::new) } else { fuzzy(q, &c.command).map(|(_, idx)| idx) };
            if let Some(idx) = hit {
                out.push(PalRow::Slash { command: c.command, description: c.description, idx });
            }
        }
    }
    let port = app.dispatch.as_ref();
    out.extend(tool_rows(port, q, commands_only, &app.prefs.keymap, &|id| tool_hidden(app, id)));
    if let Some(r) = app.remote.as_ref() {
        let rows = [
            ("remote_disconnect", format!("Disconnect from {}", r.host_name), "Close this window. The office keeps running."),
            ("remote_quit_office", format!("Quit CodeShellManager on {}…", r.host_name), "Ask the office to quit: its sessions get time to exit. Asks first."),
        ];
        for (id, title, summary) in rows {
            let hit = if q.is_empty() { Some(Vec::new()) } else { fuzzy(q, &title).map(|(_, idx)| idx) };
            if let Some(idx) = hit {
                out.push(PalRow::Tool { id: id.into(), title, summary: summary.into(), idx, hint: None });
            }
        }
    }
    // An enabled add-on's commands whose tool the port offers.
    let addon_cmds: Vec<(&csm_engine::addons::CommandSummary, String)> = app
        .model
        .addons
        .iter()
        .filter(|a| a.state == csm_engine::addons::AddonState::Loaded)
        .flat_map(|a| a.commands.iter())
        .filter(|c| port.has_tool(&c.tool))
        .map(|c| (c, c.tool.clone()))
        .collect();
    // Enabled add-ons' palette commands: all of them after ">", the matching ones when typing.
    for (c, tool) in &addon_cmds {
        if q.is_empty() {
            if commands_only {
                out.push(PalRow::Addon { title: c.title.clone(), tool: tool.clone(), params: c.params.clone(), idx: Vec::new() });
            }
        } else if let Some((_, idx)) = fuzzy(q, &c.title) {
            out.push(PalRow::Addon { title: c.title.clone(), tool: tool.clone(), params: c.params.clone(), idx });
        }
    }
    // Add-on screens: all of them after ">", the matching ones when typing.
    for d in app.backend.screen_declared() {
        let title = format!("Open {}", d.title);
        let hit = if q.is_empty() { commands_only.then(Vec::new) } else { fuzzy(q, &title).map(|(_, idx)| idx) };
        if let Some(idx) = hit {
            out.push(PalRow::Screen { id: d.id, title, addon_name: d.addon_name, icon: d.icon, idx });
        }
    }
    // Process add-ons' remotes: all of them after ">", the matching ones when typing.
    for r in &app.model.remotes {
        let title = remote_title(r);
        let hit = if q.is_empty() { commands_only.then(Vec::new) } else { fuzzy(q, &title).map(|(_, idx)| idx) };
        if let Some(idx) = hit {
            out.push(PalRow::Remote { addon: r.addon.clone(), remote_id: r.id.clone(), title, detail: r.detail.clone(), idx });
        }
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
    input_row_with(ui, icon_name, buf, hint, focus, id, 0.0).0
}

/// [`input_row`] with `reserve` points kept free left of the Esc key cap; returns that slot
/// (empty when `reserve` is 0) for a control the caller draws there.
fn input_row_with(ui: &mut Ui, icon_name: &str, buf: &mut String, hint: &str, focus: &mut bool, id: &str, reserve: f32) -> (egui::Response, Rect) {
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
    let slot_w = if reserve > 0.0 { reserve + space::SM } else { 0.0 };
    let er = Rect::from_min_max(pos2(rect.min.x + space::XXL + space::LG, rect.min.y), pos2(rect.max.x - space::XL - kw - slot_w, rect.max.y));
    let slot = Rect::from_min_max(pos2(er.max.x + space::SM, rect.min.y), pos2(er.max.x + slot_w, rect.max.y));
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
    (resp, slot)
}

/// A typed absolute path in the quick-open's field, by this platform's rule (`C:\src\app` and
/// `\\server\share` on Windows, `/srv/app` elsewhere). Pure: whether it exists is the engine's
/// call when the session starts.
pub fn typed_path(query: &str) -> Option<&str> {
    let q = query.trim();
    std::path::Path::new(q).is_absolute().then_some(q)
}

/// The quick-open is showing a typed path that Enter will use as is (no row is picked).
fn typed_path_wins(q: &crate::app::QuickOpenState) -> bool {
    q.edit.is_none() && !q.navigated && typed_path(&q.query).is_some()
}

/// The tools of the built-in Claude usage add-on that the palette leaves out while it is off.
pub const USAGE_ADDON_TOOLS: [&str; 3] = ["usage_report", "usage_session", "usage_summary"];

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
                    PalRow::Addon { .. } => "Add-on commands",
                    PalRow::Remote { .. } => "Remote",
                    PalRow::Screen { .. } => "Add-on screens",
                    PalRow::Slash { .. } => "Claude commands",
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
                        let key = app.prefs.keymap.label("search_output").unwrap_or_default();
                        let kw = widgets::kbd_width(ui, &key);
                        widgets::paint_kbd(ui, Rect::from_center_size(pos2(rect.max.x - space::SM - kw / 2.0, cy), vec2(kw, size::KBD)), &key);
                        l
                    }
                    PalRow::Addon { title, tool, idx, .. } => {
                        let ic = tool_icon(tool);
                        icons::paint(ui, Rect::from_min_size(pos2(x, cy - icon::LG / 2.0), Vec2::splat(icon::LG)), ic, icon::LG, t.muted);
                        x += icon::LG + space::ROW_X;
                        let job = highlighted(ui, title, idx, t.fg);
                        let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                        ui.painter().galley(pos2(x, cy - g.size().y / 2.0), g, t.fg);
                        title.clone()
                    }
                    PalRow::Screen { title, addon_name, icon: ic, idx, .. } => {
                        let ic = if icons::exists(ic) { ic.as_str() } else { "layers" };
                        icons::paint(ui, Rect::from_min_size(pos2(x, cy - icon::LG / 2.0), Vec2::splat(icon::LG)), ic, icon::LG, t.muted);
                        x += icon::LG + space::ROW_X;
                        let job = highlighted(ui, title, idx, t.fg);
                        let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                        let gw = g.size().x;
                        ui.painter().galley(pos2(x, cy - g.size().y / 2.0), g, t.fg);
                        widgets::paint_text(
                            ui,
                            pos2(x + gw + space::ROW_X, cy),
                            Align2::LEFT_CENTER,
                            addon_name,
                            theme::regular(text::CAPTION),
                            t.faint,
                            (rect.max.x - x - gw - space::LG).max(0.0),
                        );
                        title.clone()
                    }
                    PalRow::Remote { title, detail, idx, .. } => {
                        icons::paint(ui, Rect::from_min_size(pos2(x, cy - icon::LG / 2.0), Vec2::splat(icon::LG)), "server", icon::LG, t.muted);
                        x += icon::LG + space::ROW_X;
                        let job = highlighted(ui, title, idx, t.fg);
                        let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                        let gw = g.size().x;
                        ui.painter().galley(pos2(x, cy - g.size().y / 2.0), g, t.fg);
                        widgets::paint_text(
                            ui,
                            pos2(x + gw + space::ROW_X, cy),
                            Align2::LEFT_CENTER,
                            detail,
                            theme::regular(text::CAPTION),
                            t.faint,
                            (rect.max.x - x - gw - space::LG).max(0.0),
                        );
                        title.clone()
                    }
                    PalRow::Tool { id, title, summary, idx, hint } => {
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
                        if !summary.is_empty() {
                            widgets::paint_text(
                                ui,
                                pos2(x + gw + space::ROW_X, cy),
                                Align2::LEFT_CENTER,
                                summary,
                                theme::regular(text::CAPTION),
                                t.faint,
                                (right - x - gw - space::LG).max(0.0),
                            );
                        }
                        title.clone()
                    }
                    PalRow::Slash { command, description, idx } => {
                        icons::paint(ui, Rect::from_min_size(pos2(x, cy - icon::LG / 2.0), Vec2::splat(icon::LG)), "send", icon::LG, t.muted);
                        x += icon::LG + space::ROW_X;
                        let job = highlighted(ui, command, idx, t.fg);
                        let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                        let gw = g.size().x;
                        ui.painter().galley(pos2(x, cy - g.size().y / 2.0), g, t.fg);
                        let who = model.active.as_deref().and_then(|a| model.session(a)).map(|s| s.name.clone()).unwrap_or_default();
                        let detail = if description.is_empty() { format!("into {who}") } else { format!("{description} · into {who}") };
                        widgets::paint_text(
                            ui,
                            pos2(x + gw + space::ROW_X, cy),
                            Align2::LEFT_CENTER,
                            &detail,
                            theme::regular(text::CAPTION),
                            t.faint,
                            (rect.max.x - space::SM - x - gw - space::LG).max(0.0),
                        );
                        command.clone()
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
    if let Some(mode) = id.strip_prefix("layout:") {
        return crate::model::LayoutMode::ALL.iter().find(|m| m.id() == mode).map_or("grid", |m| m.icon());
    }
    let area = id.split('_').next().unwrap_or("");
    match id {
        "inbox_next" | "inbox_list" | "inbox_panel_set" => "inbox",
        "session_new" | "group_new" => "plus",
        "inspector_show" => "panelR",
        "session_reopen_closed" | "recently_closed_list" | "session_recent" => "history",
        "session_sleep" | "group_sleep_all" => "moon",
        "session_restart" | "group_restart_all" => "restart",
        "open_with" => "braces",
        "open_folder" => "folder",
        "session_status_clear" => "eraser",
        "group_tear_off" | "pane_tear_off" => "tear",
        "pane_minimize" => "min",
        "pane_maximize" => "max",
        "pane_restore" => "unmax",
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
        PalRow::Addon { tool, params, .. } => out.push(Action::ToolNamed(tool.clone(), params.clone())),
        PalRow::Remote { addon, remote_id, .. } => out.push(Action::Tool("remote_open", json!({"addon": addon, "remote_id": remote_id}))),
        PalRow::Screen { id, .. } => {
            let context = app.model.active.as_ref().map_or_else(|| json!({}), |a| json!({"session_id": a}));
            out.push(Action::Tool("screen_open", json!({"screen": id, "context": context})));
        }
        PalRow::Slash { command, .. } => {
            if let Some(a) = &app.model.active {
                out.push(Action::Tool("session_send_command", json!({"session_id": a, "command": command, "force": true})));
                out.push(Action::FocusTerminal(a.clone()));
            }
        }
        PalRow::Tool { id, .. } => {
            // A layout row: that mode. The generic layout tool: cycle to the next mode.
            if let Some(mode) = id.strip_prefix("layout:") {
                let scope = app.model.pane_scope();
                out.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": mode})));
            } else if id == "layout_set" {
                let scope = app.model.pane_scope();
                let cur = app.model.layout(&scope).mode;
                let modes = crate::model::LayoutMode::ALL;
                let next = modes.iter().position(|m| *m == cur).map_or(modes[0], |i| modes[(i + 1) % modes.len()]);
                out.push(Action::Tool("layout_set", json!({"group": scope.key(), "mode": next.id()})));
            } else {
                out.push(Action::PaletteTool(id.clone()));
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
        // The look is its own tool: it never needs a restart.
        match &q.profile {
            ProfilePick::Keep => {}
            ProfilePick::None => {
                if app.model.session(id).is_some_and(|s| s.look.is_some()) {
                    out.push(Action::Tool("session_look_set", json!({"session_id": id, "clear": true})));
                }
            }
            ProfilePick::Profile(_) => {
                if let Some(look) = picked_look(q) {
                    out.push(Action::Tool("session_look_set", json!({"session_id": id, "look": look})));
                }
            }
        }
        return out;
    }
    if q.remote {
        let mut params = match q.rkind {
            RemoteKind::Ssh => {
                let (program, args) = split_command(&cmd);
                let program = if program == "claude" { "bash".to_string() } else { program };
                json!({"command": program, "args": args, "kind": "ssh", "group": q.group, "ssh": ssh_params(q)})
            }
            RemoteKind::Wsl => {
                json!({"command": split_command(&cmd).0, "args": split_command(&cmd).1, "kind": "wsl", "group": q.group, "wsl": wsl_params(q)})
            }
        };
        stamp_profile(&mut params, q);
        use_addon_profile(&mut params, q);
        out.push(Action::Tool("session_new", params));
        return out;
    }
    let Some(folder) = chosen_folder(app) else { return out };
    let (program, args) = split_command(&cmd);
    let mut params = json!({"folder": folder, "command": program, "args": args, "group": q.group});
    stamp_profile(&mut params, q);
    use_addon_profile(&mut params, q);
    out.push(Action::Tool("session_new", params));
    // The ticked sibling worktrees: the same command, group and look, not focused (WPF's "Also
    // start sessions in sibling worktrees"). The engine staggers Claude launches.
    let look = picked_look(q);
    for path in checked_siblings(app, &folder) {
        let mut p = json!({"folder": path, "command": program, "args": args, "group": q.group, "focus": false});
        if let Some(look) = &look {
            p["look"] = look.clone();
        }
        use_addon_profile(&mut p, q);
        out.push(Action::Tool("session_new", p));
    }
    out
}

/// A new session that the picked add-on profile starts: `profile` replaces `command` and `args`
/// (the engine takes the command line from the profile, and refuses both together).
fn use_addon_profile(params: &mut serde_json::Value, q: &QuickOpenState) {
    let Some(id) = &q.addon_profile else { return };
    if let Some(o) = params.as_object_mut() {
        o.remove("command");
        o.remove("args");
        o.insert("profile".into(), json!(id));
    }
}

/// A new session's params with the picked profile: its look, and its name when the panel has
/// none typed.
fn stamp_profile(params: &mut serde_json::Value, q: &QuickOpenState) {
    let Some(look) = picked_look(q) else { return };
    params["look"] = look;
    if let ProfilePick::Profile(name) = &q.profile
        && q.name.trim().is_empty()
    {
        params["name"] = json!(name);
    }
}

/// The picked profile's look, as `profile_list` gave it.
fn picked_look(q: &QuickOpenState) -> Option<serde_json::Value> {
    let ProfilePick::Profile(name) = &q.profile else { return None };
    q.profiles.as_ref()?.iter().find(|p| p["name"].as_str() == Some(name.as_str())).map(|p| p["look"].clone()).filter(serde_json::Value::is_object)
}

/// Picking a profile in a new session fills what the person left empty (the WPF dialog's rule):
/// the folder from its starting directory, and its command line as the command.
fn take_profile(q: &mut QuickOpenState, profile: &serde_json::Value, launch: &[String]) {
    let name = profile["name"].as_str().unwrap_or_default().to_string();
    q.profile = ProfilePick::Profile(name);
    if q.edit.is_some() {
        return;
    }
    let dir = profile["starting_directory"].as_str().unwrap_or_default();
    if !q.remote && q.query.trim().is_empty() && !dir.trim().is_empty() {
        q.query = dir.to_string();
        q.navigated = false;
    }
    let cmd = profile["commandline"].as_str().unwrap_or_default().trim();
    if !cmd.is_empty() && !cmd.chars().any(char::is_control) {
        match launch.iter().position(|c| c.trim() == cmd) {
            Some(i) => {
                q.cmd = i;
                q.custom_cmd = None;
            }
            None => {
                q.custom_cmd = Some(cmd.to_string());
                q.cmd = launch.len();
            }
        }
    }
}

/// The local folder Enter starts in (new mode): a typed path, else the selected suggestion, else
/// the typed text.
pub fn chosen_folder(app: &CsmApp) -> Option<String> {
    let q = &app.ui.quick;
    let rows = folder_rows(app, &q.query);
    match rows.get(q.sel.min(rows.len().saturating_sub(1))).and_then(|(i, _)| app.model.folders.get(*i)) {
        // A typed (or browsed-to) path wins over a suggestion it merely resembles:
        // `C:\src\app` must not start in the suggested `C:\src\app-old`.
        _ if typed_path_wins(q) => Some(q.query.trim().to_string()),
        Some(f) => Some(f.path.clone()),
        None if !q.query.trim().is_empty() => Some(q.query.trim().to_string()),
        None => None,
    }
}

/// The other worktrees of `folder`'s repository the quick-open offers (from the last
/// `worktree_list {folder}` reply; `None` before it arrives): not the folder's own, not missing.
pub fn sibling_worktrees(model: &UiModel, folder: &str) -> Option<Vec<WorktreeRow>> {
    let key = folder_key(folder);
    model.folder_worktrees.get(&key).map(|rows| rows.iter().filter(|w| !w.current && !w.prunable && folder_key(&w.path) != key).cloned().collect())
}

/// The ticked sibling paths that still apply to `folder` (ticks for another folder don't).
fn checked_siblings(app: &CsmApp, folder: &str) -> Vec<String> {
    let q = &app.ui.quick;
    if q.edit.is_some() || q.remote || q.siblings_for.as_ref().is_none_or(|(f, _, _)| folder_key(f) != folder_key(folder)) {
        return Vec::new();
    }
    let offered = sibling_worktrees(&app.model, folder).unwrap_or_default();
    // The engine's `open` folder is the path in the system's form (git lists `C:/src/x`).
    offered
        .into_iter()
        .filter(|w| q.siblings.contains(&w.path))
        .map(|w| w.open.get("folder").and_then(serde_json::Value::as_str).filter(|f| !f.trim().is_empty()).map_or(w.path.clone(), str::to_string))
        .collect()
}

/// "Also start in sibling worktrees": one checkbox per other worktree of the chosen folder's
/// repository (`name  ⎇ branch`, and how many sessions are already there).
fn siblings_list(ui: &mut Ui, sibs: &[WorktreeRow], checked: &mut Vec<String>) {
    let t = Tokens::get(ui.ctx());
    let inset = space::MD + space::XS;
    ui.painter().hline(ui.max_rect().x_range(), ui.cursor().min.y, Stroke::new(size::HAIRLINE, t.surface0));
    ui.add_space(space::SM);
    ui.horizontal(|ui| {
        ui.add_space(inset);
        widgets::caps(ui, "Also start in sibling worktrees");
    });
    ui.add_space(space::XS);
    for w in sibs {
        ui.horizontal(|ui| {
            ui.add_space(inset);
            let mut on = checked.contains(&w.path);
            let shown = match &w.branch {
                Some(b) => format!("{}  ⎇ {b}", w.name),
                None => w.name.clone(),
            };
            let r = ui.checkbox(&mut on, egui::RichText::new(shown).font(theme::regular(text::BODY)).color(t.fg));
            let label = format!("Sibling worktree: {}", w.name);
            r.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Checkbox, true, on, &label));
            let r = r.on_hover_text(&w.path);
            // Tab stop: Enter flips the box the way a click does.
            let mut changed = r.changed();
            if qstop(ui, &r, false) && !changed {
                on = !on;
                changed = true;
            }
            if changed {
                if on {
                    checked.push(w.path.clone());
                } else {
                    checked.retain(|p| p != &w.path);
                }
            }
            let n = w.sessions.len();
            if n > 0 {
                let open = if n == 1 { "1 session there".to_string() } else { format!("{n} sessions there") };
                ui.label(egui::RichText::new(open).font(theme::regular(text::CAPTION)).color(t.muted));
            }
        });
    }
    ui.add_space(space::SM);
}

/// How long a folder stays chosen before its worktrees are read (typing a path doesn't run git
/// on every keystroke).
const SIBLING_READ_DELAY_S: f64 = 0.35;

/// New, local: keeps the sibling checkboxes on the chosen folder and asks for its worktrees once
/// it has stayed chosen [`SIBLING_READ_DELAY_S`] (once per opening of the quick-open, so a
/// worktree made since shows). The read is `worktree_list {folder}` on an engine worker.
fn track_siblings(ctx: &egui::Context, app: &mut CsmApp) {
    let q = &app.ui.quick;
    if q.edit.is_some() || q.remote {
        app.ui.quick.siblings_for = None;
        app.ui.quick.siblings.clear();
        return;
    }
    let now = ctx.input(|i| i.time);
    let chosen = chosen_folder(app).filter(|f| typed_path(f).is_some());
    let same = match (&chosen, &app.ui.quick.siblings_for) {
        (Some(f), Some((g, _, _))) => folder_key(f) == folder_key(g),
        (None, None) => true,
        _ => false,
    };
    if !same {
        app.ui.quick.siblings_for = chosen.map(|f| (f, now, false));
        app.ui.quick.siblings.clear();
    }
    if let Some((f, since, asked)) = &mut app.ui.quick.siblings_for
        && !*asked
    {
        let wait = SIBLING_READ_DELAY_S - (now - *since);
        if wait <= 0.0 {
            *asked = true;
            let folder = f.clone();
            app.dispatch.submit("worktree_list", json!({"folder": folder}));
        } else {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64(wait));
        }
    }
}

/// The Remote row: one chip per place an enabled process add-on offers (`remote_open`), under
/// "Remote…". Clicking one starts that session at once.
fn remote_row(ui: &mut Ui, remotes: &[csm_engine::RemoteView], picked: &mut Option<(String, String)>) {
    let t = Tokens::get(ui.ctx());
    let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::HAIRLINE), Sense::hover());
    ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(size::HAIRLINE, t.surface0));
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric((space::MD + space::XXS) as i8, space::SM as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Remote…").font(theme::regular(text::CAPTION)).color(t.muted))
                .on_hover_text("Places an add-on offers to open a session on");
            for r in remotes.iter().take(24) {
                let shown: String = r.label.chars().take(40).collect();
                let tip = format!("{} · {}
{}", r.addon_name, r.detail, "Click to open it");
                let resp = widgets::button(ui, ButtonKind::Ghost, Some("server"), &shown, None).on_hover_text(tip);
                let l = format!("Remote: {} › {}", r.addon_name, r.label);
                resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, l.clone()));
                if qstop(ui, &resp, false) {
                    *picked = Some((r.addon.clone(), r.id.clone()));
                }
            }
        });
    });
}

/// Whether a New session form can be submitted now: its choices are loaded, no submit is running
/// and the picked choice is one of them. Pure, so the dialog and its tests share the rule.
pub(crate) fn form_can_submit(f: &csm_engine::NewSessionView, choice: Option<&str>) -> bool {
    f.load == "ready" && f.submit != "running" && choice.is_some_and(|c| f.choices.iter().any(|x| x.id == c))
}

/// The New session row: one chip per form an enabled process add-on offers ("New NEXUS agent").
/// A chip opens the form under the row: its choices are asked (`new_session_load`), then the
/// picker and the submit (`new_session_submit`). The add-on's own tools do the work.
fn new_session_row(ui: &mut Ui, forms: &[csm_engine::NewSessionView], q: &mut QuickOpenState, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::HAIRLINE), Sense::hover());
    ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(size::HAIRLINE, t.surface0));
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric((space::MD + space::XXS) as i8, space::SM as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("New session").font(theme::regular(text::CAPTION)).color(t.muted));
            for f in forms.iter().take(8) {
                let open = q.new_form.as_ref().is_some_and(|(a, s)| *a == f.addon && *s == f.source);
                let kind = if open { ButtonKind::Primary } else { ButtonKind::Ghost };
                let resp = widgets::button(ui, kind, Some("server"), &f.title, None).on_hover_text(format!("{} · opens the form below", f.addon_name));
                let l = format!("New session form: {}", f.title);
                resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::Button, true, l.clone()));
                if qstop(ui, &resp, false) {
                    if open {
                        q.new_form = None;
                    } else {
                        q.new_form = Some((f.addon.clone(), f.source.clone()));
                        q.new_choice = None;
                        q.new_name.clear();
                        q.new_pending = false;
                        actions.push(Action::Tool("new_session_load", json!({"addon": f.addon, "source": f.source})));
                    }
                }
            }
        });
        if let Some(f) = q.new_form.clone().and_then(|(a, s)| forms.iter().find(|f| f.addon == a && f.source == s)) {
            new_session_panel(ui, f, q, actions);
        }
    });
}

/// The open form: the picker (loading, the reason it failed, empty, or the choices) and the submit
/// with its progress and outcome. A successful submit closes the dialog: the add-on has opened
/// (or listed) its session by then.
fn new_session_panel(ui: &mut Ui, f: &csm_engine::NewSessionView, q: &mut QuickOpenState, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    if q.new_pending && f.submit == "done" {
        q.new_pending = false;
        // The add-on's line (for example "Launched agent-x") outlives the dialog as a toast.
        if let Some(line) = f.submit_text.clone() {
            actions.push(Action::Toast(line));
        }
        actions.push(Action::CloseOverlays);
        return;
    }
    let ident = json!({"addon": f.addon, "source": f.source});
    ui.add_space(space::XS);
    match f.load {
        "ready" if f.choices.is_empty() => {
            ui.label(egui::RichText::new("Nothing to choose from yet.").font(theme::regular(text::BODY)).color(t.muted));
        }
        "ready" => {
            ui.label(egui::RichText::new(&f.choice_title).font(theme::regular(text::CAPTION)).color(t.muted));
            // Every choice the engine kept (up to its cap), in a scroll area: none is cut off.
            egui::ScrollArea::vertical().id_salt(("new_session_choices", f.addon.as_str(), f.source.as_str())).max_height(size::NEW_SESSION_CHOICES_H).auto_shrink([false, true]).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for c in &f.choices {
                        let on = q.new_choice.as_deref() == Some(c.id.as_str());
                        let kind = if on { ButtonKind::Primary } else { ButtonKind::Ghost };
                        let tip = if c.detail.is_empty() { c.id.clone() } else { format!("{} · {}", c.id, c.detail) };
                        let resp = widgets::button(ui, kind, None, &c.label, None).on_hover_text(tip);
                        let l = format!("{}: {}", f.choice_title, c.label);
                        resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::RadioButton, on, l.clone()));
                        if qstop(ui, &resp, false) {
                            q.new_choice = Some(c.id.clone());
                        }
                    }
                });
            });
        }
        "failed" => {
            let msg = f.load_error.clone().unwrap_or_else(|| "The add-on did not answer.".to_owned());
            ui.horizontal_wrapped(|ui| {
                ui.label(egui::RichText::new(msg).font(theme::regular(text::BODY)).color(t.red));
                let again = widgets::button(ui, ButtonKind::Secondary, None, "Try again", None);
                if qstop(ui, &again, false) {
                    actions.push(Action::Tool("new_session_load", ident.clone()));
                }
            });
        }
        _ => {
            ui.label(egui::RichText::new("Loading…").font(theme::regular(text::BODY)).color(t.muted));
        }
    }
    if let Some(title) = f.name_title.as_deref() {
        // The name field: optional, sent with the submit only when it is not blank (the engine
        // refuses a name with control characters or more than the cap, and says so on the form).
        ui.label(egui::RichText::new(title).font(theme::regular(text::CAPTION)).color(t.muted));
        let edit = egui::TextEdit::singleline(&mut q.new_name)
            .id(Id::new(("new_session_name", f.addon.as_str(), f.source.as_str())))
            .font(theme::regular(text::INPUT))
            .char_limit(csm_core::addons::manifest::MAX_NEW_SESSION_NAME)
            .desired_width(ui.available_width());
        let resp = ui.add(edit);
        let l = format!("{title} (optional)");
        resp.widget_info(move || egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, l.clone()));
        qstop_text(ui, &resp);
    }
    let can = form_can_submit(f, q.new_choice.as_deref());
    let running = f.submit == "running";
    let label = if running { "Starting…" } else { f.submit_label.as_str() };
    ui.horizontal(|ui| {
        let resp = ui.add_enabled_ui(can, |ui| widgets::button(ui, ButtonKind::Primary, None, label, None)).inner;
        if qstop(ui, &resp, false) && can {
            let choice = q.new_choice.clone().unwrap_or_default();
            let mut args = json!({"addon": f.addon, "source": f.source, "choice": choice});
            let name = q.new_name.trim();
            if f.name_title.is_some() && !name.is_empty() {
                args["name"] = json!(name);
            }
            q.new_pending = true;
            actions.push(Action::Tool("new_session_submit", args));
        }
        match (f.submit, f.submit_text.as_deref()) {
            ("running", progress) => {
                // The add-on's own progress line while it works; the dialog closes when it is done.
                let line = progress.unwrap_or("Starting. The session opens when the agent is ready.");
                ui.label(egui::RichText::new(line).font(theme::regular(text::CAPTION)).color(t.muted));
            }
            ("failed", Some(why)) => {
                ui.label(egui::RichText::new(why).font(theme::regular(text::CAPTION)).color(t.red));
            }
            _ => {}
        }
    });
}

/// The Profile row: none / keep / each Windows Terminal profile, one click each, then the enabled
/// add-ons' launch profiles (`addon_profiles`: id, name, command line; new sessions only).
fn profile_row(ui: &mut Ui, q: &mut QuickOpenState, has_look: bool, import_on: bool, launch: &[String], addon_profiles: &[(String, String, String)]) {
    let t = Tokens::get(ui.ctx());
    let (sep, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::HAIRLINE), Sense::hover());
    ui.painter().hline(sep.x_range(), sep.center().y, Stroke::new(size::HAIRLINE, t.surface0));
    egui::Frame::NONE.inner_margin(egui::Margin::symmetric((space::MD + space::XXS) as i8, space::SM as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(egui::RichText::new("Profile").font(theme::regular(text::CAPTION)).color(t.muted))
                .on_hover_text("A Windows Terminal profile's colours, font size, cursor and padding for this session");
            let chip = |ui: &mut Ui, label: &str, on: bool, tip: &str| -> bool {
                let kind = if on { ButtonKind::Primary } else { ButtonKind::Ghost };
                let shown: String = label.chars().take(40).collect();
                let r = widgets::button(ui, kind, None, &shown, None).on_hover_text(tip);
                let l = format!("Profile: {label}");
                r.widget_info(move || egui::WidgetInfo::selected(egui::WidgetType::Button, true, on, l.clone()));
                qstop(ui, &r, false)
            };
            if q.edit.is_some() && has_look && chip(ui, "Keep current", q.profile == ProfilePick::Keep, "Leave this session's look as it is") {
                q.profile = ProfilePick::Keep;
            }
            let none_on = q.profile == ProfilePick::None || (q.profile == ProfilePick::Keep && !has_look);
            if chip(ui, "None", none_on, "The app's terminal look") {
                q.profile = ProfilePick::None;
            }
            let list = q.profiles.clone().unwrap_or_default();
            for p in &list {
                let name = p["name"].as_str().unwrap_or_default();
                if name.is_empty() {
                    continue;
                }
                let on = q.profile == ProfilePick::Profile(name.to_string());
                let tip = format!("{} · {}", p["commandline"].as_str().unwrap_or_default(), p["source"].as_str().unwrap_or_default());
                if chip(ui, name, on, &tip) {
                    take_profile(q, p, launch);
                }
            }
            if q.edit.is_none() {
                for (id, name, command_line) in addon_profiles {
                    let on = q.addon_profile.as_deref() == Some(id.as_str());
                    let tip = format!("Add-on profile {id}: runs `{command_line}`. Click again to start a plain session.");
                    if chip(ui, &format!("Add-on: {name}"), on, &tip) {
                        q.addon_profile = if on { None } else { Some(id.clone()) };
                    }
                }
            }
            if import_on && q.profiles.as_ref().is_some_and(Vec::is_empty) {
                ui.label(egui::RichText::new("No Windows Terminal profiles found.").font(theme::regular(text::CAPTION)).color(t.faint));
            } else if !import_on {
                ui.label(
                    egui::RichText::new("Turn on Windows Terminal profiles in Settings › Appearance to pick one.")
                        .font(theme::regular(text::CAPTION))
                        .color(t.faint),
                );
            }
        });
    });
}

/// A launch command as `session_new` takes it: the program, then the rest as typed arguments
/// (`claude --continue` → `claude`, `--continue`).
pub fn split_command(cmd: &str) -> (String, Option<String>) {
    let cmd = cmd.trim();
    // A quoted program (`"C:\Program Files\PowerShell\7\pwsh.exe" -NoLogo`, as Windows Terminal
    // profiles write it): the path without its quotes, then the rest.
    if let Some(rest) = cmd.strip_prefix('"')
        && let Some((program, args)) = rest.split_once('"')
        && !program.trim().is_empty()
    {
        let args = args.trim();
        return (program.to_string(), (!args.is_empty()).then(|| args.to_string()));
    }
    match cmd.split_once(char::is_whitespace) {
        Some((p, a)) if !a.trim().is_empty() => (p.to_string(), Some(a.trim().to_string())),
        _ => (cmd.to_string(), None),
    }
}

/// The id the quick-open's Tab stops are kept under (`dialog_keys`).
pub const QUICK_ID: &str = "dlg-quick";

fn quick_id() -> Id {
    Id::new(QUICK_ID)
}

fn quick_seen_key() -> Id {
    quick_id().with("seen")
}

/// The stops drawn last frame, in Tab order: true where Enter starts the session (a text input or
/// the command row) rather than pressing a button. Clears the list for this frame.
fn quick_stops_prev(ctx: &egui::Context) -> Vec<bool> {
    let prev = ctx.data(|d| d.get_temp::<Vec<bool>>(quick_seen_key())).unwrap_or_default();
    ctx.data_mut(|d| d.insert_temp(quick_seen_key(), Vec::<bool>::new()));
    prev
}

/// The index the next stop drawn this frame gets.
fn quick_seen(ctx: &egui::Context) -> usize {
    ctx.data(|d| d.get_temp::<Vec<bool>>(quick_seen_key())).map_or(0, |v| v.len())
}

/// Registers a stop drawn this frame and returns its index.
fn quick_stop(ctx: &egui::Context, starts: bool) -> usize {
    let mut v = ctx.data(|d| d.get_temp::<Vec<bool>>(quick_seen_key())).unwrap_or_default();
    v.push(starts);
    let idx = v.len() - 1;
    ctx.data_mut(|d| d.insert_temp(quick_seen_key(), v));
    idx
}

fn quick_on(ctx: &egui::Context, idx: usize) -> bool {
    dialog_keys::current(ctx, quick_id()) == Some(Slot::Button(idx))
}

/// Forgets the Tab stops and focus of the quick-open (when it closes).
pub fn release_quick(ctx: &egui::Context) {
    dialog_keys::forget(ctx, quick_id());
    ctx.data_mut(|d| d.remove::<Vec<bool>>(quick_seen_key()));
}

/// A button as a Tab stop of the quick-open. It takes the focus ring while focused, a click moves
/// the focus to it, and Enter on it counts as a click. True when it was activated.
fn qstop(ui: &Ui, resp: &egui::Response, starts: bool) -> bool {
    let ctx = ui.ctx();
    let idx = quick_stop(ctx, starts);
    if quick_on(ctx, idx) {
        widgets::focus_ring(ui, resp.rect.expand(space::XS), radius::SM);
    }
    if resp.clicked() {
        // A click returns the focus to the folder field, where Enter has always started the session
        // (a field that is not drawn just never matches, so Enter then starts too).
        dialog_keys::set(ctx, quick_id(), Some(Slot::Field));
    }
    resp.clicked() || dialog_keys::pressed(ctx, quick_id()) == Some(Pressed::Button(idx))
}

/// A text input as a Tab stop of the quick-open: Tab moves into it and out of it (its own egui
/// focus follows the dialog's), and a click in it makes it the focus. Enter in it starts the session.
fn qstop_text(ui: &Ui, resp: &egui::Response) {
    let ctx = ui.ctx();
    let idx = quick_stop(ctx, true);
    qfocus(ctx, resp, Slot::Button(idx));
    if quick_on(ctx, idx) {
        widgets::focus_ring(ui, resp.rect.expand(space::XS), radius::SM);
    }
}

/// Keeps a text input's egui focus and the dialog's focus in step. `mine` is the slot the input is.
fn qfocus(ctx: &egui::Context, resp: &egui::Response, mine: Slot) {
    let on = dialog_keys::current(ctx, quick_id()) == Some(mine);
    // Only a click moves the dialog's focus here. egui's own Tab traversal also moves its focus
    // (it runs before the dialog reads the key), and that must not overrule the dialog's stop.
    if resp.clicked() {
        dialog_keys::set(ctx, quick_id(), Some(mine));
    } else if on && !resp.has_focus() {
        resp.request_focus();
    } else if !on && resp.has_focus() {
        ctx.memory_mut(|m| m.surrender_focus(resp.id));
    }
}

fn quick_open(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    scrim(ctx, "quick-scrim", actions);
    let editing = app.ui.quick.edit.clone();
    let commands = quick_commands(app);
    let n = folder_rows(app, &app.ui.quick.query).len();
    // The Tab stops (`dialog_keys`) are the controls drawn last frame, in draw order. Tab and
    // Shift+Tab move between them. Enter presses a button stop; in the folder field, on the
    // command row or in a text input it starts the session, as it always did.
    let prev = quick_stops_prev(ctx);
    let field = if app.ui.quick.edit.is_some() && app.ui.quick.remote { Field::None } else { Field::Text };
    let press = dialog_keys::take(ctx, quick_id(), &Controls { field, buttons: prev.len(), default: None });
    let stop_starts = matches!(press, Some(Pressed::Button(b)) if prev.get(b) == Some(&true));
    let (down, up, enter, ctrl_enter) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::COMMAND, Key::Enter),
        )
    });
    let enter = enter || stop_starts;
    if n > 0 && down {
        // The first arrow from a typed path lands on the first row, not the second.
        app.ui.quick.sel = if typed_path_wins(&app.ui.quick) { 0 } else { (app.ui.quick.sel + 1) % n };
        app.ui.quick.scroll_to_sel = true;
        app.ui.quick.navigated = true;
    }
    if n > 0 && up {
        app.ui.quick.sel = (app.ui.quick.sel + n - 1) % n;
        app.ui.quick.scroll_to_sel = true;
        app.ui.quick.navigated = true;
    }
    if editing.is_some() && (up || down) {
        // Edit: arrows pick a folder into the field (Enter saves, it doesn't start anything).
        let rows = folder_rows(app, &app.ui.quick.query);
        if let Some(f) = rows.get(app.ui.quick.sel).and_then(|(i, _)| app.model.folders.get(*i)) {
            app.ui.quick.query = f.path.clone();
        }
    }
    track_siblings(ctx, app);
    if enter || ctrl_enter {
        actions.extend(start_quick(app, ctrl_enter));
        if !ctrl_enter || editing.is_some() {
            return;
        }
        // Kept open: the ticked siblings started; another Enter must not start them again.
        app.ui.quick.siblings.clear();
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
    let mut browse = false;
    // The Profile picker: when profile import is on, or to clear the look an edited session has.
    let import_on = app.ui.settings.bool("ImportWindowsTerminalProfiles") == Some(true);
    let has_look = q.edit.as_deref().and_then(|id| model.session(id)).is_some_and(|s| s.look.is_some());
    let ask_profiles = import_on && q.profiles.is_none();
    if ask_profiles {
        // Asked once per opening, answered off the engine thread (it reads WT's settings file).
        q.profiles = Some(Vec::new());
    }
    let launch = model.launch_commands.clone();
    // Enabled add-ons' launch profiles: `addon-id.name`, its command line, for the Profile row.
    let addon_profiles: Vec<(String, String, String)> = model
        .addons
        .iter()
        .filter(|a| a.state == csm_engine::addons::AddonState::Loaded)
        .flat_map(|a| a.profiles.iter().map(|p| (p.id.clone(), p.name.clone(), p.command_line.clone())))
        .collect();
    let remotes = model.remotes.clone();
    let mut picked_remote: Option<(String, String)> = None;
    let new_forms = model.new_sessions.clone();
    let mut form_actions: Vec<Action> = Vec::new();
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
            qstop_text(ui, &r);
        }
        let (icon_name, hint) = if q.edit.is_some() { ("folder", "Folder: a path") } else { ("plus", "Folder to start in: type a name or a path") };
        let field_stop = !(q.remote && q.edit.is_some());
        let resp = if q.remote && q.edit.is_some() {
            // An SSH or WSL session's folder is in the connection fields below.
            ui.allocate_response(vec2(ui.available_width(), space::XS), Sense::hover())
        } else if q.remote {
            // A remote folder is typed in the connection fields: nothing local to browse to.
            input_row(ui, icon_name, &mut q.query, hint, &mut q.focus_input, "quick-input")
        } else {
            // Browse…: a native folder dialog, shown by the host after this frame.
            let bw = widgets::button_width(ui, Some("folder"), BROWSE, None);
            let (resp, slot) = input_row_with(ui, icon_name, &mut q.query, hint, &mut q.focus_input, "quick-input", bw);
            let br = Rect::from_center_size(pos2(slot.max.x - bw / 2.0, slot.center().y), vec2(bw, size::CONTROL));
            let b = ui
                .scope_builder(egui::UiBuilder::new().max_rect(br), |ui| widgets::button(ui, ButtonKind::Ghost, Some("folder"), BROWSE, None))
                .inner;
            if qstop(ui, &b.on_hover_text("Choose a folder with the system's folder dialog"), false) {
                browse = true;
            }
            resp
        };
        if field_stop {
            // The folder field is the dialog's field: Tab starts and ends there.
            qfocus(ui.ctx(), &resp, Slot::Field);
            if dialog_keys::current(ui.ctx(), quick_id()) == Some(Slot::Field) {
                widgets::focus_ring(ui, resp.rect.expand(space::XS), radius::SM);
            }
        }
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Folder"));
        changed = resp.changed();
        // Typing resets `navigated` (below, after the card), so a change this frame counts as unset.
        let path_wins = q.edit.is_none() && (changed || !q.navigated) && typed_path(&q.query).is_some();
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
                    let msg = if q.query.trim().is_empty() {
                        "No recent folders yet. Type a path, or Browse… for one.".to_string()
                    } else {
                        format!("No folder matches. Enter starts in “{}” if it exists.", q.query.trim())
                    };
                    ui.label(egui::RichText::new(msg).color(t.muted))
                });
                ui.add_space(space::LG);
            } else if path_wins {
                ui.horizontal(|ui| {
                    ui.add_space(space::MD + space::XS);
                    ui.label(egui::RichText::new(format!("Enter starts in “{}”. ↓ picks a folder below.", q.query.trim())).color(t.muted));
                });
                ui.add_space(space::XS);
            }
            for (i, (fi, idx)) in rows.iter().enumerate() {
                let Some(f) = model.folders.get(*fi) else { continue };
                let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::PALETTE_ROW), Sense::hover());
                let rect = outer.shrink2(vec2(space::XS + space::XXS, 0.0));
                let r = ui.interact(rect, Id::new(("qo-row", i)), Sense::click());
                if (i == q.sel && !path_wins) || r.hovered() {
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
                    q.navigated = true;
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
        if let Some(sibs) = q.siblings_for.as_ref().and_then(|(f, _, _)| sibling_worktrees(&model, f)).filter(|s| !s.is_empty()) {
            siblings_list(ui, &sibs, &mut q.siblings);
        }
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
                // The command row is one Tab stop: its focus takes Left and Right, which cycle it.
                let seg_resp = ui.interact(seg, Id::new("qo-cmd-seg"), Sense::hover());
                let seg_stop = quick_seen(ui.ctx());
                qstop(ui, &seg_resp, true);
                if quick_on(ui.ctx(), seg_stop) {
                    let (left, right) = ui.input_mut(|i| (i.consume_key(Modifiers::NONE, Key::ArrowLeft), i.consume_key(Modifiers::NONE, Key::ArrowRight)));
                    let len = commands.len().max(1);
                    if right {
                        q.cmd = (q.cmd + 1) % len;
                    }
                    if left {
                        q.cmd = (q.cmd + len - 1) % len;
                    }
                }
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
                widgets::kbd(ui, "Tab").on_hover_text("Tab and Shift+Tab move between the controls. On the command row, Left and Right change the command");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let kind = if q.remote { ButtonKind::Primary } else { ButtonKind::Secondary };
                    let tip = if q.remote { "Back to a local folder" } else { "Connect over SSH or WSL instead" };
                    let remote_btn = widgets::button(ui, kind, Some("server"), "Remote…", None).on_hover_text(tip);
                    if qstop(ui, &remote_btn, false) {
                        q.remote = !q.remote;
                    }
                    if q.edit.is_none() {
                        let gname = q.group.as_deref().and_then(|g| model.group(g)).map_or_else(|| "Ungrouped".to_string(), |g| g.name.clone());
                        let gr = widgets::button(ui, ButtonKind::Secondary, Some("layers"), &gname, None)
                            .on_hover_text("Group: suggested from the scope; pick another");
                        if qstop(ui, &gr, false) {
                            open_group_menu = Some(gr.rect.left_bottom());
                        }
                    }
                });
            });
        });
        if import_on || has_look || (q.edit.is_none() && !addon_profiles.is_empty()) {
            profile_row(ui, &mut q, has_look, import_on, &launch, &addon_profiles);
        }
        if q.edit.is_none() && !remotes.is_empty() {
            remote_row(ui, &remotes, &mut picked_remote);
        }
        if q.edit.is_none() && !new_forms.is_empty() {
            new_session_row(ui, &new_forms, &mut q, &mut form_actions);
        }
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
                                let kind_btn = widgets::button(ui, kind, None, l, None);
                                if qstop(ui, &kind_btn, false) {
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
                            qstop_text(ui, &r);
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
                            let distro_btn = widgets::button(ui, kind, None, d, None);
                            if qstop(ui, &distro_btn, false) {
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
            vec![
                (
                    "Enter",
                    if q.siblings.is_empty() {
                        format!("start {cmd}")
                    } else {
                        format!("start {cmd} here and in {} sibling{}", q.siblings.len(), if q.siblings.len() == 1 { "" } else { "s" })
                    },
                ),
                ("Ctrl Enter", "start and keep this open".to_string()),
            ]
        };
        for (k, what) in keys {
            let kw = widgets::kbd_width(ui, k);
            widgets::paint_kbd(ui, Rect::from_min_size(pos2(fx, fy - size::KBD / 2.0), vec2(kw, size::KBD)), k);
            let r = ui.painter().text(pos2(fx + kw + space::XS + space::XXS, fy), Align2::LEFT_CENTER, what, theme::regular(text::CAPTION), t.muted);
            fx = r.max.x + space::LG;
        }
        let tail = if q.edit.is_some() { "A new folder, command or connection needs a restart" } else { "Type a path or a folder name" };
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
            let save = ui.scope_builder(egui::UiBuilder::new().max_rect(br), |ui| widgets::button(ui, ButtonKind::Primary, None, "Save", None)).inner;
            if qstop(ui, &save, false) {
                save_clicked = true;
            }
        }
    });
    if changed {
        q.sel = 0;
        q.navigated = false;
    }
    if browse {
        // Start where the field points when it's a path, else in the selected suggestion.
        let start = typed_path(&q.query)
            .map(str::to_string)
            .or_else(|| rows.get(q.sel).and_then(|(i, _)| model.folders.get(*i)).map(|f| f.path.clone()))
            .unwrap_or_default();
        app.ui.dialog_request = Some(crate::app::DialogRequest::Folder { start });
    }
    actions.extend(form_actions);
    if let Some((addon, remote_id)) = picked_remote {
        app.ui.quick = q;
        actions.push(Action::CloseOverlays);
        actions.push(Action::Tool("remote_open", json!({"addon": addon, "remote_id": remote_id})));
        return;
    }
    if ask_distros {
        actions.push(Action::Tool("wsl_distros", json!({})));
    }
    if ask_profiles {
        actions.push(Action::Tool("profile_list", json!({})));
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
        PromptKind::RenameSession(_) => ("Rename session".to_string(), "Name"),
        PromptKind::RenameGroup(_) => ("Rename group".to_string(), "Name"),
        PromptKind::NewGroup => ("New group".to_string(), "Group name, e.g. Client X"),
        PromptKind::Broadcast(g) => (crate::screens::broadcast::title(&app.model, g), "Text for each session, e.g. git pull"),
        PromptKind::NewTask { assign_to: Some(s), .. } => {
            (format!("New task for {}", app.model.session(s).map_or("the session", |x| x.name.as_str())), "What should it do, e.g. Add tests for the parser")
        }
        PromptKind::NewTask { project, .. } => {
            let name = csm_core::tasks::project_label(project, |g| app.model.group(g).map(|x| x.name.clone()));
            (format!("New task in {name}"), "What should be done, e.g. Add tests for the parser")
        }
    };
    let ok = match &p.kind {
        PromptKind::Broadcast(_) => "Send…",
        PromptKind::NewTask { assign_to: Some(_), .. } => "Add and send",
        PromptKind::NewTask { .. } => "Add",
        _ => "Save",
    };
    // Tab moves between the text field and the buttons (`dialog_keys`); the field takes the
    // typing only while it has the focus.
    let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let press = crate::screens::overlays::pressed_button(ctx, crate::screens::overlays::PROMPT_ID);
    let mut submit = enter || press == Some(0);
    let mut cancel = press == Some(1);
    let prompt_id = Id::new(crate::screens::overlays::PROMPT_ID);
    let input = Id::new("prompt-input");
    let on_field = dialog_keys::current(ctx, prompt_id) == Some(Slot::Field);
    if on_field && !ctx.memory(|m| m.has_focus(input)) {
        p.focus = true;
    } else if !on_field && ctx.memory(|m| m.has_focus(input)) {
        ctx.memory_mut(|m| m.surrender_focus(input));
    }
    card(ctx, "prompt", size::DIALOG_W, |ui| {
        let t = Tokens::get(ui.ctx());
        ui.add_space(space::MD);
        ui.horizontal(|ui| {
            ui.add_space(space::LG);
            ui.label(egui::RichText::new(title.as_str()).font(theme::semibold(text::TITLE)).color(t.fg));
        });
        input_row(ui, "pencil", &mut p.text, hint, &mut p.focus, "prompt-input");
        ui.add_space(space::SM);
        ui.horizontal(|ui| {
            ui.add_space(space::LG);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(space::LG);
                if crate::screens::overlays::dialog_button(ui, prompt_id, 0, None, ButtonKind::Primary, ok, Some("Enter")).clicked() {
                    submit = true;
                }
                if crate::screens::overlays::dialog_button(ui, prompt_id, 1, None, ButtonKind::Ghost, "Cancel", None).clicked() {
                    cancel = true;
                }
            });
        });
        ui.add_space(space::MD);
    });
    if cancel {
        actions.push(Action::CloseOverlays);
    }
    let text_ = p.text.trim().to_string();
    if submit && !text_.is_empty() {
        actions.push(Action::CloseOverlays);
        actions.push(match &p.kind {
            PromptKind::RenameSession(id) => Action::Tool("session_rename", json!({"session_id": id, "name": text_})),
            PromptKind::RenameGroup(id) => Action::Tool("group_rename", json!({"group_id": id, "name": text_})),
            PromptKind::NewGroup => Action::Tool("group_new", json!({"name": text_})),
            PromptKind::Broadcast(g) => crate::screens::broadcast::confirm(&app.model, g, &text_),
            PromptKind::NewTask { project, assign_to } => {
                Action::NewTask { project: project.clone(), title: text_.clone(), assign_to: assign_to.clone() }
            }
        });
    }
    app.ui.prompt = Some(p);
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::sync::Arc;

    use csm_engine::{Effects, Event, Reply, Toolset};

    use super::*;
    use crate::backend::Call;

    #[test]
    fn a_picked_add_on_profile_replaces_the_command_and_args() {
        let mut params = json!({"folder": "C:/work", "command": "claude", "args": "--continue", "group": null});
        let mut q = QuickOpenState::default();
        use_addon_profile(&mut params, &q);
        assert_eq!(params["command"], "claude", "no profile picked: the launch command stays");
        q.addon_profile = Some("demo.agent".into());
        use_addon_profile(&mut params, &q);
        assert!(params.get("command").is_none() && params.get("args").is_none(), "{params}");
        assert_eq!(params["profile"], "demo.agent");
        assert_eq!(params["folder"], "C:/work");
    }

    /// A port with a fixed tool list that records any submitted call (the palette must not submit).
    struct FakePort {
        tools: Arc<[ToolInfo]>,
        calls: Vec<Call>,
    }

    impl ToolPort for FakePort {
        fn submit(&mut self, id: &str, params: serde_json::Value) {
            self.calls.push(Call { id: id.to_string(), params });
        }
        fn poll(&mut self) -> (Vec<(Call, Reply)>, Vec<Event>) {
            (Vec::new(), Vec::new())
        }
        fn busy(&self) -> bool {
            false
        }
        fn submitted(&self) -> &[Call] {
            &self.calls
        }
        fn tools(&self) -> Arc<[ToolInfo]> {
            self.tools.clone()
        }
    }

    fn info(id: &str, title: &str) -> ToolInfo {
        ToolInfo {
            id: id.into(),
            title: title.into(),
            summary: format!("{title}, summarised"),
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            input_schema: json!({}),
        }
    }

    fn fake_port() -> FakePort {
        let tools = vec![
            info("session_new", "New session…"),
            info("session_list", "List sessions"),
            info("inbox_next", "Next inbox item"),
            info("consent_answer", "Answer consent"),
            info("git_status", "Git status"),
        ];
        FakePort { tools: Arc::from(tools), calls: Vec::new() }
    }

    fn tool_ids(rows: &[PalRow]) -> Vec<&str> {
        rows.iter()
            .filter_map(|r| match r {
                PalRow::Tool { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_palette_lists_a_fake_ports_suggested_tools_with_their_summaries() {
        let port = fake_port();
        let rows = tool_rows(&port, "", false, &Keymap::default(), &|_| false);
        // SUGGESTED order, and only the ones the port offers.
        assert_eq!(tool_ids(&rows), ["inbox_next", "session_new"]);
        let PalRow::Tool { summary, title, .. } = &rows[1] else { panic!("{rows:?}") };
        assert_eq!((title.as_str(), summary.as_str()), ("New session…", "New session…, summarised"));
        assert!(port.calls.is_empty(), "listing never submits a tool");
    }

    #[test]
    fn after_a_greater_than_the_palette_lists_every_tool_but_consent() {
        let port = fake_port();
        let rows = tool_rows(&port, "", true, &Keymap::default(), &|_| false);
        assert_eq!(tool_ids(&rows), ["inbox_next", "session_new", "session_list", "git_status"]);
    }

    #[test]
    fn a_typed_query_matches_the_fake_ports_titles() {
        let port = fake_port();
        let rows = tool_rows(&port, "list", false, &Keymap::default(), &|_| false);
        let ids = tool_ids(&rows);
        assert!(ids.contains(&"session_list"), "{ids:?}");
        assert!(!ids.contains(&"consent_answer"), "consent is never a palette command: {ids:?}");
        assert!(port.calls.is_empty());
    }
}

#[cfg(test)]
mod new_session_form_tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
    use super::form_can_submit;
    use csm_engine::{Choice, NewSessionView};

    fn form(load: &'static str, submit: &'static str, choices: &[&str]) -> NewSessionView {
        NewSessionView {
            addon: "nexus".into(),
            addon_name: "NEXUS".into(),
            source: "agent".into(),
            title: "New NEXUS agent".into(),
            submit_label: "Create".into(),
            choice_title: "Project".into(),
            name_title: None,
            load,
            choices: choices.iter().map(|c| Choice { id: (*c).into(), label: (*c).into(), detail: String::new() }).collect(),
            load_error: None,
            submit,
            submit_text: None,
        }
    }

    #[test]
    fn a_form_submits_only_with_a_loaded_choice_and_no_submit_running() {
        let ready = form("ready", "idle", &["uog", "demo"]);
        assert!(form_can_submit(&ready, Some("uog")));
        assert!(!form_can_submit(&ready, None), "no choice picked");
        assert!(!form_can_submit(&ready, Some("other")), "a choice that is not offered");
        assert!(!form_can_submit(&form("loading", "idle", &[]), Some("uog")), "still loading");
        assert!(!form_can_submit(&form("failed", "idle", &[]), Some("uog")), "the load failed");
        assert!(!form_can_submit(&form("ready", "running", &["uog"]), Some("uog")), "a launch is running");
        assert!(form_can_submit(&form("ready", "failed", &["uog"]), Some("uog")), "a failed launch can be tried again");
        assert!(form_can_submit(&form("ready", "done", &["uog"]), Some("uog")));
    }

    #[test]
    fn an_empty_picker_offers_no_submit() {
        assert!(!form_can_submit(&form("ready", "idle", &[]), Some("")));
    }
}
