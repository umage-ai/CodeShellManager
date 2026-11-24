//! The components every screen draws with (the mock-up's `.ibtn`, `.btn`, `.kbd`, `.pill`,
//! `.chip`, `.count`, `.mi` …). Each names itself for screen readers and tests.

use egui::{
    Align2, Color32, CornerRadius, CursorIcon, FontId, Rect, Response, Sense, Stroke, StrokeKind, Ui, Vec2, WidgetInfo, WidgetType, pos2, vec2,
};

use crate::icons;
use crate::model::Status;
use crate::theme::{Tokens, icon, radius, semibold, size, space, text};

fn info(resp: &Response, ty: WidgetType, label: &str) {
    let label = label.to_string();
    resp.widget_info(move || WidgetInfo::labeled(ty, true, label.clone()));
}

/// The keyboard focus ring around `rect`: `size::RING` wide, inside the rect, in the focus
/// colour. The caller draws it only where keyboard focus is, never for mouse focus.
pub fn focus_ring(ui: &Ui, rect: Rect, radius: u8) {
    let t = Tokens::get(ui.ctx());
    ui.painter().rect_stroke(rect, CornerRadius::same(radius), Stroke::new(size::RING, t.focus()), StrokeKind::Inside);
}

/// Text width with a font, for laying out painted rows.
pub fn text_width(ui: &Ui, s: &str, font: FontId) -> f32 {
    ui.ctx().fonts_mut(|f| f.layout_no_wrap(s.to_string(), font, Color32::PLACEHOLDER).size().x)
}

/// Paints `s` elided to `max_w` and returns its rect.
pub fn paint_text(ui: &Ui, pos: egui::Pos2, align: Align2, s: &str, font: FontId, color: Color32, max_w: f32) -> Rect {
    let mut job = egui::text::LayoutJob::simple_singleline(s.to_string(), font, color);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(1.0));
    let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
    let rect = align.anchor_size(pos, galley.size());
    ui.painter().galley(rect.min, galley, color);
    rect
}

/// Which way a 3D edge faces (`borders = "bevel"`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// Buttons and panels: light top-left, dark bottom-right.
    Raised,
    /// Fields, lists and the terminal's frame: the other way round.
    Sunken,
}

/// Paints the classic two-pixel 3D edge just inside `rect`, in the bevel greys the theme's face
/// colour gives (`Tokens::bevel_*`). Call it only when `Tokens::style.bevel` is on.
pub fn paint_bevel(p: &egui::Painter, rect: Rect, edge: Edge, t: &Tokens) {
    let (outer_tl, outer_br, inner_tl, inner_br) = match edge {
        Edge::Raised => (t.bevel_hi(), t.bevel_dk(), t.bevel_lt(), t.bevel_sh()),
        Edge::Sunken => (t.bevel_sh(), t.bevel_hi(), t.bevel_dk(), t.bevel_lt()),
    };
    let w = size::HAIRLINE;
    let ring = |r: Rect, tl: Color32, br: Color32| {
        p.rect_filled(Rect::from_min_max(r.min, pos2(r.max.x, r.min.y + w)), 0.0, tl);
        p.rect_filled(Rect::from_min_max(r.min, pos2(r.min.x + w, r.max.y)), 0.0, tl);
        p.rect_filled(Rect::from_min_max(pos2(r.min.x, r.max.y - w), r.max), 0.0, br);
        p.rect_filled(Rect::from_min_max(pos2(r.max.x - w, r.min.y), r.max), 0.0, br);
    };
    ring(rect, outer_tl, outer_br);
    ring(rect.shrink(w), inner_tl, inner_br);
}

/// A frame that is also a 3D panel when the theme asks for bevelled borders: [`card_frame`] and
/// [`pop_frame`] return it. It offers the same builder calls as `egui::Frame`.
pub struct Panel {
    frame: egui::Frame,
    bevel: Option<(Tokens, Edge)>,
}

impl Panel {
    pub fn inner_margin(mut self, m: impl Into<egui::Margin>) -> Self {
        self.frame = self.frame.inner_margin(m);
        self
    }

    pub fn fill(mut self, c: Color32) -> Self {
        self.frame = self.frame.fill(c);
        self
    }

    pub fn stroke(mut self, st: impl Into<Stroke>) -> Self {
        self.frame = self.frame.stroke(st);
        self
    }

    pub fn show<R>(self, ui: &mut Ui, add_contents: impl FnOnce(&mut Ui) -> R) -> egui::InnerResponse<R> {
        let r = self.frame.show(ui, add_contents);
        if let Some((t, edge)) = self.bevel {
            paint_bevel(ui.painter(), r.response.rect, edge, &t);
        }
        r
    }
}

/// A square icon button (`.ibtn`): transparent until hovered; `on` gets the accent treatment.
pub fn icon_button(ui: &mut Ui, name: &str, side: f32, on: bool, tooltip: &str) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    paint_icon_button(ui, rect, &resp, name, on, None);
    info(&resp, WidgetType::Button, tooltip);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tooltip)
}

/// What the Remote Control mark says (its tooltip and its name for screen readers and tests).
pub const REMOTE_CONTROL_TIP: &str = "Remote Control is on \u{2014} continue from claude.ai or the Claude app";

/// The quiet "Claude Code Remote Control is on" mark (a phone glyph) centred in `rect`, sized
/// `side`. With the session's link it is a button (the caller opens the link through
/// `open_url`); without one it only explains itself.
pub fn remote_control_mark(ui: &mut Ui, rect: Rect, id: egui::Id, side: f32, url: Option<&str>) -> Response {
    let t = Tokens::get(ui.ctx());
    let sense = if url.is_some() { Sense::click() } else { Sense::hover() };
    let resp = ui.interact(rect, id, sense);
    let tint = if url.is_some() && resp.hovered() { t.fg } else { t.teal };
    icons::paint(ui, rect, "remote", side, tint);
    let ty = if url.is_some() { WidgetType::Button } else { WidgetType::Label };
    info(&resp, ty, REMOTE_CONTROL_TIP);
    match url {
        Some(u) => resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(format!("{REMOTE_CONTROL_TIP}\nClick to open {u}")),
        None => resp.on_hover_text(REMOTE_CONTROL_TIP),
    }
}

/// An icon button at a fixed place (inside a painted header).
pub fn icon_button_at(ui: &mut Ui, rect: Rect, id: egui::Id, name: &str, on: bool, tooltip: &str) -> Response {
    icon_button_at_in(ui, rect, id, name, on, tooltip, None)
}

/// [`icon_button_at`] with the icon in `fg` while the button is idle (the title bar's own text colour).
pub fn icon_button_at_in(ui: &mut Ui, rect: Rect, id: egui::Id, name: &str, on: bool, tooltip: &str, fg: Option<Color32>) -> Response {
    let resp = ui.interact(rect, id, Sense::click());
    paint_icon_button(ui, rect, &resp, name, on, fg);
    info(&resp, WidgetType::Button, tooltip);
    resp.on_hover_cursor(CursorIcon::PointingHand).on_hover_text(tooltip)
}

fn paint_icon_button(ui: &Ui, rect: Rect, resp: &Response, name: &str, on: bool, idle_fg: Option<Color32>) {
    let t = Tokens::get(ui.ctx());
    let small = rect.width() < size::CONTROL;
    let r = CornerRadius::same(if small { radius::sm() } else { radius::md() });
    if t.style.bevel {
        // Flat until pointed at, then raised; pressed or on: sunken.
        let sunk = on || resp.is_pointer_button_down_on();
        if sunk || resp.hovered() {
            ui.painter().rect_filled(rect, 0.0, t.bg);
            paint_bevel(ui.painter(), rect, if sunk { Edge::Sunken } else { Edge::Raised }, &t);
        }
        let nudge = if sunk { vec2(size::HAIRLINE, size::HAIRLINE) } else { Vec2::ZERO };
        let sz = if small { icon::MD } else { icon::XL };
        let fg = if sunk || resp.hovered() { t.fg } else { idle_fg.unwrap_or(t.muted) };
        icons::paint(ui, rect.translate(nudge), name, sz, fg);
        if resp.has_focus() {
            focus_ring(ui, rect, 0);
        }
        return;
    }
    let (bg, fg) = if on {
        (Tokens::tint(t.blue, 0.14), t.blue)
    } else if resp.hovered() {
        (t.row_sel, t.fg)
    } else {
        (Color32::TRANSPARENT, idle_fg.unwrap_or(t.muted))
    };
    ui.painter().rect_filled(rect, r, bg);
    let sz = if small { icon::MD } else { icon::XL };
    icons::paint(ui, rect, name, sz, fg);
    if resp.has_focus() {
        focus_ring(ui, rect, if small { radius::sm() } else { radius::md() });
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    Primary,
    Secondary,
    Ghost,
    Danger,
}

/// A text button (`.btn`), with an optional leading icon and trailing key cap.
/// The width [`button`] takes for this icon, label and shortcut (to reserve room before drawing it).
pub fn button_width(ui: &Ui, icon_name: Option<&str>, label: &str, kbd_text: Option<&str>) -> f32 {
    let tw = text_width(ui, label, crate::theme::medium(text::BODY));
    let iw = if icon_name.is_some() { icon::LG + space::XS + space::XXS } else { 0.0 };
    let kw = kbd_text.map_or(0.0, |k| kbd_width(ui, k) + space::SM);
    space::ROW_X * 2.0 + iw + tw + kw
}

pub fn button(ui: &mut Ui, kind: ButtonKind, icon_name: Option<&str>, label: &str, kbd_text: Option<&str>) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = crate::theme::medium(text::BODY);
    let w = button_width(ui, icon_name, label, kbd_text);
    let iw = if icon_name.is_some() { icon::LG + space::XS + space::XXS } else { 0.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::CONTROL), Sense::click());
    let hovered = resp.hovered();
    let (bg, fg, stroke) = match kind {
        ButtonKind::Primary => (t.blue, t.on_accent, t.blue),
        ButtonKind::Secondary => (t.field, t.fg, if hovered { t.surface2 } else { t.surface0 }),
        ButtonKind::Ghost => (if hovered { t.row_sel } else { Color32::TRANSPARENT }, t.fg2, Color32::TRANSPARENT),
        ButtonKind::Danger => (t.field, t.red, if hovered { t.surface2 } else { t.surface0 }),
    };
    let r = CornerRadius::same(radius::md());
    let pressed = t.style.bevel && resp.is_pointer_button_down_on();
    let nudge = if pressed { vec2(size::HAIRLINE, size::HAIRLINE) } else { Vec2::ZERO };
    let (bg, fg) = if t.style.bevel {
        let face = if kind == ButtonKind::Ghost && !hovered && !pressed { Color32::TRANSPARENT } else { t.bg };
        let ink = if kind == ButtonKind::Danger { t.red } else { t.fg };
        (face, ink)
    } else {
        (bg, fg)
    };
    if t.style.bevel {
        // The default button of a 3D dialog has a dark outline around its bevel.
        let inner = if kind == ButtonKind::Primary {
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(size::HAIRLINE, t.bevel_dk()), StrokeKind::Inside);
            rect.shrink(size::HAIRLINE)
        } else {
            rect
        };
        ui.painter().rect_filled(inner, 0.0, bg);
        if bg != Color32::TRANSPARENT {
            paint_bevel(ui.painter(), inner, if pressed { Edge::Sunken } else { Edge::Raised }, &t);
        }
    } else {
        ui.painter().rect(rect, r, bg, Stroke::new(size::HAIRLINE, stroke), StrokeKind::Inside);
    }
    let mut x = rect.min.x + space::ROW_X + nudge.x;
    if let Some(n) = icon_name {
        let ir = Rect::from_min_size(pos2(x, rect.center().y - icon::LG / 2.0 + nudge.y), Vec2::splat(icon::LG));
        icons::paint(ui, ir, n, icon::LG, if kind == ButtonKind::Ghost { t.fg2 } else { fg });
        x += iw;
    }
    ui.painter().text(pos2(x, rect.center().y + nudge.y), Align2::LEFT_CENTER, label, font, fg);
    if let Some(k) = kbd_text {
        let kw = kbd_width(ui, k);
        paint_kbd(ui, Rect::from_center_size(pos2(rect.max.x - space::ROW_X - kw / 2.0, rect.center().y), vec2(kw, size::KBD)), k);
    }
    if resp.has_focus() {
        // Outside the button, so it shows on a primary (blue) button too.
        focus_ring(ui, rect.expand(space::XS), radius::md());
    }
    info(&resp, WidgetType::Button, label);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Width of a key cap.
pub fn kbd_width(ui: &Ui, s: &str) -> f32 {
    text_width(ui, s, crate::theme::mono(text::TINY)) + space::SM + space::XXS
}

/// Paints a key cap (`.kbd`) in `rect`.
pub fn paint_kbd(ui: &Ui, rect: Rect, s: &str) {
    let t = Tokens::get(ui.ctx());
    let p = ui.painter();
    p.rect(rect, CornerRadius::same(radius::sm()), t.field, Stroke::new(size::HAIRLINE, t.surface1), StrokeKind::Inside);
    p.hline(rect.x_range().shrink(space::XXS), rect.max.y - size::HAIRLINE, Stroke::new(size::HAIRLINE, t.surface1));
    p.text(rect.center(), Align2::CENTER_CENTER, s, crate::theme::mono(text::TINY), t.muted);
}

/// A key cap that takes layout space.
pub fn kbd(ui: &mut Ui, s: &str) -> Response {
    let w = kbd_width(ui, s);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::KBD), Sense::hover());
    paint_kbd(ui, rect, s);
    resp
}

/// A count badge (`.count`): filled in the approval colour, or quiet when `quiet`.
pub fn paint_count(ui: &Ui, center: egui::Pos2, n: usize, quiet: bool, h: f32) -> Rect {
    let t = Tokens::get(ui.ctx());
    let s = n.to_string();
    let font = semibold(text::TINY);
    let w = (text_width(ui, &s, font.clone()) + space::SM + space::XXS).max(h);
    let rect = Rect::from_center_size(center, vec2(w, h));
    let (bg, fg) = if quiet { (t.surface0, t.fg2) } else { (t.st_approval, t.on_accent) };
    ui.painter().rect_filled(rect, CornerRadius::same(radius::pill()), bg);
    ui.painter().text(rect.center(), Align2::CENTER_CENTER, s, font, fg);
    rect
}

/// The status pill (`.pill`): glyph and word on a tint of the status colour.
pub fn status_pill(ui: &mut Ui, status: Status, label: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = crate::theme::medium(text::CAPTION);
    let quiet = matches!(status, Status::Idle | Status::Asleep);
    let tw = text_width(ui, label, font.clone());
    let w = icon::SM + space::XS + tw + space::SM + if quiet { space::XXS } else { space::XS + space::XXS };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::CHIP), Sense::hover());
    let color = icons::status_color(&t, status);
    if !quiet {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::pill()), Tokens::tint(color, 0.15));
    }
    let gx = rect.min.x + if quiet { space::XXS } else { space::XS + space::XXS };
    let g = Rect::from_min_size(pos2(gx, rect.center().y - icon::SM / 2.0), Vec2::splat(icon::SM));
    icons::paint_status(ui, g, status, icon::SM);
    if status == Status::Working {
        icons::request_spinner_repaint(ui);
    }
    let fg = if quiet { t.muted } else { t.fg };
    ui.painter().text(pos2(g.max.x + space::XS + space::XXS, rect.center().y), Align2::LEFT_CENTER, label, font, fg);
    info(&resp, WidgetType::Label, label);
    resp
}

/// The branch chip (`.chip`): branch, a peach dot when dirty, ↑ahead.
pub fn branch_chip(ui: &mut Ui, branch: &str, dirty: bool, ahead: u32, max_w: f32, clickable: bool) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = crate::theme::mono(text::MONO_SM);
    let ahead_s = (ahead > 0).then(|| format!("↑{ahead}"));
    let extra =
        icon::XS + space::XS + if dirty { space::SM } else { 0.0 } + ahead_s.as_ref().map_or(0.0, |a| text_width(ui, a, font.clone()) + space::XS);
    let tw = text_width(ui, branch, font.clone());
    let w = (space::SM + space::XXS + extra + tw).min(max_w.max(extra + space::XXL));
    let sense = if clickable { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::CHIP), sense);
    let stroke = if clickable && resp.hovered() { t.surface2 } else { t.surface0 };
    ui.painter().rect(rect, CornerRadius::same(radius::sm()), t.crust, Stroke::new(size::HAIRLINE, stroke), StrokeKind::Inside);
    let mut x = rect.min.x + space::XS + space::XXS / 2.0;
    icons::paint(ui, Rect::from_min_size(pos2(x, rect.center().y - icon::XS / 2.0), Vec2::splat(icon::XS)), "branch", icon::XS, t.fg2);
    x += icon::XS + space::XS;
    let tail = if dirty { space::SM } else { 0.0 } + ahead_s.as_ref().map_or(0.0, |a| text_width(ui, a, font.clone()) + space::XS);
    let name_w = (rect.max.x - space::XS - tail - x).max(space::SM);
    let r = paint_text(ui, pos2(x, rect.center().y), Align2::LEFT_CENTER, branch, font.clone(), t.fg2, name_w);
    x = r.max.x + space::XS;
    if dirty {
        ui.painter().circle_filled(pos2(x + space::XXS, rect.center().y), space::XS - size::HAIRLINE, t.peach);
        x += space::SM;
    }
    if let Some(a) = ahead_s {
        ui.painter().text(pos2(x, rect.center().y), Align2::LEFT_CENTER, a, font, t.blue);
    }
    info(&resp, if clickable { WidgetType::Button } else { WidgetType::Label }, &format!("Git: {branch}"));
    resp
}

/// An uppercase section caption (`.sec-h`, `.cap2`).
pub fn caps(ui: &mut Ui, s: &str) -> Response {
    let t = Tokens::get(ui.ctx());
    ui.label(egui::RichText::new(s.to_uppercase()).font(semibold(text::CAPS)).color(t.muted).extra_letter_spacing(size::HAIRLINE / 2.0))
}

/// One row of a menu (`.mi`): icon, label, and a right-aligned hint or tag.
pub fn menu_item(ui: &mut Ui, icon_name: &str, label: &str, hint: Option<&str>, danger: bool) -> Response {
    menu_item_at(ui, icon_name, label, hint, danger, false, None)
}

/// A menu row that cannot run now: drawn muted, with no highlight and no hover. The keys skip it.
pub fn menu_item_off(ui: &mut Ui, icon_name: &str, label: &str, hint: Option<&str>) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = crate::theme::regular(text::BODY);
    let w = ui.available_width().max(size::MENU_W - space::SM);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::MENU_ROW), Sense::hover());
    let ir = Rect::from_min_size(pos2(rect.min.x + space::SM, rect.center().y - icon::LG / 2.0), Vec2::splat(icon::LG));
    icons::paint(ui, ir, icon_name, icon::LG, t.muted);
    ui.painter().text(pos2(ir.max.x + space::ROW_X, rect.center().y), Align2::LEFT_CENTER, label, font, t.muted);
    if let Some(h) = hint {
        ui.painter().text(pos2(rect.max.x - space::ROW_X, rect.center().y), Align2::RIGHT_CENTER, h, crate::theme::mono(text::TINY), t.muted);
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, false, label));
    resp
}

/// A menu row. `selected`: the keyboard highlight (the row fill and the focus ring). `underline`:
/// the char index of the access letter in `label`, drawn underlined.
pub fn menu_item_at(
    ui: &mut Ui,
    icon_name: &str,
    label: &str,
    hint: Option<&str>,
    danger: bool,
    selected: bool,
    underline: Option<usize>,
) -> Response {
    let t = Tokens::get(ui.ctx());
    let font = crate::theme::regular(text::BODY);
    let hint_font = crate::theme::mono(text::TINY);
    let hw = hint.map_or(0.0, |h| text_width(ui, h, hint_font.clone()) + space::LG);
    let need = space::SM + icon::LG + space::ROW_X + text_width(ui, label, font.clone()) + hw + space::ROW_X;
    let w = ui.available_width().max(size::MENU_W - space::SM).max(need);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::MENU_ROW), Sense::click());
    // A 3D menu lights its hovered (or keyboard-selected) row in the accent, with the text inverted.
    let lit = t.style.bevel && (resp.hovered() || selected);
    if lit {
        ui.painter().rect_filled(rect, 0.0, t.blue);
    } else if resp.hovered() || selected {
        ui.painter().rect_filled(rect, CornerRadius::same(radius::sm() + 1), t.row_sel);
    }
    if selected {
        if lit {
            // The accent fill hides a blue ring: ring the lit row in the text colour.
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(size::RING, t.on_accent), StrokeKind::Inside);
        } else {
            focus_ring(ui, rect, radius::sm() + 1);
        }
    }
    let fg = if lit { t.on_accent } else if danger { t.red } else { t.fg };
    let ir = Rect::from_min_size(pos2(rect.min.x + space::SM, rect.center().y - icon::LG / 2.0), Vec2::splat(icon::LG));
    icons::paint(ui, ir, icon_name, icon::LG, if lit { t.on_accent } else if danger { t.red } else { t.muted });
    let at = pos2(ir.max.x + space::ROW_X, rect.center().y);
    match underline {
        Some(ci) => paint_underlined(ui, at, label, ci, font, fg),
        None => {
            ui.painter().text(at, Align2::LEFT_CENTER, label, font, fg);
        }
    }
    if let Some(h) = hint {
        ui.painter().text(pos2(rect.max.x - space::ROW_X, rect.center().y), Align2::RIGHT_CENTER, h, hint_font, if lit { t.on_accent } else { t.muted });
    }
    info(&resp, WidgetType::Button, label);
    resp
}

/// `label` left-aligned at `pos`, its character at char index `ci` underlined (a menu's access letter).
fn paint_underlined(ui: &Ui, pos: egui::Pos2, label: &str, ci: usize, font: FontId, color: Color32) {
    let start = label.char_indices().nth(ci).map_or(label.len(), |(b, _)| b);
    let end = label.get(start..).and_then(|s| s.chars().next()).map_or(start, |c| start + c.len_utf8());
    let plain = egui::TextFormat { font_id: font, color, ..Default::default() };
    let under = egui::TextFormat { underline: Stroke::new(size::HAIRLINE, color), ..plain.clone() };
    let mut job = egui::text::LayoutJob::default();
    job.append(label.get(..start).unwrap_or(""), 0.0, plain.clone());
    job.append(label.get(start..end).unwrap_or(""), 0.0, under);
    job.append(label.get(end..).unwrap_or(""), 0.0, plain);
    let galley = ui.painter().layout_job(job);
    let top = pos2(pos.x, pos.y - galley.size().y / 2.0);
    ui.painter().galley(top, galley, color);
}

/// A menu's uppercase heading (`.mhead`).
pub fn menu_head(ui: &mut Ui, s: &str) {
    ui.add_space(space::XXS);
    ui.horizontal(|ui| {
        ui.add_space(space::ROW_X - space::SM);
        caps(ui, s);
    });
}

/// A thin separator line (`.msep`).
pub fn separator(ui: &mut Ui) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), space::SM + size::HAIRLINE), Sense::hover());
    ui.painter().hline(rect.x_range().shrink(space::XS + space::XXS), rect.center().y, Stroke::new(size::HAIRLINE, t.surface0));
}

/// A card surface (`.card`).
pub fn card_frame(t: &Tokens) -> Panel {
    let line = if t.style.bevel { t.bevel_sh() } else { t.surface0 };
    Panel {
        frame: egui::Frame::new()
            .fill(t.bg)
            .stroke(Stroke::new(size::HAIRLINE, line))
            .corner_radius(CornerRadius::same(radius::lg()))
            .inner_margin(egui::Margin::same(space::MD as i8)),
        bevel: None,
    }
}

/// A popup surface (`.menu`): pop fill, hairline, shadow.
pub fn pop_frame(t: &Tokens) -> Panel {
    if t.style.bevel {
        // A raised 3D panel: no soft shadow, the bevel is the edge.
        return Panel {
            frame: egui::Frame::new().fill(t.pop).corner_radius(0.0).inner_margin(egui::Margin::same(space::XS as i8)),
            bevel: Some((*t, Edge::Raised)),
        };
    }
    Panel {
        frame: egui::Frame::new()
            .fill(t.pop)
            .stroke(Stroke::new(size::HAIRLINE, t.surface1))
            .corner_radius(CornerRadius::same(radius::lg()))
            .shadow(egui::Shadow { offset: [0, 10], blur: 28, spread: 0, color: t.shadow })
            .inner_margin(egui::Margin::same(space::XS as i8)),
        bevel: None,
    }
}

/// The surface of the command palette and the search card: a big popup with no inner margin.
pub fn overlay_frame(t: &Tokens) -> Panel {
    if t.style.bevel {
        return Panel { frame: egui::Frame::new().fill(t.pop).corner_radius(0.0).inner_margin(egui::Margin::same(space::XXS as i8)), bevel: Some((*t, Edge::Raised)) };
    }
    Panel {
        frame: egui::Frame::new()
            .fill(t.pop)
            .stroke(Stroke::new(size::HAIRLINE, t.surface1))
            .corner_radius(CornerRadius::same(radius::xl()))
            .shadow(egui::Shadow { offset: [0, 16], blur: 40, spread: 0, color: t.shadow }),
        bevel: None,
    }
}

/// A thin determinate bar (`.bar`): `fraction` 0–1 in `color`.
pub fn bar(ui: &mut Ui, fraction: f32, color: Color32, height: f32) {
    let t = Tokens::get(ui.ctx());
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), height), Sense::hover());
    let r = CornerRadius::same(radius::pill());
    ui.painter().rect_filled(rect, r, t.surface0);
    let mut fill = rect;
    fill.set_width(rect.width() * fraction.clamp(0.0, 1.0));
    ui.painter().rect_filled(fill, r, color);
}

/// An on/off switch with its label and an optional explanation under it. Returns the response
/// of the whole row (clicked = flip it).
pub fn toggle(ui: &mut Ui, on: bool, label: &str, hint: Option<&str>) -> Response {
    let t = Tokens::get(ui.ctx());
    let w = ui.available_width();
    // The explanation wraps under the label, inside the clickable row.
    let hint_galley = hint.map(|h| {
        let job = egui::text::LayoutJob::simple(h.to_string(), crate::theme::regular(text::CAPTION), t.muted, w - size::SWITCH[0] - space::LG);
        ui.ctx().fonts_mut(|f| f.layout_job(job))
    });
    let hint_h = hint_galley.as_ref().map_or(0.0, |g| g.size().y + space::XS);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, size::CONTROL + hint_h), Sense::click());
    if resp.hovered() {
        ui.painter().rect_filled(rect.expand2(vec2(space::XS, 0.0)), CornerRadius::same(radius::md()), t.row_hover);
    }
    let cy = rect.min.y + size::CONTROL / 2.0;
    let track = Rect::from_min_size(pos2(rect.max.x - size::SWITCH[0], cy - size::SWITCH[1] / 2.0), vec2(size::SWITCH[0], size::SWITCH[1]));
    let (bg, knob) = if on { (t.blue, t.on_accent) } else { (t.surface1, t.fg2) };
    let r = size::SWITCH[1] / 2.0 - space::XXS;
    let kx = if on { track.max.x - r - space::XXS } else { track.min.x + r + space::XXS };
    if t.style.bevel {
        // A sunken slot with a raised square knob.
        ui.painter().rect_filled(track, 0.0, if on { t.blue } else { t.field });
        paint_bevel(ui.painter(), track, Edge::Sunken, &t);
        let k = Rect::from_center_size(pos2(kx, cy), Vec2::splat(r * 2.0));
        ui.painter().rect_filled(k, 0.0, t.bg);
        paint_bevel(ui.painter(), k, Edge::Raised, &t);
    } else {
        ui.painter().rect_filled(track, CornerRadius::same(radius::pill()), bg);
        ui.painter().circle_filled(pos2(kx, cy), r, knob);
    }
    if resp.has_focus() {
        ui.painter().rect_stroke(track.expand(size::RING), CornerRadius::same(radius::pill()), Stroke::new(size::RING, t.focus()), StrokeKind::Outside);
    }
    paint_text(ui, pos2(rect.min.x, cy), Align2::LEFT_CENTER, label, crate::theme::regular(text::BODY), t.fg, track.min.x - rect.min.x - space::MD);
    if let Some(g) = hint_galley {
        ui.painter().galley(pos2(rect.min.x, cy + size::CONTROL / 2.0 - space::XXS), g, t.muted);
    }
    let label_s = label.to_string();
    resp.widget_info(move || WidgetInfo::selected(WidgetType::Checkbox, true, on, label_s.clone()));
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A segmented control (`.seg`): one of `options` is on. Returns the index clicked, if any.
pub fn segmented(ui: &mut Ui, id: &str, options: &[&str], selected: Option<usize>) -> Option<usize> {
    let t = Tokens::get(ui.ctx());
    let font = crate::theme::medium(text::SMALL);
    let widths: Vec<f32> = options.iter().map(|o| text_width(ui, o, font.clone()) + space::LG + space::XS).collect();
    let total: f32 = widths.iter().sum::<f32>() + space::XXS * (widths.len() as f32 + 1.0);
    let (seg, _) = ui.allocate_exact_size(vec2(total, size::CONTROL), Sense::hover());
    ui.painter().rect(seg, CornerRadius::same(radius::md() + 1), t.crust, Stroke::new(size::HAIRLINE, t.surface0), StrokeKind::Inside);
    if t.style.bevel {
        paint_bevel(ui.painter(), seg, Edge::Sunken, &t);
    }
    let mut x = seg.min.x + space::XXS;
    let mut picked = None;
    for (i, (o, w)) in options.iter().zip(widths).enumerate() {
        let b = Rect::from_min_size(pos2(x, seg.min.y + space::XXS), vec2(w, size::CONTROL - space::XS));
        x += w + space::XXS;
        let r = ui.interact(b, ui.id().with((id, i)), Sense::click());
        let on = selected == Some(i);
        if on && t.style.bevel {
            ui.painter().rect_filled(b, 0.0, t.bg);
            paint_bevel(ui.painter(), b, Edge::Raised, &t);
        } else if on {
            ui.painter().rect_filled(b, CornerRadius::same(radius::sm() + 1), t.field);
        } else if r.hovered() {
            ui.painter().rect_filled(b, CornerRadius::same(radius::sm() + 1), t.row_hover);
        }
        ui.painter().text(b.center(), Align2::CENTER_CENTER, *o, font.clone(), if on || r.hovered() { t.fg } else { t.muted });
        let label = format!("{id}: {o}");
        r.widget_info(move || WidgetInfo::selected(WidgetType::RadioButton, true, on, label.clone()));
        if r.on_hover_cursor(CursorIcon::PointingHand).clicked() {
            picked = Some(i);
        }
    }
    picked
}
