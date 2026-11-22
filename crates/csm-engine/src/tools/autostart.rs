//! Starting the app when the person signs in (`app_autostart_get`, `app_autostart_set`).
//!
//! The OS's own entry is the setting (`csm_services::autostart`): read back on every call, never
//! kept in `state.json`. Changing it decides what runs at the next sign-in, so an MCP caller asks
//! first (`LAUNCHES_PROCESS`, and it writes outside the app's state: `WRITES_FILESYSTEM`). The
//! registry and file access is a few small reads and writes, done here on the engine thread.

use std::path::PathBuf;

use csm_services::autostart::{self, Status};
use serde::Deserialize;
use serde_json::Value;

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

/// The pseudo-setting the UI follows: `SettingChanged { key, value: <status> }` after a change.
pub const SETTING_KEY: &str = "StartAtSignIn";

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "app_autostart_get",
            title: "Start at sign-in",
            summary: "Whether the OS starts CodeShellManager when you sign in, read from the OS itself (the Windows Run key, a macOS LaunchAgent, an XDG autostart entry): enabled, whether Task Manager or the desktop turned it off, and the program it starts.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: autostart_get,
        },
        ToolSpec {
            id: "app_autostart_set",
            title: "Start at sign-in…",
            summary: "Start CodeShellManager when you sign in (enabled: true) or not (false). Writes or removes the current user's OS entry for this copy of the app, started with --autostart; turning it on also clears a Task Manager disable.",
            toolset: Toolset::App,
            effects: Effects::LAUNCHES_PROCESS.union(Effects::WRITES_FILESYSTEM).union(Effects::IDEMPOTENT),
            input_schema: schema_of::<SetParams>,
            run: autostart_set,
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParams {
    /// Start at sign-in (true) or not (false).
    enabled: bool,
}

fn unsupported() -> Status {
    Status { supported: false, enabled: false, registered: false, disabled_by_os: false, program: None, current: false, location: String::new() }
}

/// What the person reads: on, off, off in the OS, or on for another copy.
fn describe(s: &Status) -> String {
    if !s.supported {
        return "Starting at sign-in isn't available on this system.".into();
    }
    let other = match (&s.program, s.current) {
        (Some(p), false) => format!(" It starts {p}, not this copy."),
        _ => String::new(),
    };
    if s.disabled_by_os {
        let where_ = if cfg!(windows) { "in Task Manager (Startup apps)" } else { "by the desktop's startup settings" };
        format!("The entry is there but turned off {where_}; turn it on here to start at sign-in again.{other}")
    } else if s.enabled {
        format!("CodeShellManager starts when you sign in.{other}")
    } else {
        "CodeShellManager does not start when you sign in.".into()
    }
}

fn reply(s: &Status) -> Result<ToolOutput, ToolError> {
    let data = serde_json::to_value(s).map_err(|e| ToolError::Failed(e.to_string()))?;
    Ok(ToolOutput::new(describe(s), data))
}

fn parts(ctx: &Ctx<'_>) -> Option<(std::sync::Arc<dyn autostart::Backend>, PathBuf)> {
    let a = &ctx.state.options.autostart;
    Some((a.backend.clone()?, a.program.clone()?))
}

fn autostart_get(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let Some((b, exe)) = parts(ctx) else { return reply(&unsupported()) };
    let s = autostart::status(b.as_ref(), &exe).map_err(|e| ToolError::Failed(format!("could not read {}: {e}", b.location())))?;
    reply(&s)
}

fn autostart_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SetParams = parse_params(params)?;
    let Some((b, exe)) = parts(ctx) else {
        return Err(ToolError::Refused("starting at sign-in isn't available on this system".into()));
    };
    let s = autostart::set(b.as_ref(), &exe, p.enabled).map_err(|e| match e {
        autostart::Error::BadPath(why) => ToolError::Failed(format!("this copy of the app can't be started at sign-in: {why}")),
        autostart::Error::Io(e) => ToolError::Failed(format!("could not change {}: {e}", b.location())),
    })?;
    if p.enabled {
        log::info!("autostart: on ({})", s.location);
    } else {
        log::info!("autostart: off");
    }
    let value = serde_json::to_value(&s).unwrap_or(Value::Null);
    ctx.events.emit(crate::Event::SettingChanged { key: SETTING_KEY.into(), value });
    reply(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(enabled: bool, disabled: bool, current: bool) -> Status {
        Status {
            supported: true,
            enabled,
            registered: enabled || disabled,
            disabled_by_os: disabled,
            program: Some("/opt/csm/codeshell".into()),
            current,
            location: "x".into(),
        }
    }

    #[test]
    fn the_message_says_on_off_turned_off_in_the_os_and_another_copy() {
        assert_eq!(describe(&st(true, false, true)), "CodeShellManager starts when you sign in.");
        assert_eq!(describe(&st(false, false, false)), "CodeShellManager does not start when you sign in.");
        assert!(describe(&st(false, true, true)).contains("turned off"));
        assert!(describe(&st(true, false, false)).contains("It starts /opt/csm/codeshell, not this copy."));
        assert!(describe(&unsupported()).contains("isn't available"));
    }
}
