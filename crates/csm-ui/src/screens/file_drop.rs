//! Files dropped on a pane insert their paths, quoted, at the prompt (WPF
//! `TerminalBridge.BuildDroppedPathsPayload`).
//!
//! The paths are untrusted text. The office's `session_drop_paths` tool quotes them for the
//! session's shell (`csm_core::dropped_paths` rejects any with a control or invisible format
//! character: a newline in a "filename" would be Enter) and pastes them: a bracketed paste, never
//! typed, never followed by Enter. The same tool serves a local window and a remote one.
//!
//! In a remote window the paths are on this laptop, which the office cannot read. So the files are
//! uploaded first (`Action::UploadFiles`, see `upload_card`), and the office's paths are pasted when
//! every file is there. While files are dragged over the window the pane that will receive them
//! shows a drop overlay.
//!
//! Which pane: the one under the pointer. During an OS drag the window may not get pointer
//! moves (winit reports hovered files without a position), so when the last known pointer
//! position is in no pane the drop goes to the active pane, which is the one the overlay shows.

use egui::{CornerRadius, Id, Order, Rect, Stroke, StrokeKind, Ui, vec2};
use serde_json::json;

use crate::app::Action;
use crate::model::{SessionId, UiModel};
use crate::theme::{self, Tokens, alpha, radius, size, space, text};

/// The pane a drop goes to: the first of `panes` (top-most first) under the pointer, else the
/// active session's pane when it is one of them.
pub fn target(pointer: Option<egui::Pos2>, panes: &[(SessionId, Rect)], active: Option<&str>) -> Option<(SessionId, Rect)> {
    pointer.and_then(|p| panes.iter().find(|(_, r)| r.contains(p))).or_else(|| active.and_then(|a| panes.iter().find(|(id, _)| id == a))).cloned()
}

/// What a drop of `paths` onto `session` does in a window whose engine is on this machine: the
/// engine quotes the paths for the session's shell and pastes them (`session_drop_paths`). It
/// refuses, with a reason, what cannot be typed safely.
pub fn drop_actions(session: &str, paths: &[String]) -> Vec<Action> {
    if paths.is_empty() {
        return Vec::new();
    }
    vec![Action::Tool("session_drop_paths", json!({"session_id": session, "paths": paths})), Action::FocusTerminal(session.to_string())]
}

/// What a drop onto a remote window does: the files go up to the office first. The paste follows
/// from the upload card once every file is there.
pub fn remote_drop_actions(session: &str, paths: &[String]) -> Vec<Action> {
    if paths.is_empty() {
        return Vec::new();
    }
    vec![Action::UploadFiles { session: session.to_string(), paths: paths.to_vec() }]
}

/// Draws the drop overlay over the target pane while files hover, and turns a drop into
/// [`drop_actions`]. `panes`: the live panes on screen, top-most first.
pub fn show(ui: &Ui, model: &UiModel, panes: &[(SessionId, Rect)], actions: &mut Vec<Action>) {
    let ctx = ui.ctx();
    let (hovered, dropped, pointer) = ctx.input(|i| {
        let dropped: Vec<Option<String>> = i.raw.dropped_files.iter().map(|f| f.path().to_str().map(str::to_owned)).collect();
        (i.raw.hovered_files.len(), dropped, i.pointer.latest_pos())
    });
    if hovered == 0 && dropped.is_empty() {
        return;
    }
    let Some((id, rect)) = target(pointer, panes, model.active.as_deref()) else { return };
    if !dropped.is_empty() {
        // A path that isn't valid Unicode can't be shown or quoted faithfully: left out.
        let paths: Vec<String> = dropped.into_iter().flatten().collect();
        // A remote window's paths are on this laptop: they are uploaded, not pasted as they are.
        if model.remote {
            actions.extend(remote_drop_actions(&id, &paths));
        } else {
            actions.extend(drop_actions(&id, &paths));
        }
        return;
    }
    let t = Tokens::get(ctx);
    let name = model.session(&id).map(|s| s.name.clone()).unwrap_or_default();
    let what = if hovered == 1 { "Drop to insert the path".to_string() } else { format!("Drop to insert the {hovered} paths") };
    egui::Area::new(Id::new("file-drop-overlay")).order(Order::Foreground).interactable(false).fixed_pos(rect.min).show(ctx, |ui| {
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(radius::md()), Tokens::tint(t.blue, alpha::DROP));
        p.rect_stroke(rect.shrink(size::RING / 2.0), CornerRadius::same(radius::md()), Stroke::new(size::RING, t.blue), StrokeKind::Inside);
        let card = Rect::from_center_size(rect.center(), vec2((rect.width() - space::XL).clamp(0.0, size::DIALOG_W), size::CONTROL_LG * 2.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(card), |ui| {
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(&what).font(theme::semibold(text::EMPH)).color(t.fg));
                ui.label(egui::RichText::new(format!("into {name} · quoted, not run")).font(theme::regular(text::CAPTION)).color(t.fg2));
            });
        });
    });
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn panes() -> Vec<(SessionId, Rect)> {
        vec![
            ("float".into(), Rect::from_min_size(pos2(50.0, 50.0), vec2(100.0, 100.0))),
            ("a".into(), Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 200.0))),
            ("b".into(), Rect::from_min_size(pos2(200.0, 0.0), vec2(200.0, 200.0))),
        ]
    }

    #[test]
    fn the_pane_under_the_pointer_wins_top_most_first() {
        assert_eq!(target(Some(pos2(60.0, 60.0)), &panes(), Some("b")).map(|t| t.0), Some("float".into()));
        assert_eq!(target(Some(pos2(250.0, 10.0)), &panes(), Some("a")).map(|t| t.0), Some("b".into()));
    }

    #[test]
    fn without_a_pointer_over_a_pane_the_active_one_takes_it() {
        assert_eq!(target(None, &panes(), Some("b")).map(|t| t.0), Some("b".into()));
        assert_eq!(target(Some(pos2(900.0, 900.0)), &panes(), Some("a")).map(|t| t.0), Some("a".into()));
        assert_eq!(target(None, &panes(), Some("asleep")), None);
    }

    #[test]
    fn a_local_drop_pastes_the_paths_through_the_office_tool_and_never_presses_enter() {
        let p = if cfg!(windows) { r"C:\Program Files\a.txt" } else { "/tmp/my file.txt" };
        let a = drop_actions("s1", &[p.to_string(), "evil\nrm -rf ~".to_string()]);
        // The engine quotes for the session's shell and drops what cannot be typed (its own tests).
        assert_eq!(a[0], Action::Tool("session_drop_paths", json!({"session_id": "s1", "paths": [p, "evil\nrm -rf ~"]})));
        assert_eq!(a[1], Action::FocusTerminal("s1".into()));
        assert!(drop_actions("s1", &[]).is_empty(), "nothing dropped, nothing pasted");
    }

    #[test]
    fn a_remote_drop_uploads_the_files_and_pastes_nothing_itself() {
        let paths = vec![r"C:\Users\me\report.pdf".to_string(), "/home/me/a b.txt".to_string()];
        assert_eq!(remote_drop_actions("s1", &paths), vec![Action::UploadFiles { session: "s1".into(), paths: paths.clone() }]);
        assert!(remote_drop_actions("s1", &[]).is_empty());
    }
}
