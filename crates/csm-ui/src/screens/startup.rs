//! Settings → General → Start-up: start when the person signs in, and start minimized.
//!
//! "Start CodeShellManager when I sign in" shows what the OS says (`app_autostart_get`, asked
//! each time Settings opens and followed through `SettingChanged { key: "StartAtSignIn" }` when
//! the CLI or an MCP client changes it), never a copy kept here; the switch is
//! `app_autostart_set`. "Start minimized" and its tray choice are ordinary settings
//! (`StartMinimized`, `StartMinimizedToTray`), read by the host at the next start
//! (`crate::start_window`). The tray choice is offered only where there is a tray icon.

use egui::{Id, Ui};
use serde_json::{Value, json};

use super::settings::{caption, section};
use crate::app::{Action, CsmApp};
use crate::backend::Call;
use crate::widgets;

/// The pseudo-setting key the engine announces a change under (`app_autostart_set`).
pub const STATUS_KEY: &str = "StartAtSignIn";

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The explanation under the switch, from what the OS said.
pub fn hint(status: &Value) -> String {
    if status.is_null() {
        return "Checking with the system…".into();
    }
    if flag(status, "disabled_by_os") {
        return if cfg!(windows) {
            "Turned off in Task Manager (Startup apps). Turn it on here to start at sign-in again.".into()
        } else {
            "Turned off in the desktop's startup settings. Turn it on here to start at sign-in again.".into()
        };
    }
    if flag(status, "enabled") && !flag(status, "current") {
        let program = status.get("program").and_then(Value::as_str).unwrap_or("another copy");
        return format!("It starts {program}, not this copy. Turn it off and on again to start this one.");
    }
    "Your system starts it when you sign in (your user only). Settings below decide how the window opens.".into()
}

pub fn show(ui: &mut Ui, app: &CsmApp, actions: &mut Vec<Action>) {
    section(ui, "Start-up");
    let status = &app.ui.settings.autostart;
    if !status.is_null() && !flag(status, "supported") {
        caption(ui, "Starting at sign-in isn't available on this system.");
    } else {
        let on = flag(status, "enabled");
        if widgets::toggle(ui, on, "Start CodeShellManager when I sign in", Some(&hint(status))).clicked() && !status.is_null() {
            actions.push(Action::Tool("app_autostart_set", json!({"enabled": !on})));
        }
    }
    let minimized = app.ui.settings.bool("StartMinimized").unwrap_or(false);
    let hint = "The window opens minimized on every start, at sign-in too; sessions restore as usual.";
    if widgets::toggle(ui, minimized, "Start minimized", Some(hint)).clicked() {
        actions.push(Action::Tool("settings_set", json!({"key": "StartMinimized", "value": !minimized})));
    }
    // Only where a tray icon exists to show it again (not on Linux, not if the OS refused one).
    if minimized && app.background.tray_available {
        let to_tray = app.ui.settings.bool("StartMinimizedToTray").unwrap_or(false);
        ui.indent(Id::new("start-minimized-to-tray"), |ui| {
            let hint = "Only the tray icon shows; click it to open the window. Off: minimized to the taskbar.";
            if widgets::toggle(ui, to_tray, "Start in the tray (notification area)", Some(hint)).clicked() {
                actions.push(Action::Tool("settings_set", json!({"key": "StartMinimizedToTray", "value": !to_tray})));
            }
        });
    }
}

/// Replies this module follows (called for every finished call).
pub(crate) fn on_reply(app: &mut CsmApp, call: &Call, reply: &csm_engine::Reply) {
    if let ("app_autostart_get" | "app_autostart_set", Ok(o)) = (call.id.as_str(), reply) {
        app.ui.settings.autostart = o.data.clone();
    }
}

/// Events this module follows: a change by the CLI or an MCP client.
pub(crate) fn on_event(app: &mut CsmApp, e: &csm_engine::Event) {
    if let csm_engine::Event::SettingChanged { key, value } = e
        && key == STATUS_KEY
    {
        app.ui.settings.autostart = value.clone();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hint_says_what_the_os_said() {
        assert!(hint(&Value::Null).contains("Checking"));
        assert!(hint(&json!({"supported": true, "enabled": false, "disabled_by_os": true})).contains("Turned off"));
        assert!(hint(&json!({"supported": true, "enabled": true, "current": false, "program": "/old/codeshell"})).contains("/old/codeshell"));
        assert!(hint(&json!({"supported": true, "enabled": true, "current": true})).contains("when you sign in"));
    }
}
