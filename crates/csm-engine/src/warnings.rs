//! Standing warnings: things the person must know about for as long as they are true, shown by the
//! UI as a strip under the title bar (`Snapshot::warnings`). They are derived from the state on
//! every snapshot, so none can be forgotten when its cause goes away:
//!
//! - `state_save_failed` / `tasks_save_failed`: the saver could not write the file (a full disk, a
//!   locked folder). It keeps retrying with a growing pause; the warning goes with the first save
//!   that works, and a toast says so.
//! - `state_read_only`: `state.json` exists but could not be read at start-up, so this run starts
//!   empty and saves nothing (the real file is never replaced).
//! - `state_newer`: `state.json` was written by a newer app (its `CsmRsVersion` is above what this
//!   build understands): the same read-only start, with its own text.
//! - `tasks_unreadable`: the same for the Board's `tasks.json`.
//! - `beta_import`: first run of the stable app with no state of its own but a beta's: offers
//!   `state_import_beta`.

use serde::Serialize;
use serde_json::{Value, json};

use crate::state::State;

/// A button on a warning: runs a tool.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WarningAction {
    pub label: String,
    pub tool: &'static str,
    pub params: Value,
}

/// One standing warning.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Warning {
    /// Stable key (`state_save_failed`, …).
    pub id: String,
    pub text: String,
    pub action: Option<WarningAction>,
    /// A second button that hides it (offers only; a failure cannot be hidden).
    pub dismiss: Option<WarningAction>,
}

fn warn(id: &str, text: String) -> Warning {
    Warning { id: id.into(), text, action: None, dismiss: None }
}

/// Everything that is wrong or on offer now.
pub(crate) fn current(state: &State) -> Vec<Warning> {
    let mut out = Vec::new();
    if let Some(v) = state.options.read_only.as_deref().and_then(|w| w.strip_prefix(crate::persist::NEWER_STATE_MARKER)) {
        out.push(warn(
            "state_newer",
            format!("Your saved sessions were written by a newer CodeShellManager (file version {v}; this one understands {}), so this one started empty and will not save anything this time. The file is untouched: install the newer version again to use it.", crate::persist::STATE_FILE_VERSION),
        ));
    } else if let Some(why) = &state.options.read_only {
        out.push(warn(
            "state_read_only",
            format!("Your saved sessions could not be read ({why}), so CodeShellManager started empty and will not save anything this time. Close what is using the file and restart; the file itself is untouched."),
        ));
    }
    if state.rt.tasks_unreadable {
        out.push(warn("tasks_unreadable", "The Board's tasks.json could not be read, so the Board is empty and will not be saved this time. The file is untouched.".into()));
    }
    if let Some(why) = state.rt.saver.as_ref().and_then(|s| s.health().failing()) {
        out.push(warn(
            "state_save_failed",
            format!("Your sessions are NOT being saved ({why}). Free some disk space; CodeShellManager keeps retrying and this goes away when a save works."),
        ));
    }
    if let Some(why) = state.rt.tasks_saver.as_ref().and_then(|s| s.health().failing()) {
        out.push(warn("tasks_save_failed", format!("The Board is NOT being saved ({why}). CodeShellManager keeps retrying.")));
    }
    if state.rt.beta_offer.is_some() {
        out.push(Warning {
            id: "beta_import".into(),
            text: "Found sessions from a CodeShellManager beta on this computer. Bring them into this version?".into(),
            action: Some(WarningAction { label: "Import from the beta".into(), tool: "state_import_beta", params: json!({}) }),
            dismiss: Some(WarningAction { label: "No thanks".into(), tool: "state_import_beta", params: json!({"dismiss": true}) }),
        });
    }
    out
}

/// On every engine wake-up: a saver's failure or recovery marks the snapshot stale (the warning
/// appears or goes) and a recovery says so in a toast.
pub(crate) fn tick(ctx: &mut crate::engine::Ctx<'_>) {
    let healths = [ctx.state.rt.saver.as_ref().map(|s| ("state", s.health())), ctx.state.rt.tasks_saver.as_ref().map(|s| ("tasks", s.health()))];
    for (what, h) in healths.into_iter().flatten() {
        match h.take_change() {
            Some(crate::persist::HealthChange::Failed(why)) => {
                log::error!("{what}: saving failed: {why}");
                ctx.state.touch();
            }
            Some(crate::persist::HealthChange::Recovered) => {
                log::info!("{what}: saving works again");
                ctx.events.toast("Saving works again; your sessions are saved.");
                ctx.state.touch();
            }
            None => {}
        }
    }
}
