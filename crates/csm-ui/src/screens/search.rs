//! The search panel (Ctrl+F, or the palette's "Search all output and notes for …" row): the
//! results of `search_output` and `search_notes`, grouped by session with the matched terms
//! highlighted. Clicking a result focuses its session (`session_focus`); a session that is gone
//! offers to be reopened from its kept configuration (`search_reopen_session`). Notes are
//! labelled as notes and open the session's inspector on the Notes tab.
//!
//! The panel only draws state the app keeps in [`crate::app::SearchState`], filled from the
//! tools' replies; searching itself is [`Action::RunSearch`].

use egui::{Align2, CornerRadius, Id, Key, Modifiers, Order, Rect, Sense, Stroke, Ui, Vec2, pos2, vec2};
use serde_json::{Value, json};

use crate::app::{Action, CsmApp};
use crate::icons;
use crate::theme::{self, Tokens, icon, radius, size, space, text};
use crate::widgets::{self, ButtonKind};

/// One line of the result list.
enum Item {
    Session { id: String, name: String, folder: String, count: usize },
    Hit { session_id: String, session_name: String, ts: i64, excerpt: String, matches: Vec<[usize; 2]>, reopenable: bool },
    NotesHead { count: usize },
    Note { folder: String, session_ids: Vec<String>, excerpt: String, matches: Vec<[usize; 2]> },
}

fn ranges(v: &Value) -> Vec<[usize; 2]> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|r| {
                    let r = r.as_array()?;
                    Some([usize::try_from(r.first()?.as_u64()?).ok()?, usize::try_from(r.get(1)?.as_u64()?).ok()?])
                })
                .collect()
        })
        .unwrap_or_default()
}

fn text_of(v: &Value, key: &str) -> String {
    v[key].as_str().unwrap_or("").to_owned()
}

/// The list to draw: output hits grouped by session (in the order sessions first appear, i.e.
/// newest first), then the notes.
fn items(app: &CsmApp) -> Vec<Item> {
    let s = &app.ui.search;
    let mut order: Vec<String> = Vec::new();
    for h in &s.output {
        let id = text_of(h, "session_id");
        if !order.contains(&id) {
            order.push(id);
        }
    }
    let mut out = Vec::new();
    for id in order {
        let hits: Vec<&Value> = s.output.iter().filter(|h| h["session_id"].as_str() == Some(id.as_str())).collect();
        let live = app.model.session(&id);
        let first = hits.first();
        let name = live.map(|l| l.name.clone()).or_else(|| first.map(|h| text_of(h, "session_name"))).unwrap_or_default();
        let folder = live.map(|l| l.folder.clone()).or_else(|| first.map(|h| text_of(h, "folder"))).unwrap_or_default();
        out.push(Item::Session { id: id.clone(), name: name.clone(), folder, count: hits.len() });
        for h in hits {
            out.push(Item::Hit {
                session_id: id.clone(),
                session_name: name.clone(),
                ts: h["ts"].as_i64().unwrap_or(0),
                excerpt: text_of(h, "excerpt"),
                matches: ranges(&h["matches"]),
                reopenable: h["reopenable"].as_bool().unwrap_or(false),
            });
        }
    }
    if !s.notes.is_empty() {
        out.push(Item::NotesHead { count: s.notes.len() });
        for n in &s.notes {
            out.push(Item::Note {
                folder: text_of(n, "folder"),
                session_ids: n["session_ids"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default(),
                excerpt: text_of(n, "excerpt"),
                matches: ranges(&n["matches"]),
            });
        }
    }
    out
}

/// "now", "4m ago", "3h ago", "2d ago".
pub fn ago(ts_ms: i64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| i64::try_from(d.as_millis()).unwrap_or(0)).unwrap_or(0);
    let s = (now - ts_ms).max(0) / 1000;
    match s {
        0..=44 => "now".into(),
        45..=3599 => format!("{}m ago", (s / 60).max(1)),
        3600..=86_399 => format!("{}h ago", s / 3600),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// An excerpt with its matched terms in the accent colour, one line, ellipsised.
fn excerpt_job(ui: &Ui, excerpt: &str, matches: &[[usize; 2]], max_w: f32) -> egui::text::LayoutJob {
    let t = Tokens::get(ui.ctx());
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = max_w;
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    let mut buf = String::new();
    let mut on = false;
    let flush = |job: &mut egui::text::LayoutJob, buf: &mut String, on: bool| {
        if buf.is_empty() {
            return;
        }
        let (font, color) = if on { (theme::semibold(text::BODY), t.blue) } else { (theme::regular(text::BODY), t.fg2) };
        job.append(buf, 0.0, egui::TextFormat { font_id: font, color, ..Default::default() });
        buf.clear();
    };
    for (i, c) in excerpt.chars().enumerate() {
        let hit = matches.iter().any(|m| i >= m[0] && i < m[1]);
        if hit != on {
            flush(&mut job, &mut buf, on);
            on = hit;
        }
        buf.push(c);
    }
    flush(&mut job, &mut buf, on);
    job
}

fn scrim(ctx: &egui::Context, actions: &mut Vec<Action>) {
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("search-scrim")).order(Order::Foreground).fixed_pos(screen.min).show(ctx, |ui| {
        let (rect, resp) = ui.allocate_exact_size(screen.size(), Sense::click());
        ui.painter().rect_filled(rect, 0.0, t.scrim);
        if resp.clicked() {
            actions.push(Action::CloseOverlays);
        }
    });
}

pub fn show(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    scrim(ctx, actions);
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    let w = size::PALETTE_W;
    let pos = pos2(screen.center().x - w / 2.0, screen.min.y + size::PALETTE_TOP);
    let list = items(app);
    let enter = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter));
    let mut query = app.ui.search.query.clone();
    let mut focus = app.ui.search.focus_input;
    let mut picked: Option<usize> = None;
    let mut offer_action: Option<bool> = None;
    let model = app.model.clone();
    let (pending, error, searched, offer) = {
        let s = &app.ui.search;
        (s.pending > 0, s.error.clone(), s.searched.clone(), s.offer.clone())
    };
    egui::Area::new(Id::new("search-card")).order(Order::Foreground).fixed_pos(pos).show(ctx, |ui| {
        egui::Frame::new()
            .fill(t.pop)
            .stroke(Stroke::new(size::HAIRLINE, t.surface1))
            .corner_radius(CornerRadius::same(radius::XL))
            .shadow(egui::Shadow { offset: [0, 16], blur: 40, spread: 0, color: t.shadow })
            .show(ui, |ui| {
                ui.set_width(w);
                ui.spacing_mut().item_spacing.y = 0.0;
                // Input row.
                let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::PALETTE_INPUT), Sense::hover());
                ui.painter().hline(rect.x_range(), rect.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, t.surface0));
                icons::paint(
                    ui,
                    Rect::from_min_size(pos2(rect.min.x + space::MD + space::XXS, rect.center().y - icon::XXL / 2.0), Vec2::splat(icon::XXL)),
                    "search",
                    icon::XXL,
                    t.muted,
                );
                let kw = widgets::kbd_width(ui, "Esc");
                widgets::paint_kbd(ui, Rect::from_center_size(pos2(rect.max.x - space::MD - kw / 2.0, rect.center().y), vec2(kw, size::KBD)), "Esc");
                let er = Rect::from_min_max(pos2(rect.min.x + space::XXL + space::LG, rect.min.y), pos2(rect.max.x - space::XL - kw, rect.max.y));
                let edit = egui::TextEdit::singleline(&mut query)
                    .id(Id::new("search-input"))
                    .frame(egui::Frame::NONE)
                    .hint_text(egui::RichText::new("Search all terminal output and notes").color(t.faint))
                    .font(theme::regular(text::INPUT))
                    .vertical_align(egui::Align::Center)
                    .desired_width(er.width());
                let resp = ui.put(er, edit);
                if focus {
                    resp.request_focus();
                    focus = false;
                }
                // Status line.
                let status = if let Some(e) = &error {
                    Some((e.clone(), t.red))
                } else if pending {
                    Some(("Searching…".to_owned(), t.muted))
                } else if let Some(q) = &searched {
                    let n = list.iter().filter(|i| matches!(i, Item::Hit { .. } | Item::Note { .. })).count();
                    Some((if n == 0 { format!("No output or note matches “{q}”.") } else { format!("{n} results for “{q}”") }, t.muted))
                } else {
                    None
                };
                if let Some((s, color)) = status {
                    ui.add_space(space::XS);
                    ui.horizontal(|ui| {
                        ui.add_space(space::MD + space::XS);
                        ui.add(egui::Label::new(egui::RichText::new(s).font(theme::regular(text::CAPTION)).color(color)).truncate());
                    });
                    ui.add_space(space::XS);
                }
                egui::ScrollArea::vertical().max_height(size::PALETTE_LIST_H).auto_shrink([false, true]).show(ui, |ui| {
                    if list.is_empty() && searched.is_none() {
                        ui.add_space(space::XL);
                        ui.vertical_centered(|ui| {
                            ui.label(egui::RichText::new("Type a word or phrase and press Enter.").color(t.muted));
                        });
                        ui.add_space(space::XL);
                    }
                    for (i, item) in list.iter().enumerate() {
                        let (outer, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::PALETTE_ROW), Sense::hover());
                        let rect = outer.shrink2(vec2(space::XS + space::XXS, 0.0));
                        let cy = rect.center().y;
                        match item {
                            Item::Session { id, name, folder, count } => {
                                let accent = model.session(id).map(|s| t.accent(s.accent)).unwrap_or(t.faint);
                                let x = rect.min.x + space::SM;
                                ui.painter().rect_filled(
                                    Rect::from_center_size(pos2(x + size::STRIPE / 2.0, cy), vec2(size::STRIPE, space::LG)),
                                    CornerRadius::same(radius::SM / 2),
                                    accent,
                                );
                                let nx = x + size::STRIPE + space::ROW_X;
                                let r = widgets::paint_text(
                                    ui,
                                    pos2(nx, cy),
                                    Align2::LEFT_CENTER,
                                    name,
                                    theme::semibold(text::BODY),
                                    t.fg,
                                    rect.width() / 2.0,
                                );
                                let gone = if model.session(id).is_none() { "  ·  closed" } else { "" };
                                widgets::paint_text(
                                    ui,
                                    pos2(r.max.x + space::ROW_X, cy),
                                    Align2::LEFT_CENTER,
                                    &format!("{folder}{gone}"),
                                    theme::regular(text::CAPTION),
                                    t.muted,
                                    (rect.max.x - r.max.x - space::XXL * 2.0).max(0.0),
                                );
                                ui.painter().text(
                                    pos2(rect.max.x - space::SM, cy),
                                    Align2::RIGHT_CENTER,
                                    count.to_string(),
                                    theme::regular(text::CAPTION),
                                    t.faint,
                                );
                            }
                            Item::NotesHead { count } => {
                                icons::paint(
                                    ui,
                                    Rect::from_min_size(pos2(rect.min.x + space::SM, cy - icon::MD / 2.0), Vec2::splat(icon::MD)),
                                    "note",
                                    icon::MD,
                                    t.yellow,
                                );
                                ui.painter().text(
                                    pos2(rect.min.x + space::SM + icon::MD + space::ROW_X, cy),
                                    Align2::LEFT_CENTER,
                                    "Notes",
                                    theme::semibold(text::BODY),
                                    t.fg,
                                );
                                ui.painter().text(
                                    pos2(rect.max.x - space::SM, cy),
                                    Align2::RIGHT_CENTER,
                                    count.to_string(),
                                    theme::regular(text::CAPTION),
                                    t.faint,
                                );
                            }
                            Item::Hit { session_name, ts, excerpt, matches, .. } => {
                                let r = ui.interact(rect, Id::new(("search-hit", i)), Sense::click());
                                if r.hovered() {
                                    ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), t.row_sel);
                                }
                                let age = ago(*ts);
                                let aw = widgets::text_width(ui, &age, theme::regular(text::CAPTION));
                                ui.painter().text(
                                    pos2(rect.max.x - space::SM, cy),
                                    Align2::RIGHT_CENTER,
                                    &age,
                                    theme::regular(text::CAPTION),
                                    t.faint,
                                );
                                let x = rect.min.x + space::SM + size::STRIPE + space::ROW_X;
                                let job = excerpt_job(ui, excerpt, matches, (rect.max.x - x - aw - space::LG).max(0.0));
                                let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                                ui.painter().galley(pos2(x, cy - g.size().y / 2.0), g, t.fg);
                                r.widget_info(|| {
                                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Search result: {session_name}: {excerpt}"))
                                });
                                if r.clicked() {
                                    picked = Some(i);
                                }
                            }
                            Item::Note { folder, excerpt, matches, .. } => {
                                let r = ui.interact(rect, Id::new(("search-note", i)), Sense::click());
                                if r.hovered() {
                                    ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), t.row_sel);
                                }
                                let x = rect.min.x + space::SM + size::STRIPE + space::ROW_X;
                                let fr = widgets::paint_text(
                                    ui,
                                    pos2(x, cy),
                                    Align2::LEFT_CENTER,
                                    folder,
                                    theme::regular(text::CAPTION),
                                    t.muted,
                                    rect.width() / 3.0,
                                );
                                let x2 = fr.max.x + space::ROW_X;
                                let job = excerpt_job(ui, excerpt, matches, (rect.max.x - x2 - space::LG).max(0.0));
                                let g = ui.ctx().fonts_mut(|f| f.layout_job(job));
                                ui.painter().galley(pos2(x2, cy - g.size().y / 2.0), g, t.fg);
                                r.widget_info(|| {
                                    egui::WidgetInfo::labeled(egui::WidgetType::Button, true, format!("Search note: {folder}: {excerpt}"))
                                });
                                if r.clicked() {
                                    picked = Some(i);
                                }
                            }
                        }
                    }
                    ui.add_space(space::XS + space::XXS);
                });
                // A result whose session is gone: offer to reopen it.
                if let Some((_, name)) = &offer {
                    let (frect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL + space::MD), Sense::hover());
                    ui.painter().hline(frect.x_range(), frect.min.y, Stroke::new(size::HAIRLINE, t.surface0));
                    ui.scope_builder(egui::UiBuilder::new().max_rect(frect.shrink2(vec2(space::MD, 0.0))), |ui| {
                        ui.horizontal_centered(|ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(format!("“{name}” is closed. Reopen it from its saved configuration?"))
                                        .font(theme::regular(text::BODY))
                                        .color(t.fg),
                                )
                                .truncate(),
                            );
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if widgets::button(ui, ButtonKind::Primary, None, "Reopen", None).clicked() {
                                    offer_action = Some(true);
                                }
                                if widgets::button(ui, ButtonKind::Ghost, None, "Not now", None).clicked() {
                                    offer_action = Some(false);
                                }
                            });
                        });
                    });
                }
                // Footer.
                let (frect, _) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL + space::SM), Sense::hover());
                ui.painter().hline(frect.x_range(), frect.min.y, Stroke::new(size::HAIRLINE, t.surface0));
                let fy = frect.center().y;
                let mut fx = frect.min.x + space::MD + space::XXS;
                let kw = widgets::kbd_width(ui, "Enter");
                widgets::paint_kbd(ui, Rect::from_min_size(pos2(fx, fy - size::KBD / 2.0), vec2(kw, size::KBD)), "Enter");
                fx += kw + space::XS + space::XXS;
                ui.painter().text(pos2(fx, fy), Align2::LEFT_CENTER, "search", theme::regular(text::CAPTION), t.muted);
                ui.painter().text(
                    pos2(frect.max.x - space::MD, fy),
                    Align2::RIGHT_CENTER,
                    "Click a result to jump to its session",
                    theme::regular(text::CAPTION),
                    t.muted,
                );
            });
    });
    app.ui.search.focus_input = focus;
    app.ui.search.query = query;
    if enter {
        actions.push(Action::RunSearch);
    }
    match offer_action {
        Some(true) => {
            if let Some((id, _)) = app.ui.search.offer.take() {
                actions.push(Action::CloseOverlays);
                actions.push(Action::Tool("search_reopen_session", json!({"session_id": id})));
            }
        }
        Some(false) => app.ui.search.offer = None,
        None => {}
    }
    if let Some(i) = picked
        && let Some(item) = list.get(i)
    {
        match item {
            Item::Hit { session_id, session_name, reopenable, .. } => {
                if app.model.session(session_id).is_some() {
                    actions.push(Action::CloseOverlays);
                    actions.push(Action::Tool("session_focus", json!({"session_id": session_id})));
                } else if *reopenable {
                    app.ui.search.offer = Some((session_id.clone(), session_name.clone()));
                } else {
                    actions.push(Action::Toast(format!("“{session_name}” is closed and its configuration was not kept.")));
                }
            }
            Item::Note { folder, session_ids, .. } => {
                if let Some(id) = session_ids.iter().find(|s| app.model.session(s).is_some()) {
                    actions.push(Action::CloseOverlays);
                    actions.push(Action::Tool("session_focus", json!({"session_id": id})));
                    actions.push(Action::Tool("inspector_show", json!({"tab": "notes"})));
                } else {
                    actions.push(Action::Toast(format!("No open session in {folder}: open one there to read the note.")));
                }
            }
            Item::Session { .. } | Item::NotesHead { .. } => {}
        }
    }
}
