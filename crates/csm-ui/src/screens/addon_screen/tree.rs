//! Draws a validated add-on view tree (`csm_core::addons::screen::Node`) with CSM's own widgets
//! and theme tokens. The tree is data: a plugin picks semantic tones, gaps and styles, never
//! colours or sizes, and nothing here lets it place a node outside its frame.
//!
//! Every string the plugin sent is drawn through `reveal` (invisible characters become visible
//! markers) or was cleaned when the tree was parsed (labels).

use std::sync::Arc;

use csm_core::addons::screen::{
    ActionPolicy, Align, Badge, Bind, ButtonKind as BK, Comment, EventKind, FileEntry, FileRow, Gap, Kind, ListItem, Node, Opt, TextStyle, Tone, reveal,
};
use egui::text::{LayoutJob, TextFormat};
use egui::{Align2, Color32, CornerRadius, Id, Layout, Rect, Sense, Stroke, TextWrapMode, Ui, UiBuilder, WidgetInfo, WidgetType, pos2, vec2};
use serde_json::{Value, json};

use super::{Cx, Out, md};
use crate::icons;
use crate::model::Status;
use crate::theme::{self, Tokens, alpha, icon, radius, semibold, size, space, text};
use crate::widgets::{self, ButtonKind};

/// The tallest a list, a file list or a split takes when its parent gives no bound (inside a
/// scroll area).
const UNBOUNDED_H: f32 = 420.0;

pub(super) fn tone_color(t: &Tokens, tone: Tone) -> Color32 {
    match tone {
        Tone::Default => t.fg,
        Tone::Muted => t.muted,
        Tone::Info => t.blue,
        Tone::Success => t.green,
        Tone::Warning => t.yellow,
        Tone::Danger => t.red,
        Tone::Accent => t.mauve,
    }
}

fn gap_px(g: Gap) -> f32 {
    match g {
        Gap::None => 0.0,
        Gap::Xs => space::XS,
        Gap::Sm => space::SM,
        Gap::Md => space::MD,
        Gap::Lg => space::LG,
    }
}

fn named(resp: &egui::Response, ty: WidgetType, label: impl Into<String>) {
    let l = label.into();
    resp.widget_info(move || WidgetInfo::labeled(ty, true, l.clone()));
}

/// A label's text with invisible characters marked.
pub(super) fn job(t: &Tokens, s: &str, font: egui::FontId, color: Color32) -> LayoutJob {
    let mut job = LayoutJob::default();
    for seg in reveal(s) {
        let fmt = TextFormat {
            font_id: font.clone(),
            color: if seg.marker { t.yellow } else { color },
            background: if seg.marker { Tokens::tint(t.yellow, alpha::PILL) } else { Color32::TRANSPARENT },
            ..TextFormat::default()
        };
        job.append(&seg.text, 0.0, fmt);
    }
    job
}

fn finite_h(ui: &Ui) -> f32 {
    let h = ui.available_height();
    if h.is_finite() && h > 0.0 { h.min(4000.0) } else { UNBOUNDED_H }
}

/// Draws `n` and its children.
pub(super) fn node(ui: &mut Ui, cx: &mut Cx<'_>, n: &Node) {
    let key = n.key.clone().unwrap_or_default();
    let t = cx.t;
    match &n.kind {
        Kind::Column(s) => {
            let layout = match s.align {
                Align::Center => Layout::top_down(egui::Align::Center),
                Align::End => Layout::top_down(egui::Align::Max),
                Align::Stretch => Layout::top_down_justified(egui::Align::Min),
                Align::Start => Layout::top_down(egui::Align::Min),
            };
            ui.with_layout(layout, |ui| {
                ui.spacing_mut().item_spacing.y = gap_px(s.gap);
                for c in &s.children {
                    node(ui, cx, c);
                }
            });
        }
        Kind::Row(s) => {
            let valign = match s.align {
                Align::Center | Align::Stretch => egui::Align::Center,
                Align::End => egui::Align::Max,
                Align::Start => egui::Align::Min,
            };
            ui.with_layout(Layout::left_to_right(valign).with_main_wrap(true), |ui| {
                ui.spacing_mut().item_spacing.x = gap_px(s.gap);
                ui.set_row_height(size::CONTROL);
                for c in &s.children {
                    node(ui, cx, c);
                }
            });
        }
        Kind::Toolbar { children } => {
            let r = ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::SM;
                ui.set_min_height(size::CONTROL_LG);
                ui.with_layout(Layout::right_to_left(egui::Align::Center), |ui| {
                    // Right-aligned, in document order from the left of that group.
                    for c in children.iter().rev() {
                        node(ui, cx, c);
                    }
                });
            });
            ui.painter().hline(r.response.rect.x_range(), r.response.rect.max.y + size::HAIRLINE, Stroke::new(size::HAIRLINE, t.surface0));
        }
        Kind::Split { children, ratio, min_px } => split(ui, cx, &key, children, *ratio, *min_px),
        Kind::Scroll { child } => {
            egui::ScrollArea::vertical().id_salt((cx.info.instance.clone(), "scroll", key.clone(), ui.next_auto_id())).auto_shrink([false, false]).show(ui, |ui| {
                ui.set_width(ui.available_width());
                node(ui, cx, child);
            });
        }
        Kind::Tabs { tabs, selected, child } => {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = space::XS;
                for tab in tabs {
                    let on = &tab.key == selected;
                    let font = if on { semibold(text::BODY) } else { theme::medium(text::BODY) };
                    let badge = tab.badge.as_deref().unwrap_or("");
                    let lw = widgets::text_width(ui, &tab.label, font.clone());
                    let bw = if badge.is_empty() { 0.0 } else { widgets::text_width(ui, badge, theme::medium(text::TINY)) + space::MD };
                    let w = lw + bw + space::LG + if badge.is_empty() { 0.0 } else { space::XS };
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::CONTROL), Sense::click());
                    if on {
                        ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), t.row_sel);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, CornerRadius::same(radius::MD), t.row_hover);
                    }
                    let fg = if on { t.fg } else { t.muted };
                    ui.painter().text(pos2(rect.min.x + space::SM, rect.center().y), Align2::LEFT_CENTER, &tab.label, font, fg);
                    if !badge.is_empty() {
                        let br = Rect::from_center_size(pos2(rect.max.x - space::SM - bw / 2.0, rect.center().y), vec2(bw, size::COUNT));
                        ui.painter().rect_filled(br, CornerRadius::same(radius::PILL), t.surface0);
                        ui.painter().text(br.center(), Align2::CENTER_CENTER, badge, theme::medium(text::TINY), t.fg2);
                    }
                    resp.widget_info({
                        let l = format!("Tab: {}", tab.label);
                        move || WidgetInfo::selected(WidgetType::RadioButton, true, on, l.clone())
                    });
                    if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !on {
                        cx.event(&key, EventKind::Select, json!({"key": tab.key}));
                    }
                }
            });
            if let Some(c) = child {
                ui.add_space(space::SM);
                node(ui, cx, c);
            }
        }
        Kind::Section { title, badge, collapsible, collapsed, children } => {
            let id = (cx.info.instance.clone(), key.clone());
            let folded = *collapsible && cx.st.sections.get(&id).copied().unwrap_or(*collapsed);
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), size::CONTROL), if *collapsible { Sense::click() } else { Sense::hover() });
            let mut x = rect.min.x;
            if *collapsible {
                icons::paint(ui, Rect::from_min_size(pos2(x, rect.center().y - icon::MD / 2.0), vec2(icon::MD, icon::MD)), if folded { "chevR" } else { "chevD" }, icon::MD, t.muted);
                x += icon::MD + space::XS;
            }
            let tr = ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, title, semibold(text::BODY), t.fg);
            if let Some(b) = badge {
                let bw = widgets::text_width(ui, b, theme::medium(text::TINY)) + space::MD;
                let br = Rect::from_min_size(pos2(tr.max.x + space::SM, rect.center().y - size::COUNT / 2.0), vec2(bw, size::COUNT));
                ui.painter().rect_filled(br, CornerRadius::same(radius::PILL), t.surface0);
                ui.painter().text(br.center(), Align2::CENTER_CENTER, b, theme::medium(text::TINY), t.fg2);
            }
            ui.painter().hline(rect.x_range(), rect.max.y, Stroke::new(size::HAIRLINE, t.surface0));
            if *collapsible {
                named(&resp, WidgetType::Button, format!("Section: {title}"));
                if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                    cx.st.sections.insert(id, !folded);
                }
            }
            if !folded {
                ui.add_space(space::XS);
                for c in children {
                    node(ui, cx, c);
                }
            }
        }
        Kind::Divider => widgets::separator(ui),
        Kind::Spacer => {
            ui.add_space(space::LG);
        }
        Kind::Text { text: s, style, tone, wrap } => {
            let (font, color) = match style {
                TextStyle::Body => (theme::regular(text::BODY), tone_color(&t, *tone)),
                TextStyle::Caption => (theme::regular(text::CAPTION), if *tone == Tone::Default { t.muted } else { tone_color(&t, *tone) }),
                TextStyle::Title => (semibold(text::TITLE), tone_color(&t, *tone)),
                TextStyle::Mono => (theme::mono(text::MONO), tone_color(&t, *tone)),
            };
            let label = egui::Label::new(job(&t, s, font, color));
            ui.add(if *wrap { label.wrap() } else { label.wrap_mode(TextWrapMode::Truncate) });
        }
        Kind::Markdown { blocks } => md::show(ui, cx, &key, blocks),
        Kind::Badge(b) => {
            chip(ui, &t, b);
        }
        Kind::Icon { name, tone } => {
            icons::show(ui, name, icon::LG, tone_color(&t, *tone));
        }
        Kind::Kv { rows } => {
            egui::Grid::new((cx.info.instance.clone(), "kv", ui.next_auto_id())).num_columns(2).spacing([space::LG, space::XS]).show(ui, |ui| {
                for (k, v) in rows {
                    ui.add(egui::Label::new(job(&t, k, theme::regular(text::CAPTION), t.muted)));
                    ui.add(egui::Label::new(job(&t, v, theme::regular(text::BODY), t.fg)).wrap());
                    ui.end_row();
                }
            });
        }
        Kind::Empty { title, body, icon: ic } => {
            ui.vertical_centered(|ui| {
                ui.add_space(space::XL);
                if !ic.is_empty() {
                    icons::show(ui, ic, icon::HERO, t.faint);
                    ui.add_space(space::SM);
                }
                ui.add(egui::Label::new(job(&t, title, semibold(text::EMPH), t.fg)));
                if !body.is_empty() {
                    ui.add(egui::Label::new(job(&t, body, theme::regular(text::BODY), t.muted)).wrap());
                }
                ui.add_space(space::XL);
            });
        }
        Kind::Progress { value, label } => {
            ui.horizontal(|ui| match value {
                Some(v) => {
                    ui.vertical(|ui| {
                        if !label.is_empty() {
                            ui.label(egui::RichText::new(label).font(theme::regular(text::CAPTION)).color(t.muted));
                        }
                        widgets::bar(ui, *v, t.blue, size::BAR);
                    });
                }
                None => {
                    let (rect, _) = ui.allocate_exact_size(vec2(icon::XL, icon::XL), Sense::hover());
                    icons::paint_status(ui, rect, Status::Working, icon::XL);
                    icons::request_spinner_repaint(ui);
                    if !label.is_empty() {
                        ui.label(egui::RichText::new(label).font(theme::regular(text::CAPTION)).color(t.muted));
                    }
                }
            });
        }
        Kind::Comment(c) => comment(ui, cx, &key, c),
        Kind::Button(b) => button(ui, cx, &key, b),
        Kind::TextInput { value, placeholder, multiline, max_len, submit_label, on_change, reset } => {
            text_input(ui, cx, &key, value, placeholder, *multiline, *max_len, submit_label.as_deref(), *on_change, *reset);
        }
        Kind::Checkbox { label, checked } => {
            let mut c = *checked;
            let resp = ui.checkbox(&mut c, egui::RichText::new(label).font(theme::regular(text::BODY)).color(t.fg));
            if resp.changed() {
                cx.event(&key, EventKind::Toggle, json!({"checked": c}));
            }
        }
        Kind::Select { options, selected } => {
            let cur = options.iter().find(|o| &o.value == selected).map_or("", |o| o.label.as_str()).to_owned();
            egui::ComboBox::from_id_salt((cx.info.instance.clone(), "select", key.clone())).selected_text(cur).show_ui(ui, |ui| {
                for o in options {
                    if ui.selectable_label(&o.value == selected, &o.label).clicked() && &o.value != selected {
                        cx.event(&key, EventKind::Select, json!({"value": o.value}));
                    }
                }
            });
        }
        Kind::Segmented { options, selected } => segmented(ui, cx, &key, options, selected),
        Kind::List { items, selected } => list(ui, cx, &key, items, selected.as_deref()),
        Kind::FileList { files, tree, selected } => file_list(ui, cx, &key, files, *tree, selected.as_deref()),
        Kind::Code(c) => super::view::code(ui, cx, &key, c),
        Kind::Diff(d) => super::view::diff(ui, cx, &key, d),
        Kind::Unsupported { type_name } => {
            egui::Frame::new().stroke(Stroke::new(size::HAIRLINE, t.surface1)).corner_radius(CornerRadius::same(radius::MD)).inner_margin(egui::Margin::symmetric(space::SM as i8, space::XS as i8)).show(ui, |ui| {
                ui.label(egui::RichText::new(format!("unsupported: {type_name}")).font(theme::mono(text::MONO_SM)).color(t.muted));
            });
        }
    }
}

/// A chip (`badge`, or a badge on a row).
pub(super) fn chip(ui: &mut Ui, t: &Tokens, b: &Badge) -> egui::Response {
    let font = theme::medium(text::CAPTION);
    let color = tone_color(t, b.tone);
    let w = widgets::text_width(ui, &b.text, font.clone()) + space::MD;
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::CHIP), Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(radius::PILL), if b.tone == Tone::Default { t.surface0 } else { Tokens::tint(color, alpha::PILL) });
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, &b.text, font, if b.tone == Tone::Default { t.fg2 } else { color });
    named(&resp, WidgetType::Label, format!("Badge: {}", b.text));
    resp
}

fn button(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, b: &csm_core::addons::screen::Button) {
    let kind = match b.kind {
        BK::Primary => ButtonKind::Primary,
        BK::Secondary => ButtonKind::Secondary,
        BK::Ghost => ButtonKind::Ghost,
        BK::Danger => ButtonKind::Danger,
    };
    let icon_name = Some(b.icon.as_str()).filter(|n| !n.is_empty() && icons::exists(n));
    let (usable, why): (bool, Option<String>) = match &b.bind {
        Bind::Inert => (false, Some("This button does nothing.".into())),
        Bind::Action(a) => match &a.policy {
            ActionPolicy::Refused(r) => (false, Some(r.clone())),
            ActionPolicy::Confirm => (true, Some("Asks you first.".into())),
            ActionPolicy::Direct => (true, None),
        },
        Bind::Event => (true, None),
    };
    let enabled = usable && !b.disabled;
    let resp = ui.add_enabled_ui(enabled, |ui| widgets::button(ui, kind, icon_name, &b.label, None)).inner;
    let resp = match (&why, enabled) {
        (Some(w), false) => resp.on_disabled_hover_text(w.as_str()),
        (Some(w), true) => resp.on_hover_text(w.as_str()),
        _ => resp,
    };
    if enabled && resp.clicked() {
        match &b.bind {
            Bind::Event => cx.event(key, EventKind::Click, Value::Null),
            Bind::Action(a) => cx.out.push(Out::Tool { tool: a.tool.clone(), params: a.params.clone(), confirm: a.policy == ActionPolicy::Confirm }),
            Bind::Inert => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn text_input(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, value: &str, placeholder: &str, multiline: bool, max_len: usize, submit_label: Option<&str>, on_change: bool, reset: bool) {
    let t = cx.t;
    let id = (cx.info.instance.clone(), key.to_owned());
    let rev = cx.rev;
    let now = ui.input(|i| i.time);
    // The text being typed lives here, so typing never waits for a round trip, and a re-render
    // does not overwrite it unless the plugin says `reset` (after a successful submit).
    let draft = cx.st.drafts.entry(id).or_insert_with(|| super::Draft { text: value.to_owned(), reset_rev: if reset { rev } else { 0 }, change_at: None });
    if reset && draft.reset_rev < rev {
        draft.text = value.to_owned();
        draft.reset_rev = rev;
        draft.change_at = None;
    }
    let before = draft.text.clone();
    let widget = if multiline {
        egui::TextEdit::multiline(&mut draft.text).desired_rows(3).desired_width(f32::INFINITY)
    } else {
        egui::TextEdit::singleline(&mut draft.text).desired_width(f32::INFINITY)
    };
    let resp = ui.add(widget.hint_text(egui::RichText::new(placeholder).color(t.faint)).font(theme::regular(text::BODY)).char_limit(max_len).id_salt(("addon-input", &cx.info.instance, key)));
    named(&resp, WidgetType::TextEdit, if placeholder.is_empty() { format!("Input: {key}") } else { placeholder.to_owned() });
    let changed = draft.text != before;
    if changed && on_change {
        draft.change_at = Some(now + 0.3);
    }
    let enter = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter) && (!multiline || i.modifiers.command));
    let mut submit = enter && !draft.text.is_empty();
    if let Some(label) = submit_label {
        ui.add_space(space::XS);
        if widgets::button(ui, ButtonKind::Primary, None, label, None).clicked() {
            submit = true;
        }
    }
    let text_now = draft.text.clone();
    let due = draft.change_at.filter(|at| *at <= now);
    if let Some(at) = draft.change_at
        && at > now
    {
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(at - now));
    }
    if due.is_some() {
        draft.change_at = None;
    }
    if submit {
        draft.change_at = None;
    }
    if due.is_some() {
        cx.event(key, EventKind::Change, json!({"value": text_now}));
    }
    if submit {
        cx.event(key, EventKind::Submit, json!({"value": text_now}));
    }
}

fn segmented(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, options: &[Opt], selected: &str) {
    let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
    let sel = options.iter().position(|o| o.value == selected);
    if let Some(i) = widgets::segmented(ui, key, &labels, sel)
        && let Some(o) = options.get(i)
        && o.value != selected
    {
        cx.event(key, EventKind::Select, json!({"value": o.value}));
    }
}

fn split(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, children: &[Node], ratio: f32, min_px: f32) {
    let t = cx.t;
    let full = Rect::from_min_size(ui.cursor().min, vec2(ui.available_width(), finite_h(ui)));
    let id = (cx.info.instance.clone(), key.to_owned());
    let min = min_px.min(full.width() / 2.0 - space::SM);
    let r = cx.st.splits.get(&id).copied().unwrap_or(ratio);
    let left_w = (full.width() * r).clamp(min.max(0.0), (full.width() - min).max(min.max(0.0)));
    let left = Rect::from_min_max(full.min, pos2(full.min.x + left_w - size::DIVIDER / 2.0, full.max.y));
    let right = Rect::from_min_max(pos2(full.min.x + left_w + size::DIVIDER / 2.0, full.min.y), full.max);
    let grip = Rect::from_min_max(pos2(left.max.x, full.min.y), pos2(right.min.x, full.max.y));
    let resp = ui.interact(grip, Id::new(("addon-split", &id.0, &id.1)), Sense::drag());
    if resp.dragged() && full.width() > 1.0 {
        let next = ((left_w + resp.drag_delta().x) / full.width()).clamp(0.15, 0.85);
        cx.st.splits.insert(id, next);
    }
    let line = if resp.hovered() || resp.dragged() { t.surface2 } else { t.surface0 };
    ui.painter().vline(grip.center().x, grip.y_range(), Stroke::new(size::HAIRLINE, line));
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    named(&resp, WidgetType::Button, "Resize split");
    for (c, rect) in children.iter().zip([left, right]) {
        let mut child = ui.new_child(UiBuilder::new().max_rect(rect).layout(Layout::top_down(egui::Align::Min)));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        node(&mut child, cx, c);
    }
    ui.advance_cursor_after_rect(full);
}

fn comment(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, c: &Comment) {
    let t = cx.t;
    widgets::card_frame(&t).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(job(&t, &c.author, semibold(text::BODY), t.fg)));
            if c.agent {
                // The mark is CSM's, drawn for `kind: agent` only, so a plugin cannot make an agent look like a person.
                chip(ui, &t, &Badge { text: "agent".into(), tone: Tone::Accent });
            }
            if c.time_ms > 0 {
                let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
                ui.label(egui::RichText::new(crate::screens::board::age(now, Some(c.time_ms))).font(theme::regular(text::CAPTION)).color(t.muted));
            }
            for b in &c.badges {
                chip(ui, &t, b);
            }
        });
        ui.add_space(space::XS);
        md::show(ui, cx, key, &c.body);
        if !c.actions.is_empty() {
            ui.add_space(space::SM);
            ui.horizontal_wrapped(|ui| {
                for a in &c.actions {
                    node(ui, cx, a);
                }
            });
        }
    });
}

fn row_frame(ui: &mut Ui, t: &Tokens, h: f32, on: bool) -> (Rect, egui::Response) {
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    if on {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::SM), t.row_sel);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::SM), t.row_hover);
    }
    (rect, resp)
}

/// Paints badges right-aligned in a row, returns the x the badges start at.
fn row_badges(ui: &Ui, t: &Tokens, rect: Rect, badges: &[Badge]) -> f32 {
    let mut x = rect.max.x - space::SM;
    for b in badges.iter().rev() {
        let font = theme::medium(text::TINY);
        let w = widgets::text_width(ui, &b.text, font.clone()) + space::MD;
        let r = Rect::from_min_size(pos2(x - w, rect.center().y - size::COUNT / 2.0), vec2(w, size::COUNT));
        let color = tone_color(t, b.tone);
        ui.painter().rect_filled(r, CornerRadius::same(radius::PILL), if b.tone == Tone::Default { t.surface0 } else { Tokens::tint(color, alpha::PILL) });
        ui.painter().text(r.center(), Align2::CENTER_CENTER, &b.text, font, if b.tone == Tone::Default { t.fg2 } else { color });
        x -= w + space::XS;
    }
    x
}

fn list(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, items: &[ListItem], selected: Option<&str>) {
    let t = cx.t;
    if items.is_empty() {
        ui.label(egui::RichText::new("Nothing here.").font(theme::regular(text::CAPTION)).color(t.muted));
        return;
    }
    let h = size::INBOX_ROW;
    let max_h = finite_h(ui);
    egui::ScrollArea::vertical().id_salt((cx.info.instance.clone(), "list", key)).max_height(max_h).auto_shrink([false, true]).show_rows(ui, h, items.len(), |ui, range| {
        for item in items.iter().skip(range.start).take(range.len()) {
            let on = selected == Some(item.key.as_str());
            let (rect, resp) = row_frame(ui, &t, h, on);
            let mut x = rect.min.x + space::SM;
            if !item.icon.is_empty() {
                icons::paint(ui, Rect::from_min_size(pos2(x, rect.center().y - icon::LG / 2.0), vec2(icon::LG, icon::LG)), &item.icon, icon::LG, t.muted);
                x += icon::LG + space::SM;
            }
            let right = row_badges(ui, &t, rect, &item.badges);
            let w = (right - x - space::SM).max(0.0);
            if item.subtitle.is_empty() {
                widgets::paint_text(ui, pos2(x, rect.center().y), Align2::LEFT_CENTER, &visible(&item.title), theme::medium(text::BODY), t.fg, w);
            } else {
                widgets::paint_text(ui, pos2(x, rect.center().y - space::SM), Align2::LEFT_CENTER, &visible(&item.title), theme::medium(text::BODY), t.fg, w);
                widgets::paint_text(ui, pos2(x, rect.center().y + space::SM), Align2::LEFT_CENTER, &visible(&item.subtitle), theme::regular(text::CAPTION), t.muted, w);
            }
            named(&resp, WidgetType::Button, format!("Item: {}", item.title));
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                cx.event(key, EventKind::Select, json!({"key": item.key}));
            }
        }
    });
}

/// A one-line string for painting: invisible characters shown as markers.
fn visible(s: &str) -> String {
    reveal(s).into_iter().map(|g| g.text).collect::<String>().replace('\n', " ")
}

fn status_color(t: &Tokens, c: char) -> Color32 {
    match c {
        'M' => t.yellow,
        'A' => t.green,
        'D' => t.red,
        'R' | 'C' => t.blue,
        'U' => t.peach,
        _ => t.muted,
    }
}

fn file_list(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, files: &[FileEntry], tree: bool, selected: Option<&str>) {
    let t = cx.t;
    if files.is_empty() {
        ui.label(egui::RichText::new("No files.").font(theme::regular(text::CAPTION)).color(t.muted));
        return;
    }
    let id = (cx.info.instance.clone(), key.to_owned());
    let folds = cx.st.folds.get(&id).cloned().unwrap_or_default();
    let rows: Arc<Vec<FileRow>> = cx.st.rows(&id, cx.rev, tree, files, &folds);
    let h = size::ROW;
    let max_h = finite_h(ui);
    let mut toggled: Option<String> = None;
    egui::ScrollArea::vertical().id_salt((cx.info.instance.clone(), "files", key)).max_height(max_h).auto_shrink([false, true]).show_rows(ui, h, rows.len(), |ui, range| {
        for row in rows.iter().skip(range.start).take(range.len()) {
            let on = !row.dir && selected == Some(row.path.as_str());
            let (rect, resp) = row_frame(ui, &t, h, on);
            let mut x = rect.min.x + space::SM + row.depth as f32 * space::LG;
            let entry = row.entry.and_then(|i| files.get(i));
            if row.dir {
                icons::paint(ui, Rect::from_min_size(pos2(x, rect.center().y - icon::MD / 2.0), vec2(icon::MD, icon::MD)), if row.folded { "chevR" } else { "chevD" }, icon::MD, t.muted);
                x += icon::MD + space::XS;
            } else {
                x += icon::MD + space::XS;
                if let Some(s) = entry.and_then(|e| e.status) {
                    ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, s.to_string(), theme::mono(text::MONO), status_color(&t, s));
                    x += space::LG;
                }
            }
            let badges: &[Badge] = entry.map_or(&[], |e| e.badges.as_slice());
            let right = row_badges(ui, &t, rect, badges);
            let name = visible(&row.name);
            widgets::paint_text(ui, pos2(x, rect.center().y), Align2::LEFT_CENTER, &name, if row.dir { semibold(text::BODY) } else { theme::regular(text::BODY) }, if row.dir { t.fg2 } else { t.fg }, (right - x - space::SM).max(0.0));
            named(&resp, WidgetType::Button, format!("{}: {}", if row.dir { "Folder" } else { "File" }, row.path));
            if resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                if row.dir {
                    toggled = Some(row.path.clone());
                } else {
                    cx.event(key, EventKind::Select, json!({"path": row.path}));
                }
            }
        }
    });
    if let Some(dir) = toggled {
        let set = cx.st.folds.entry(id).or_default();
        if !set.remove(&dir) {
            set.insert(dir);
        }
        cx.st.fold_gen = cx.st.fold_gen.wrapping_add(1);
    }
}

