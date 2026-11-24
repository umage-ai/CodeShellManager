//! Draws the Markdown subset of `csm_core::addons::screen::markdown` (an add-on screen's text,
//! the inspector's Docs tab). The blocks are already parsed (once, not per frame), so this only
//! walks them.
//!
//! Links: the caller decides which targets are live and what their tooltip says ([`Links`]);
//! anything else is drawn as plain text. A clicked live link is returned, and the caller acts on
//! it (an `http(s)` one through `open_url`). Text that names one URL while linking to another
//! shows both. Invisible characters are drawn as visible markers.

use csm_core::addons::screen::markdown::{Block, Href, Inline, text_misleads};
use csm_core::addons::screen::reveal;
use egui::text::{LayoutJob, TextFormat};
use egui::{Color32, CornerRadius, FontId, Rect, Sense, Stroke, Ui, vec2};

use crate::theme::{self, Tokens, alpha, radius, semibold, size, space, text};

/// Which links are live: the tooltip of a live target, `None` for one drawn as plain text.
pub type Links<'a> = &'a dyn Fn(&Href) -> Option<String>;

/// What one draw needs.
struct Cx<'a> {
    t: Tokens,
    links: Links<'a>,
    clicked: Option<Href>,
}

/// Style of the run being drawn.
#[derive(Clone, Copy)]
struct Style {
    strong: bool,
    emph: bool,
    size: f32,
    color: Color32,
}

fn job_for(t: &Tokens, s: &str, st: Style, mono: bool) -> LayoutJob {
    let font: FontId = if mono {
        theme::mono(st.size - 0.5)
    } else if st.strong {
        semibold(st.size)
    } else {
        theme::regular(st.size)
    };
    let mut job = LayoutJob::default();
    for seg in reveal(s) {
        let fmt = TextFormat {
            font_id: font.clone(),
            color: if seg.marker { t.yellow } else { st.color },
            background: if seg.marker {
                Tokens::tint(t.yellow, alpha::PILL)
            } else if mono {
                t.field
            } else {
                Color32::TRANSPARENT
            },
            italics: st.emph,
            ..TextFormat::default()
        };
        job.append(&seg.text, 0.0, fmt);
    }
    job
}

fn plain_text(kids: &[Inline]) -> String {
    let mut s = String::new();
    for k in kids {
        match k {
            Inline::Text(t) | Inline::Code(t) => s.push_str(t),
            Inline::Emph(v) | Inline::Strong(v) => s.push_str(&plain_text(v)),
            Inline::Link { kids, .. } => s.push_str(&plain_text(kids)),
            Inline::Break => s.push(' '),
        }
    }
    s
}

fn inlines(ui: &mut Ui, cx: &mut Cx<'_>, kids: &[Inline], st: Style) {
    for k in kids {
        match k {
            Inline::Text(s) => {
                ui.add(egui::Label::new(job_for(&cx.t, s, st, false)).wrap());
            }
            Inline::Code(s) => {
                ui.add(egui::Label::new(job_for(&cx.t, s, st, true)).wrap());
            }
            Inline::Emph(v) => inlines(ui, cx, v, Style { emph: true, ..st }),
            Inline::Strong(v) => inlines(ui, cx, v, Style { strong: true, ..st }),
            Inline::Break => ui.end_row(),
            Inline::Link { href, kids } => link(ui, cx, href, kids, st),
        }
    }
}

fn link(ui: &mut Ui, cx: &mut Cx<'_>, href: &Href, kids: &[Inline], st: Style) {
    let label = plain_text(kids);
    let tip = if matches!(href, Href::Inert) { None } else { (cx.links)(href) };
    let Some(tip) = tip else {
        ui.add(egui::Label::new(job_for(&cx.t, &label, st, false)).wrap());
        return;
    };
    let shown = match href {
        Href::Http(url) if text_misleads(&label, href) => format!("{label} ({url})"),
        _ => label.clone(),
    };
    let job = job_for(&cx.t, &shown, Style { color: cx.t.blue, ..st }, false);
    let resp = ui.add(egui::Label::new(job).sense(Sense::click()));
    let resp = resp.on_hover_text(reveal(&tip).iter().map(|s| s.text.as_str()).collect::<String>()).on_hover_cursor(egui::CursorIcon::PointingHand);
    ui.painter().hline(resp.rect.x_range(), resp.rect.max.y - size::HAIRLINE, Stroke::new(size::HAIRLINE, cx.t.blue));
    let name = format!("Link: {label}");
    resp.widget_info({
        let name = name.clone();
        move || egui::WidgetInfo::labeled(egui::WidgetType::Link, true, name.clone())
    });
    if resp.clicked() {
        cx.clicked = Some(href.clone());
    }
}

fn paragraph(ui: &mut Ui, cx: &mut Cx<'_>, kids: &[Inline], st: Style) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
        inlines(ui, cx, kids, st);
    });
}

fn block(ui: &mut Ui, cx: &mut Cx<'_>, b: &Block) {
    let t = cx.t;
    let body = Style { strong: false, emph: false, size: text::BODY, color: t.fg };
    match b {
        Block::Paragraph(kids) => paragraph(ui, cx, kids, body),
        Block::Heading(level, kids) => {
            let size = match level {
                1 => text::TITLE + 2.0,
                2 => text::TITLE,
                3 => text::EMPH,
                _ => text::BODY,
            };
            paragraph(ui, cx, kids, Style { strong: true, size, ..body });
        }
        Block::Code { text: code, .. } => {
            egui::Frame::new().fill(t.crust).corner_radius(CornerRadius::same(radius::md())).inner_margin(egui::Margin::same(space::SM as i8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.add(egui::Label::new(job_for(&t, code.trim_end_matches('\n'), Style { color: t.fg2, size: text::MONO + 0.5, ..body }, false).tap_mono()).wrap());
            });
        }
        Block::List { start, items } => {
            for (i, item) in items.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    let marker = match start {
                        Some(n) => format!("{}.", n.saturating_add(i as u64)),
                        None => "\u{2022}".to_owned(),
                    };
                    ui.add_sized(vec2(space::XL, size::BADGE), egui::Label::new(egui::RichText::new(marker).font(theme::regular(text::BODY)).color(t.muted)));
                    ui.vertical(|ui| {
                        for sub in item {
                            block(ui, cx, sub);
                        }
                    });
                });
            }
        }
        Block::Quote(blocks) => {
            let r = ui.horizontal_top(|ui| {
                ui.add_space(space::SM + space::XS);
                ui.vertical(|ui| {
                    for sub in blocks {
                        block(ui, cx, sub);
                    }
                });
            });
            let bar = Rect::from_min_size(r.response.rect.min + vec2(0.0, 0.0), vec2(size::STRIPE, r.response.rect.height()));
            ui.painter().rect_filled(bar, CornerRadius::same(radius::sm()), t.surface1);
        }
        Block::Rule => {
            let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), space::SM), Sense::hover());
            ui.painter().hline(rect.x_range(), rect.center().y, Stroke::new(size::HAIRLINE, t.surface1));
        }
    }
}

trait TapMono {
    fn tap_mono(self) -> LayoutJob;
}

impl TapMono for LayoutJob {
    /// Code is drawn in the mono face.
    fn tap_mono(mut self) -> LayoutJob {
        for s in &mut self.sections {
            s.format.font_id = theme::mono(text::MONO);
        }
        self
    }
}

/// Draws parsed Markdown blocks as a column. Returns the live link clicked this frame, if any.
pub fn show(ui: &mut Ui, t: Tokens, blocks: &[Block], links: Links<'_>) -> Option<Href> {
    let mut cx = Cx { t, links, clicked: None };
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = space::SM;
        for b in blocks {
            block(ui, &mut cx, b);
        }
    });
    cx.clicked
}
