//! Standing warnings under the title bar: a failing save, a start that could not read the saved
//! sessions (and so saves nothing), an offer to import a beta's sessions. One strip per warning,
//! drawn for as long as the engine reports it (`Snapshot::warnings`), so it cannot be missed on a
//! full disk and goes by itself when the cause is gone. Colours are theme tokens: red for a
//! warning that data is not being saved, blue for an offer.

use egui::{Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo, WidgetType, pos2, vec2};

use crate::app::Action;
use crate::model::{WarningButton, WarningRow};
use crate::theme::{self, Tokens, size, space, text};
use crate::widgets::{self, ButtonKind};

/// Whether a warning is an offer (blue) rather than a problem (red).
fn is_offer(w: &WarningRow) -> bool {
    w.id == "beta_import"
}

/// Draws one strip in `rect` and pushes the action of a button that was clicked.
pub fn show(ui: &mut Ui, rect: Rect, w: &WarningRow, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let accent = if is_offer(w) { t.blue } else { t.red };
    let p = ui.painter();
    p.rect_filled(rect, 0.0, Tokens::tint(accent, theme::alpha::REMOTE_BAR));
    p.hline(rect.x_range(), rect.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, accent));
    p.circle_filled(pos2(rect.min.x + space::MD, rect.center().y), space::XS, accent);
    // Buttons from the right edge, the text in what is left.
    let mut right = rect.max.x - space::MD;
    let mut buttons: Vec<(&WarningButton, ButtonKind)> = Vec::new();
    if let Some(b) = &w.action {
        buttons.push((b, ButtonKind::Primary));
    }
    if let Some(b) = &w.dismiss {
        buttons.push((b, ButtonKind::Ghost));
    }
    for (b, kind) in buttons.into_iter().rev() {
        let bw = widgets::button_width(ui, None, &b.label, None);
        let r = Rect::from_min_size(pos2(right - bw, rect.center().y - size::CONTROL / 2.0), vec2(bw, size::CONTROL));
        right = r.min.x - space::SM;
        let resp = ui.scope_builder(UiBuilder::new().max_rect(r), |ui| widgets::button(ui, kind, None, &b.label, None)).inner;
        if resp.clicked() {
            actions.push(Action::Tool(b.tool, b.params.clone()));
        }
    }
    let left = rect.min.x + space::MD + space::SM + space::XXS;
    let wrap = (right - left).max(0.0);
    let galley = ui.painter().layout(w.text.clone(), theme::medium(text::CAPTION), t.fg, wrap);
    let top = rect.center().y - galley.size().y.min(rect.height()) / 2.0;
    ui.painter().with_clip_rect(Rect::from_min_max(pos2(left, rect.min.y), pos2(right.max(left), rect.max.y))).galley(pos2(left, top), galley, t.fg);
    // The whole text is the accessible name, whatever the window's width.
    let text_rect = Rect::from_min_max(pos2(left, rect.min.y), pos2(right.max(left), rect.max.y));
    let resp = ui.interact(text_rect, ui.id().with(("warn-bar", &w.id)), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, w.text.clone()));
    resp.on_hover_text(&w.text);
}
