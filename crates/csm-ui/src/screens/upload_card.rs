//! Uploads from a remote window (file drops and image paste): the progress card with Cancel, and
//! what a finished drop does next, which is pasting the office's paths into the session.
//!
//! The card sits bottom left while a drop runs. Its state comes from [`UploadEvent`]s, which the
//! backend hands over once a frame. [`apply`] turns one event into the card's rows and the actions it
//! implies, so the whole flow can be tested without a window.

use egui::{Align2, Id, Order, pos2};
use serde_json::json;

use crate::app::Action;
use crate::backend::UploadEvent;
use crate::model::SessionId;
use crate::theme::{self, Tokens, size, space, text};
use crate::widgets;

/// One drop on the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRow {
    pub drop: u64,
    /// The session the paths go to (its name is shown, not its id).
    pub session: SessionId,
    pub session_name: String,
    /// The file now being sent; empty until the host has checked the drop.
    pub file: String,
    pub sent: u64,
    pub total: u64,
    pub files_done: usize,
    pub files_total: usize,
}

impl UploadRow {
    /// A drop that has been asked for and not yet checked.
    pub fn started(drop: u64, session: SessionId, session_name: String) -> Self {
        Self { drop, session, session_name, file: String::new(), sent: 0, total: 0, files_done: 0, files_total: 0 }
    }
}

/// Adds a row for a drop the window just started (`Backend::upload_files` or the clipboard).
pub fn start(rows: &mut Vec<UploadRow>, drop: u64, session: SessionId, session_name: String) {
    rows.push(UploadRow::started(drop, session, session_name));
}

/// Applies one event to the card's rows. Returns what the event asks the window to do: a finished
/// drop pastes its paths, a failed or cancelled one says so.
pub fn apply(rows: &mut Vec<UploadRow>, ev: UploadEvent) -> Vec<Action> {
    match ev {
        UploadEvent::Progress { drop, sent, total, files_done, files_total, file } => {
            if let Some(r) = rows.iter_mut().find(|r| r.drop == drop) {
                r.sent = sent;
                r.total = total;
                r.files_done = files_done;
                r.files_total = files_total;
                r.file = file;
            }
            Vec::new()
        }
        UploadEvent::Done { drop, session, paths } => {
            rows.retain(|r| r.drop != drop);
            vec![Action::Tool("session_drop_paths", json!({"session_id": session, "paths": paths})), Action::FocusTerminal(session)]
        }
        UploadEvent::Failed { drop, message } => {
            rows.retain(|r| r.drop != drop);
            vec![Action::Toast(message)]
        }
        UploadEvent::Cancelled { drop } => {
            rows.retain(|r| r.drop != drop);
            vec![Action::Toast("Upload cancelled. Nothing was pasted.".into())]
        }
    }
}

/// The card's heading for a row: what is going where, and how far it has got.
pub fn heading(r: &UploadRow) -> String {
    if r.files_total == 0 {
        return format!("Checking files for {}…", r.session_name);
    }
    let which = if r.files_total > 1 { format!(" ({} of {})", (r.files_done + 1).min(r.files_total), r.files_total) } else { String::new() };
    format!("Uploading {}{which} to {}", r.file, r.session_name)
}

/// Draws the card while uploads run. Cancel is an action, so the drop's files are removed on the
/// host by the backend, not here.
pub fn show(ctx: &egui::Context, rows: &[UploadRow], actions: &mut Vec<Action>) {
    if rows.is_empty() {
        return;
    }
    let t = Tokens::get(ctx);
    let screen = ctx.content_rect();
    egui::Area::new(Id::new("upload-card"))
        .order(Order::Foreground)
        .pivot(Align2::LEFT_BOTTOM)
        .fixed_pos(pos2(screen.min.x + space::LG, screen.max.y - space::LG))
        .show(ctx, |ui| {
            widgets::pop_frame(&t).inner_margin(egui::Margin::same(space::MD as i8)).show(ui, |ui| {
                ui.set_max_width(size::TOAST_MAX_W);
                for r in rows {
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(heading(r)).font(theme::regular(text::BODY)).color(t.fg));
                        if ui.button("Cancel").clicked() {
                            actions.push(Action::UploadCancel(r.drop));
                        }
                    });
                    let frac = if r.total == 0 { 0.0 } else { (r.sent as f32 / r.total as f32).clamp(0.0, 1.0) };
                    ui.add(egui::ProgressBar::new(frac).desired_width(size::TOAST_MAX_W));
                }
            });
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(drop: u64) -> UploadRow {
        let mut r = UploadRow::started(drop, "s1".into(), "Claude".into());
        r.files_total = 2;
        r
    }

    #[test]
    fn progress_updates_the_row_and_asks_for_nothing() {
        let mut rows = vec![row(7)];
        let a = apply(&mut rows, UploadEvent::Progress { drop: 7, sent: 40, total: 100, files_done: 1, files_total: 2, file: "b.pdf".into() });
        assert!(a.is_empty());
        assert_eq!((rows[0].sent, rows[0].total, rows[0].file.as_str()), (40, 100, "b.pdf"));
        assert_eq!(heading(&rows[0]), "Uploading b.pdf (2 of 2) to Claude");
    }

    #[test]
    fn a_finished_drop_pastes_its_paths_into_its_session_and_leaves_the_card() {
        let mut rows = vec![row(7), row(8)];
        let paths = vec!["/srv/uploads/s1/1-a.txt".to_string(), "/srv/uploads/s1/2-b.txt".to_string()];
        let a = apply(&mut rows, UploadEvent::Done { drop: 7, session: "s1".into(), paths: paths.clone() });
        assert_eq!(rows.iter().map(|r| r.drop).collect::<Vec<_>>(), vec![8], "only the finished drop leaves");
        let [Action::Tool(id, params), Action::FocusTerminal(s)] = a.as_slice() else { panic!("{a:?}") };
        assert_eq!(*id, "session_drop_paths");
        assert_eq!(params["session_id"], "s1");
        assert_eq!(params["paths"], json!(paths));
        assert_eq!(s, "s1");
    }

    #[test]
    fn a_failed_or_cancelled_drop_says_so_and_pastes_nothing() {
        let mut rows = vec![row(7), row(8)];
        let a = apply(&mut rows, UploadEvent::Failed { drop: 7, message: "a folder was dropped".into() });
        assert_eq!(a, vec![Action::Toast("a folder was dropped".into())]);
        let a = apply(&mut rows, UploadEvent::Cancelled { drop: 8 });
        assert!(rows.is_empty());
        assert!(matches!(a.as_slice(), [Action::Toast(t)] if t.contains("Nothing was pasted")));
    }

    #[test]
    fn an_event_for_an_unknown_drop_changes_nothing_but_still_pastes_a_done_drop() {
        let mut rows = vec![row(7)];
        let a = apply(&mut rows, UploadEvent::Progress { drop: 99, sent: 1, total: 1, files_done: 0, files_total: 1, file: "x".into() });
        assert!(a.is_empty());
        assert_eq!(rows[0].sent, 0);
        let a = apply(&mut rows, UploadEvent::Done { drop: 99, session: "s1".into(), paths: vec!["/p".into()] });
        assert_eq!(a.len(), 2, "a paste never depends on the card having the row");
    }
}
