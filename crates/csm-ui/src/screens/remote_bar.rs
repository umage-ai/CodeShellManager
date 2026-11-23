//! The strip under the title bar of a remote window: which host, at which address, and how the link
//! stands. It is always there in a remote window, so the window can never be taken for the laptop's
//! own. Its colours are theme tokens: blue when connected, muted while connecting, yellow while the
//! host is offline and the window retries, red when the host refused this laptop.

use egui::{Align2, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2};

use crate::remote_window::{RemoteLink, RemoteStatus};
use crate::theme::{self, Tokens, size, space, text};

pub fn show(ui: &mut Ui, status: &RemoteStatus) {
    let t = Tokens::get(ui.ctx());
    let rect = ui.max_rect();
    let accent = match status.link {
        RemoteLink::Connected => t.blue,
        RemoteLink::Connecting => t.muted,
        RemoteLink::Offline => t.yellow,
        RemoteLink::Refused(_) => t.red,
    };
    let text_color = if matches!(status.link, RemoteLink::Refused(_)) { t.red } else { t.fg };
    let p = ui.painter();
    p.rect_filled(rect, 0.0, Tokens::tint(accent, theme::alpha::REMOTE_BAR));
    p.hline(rect.x_range(), rect.max.y - size::HAIRLINE / 2.0, Stroke::new(size::HAIRLINE, accent));
    p.circle_filled(pos2(rect.min.x + space::MD, rect.center().y), space::XS, accent);
    let label = status.bar_text();
    p.text(pos2(rect.min.x + space::MD + space::SM + space::XXS, rect.center().y), Align2::LEFT_CENTER, &label, theme::medium(text::CAPTION), text_color);
    // The whole text is the accessible name, whatever the window's width.
    let resp = ui.interact(rect, ui.id().with("remote-bar"), Sense::hover());
    resp.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, label.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strip_text_is_the_status_text() {
        let s = RemoteStatus { host_name: "OFFICE".into(), address: "10.8.0.2:47300".into(), link: RemoteLink::Offline };
        assert!(s.bar_text().starts_with("OFFICE offline"));
    }
}
