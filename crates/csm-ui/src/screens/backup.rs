//! Settings → General → Backup (the WPF Export / Import of a `.csm` setup), the import review
//! that asks before anything is replaced, Settings → Usage (`usage_stats`), and the replies they
//! follow.
//!
//! Export… and Import… ask the host for the system's file dialog
//! ([`crate::app::DialogRequest`]); the file is written or read by the engine's tool on a
//! worker, never here. Import first reads the backup with `dry_run` and shows what it would
//! replace, and the backup's Open with commands; the backup's Open with list is only taken when
//! the person ticks it (off by default, as the WPF question defaulted to No).

use std::path::Path;

use egui::{Ui, vec2};
use serde_json::{Value, json};

use super::settings::{caption, section};
use crate::app::{Action, CsmApp, DialogRequest};
use crate::backend::Call;
use crate::theme::{self, Tokens, semibold, space, text};
use crate::widgets::{self, ButtonKind};

/// Settings → Usage.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum UsageState {
    /// Asked for when the section shows.
    #[default]
    NotAsked,
    Loading,
    Ready(Vec<Value>),
    Failed(String),
}

/// A backup picked with Import…, being reviewed before it replaces anything.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ImportReview {
    pub path: String,
    /// `state_import`'s dry-run answer, once it came.
    pub summary: Option<Value>,
    pub error: Option<String>,
    /// "Also replace my Open with list with the backup's". Off unless ticked.
    pub adopt_open_with: bool,
    /// The import was submitted and hasn't answered.
    pub importing: bool,
}

/// The file name Export… suggests (`CodeShellManager-backup-2026-11-20.csm`, the WPF one).
pub fn backup_file_name(now_ms: i64) -> String {
    let ts = csm_core::time::Timestamp::from_epoch_ms(now_ms);
    format!("CodeShellManager-backup-{}.csm", ts.as_str().get(..10).unwrap_or("today"))
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0))
}

/// A save path without an extension gets `.csm` (the WPF dialog's `DefaultExt`). Pure.
fn with_csm_extension(path: String) -> String {
    if Path::new(&path).extension().is_some() { path } else { format!("{path}.csm") }
}

/// The host's answer to a Save / Open dialog this module asked for.
pub(crate) fn dialog_answered(app: &mut CsmApp, request: &DialogRequest, path: Option<String>) {
    let Some(path) = path.filter(|p| !p.trim().is_empty()) else { return };
    match request {
        DialogRequest::SaveBackup { .. } => app.dispatch.submit("state_export", json!({"path": with_csm_extension(path)})),
        DialogRequest::OpenBackup => {
            app.ui.settings.import = Some(ImportReview { path: path.clone(), ..ImportReview::default() });
            app.dispatch.submit("state_import", json!({"path": path, "dry_run": true}));
        }
        DialogRequest::Folder { .. } => {}
    }
}

/// Export / Import / Usage picked in the palette: what Settings' buttons do. Import and Usage
/// open Settings (the review card and the table live there); Export only asks for a path.
pub(crate) fn from_palette(app: &mut CsmApp, ctx: &egui::Context, id: &str) {
    match id {
        "state_export" => app.ui.dialog_request = Some(DialogRequest::SaveBackup { file_name: backup_file_name(now_ms()) }),
        "state_import" => {
            app.run(ctx, Action::OpenSettings);
            app.ui.settings.section = crate::app::SettingsSection::General;
            app.ui.dialog_request = Some(DialogRequest::OpenBackup);
        }
        "usage_stats" => {
            app.run(ctx, Action::OpenSettings);
            app.ui.settings.section = crate::app::SettingsSection::Usage;
            app.ui.settings.usage = UsageState::default();
        }
        _ => {}
    }
}

/// Replies this module follows (called for every finished call).
pub(crate) fn on_reply(app: &mut CsmApp, ctx: &egui::Context, call: &Call, reply: &csm_engine::Reply) {
    match call.id.as_str() {
        "usage_stats" => {
            app.ui.settings.usage = match reply {
                Ok(o) => UsageState::Ready(o.data["commands"].as_array().cloned().unwrap_or_default()),
                Err(e) => UsageState::Failed(e.to_string()),
            };
        }
        "state_export" => {
            if let Ok(o) = reply
                && !o.message.is_empty()
            {
                app.toast(ctx, o.message.clone());
            }
        }
        "state_import" => {
            let Some(r) = app.ui.settings.import.as_mut() else { return };
            if call.params["path"].as_str() != Some(r.path.as_str()) {
                return; // an older review answering late
            }
            let dry = call.params["dry_run"].as_bool().unwrap_or(false);
            match (dry, reply) {
                (true, Ok(o)) => r.summary = Some(o.data.clone()),
                (_, Err(e)) => {
                    r.importing = false;
                    r.error = Some(e.to_string());
                }
                (false, Ok(_)) => {
                    // Done: the snapshot already shows the backup's sessions; the settings page
                    // reads the imported values again.
                    app.ui.settings.import = None;
                    app.dispatch.submit("settings_get", json!({}));
                }
            }
        }
        _ => {}
    }
}

/// Settings → General's Backup row, and the review of a backup being imported.
pub fn general(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    section(ui, "Backup");
    caption(ui, "Export saves your sessions, groups, layouts and settings to a .csm file. Import replaces them with a backup's.");
    ui.horizontal(|ui| {
        if widgets::button(ui, ButtonKind::Secondary, Some("download"), "Export…", None).clicked() {
            app.ui.dialog_request = Some(DialogRequest::SaveBackup { file_name: backup_file_name(now_ms()) });
        }
        if widgets::button(ui, ButtonKind::Secondary, Some("enter"), "Import…", None).clicked() {
            app.ui.dialog_request = Some(DialogRequest::OpenBackup);
        }
    });
    if app.ui.settings.import.is_some() {
        review(ui, app, actions);
    }
}

fn review(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    let Some(r) = app.ui.settings.import.clone() else { return };
    let file = Path::new(&r.path).file_name().map_or_else(|| r.path.clone(), |f| f.to_string_lossy().into_owned());
    let mut close = false;
    let mut submit = None;
    let mut adopt = r.adopt_open_with;
    widgets::card_frame(&t).inner_margin(egui::Margin::same(space::MD as i8)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = space::SM;
        ui.label(egui::RichText::new(format!("Import {file}")).font(semibold(text::EMPH)).color(t.fg));
        let n = |v: &Value| v.as_u64().unwrap_or(0);
        match (&r.error, &r.summary) {
            (Some(e), _) => {
                ui.add(egui::Label::new(egui::RichText::new(e).font(theme::regular(text::BODY)).color(t.red)).wrap());
            }
            (None, None) => {
                ui.label(egui::RichText::new("Reading the backup…").font(theme::regular(text::BODY)).color(t.muted));
            }
            (None, Some(s)) => {
                let body = format!(
                    "This backup has {} sessions and {} groups. Importing replaces your {} sessions and {} groups: they are closed, busy or not (Recently closed keeps the last ten). The imported sessions arrive asleep; wake them when you need them. Launch commands, Claude commands and terminal settings come from the backup too.",
                    n(&s["sessions"]),
                    n(&s["groups"]),
                    n(&s["replaces"]["sessions"]),
                    n(&s["replaces"]["groups"]),
                );
                ui.add(egui::Label::new(egui::RichText::new(body).font(theme::regular(text::BODY)).color(t.fg2)).wrap());
                let commands: Vec<&str> = s["commands"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
                if !commands.is_empty() {
                    caption(ui, "The imported sessions run these commands when you wake them:");
                    widgets::card_frame(&t).inner_margin(egui::Margin::same(space::SM as i8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        let listing = commands.join("\n");
                        ui.add(egui::Label::new(egui::RichText::new(listing).font(theme::mono(text::MONO_SM)).color(t.fg)).wrap());
                    });
                }
                let ow = &s["open_with"];
                if ow["differs"].as_bool() == Some(true) {
                    caption(ui, "The backup has its own Open with list. These programs would run when you use { } or Open with:");
                    let listing = ow["review"].as_str().unwrap_or_default();
                    widgets::card_frame(&t).inner_margin(egui::Margin::same(space::SM as i8)).show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.add(egui::Label::new(egui::RichText::new(listing).font(theme::mono(text::MONO_SM)).color(t.fg)).wrap());
                    });
                    if widgets::toggle(
                        ui,
                        adopt,
                        "Replace my Open with list with this one",
                        Some("Leave this off unless you made this backup yourself: your list is kept."),
                    )
                    .clicked()
                    {
                        adopt = !adopt;
                    }
                }
                if n(&ow["dropped"]) > 0 {
                    caption(ui, &format!("{} Open with entries were left out: they name programs that are not accepted from a backup.", n(&ow["dropped"])));
                }
            }
        }
        ui.add_space(space::XS);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing = vec2(space::SM, 0.0);
            let ready = r.error.is_none() && r.summary.is_some() && !r.importing;
            if ready && widgets::button(ui, ButtonKind::Danger, Some("enter"), "Import and replace", None).clicked() {
                submit = Some(json!({"path": r.path, "force": true, "adopt_open_with": adopt}));
            }
            if r.importing {
                ui.label(egui::RichText::new("Importing…").font(theme::regular(text::BODY)).color(t.muted));
            }
            if widgets::button(ui, ButtonKind::Ghost, None, if r.error.is_some() { "Close" } else { "Cancel" }, None).clicked() {
                close = true;
            }
        });
    });
    if let Some(cur) = app.ui.settings.import.as_mut() {
        cur.adopt_open_with = adopt;
        if submit.is_some() {
            cur.importing = true;
        }
    }
    if let Some(p) = submit {
        actions.push(Action::Tool("state_import", p));
    }
    if close {
        app.ui.settings.import = None;
    }
}

/// `3h 05m`, `12m`, `45s`.
pub fn duration_text(seconds: i64) -> String {
    let s = seconds.max(0);
    match (s / 3600, (s / 60) % 60) {
        (0, 0) => format!("{s}s"),
        (0, m) => format!("{m}m"),
        (h, m) => format!("{h}h {m:02}m"),
    }
}

/// Settings → Usage: sessions and running time per command.
pub fn usage(ui: &mut Ui, app: &mut CsmApp, actions: &mut Vec<Action>) {
    let t = Tokens::get(ui.ctx());
    if app.ui.settings.usage == UsageState::NotAsked {
        app.ui.settings.usage = UsageState::Loading;
        actions.push(Action::Tool("usage_stats", json!({})));
    }
    caption(ui, "Counted on this machine only: how many sessions each command started, how long they ran, and when it was last used.");
    match app.ui.settings.usage.clone() {
        UsageState::NotAsked | UsageState::Loading => {
            ui.label(egui::RichText::new("Loading…").font(theme::regular(text::BODY)).color(t.muted));
        }
        UsageState::Failed(m) => caption(ui, &m),
        UsageState::Ready(rows) if rows.is_empty() => {
            ui.label(egui::RichText::new("No sessions counted yet.").font(theme::regular(text::BODY)).color(t.muted));
        }
        UsageState::Ready(rows) => {
            widgets::card_frame(&t).inner_margin(egui::Margin::same(space::MD as i8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                egui::Grid::new("usage-stats").num_columns(4).spacing(vec2(space::XL, space::SM)).striped(false).show(ui, |ui| {
                    for h in ["Command", "Sessions", "Time", "Last used"] {
                        widgets::caps(ui, h);
                    }
                    ui.end_row();
                    for r in &rows {
                        let cell = |ui: &mut Ui, s: String, mono: bool| {
                            let font = if mono { theme::mono(text::MONO) } else { theme::regular(text::BODY) };
                            ui.label(egui::RichText::new(s).font(font).color(t.fg));
                        };
                        cell(ui, r["command"].as_str().unwrap_or_default().to_string(), true);
                        cell(ui, r["sessions"].as_i64().unwrap_or(0).to_string(), false);
                        cell(ui, duration_text(r["total_seconds"].as_i64().unwrap_or(0)), false);
                        cell(ui, r["last_used"].as_str().unwrap_or_default().to_string(), false);
                        ui.end_row();
                    }
                });
            });
        }
    }
    if widgets::button(ui, ButtonKind::Secondary, Some("restart"), "Refresh", None).clicked() {
        app.ui.settings.usage = UsageState::Loading;
        actions.push(Action::Tool("usage_stats", json!({})));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_read_naturally() {
        assert_eq!(duration_text(45), "45s");
        assert_eq!(duration_text(12 * 60 + 5), "12m");
        assert_eq!(duration_text(3 * 3600 + 5 * 60), "3h 05m");
        assert_eq!(duration_text(-4), "0s");
    }

    #[test]
    fn backups_are_named_and_suffixed_like_the_wpf_ones() {
        assert_eq!(backup_file_name(1_700_000_000_000), "CodeShellManager-backup-2023-11-14.csm");
        assert_eq!(with_csm_extension("C:\\b\\mine".into()), "C:\\b\\mine.csm");
        assert_eq!(with_csm_extension("/b/mine.csm".into()), "/b/mine.csm");
    }
}
