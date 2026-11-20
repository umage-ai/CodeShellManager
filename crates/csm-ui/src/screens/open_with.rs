//! The "Open with" editor: a window over the app listing the editors (label, program, argument
//! templates per session kind, default), with "Detect installed editors". Saving is
//! `open_with_set` (the engine checks every entry and says what is wrong); detection is
//! `open_with_detect`. The unsaved rows live in egui memory until Save or Cancel.

use egui::{Id, Vec2};
use serde_json::json;

use crate::app::{Action, CsmApp};
use crate::model::OpenWithEntryView;
use crate::theme::{self, Tokens, size, space, text};
use crate::widgets::{self, ButtonKind};

/// The editor's state while open.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct EditorState {
    pub rows: Vec<OpenWithEntryView>,
    /// The list the rows were copied from: while nothing was edited, a changed list (detection
    /// finished) replaces the rows.
    pub base: Vec<OpenWithEntryView>,
    pub dirty: bool,
}

fn key() -> Id {
    Id::new("open-with-editor")
}

/// Opens the editor on a copy of the list (`Action::OpenWithEditor`).
pub fn open(ctx: &egui::Context, list: &[OpenWithEntryView]) {
    let st = EditorState { rows: list.to_vec(), base: list.to_vec(), dirty: false };
    ctx.data_mut(|d| d.insert_temp(key(), st));
}

pub fn is_open(ctx: &egui::Context) -> bool {
    ctx.data(|d| d.get_temp::<EditorState>(key()).is_some())
}

/// The `open_with_set` params for the rows.
pub fn set_params(rows: &[OpenWithEntryView]) -> serde_json::Value {
    let opt = |s: &Option<String>| s.as_ref().map(|x| x.trim().to_string()).filter(|x| !x.is_empty());
    let entries: Vec<serde_json::Value> = rows
        .iter()
        .map(|r| {
            let mut v = json!({"label": r.label.trim(), "executable": r.executable.trim(), "is_default": r.is_default});
            if !r.id.is_empty() {
                v["id"] = json!(r.id);
            }
            for (k, a) in [("local_args", &r.local_args), ("wsl_args", &r.wsl_args), ("ssh_args", &r.ssh_args)] {
                if let Some(x) = opt(a) {
                    v[k] = json!(x);
                }
            }
            v
        })
        .collect();
    json!({"entries": entries})
}

fn text_opt(ui: &mut egui::Ui, v: &mut Option<String>, hint: &str, id: Id) -> bool {
    let mut s = v.clone().unwrap_or_default();
    let r = ui.add(egui::TextEdit::singleline(&mut s).hint_text(hint).font(theme::mono(text::MONO)).desired_width(f32::INFINITY).id(id));
    if r.changed() {
        *v = (!s.is_empty()).then_some(s);
    }
    r.changed()
}

pub fn show(ctx: &egui::Context, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let Some(mut st) = ctx.data(|d| d.get_temp::<EditorState>(key())) else { return };
    let t = Tokens::get(ctx);
    if !st.dirty && st.base != app.model.open_with {
        st.rows = app.model.open_with.clone();
        st.base = app.model.open_with.clone();
    }
    let mut open = true;
    let mut close = false;
    egui::Window::new("Open with")
        .id(Id::new("open-with-window"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_width(size::DIALOG_W + size::CONTROL_LG * 4.0)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new("Editors the session's folder can be opened in. Placeholders: local {folder}; WSL {folder} {distro} {linuxPath}; SSH {user} {host} {userAtHost} {port} {remoteFolder}; add :uri to percent-encode. A blank template hides the entry for that kind.")
                    .font(theme::regular(text::CAPTION))
                    .color(t.muted),
            );
            let mut remove = None;
            let mut make_default = None;
            let mut swap = None;
            let n = st.rows.len();
            egui::ScrollArea::vertical().max_height(size::PALETTE_LIST_H).show(ui, |ui| {
                for (i, r) in st.rows.iter_mut().enumerate() {
                    widgets::card_frame(&t).inner_margin(egui::Margin::same(space::SM as i8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let mut changed = false;
                        ui.horizontal(|ui| {
                            if ui.radio(r.is_default, "").on_hover_text("The default: the { } button opens it").clicked() {
                                make_default = Some(i);
                            }
                            changed |= ui.add(egui::TextEdit::singleline(&mut r.label).hint_text("Label").desired_width(size::MENU_W).id(Id::new(("ow-label", i)))).changed();
                            if ui.add_enabled(i > 0, egui::Button::new("↑").small()).on_hover_text("Move up").clicked() {
                                swap = Some((i, i.saturating_sub(1)));
                            }
                            if ui.add_enabled(i + 1 < n, egui::Button::new("↓").small()).on_hover_text("Move down").clicked() {
                                swap = Some((i, i + 1));
                            }
                            let (del, _) = ui.allocate_exact_size(Vec2::splat(size::CONTROL_SM), egui::Sense::hover());
                            if widgets::icon_button_at(ui, del, Id::new(("ow-del", i)), "trash", false, "Delete").clicked() {
                                remove = Some(i);
                            }
                        });
                        egui::Grid::new(("ow-grid", i)).num_columns(2).spacing([space::SM, space::XS]).show(ui, |ui| {
                            let k = |ui: &mut egui::Ui, s: &str| {
                                ui.label(egui::RichText::new(s).font(theme::regular(text::SMALL)).color(t.muted));
                            };
                            k(ui, "Program");
                            changed |= ui
                                .add(egui::TextEdit::singleline(&mut r.executable).hint_text(r"C:\Program Files\Editor\editor.exe").font(theme::mono(text::MONO)).desired_width(f32::INFINITY).id(Id::new(("ow-exe", i))))
                                .changed();
                            ui.end_row();
                            k(ui, "Local");
                            changed |= text_opt(ui, &mut r.local_args, "{folder}", Id::new(("ow-local", i)));
                            ui.end_row();
                            k(ui, "WSL");
                            changed |= text_opt(ui, &mut r.wsl_args, "--folder-uri vscode-remote://wsl+{distro:uri}{linuxPath:uri}", Id::new(("ow-wsl", i)));
                            ui.end_row();
                            k(ui, "SSH");
                            changed |= text_opt(ui, &mut r.ssh_args, "--folder-uri vscode-remote://ssh-remote+{userAtHost:uri}{remoteFolder:uri}", Id::new(("ow-ssh", i)));
                            ui.end_row();
                        });
                        if changed {
                            st.dirty = true;
                        }
                    });
                }
            });
            if let Some(i) = make_default {
                for (j, r) in st.rows.iter_mut().enumerate() {
                    r.is_default = i == j;
                }
                st.dirty = true;
            }
            if let Some((a, b)) = swap
                && a < st.rows.len()
                && b < st.rows.len()
            {
                st.rows.swap(a, b);
                st.dirty = true;
            }
            if let Some(i) = remove
                && i < st.rows.len()
            {
                st.rows.remove(i);
                st.dirty = true;
            }
            ui.add_space(space::XS);
            ui.horizontal_wrapped(|ui| {
                if widgets::button(ui, ButtonKind::Ghost, Some("plus"), "Add editor", None).clicked() {
                    let first = st.rows.is_empty();
                    st.rows.push(OpenWithEntryView { local_args: Some("{folder}".into()), is_default: first, ..OpenWithEntryView::default() });
                    st.dirty = true;
                }
                if widgets::button(ui, ButtonKind::Ghost, Some("search"), "Detect installed editors", None).on_hover_text("Adds editors that aren't listed yet").clicked() {
                    actions.push(Action::Tool("open_with_detect", json!({})));
                }
            });
            ui.horizontal(|ui| {
                if widgets::button(ui, ButtonKind::Primary, Some("check"), "Save", None).clicked() {
                    actions.push(Action::Tool("open_with_set", set_params(&st.rows)));
                    close = true;
                }
                if widgets::button(ui, ButtonKind::Secondary, None, "Cancel", None).clicked() {
                    close = true;
                }
            });
        });
    ctx.data_mut(|d| {
        if close || !open {
            d.remove::<EditorState>(key());
        } else {
            d.insert_temp(key(), st);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_become_open_with_set_params_without_blank_templates() {
        let rows = vec![OpenWithEntryView {
            id: "a".into(),
            label: " Zed ".into(),
            executable: r"C:\Zed\Zed.exe".into(),
            local_args: Some("{folder}".into()),
            wsl_args: Some("  ".into()),
            is_default: true,
            ..OpenWithEntryView::default()
        }];
        let p = set_params(&rows);
        assert_eq!(
            p,
            json!({"entries": [{"id": "a", "label": "Zed", "executable": r"C:\Zed\Zed.exe", "local_args": "{folder}", "is_default": true}]})
        );
    }
}
