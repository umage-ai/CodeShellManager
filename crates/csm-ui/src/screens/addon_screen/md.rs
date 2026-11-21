//! An add-on's Markdown (`csm_core::addons::screen::markdown`), drawn by the shared renderer
//! ([`crate::screens::markdown`]). Links: an `http(s)` one shows its real target in the tooltip
//! and goes through `open_url` after a confirm; a `csm:` one stays in the screen as a `link`
//! event; anything else is plain text.

use csm_core::addons::screen::EventKind;
use csm_core::addons::screen::markdown::{Block, Href};
use egui::Ui;
use serde_json::json;

use super::{Cx, Out};

/// The tooltip of a live link in an add-on screen.
fn live(href: &Href) -> Option<String> {
    match href {
        Href::Http(url) => Some(url.clone()),
        Href::Csm(h) => Some(format!("In this screen: {h}")),
        Href::Relative(_) | Href::Inert => None,
    }
}

/// Draws parsed Markdown blocks as a column.
pub(super) fn show(ui: &mut Ui, cx: &mut Cx<'_>, key: &str, blocks: &[Block]) {
    match crate::screens::markdown::show(ui, cx.t, blocks, &live) {
        // The add-on chose the text and the address separately: CSM asks first, showing the address.
        Some(Href::Http(url)) => cx.out.push(Out::Tool { tool: "open_url".into(), params: json!({"url": url}), confirm: true }),
        Some(Href::Csm(h)) => cx.event(key, EventKind::Link, json!({"href": h})),
        Some(Href::Relative(_) | Href::Inert) | None => {}
    }
}
