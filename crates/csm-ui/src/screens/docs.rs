//! The inspector's Docs tab: the project docs (`README.md`, `CLAUDE.md`, `AGENTS.md`, ...) of the
//! active session's folder and of its repository, read-only, as rendered Markdown.
//!
//! Everything comes from the docs tools (`project_docs_list`, `project_doc_get`) through
//! [`crate::model::UiModel::docs`]; this module decides when to ask again (a poll every
//! [`LIST_EVERY_S`], a re-read when the shown doc's size or time changes) and draws. Nothing here
//! touches the disk. The doc chosen per folder is kept in [`DocsUi`] for this run only.

use std::collections::HashMap;

use csm_core::addons::screen::markdown::Href;
use egui::Ui;
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::docs_model::{DocEntry, DocList, LOOKED_FOR, MAX_BYTES, pick, resolve_link};
use crate::model::{Kind, SessionView};
use crate::theme::{self, Tokens, space, text};
use crate::widgets::{self, ButtonKind};

/// How often the tab re-lists the docs (the live reload's period).
pub const LIST_EVERY_S: f64 = 2.0;
/// A doc whose read failed is asked again after this long.
const RETRY_S: f64 = 5.0;

/// The Docs tab's own state (not persisted).
#[derive(Debug, Clone, Default)]
pub struct DocsUi {
    /// The doc chosen per folder (its `rel`).
    pub choice: HashMap<String, String>,
    /// The session last listed, and when.
    pub listed: Option<(String, f64)>,
    /// The doc version last asked for (session, rel, size, modified), and when.
    pub asked: Option<((String, String, u64, u64), f64)>,
}

fn note(ui: &mut Ui, s: &str) {
    let t = Tokens::get(ui.ctx());
    ui.add(egui::Label::new(egui::RichText::new(s).font(theme::regular(text::SMALL)).color(t.muted)).wrap());
}

/// `1.2 KB`, `340 B`.
pub fn size_text(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MB", n as f64 / (1024.0 * 1024.0))
    }
}

/// The empty state's sentence.
pub fn empty_text() -> String {
    format!("No project docs here. Looked for {} in this folder and at the top of its git repository.", LOOKED_FOR.join(", "))
}

/// The picker: a segmented control when every name fits, a drop-down otherwise.
fn picker(ui: &mut Ui, list: &DocList, on: &DocEntry) -> Option<String> {
    let labels: Vec<&str> = list.entries.iter().map(DocEntry::label).collect();
    let selected = list.entries.iter().position(|e| e.rel == on.rel);
    let font = theme::medium(text::SMALL);
    let need: f32 = labels.iter().map(|l| widgets::text_width(ui, l, font.clone()) + space::LG + space::XS + space::XXS).sum::<f32>() + space::XXS;
    if need <= ui.available_width() {
        return widgets::segmented(ui, "Doc", &labels, selected).and_then(|i| list.entries.get(i)).map(|e| e.rel.clone());
    }
    let mut picked = None;
    egui::ComboBox::from_id_salt(("docs-pick", list.folder.as_str())).selected_text(on.label()).width(ui.available_width()).show_ui(ui, |ui| {
        for e in &list.entries {
            if ui.selectable_label(e.rel == on.rel, e.label()).clicked() {
                picked = Some(e.rel.clone());
            }
        }
    });
    picked
}

/// The inspector's Docs tab for session `s`.
pub fn tab(ui: &mut Ui, app: &mut CsmApp, s: &SessionView, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    if s.kind == Kind::Ssh {
        note(ui, "Project docs are read from a local or WSL folder: an SSH session's folder is on another machine.");
        return;
    }
    let model = app.model.clone();
    let docs = &model.docs;
    let now = ui.input(|i| i.time);
    let sid = json!(s.id);
    if docs.live {
        let due = match &app.ui.docs.listed {
            Some((id, at)) => id != &s.id || now - at >= LIST_EVERY_S,
            None => true,
        };
        if due {
            app.ui.docs.listed = Some((s.id.clone(), now));
            actions.push(Action::Tool("project_docs_list", json!({"session_id": sid})));
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(LIST_EVERY_S));
    }
    if let Some(why) = docs.unavailable.get(&s.id) {
        note(ui, why);
        return;
    }
    let Some(list) = docs.lists.get(&s.id) else {
        let why = if docs.live { "Looking for project docs…".to_owned() } else { empty_text() };
        note(ui, &why);
        return;
    };
    let folder_key = if list.folder.is_empty() { s.folder.clone() } else { list.folder.clone() };
    let Some(entry) = pick(list, app.ui.docs.choice.get(&folder_key).map(String::as_str)).cloned() else {
        note(ui, &empty_text());
        return;
    };

    if let Some(rel) = picker(ui, list, &entry) {
        app.ui.docs.choice.insert(folder_key.clone(), rel);
    }

    // Header: where it is, how big and how old, and what can be done with it.
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = space::SM;
        ui.add(egui::Label::new(egui::RichText::new(&entry.rel).font(theme::mono(text::MONO)).color(t.fg)).truncate()).on_hover_text(&entry.path);
        let age = csm_core::recent_sessions::ago(i64::try_from(entry.modified_ms).ok().filter(|m| *m > 0), i64::try_from(model.now_ms).unwrap_or(i64::MAX));
        let meta = if age.is_empty() { size_text(entry.size) } else { format!("{} · {age}", size_text(entry.size)) };
        ui.label(egui::RichText::new(meta).font(theme::regular(text::CAPTION)).color(t.muted));
    });
    let open = |actions: &mut Vec<Action>| actions.push(Action::Tool("open_with", json!({"session_id": sid})));
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Secondary, Some("braces"), "Open in editor", None).on_hover_text("Open this session's folder in your default editor (Open with)").clicked() {
            open(actions);
        }
        if widgets::button(ui, ButtonKind::Ghost, Some("copy"), "Copy path", None).on_hover_text(&entry.path).clicked() {
            actions.push(Action::CopyText(entry.path.clone()));
        }
    });

    if entry.size > MAX_BYTES {
        note(ui, &format!("{} is too large to show here ({}; the most is {}).", entry.rel, size_text(entry.size), size_text(MAX_BYTES)));
        return;
    }

    // The text: asked again whenever the listed size or time differs from what is shown.
    let key = (s.id.clone(), entry.rel.clone());
    let view = docs.texts.get(&key).cloned();
    let fresh = view.as_ref().is_some_and(|v| v.size == entry.size && v.modified_ms == entry.modified_ms);
    if docs.live && !fresh {
        let version = (s.id.clone(), entry.rel.clone(), entry.size, entry.modified_ms);
        let due = match &app.ui.docs.asked {
            Some((v, at)) => v != &version || (docs.text_errors.contains_key(&key) && now - at >= RETRY_S),
            None => true,
        };
        if due {
            app.ui.docs.asked = Some((version, now));
            actions.push(Action::Tool("project_doc_get", json!({"session_id": sid, "file": entry.rel})));
        }
    }
    let Some(view) = view else {
        match docs.text_errors.get(&key) {
            Some(e) => note(ui, e),
            None => note(ui, "Reading…"),
        }
        return;
    };
    if view.truncated {
        note(ui, &format!("{} is too large to show here (over {}).", entry.rel, size_text(MAX_BYTES)));
        return;
    }
    if view.lossy {
        note(ui, "This file is not valid UTF-8: the bytes that are not are shown as \u{FFFD}.");
    }
    let mut clicked = None;
    egui::ScrollArea::vertical().id_salt(("docs-body", s.id.as_str(), entry.rel.as_str())).auto_shrink([false, false]).show(ui, |ui| {
        if view.blocks.is_empty() {
            note(ui, "This doc is empty.");
            return;
        }
        let links = |h: &Href| match h {
            Href::Http(url) => Some(url.clone()),
            Href::Relative(target) => resolve_link(list, &entry.rel, target).map(|e| format!("Show {}", e.rel)),
            Href::Csm(_) | Href::Inert => None,
        };
        clicked = super::markdown::show(ui, t, &view.blocks, &links);
    });
    match clicked {
        Some(Href::Http(url)) => actions.push(Action::Tool("open_url", json!({"url": url}))),
        Some(Href::Relative(target)) => {
            if let Some(e) = resolve_link(list, &entry.rel, &target) {
                app.ui.docs.choice.insert(folder_key, e.rel.clone());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_well() {
        assert_eq!(size_text(340), "340 B");
        assert_eq!(size_text(2048), "2.0 KB");
        assert_eq!(size_text(MAX_BYTES * 3), "3.0 MB");
        assert!(empty_text().contains("README.md, CLAUDE.md, AGENTS.md, CLAUDE.local.md, GEMINI.md, .claude/CLAUDE.md"));
    }
}
