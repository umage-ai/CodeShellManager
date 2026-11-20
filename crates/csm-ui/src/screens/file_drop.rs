//! Files dropped on a pane insert their paths, quoted, at the prompt (WPF
//! `TerminalBridge.BuildDroppedPathsPayload`).
//!
//! The paths are untrusted text: `csm_core::dropped_paths` rejects any with a control or
//! invisible format character (a newline in a "filename" would be Enter) and quotes the rest for
//! the platform's shells, and the result goes in through `session_send_text`: a bracketed paste,
//! never typed, never followed by Enter. While files are dragged over the window the pane that
//! will receive them shows a drop overlay.
//!
//! Which pane: the one under the pointer. During an OS drag the window may not get pointer
//! moves (winit reports hovered files without a position), so when the last known pointer
//! position is in no pane the drop goes to the active pane, which is the one the overlay shows.

use egui::{CornerRadius, Id, Order, Rect, Stroke, StrokeKind, Ui, vec2};
use serde_json::json;

use crate::app::Action;
use crate::model::{SessionId, UiModel};
use crate::theme::{self, Tokens, alpha, radius, size, space, text};
use csm_core::dropped_paths::{Quoting, build_dropped_paths_payload};

/// The pane a drop goes to: the first of `panes` (top-most first) under the pointer, else the
/// active session's pane when it is one of them.
pub fn target(pointer: Option<egui::Pos2>, panes: &[(SessionId, Rect)], active: Option<&str>) -> Option<(SessionId, Rect)> {
    pointer.and_then(|p| panes.iter().find(|(_, r)| r.contains(p))).or_else(|| active.and_then(|a| panes.iter().find(|(id, _)| id == a))).cloned()
}

/// What a drop of `paths` onto `session` does: paste the quoted paths, or say why nothing was
/// inserted.
pub fn drop_actions(session: &str, paths: &[String]) -> Vec<Action> {
    let payload = build_dropped_paths_payload(paths, Quoting::native());
    if payload.is_empty() {
        return vec![Action::Toast("Nothing was inserted: those paths have characters that can't be typed safely at a prompt.".into())];
    }
    vec![
        // The person dropped it on purpose: like typing, it goes in even while the session works.
        Action::Tool("session_send_text", json!({"session_id": session, "text": payload, "force": true})),
        Action::FocusTerminal(session.to_string()),
    ]
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
        actions.extend(drop_actions(&id, &paths));
        return;
    }
    let t = Tokens::get(ctx);
    let name = model.session(&id).map(|s| s.name.clone()).unwrap_or_default();
    let what = if hovered == 1 { "Drop to insert the path".to_string() } else { format!("Drop to insert the {hovered} paths") };
    egui::Area::new(Id::new("file-drop-overlay")).order(Order::Foreground).interactable(false).fixed_pos(rect.min).show(ctx, |ui| {
        let p = ui.painter();
        p.rect_filled(rect, CornerRadius::same(radius::MD), Tokens::tint(t.blue, alpha::DROP));
        p.rect_stroke(rect.shrink(size::RING / 2.0), CornerRadius::same(radius::MD), Stroke::new(size::RING, t.blue), StrokeKind::Inside);
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
    fn a_drop_pastes_quoted_paths_and_never_presses_enter() {
        let p = if cfg!(windows) { r"C:\Program Files\a.txt" } else { "/tmp/my file.txt" };
        let a = drop_actions("s1", &[p.to_string(), "evil\nrm -rf ~".to_string()]);
        let Some(Action::Tool(id, params)) = a.first() else { panic!("{a:?}") };
        assert_eq!(*id, "session_send_text");
        let want = if cfg!(windows) { "\"C:\\Program Files\\a.txt\"" } else { "'/tmp/my file.txt'" };
        assert_eq!(params["text"], want, "the hostile path is left out, the good one quoted");
        assert!(params.get("submit").is_none(), "no Enter");
        assert!(matches!(drop_actions("s1", &["a\u{202E}b".to_string()]).first(), Some(Action::Toast(_))));
    }
}
