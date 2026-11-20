//! Icons: the mock-up's hand-drawn Lucide-style set (24-unit grid, 1.8 stroke), embedded as SVG,
//! rasterised by egui_extras at the on-screen size and tinted per use. Status glyphs are drawn
//! with the painter instead (they mix a filled shape with an inner mark in the "on accent" ink).

use std::f32::consts::{FRAC_PI_2, PI, TAU};

use egui::epaint::{Mesh, Vertex, WHITE_UV};
use egui::{Color32, Pos2, Rect, Response, Sense, Shape, Stroke, Ui, Vec2, vec2};

use crate::icon_data::ICONS;
use crate::model::Status;
use crate::theme::{Tokens, timing};

fn svg(name: &str) -> Option<String> {
    let body = ICONS.iter().find(|(n, _)| *n == name).map(|(_, b)| *b)?;
    Some(format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="#ffffff" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round">{body}</svg>"##
    ))
}

/// Whether `name` is in the set.
pub fn exists(name: &str) -> bool {
    ICONS.iter().any(|(n, _)| *n == name)
}

/// An egui image for an icon, tinted. Unknown names draw nothing.
pub fn image(name: &str, size: f32, tint: Color32) -> egui::Image<'static> {
    let bytes = svg(name).unwrap_or_default().into_bytes();
    egui::Image::from_bytes(format!("bytes://csm-icon/{name}.svg"), bytes).fit_to_exact_size(Vec2::splat(size)).tint(tint)
}

/// Paints an icon centred in `rect`.
pub fn paint(ui: &Ui, rect: Rect, name: &str, size: f32, tint: Color32) {
    if name == "blank" || name.is_empty() || !exists(name) {
        return;
    }
    image(name, size, tint).paint_at(ui, Rect::from_center_size(rect.center(), Vec2::splat(size)));
}

/// An inline icon that takes layout space.
pub fn show(ui: &mut Ui, name: &str, size: f32, tint: Color32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint(ui, rect, name, size, tint);
    resp
}

// ---- status glyphs -------------------------------------------------------------------------

/// The colour of a status in the active theme.
pub fn status_color(t: &Tokens, s: Status) -> Color32 {
    match s {
        Status::Working => t.st_working,
        Status::Approval => t.st_approval,
        Status::Question => t.st_question,
        Status::Done => t.st_done,
        Status::Error => t.st_error,
        Status::Idle => t.st_idle,
        Status::Asleep => t.st_asleep,
        Status::Starting => t.st_starting,
    }
}

/// The turn of the working spinner at `time` (seconds), stepped at `timing::SPINNER_FPS`.
pub fn spinner_angle(time: f64) -> f32 {
    let fps = f64::from(timing::SPINNER_FPS);
    let step = (time * fps).floor() / fps;
    (step.rem_euclid(1.0) * std::f64::consts::TAU) as f32
}

fn arc(center: Pos2, r: f32, from: f32, sweep: f32, n: usize) -> Vec<Pos2> {
    (0..=n).map(|i| from + sweep * i as f32 / n as f32).map(|a| center + vec2(a.cos(), a.sin()) * r).collect()
}

/// A crescent: the part of the disc (c, big) outside the disc (b, small), as a triangle strip
/// along rays from `c` (which lies inside the bite).
fn crescent(c: Pos2, big: f32, b: Pos2, small: f32, color: Color32) -> Mesh {
    let mut mesh = Mesh::default();
    let w = c - b;
    let n = 48;
    for i in 0..=n {
        let a = i as f32 / n as f32 * TAU;
        let u = vec2(a.cos(), a.sin());
        let uw = u.dot(w);
        let disc = uw * uw - (w.length_sq() - small * small);
        let exit = if disc >= 0.0 { (-uw + disc.sqrt()).max(0.0) } else { 0.0 };
        let inner = c + u * exit.min(big);
        let outer = c + u * big;
        mesh.vertices.push(Vertex { pos: inner, uv: WHITE_UV, color });
        mesh.vertices.push(Vertex { pos: outer, uv: WHITE_UV, color });
        if i > 0 {
            let k = (2 * i) as u32;
            mesh.indices.extend_from_slice(&[k - 2, k - 1, k, k - 1, k + 1, k]);
        }
    }
    mesh
}

/// Draws the status glyph (the mock's 16-unit drawings) centred in `rect` at `size` points.
pub fn paint_status(ui: &Ui, rect: Rect, status: Status, size: f32) {
    let t = Tokens::get(ui.ctx());
    let color = status_color(&t, status);
    let ink = t.on_accent;
    let painter = ui.painter();
    let s = size / 16.0;
    let o = rect.center() - vec2(8.0 * s, 8.0 * s);
    let p = |x: f32, y: f32| -> Pos2 { o + vec2(x * s, y * s) };
    let c = p(8.0, 8.0);
    match status {
        Status::Working => {
            painter.circle_stroke(c, 5.6 * s, Stroke::new(2.0 * s, color.gamma_multiply(0.28)));
            let start = spinner_angle(ui.input(|i| i.time)) - FRAC_PI_2;
            painter.add(Shape::line(arc(c, 5.6 * s, start, FRAC_PI_2, 12), Stroke::new(2.0 * s, color)));
        }
        Status::Approval => {
            let shield = vec![p(8.0, 1.2), p(13.6, 3.3), p(13.6, 7.4), p(12.4, 11.2), p(8.0, 14.8), p(3.6, 11.2), p(2.4, 7.4), p(2.4, 3.3)];
            painter.add(Shape::convex_polygon(shield, color, Stroke::NONE));
            painter.line_segment([p(8.0, 4.7), p(8.0, 8.4)], Stroke::new(1.7 * s, ink));
            painter.circle_filled(p(8.0, 10.9), 1.0 * s, ink);
        }
        Status::Question => {
            painter.circle_filled(c, 6.8 * s, color);
            let mut hook = arc(p(8.05, 6.5), 1.95 * s, PI * 1.05, PI * 1.35, 10);
            hook.push(p(8.0, 9.4));
            painter.add(Shape::line(hook, Stroke::new(1.6 * s, ink)));
            painter.circle_filled(p(8.0, 11.7), 0.95 * s, ink);
        }
        Status::Done => {
            painter.circle_filled(c, 6.8 * s, color);
            painter.add(Shape::line(vec![p(5.0, 8.2), p(7.1, 10.3), p(11.0, 6.3)], Stroke::new(1.7 * s, ink)));
        }
        Status::Error => {
            painter.circle_filled(c, 6.8 * s, color);
            painter.line_segment([p(5.7, 5.7), p(10.3, 10.3)], Stroke::new(1.7 * s, ink));
            painter.line_segment([p(10.3, 5.7), p(5.7, 10.3)], Stroke::new(1.7 * s, ink));
        }
        Status::Idle => {
            painter.circle_stroke(c, 4.3 * s, Stroke::new(1.6 * s, color));
        }
        Status::Asleep => {
            painter.add(Shape::mesh(crescent(c, 5.9 * s, p(10.8, 4.9), 4.7 * s, color)));
        }
        Status::Starting => {
            let segments = 8;
            for i in 0..segments {
                let a0 = i as f32 / segments as f32 * TAU;
                painter.add(Shape::line(arc(c, 5.5 * s, a0, TAU / segments as f32 * 0.5, 4), Stroke::new(1.6 * s, color)));
            }
        }
    }
}

/// A status glyph that takes layout space, with its label as tooltip and for screen readers.
pub fn status(ui: &mut Ui, status: Status, size: f32) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    paint_status(ui, rect, status, size);
    if status == Status::Working {
        request_spinner_repaint(ui);
    }
    resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Label, true, status.label()));
    resp.on_hover_text(status.label())
}

/// Working glyphs animate at a low fixed rate, and only while drawn and on screen (never
/// `request_repaint` every frame: with many panes that pins a core). A row scrolled out of the
/// sidebar, or anything clipped away, asks for nothing.
pub fn request_spinner_repaint(ui: &Ui) {
    if !ui.is_rect_visible(ui.max_rect()) {
        return;
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f32(1.0 / timing::SPINNER_FPS));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_set_has_what_the_screens_use() {
        for n in [
            "more", "search", "plus", "x", "branch", "play", "pin", "inbox", "panelR", "history", "sliders", "focus", "side", "overview", "grid",
            "free", "rail", "layers", "list", "chevD", "chevR", "tear", "float", "dock", "moon", "restart", "monitor",
        ] {
            assert!(exists(n), "{n}");
            assert!(svg(n).is_some_and(|s| s.starts_with("<svg")), "{n}");
        }
    }

    #[test]
    fn spinner_steps_at_a_fixed_rate() {
        // Within one step the angle doesn't change, so a repaint inside it draws nothing new.
        assert_eq!(spinner_angle(1.0), spinner_angle(1.0 + 0.5 / f64::from(timing::SPINNER_FPS)));
        assert_ne!(spinner_angle(1.0), spinner_angle(1.0 + 1.0 / f64::from(timing::SPINNER_FPS)));
    }
}
