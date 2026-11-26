//! Explorer's "Open in CodeShellManager" entry: the per-user off switch (`app_explorer_menu_get`,
//! `app_explorer_menu_set`).
//!
//! The entry is the installer's (per machine); this is only the person's choice to hide it from
//! their own right-click menu (`csm_services::explorer_menu`). The registry is the setting: read
//! back on every call, never kept in `state.json`, and not a `settings_set` key, so an MCP client
//! cannot reach it that way. Changing it writes outside the app's state, so an MCP caller asks
//! first (`WRITES_FILESYSTEM`). A few small registry reads and writes, done on the engine thread
//! like the sign-in entry's; never on the UI thread.

use std::sync::Arc;

use csm_core::channel::Channel;
use csm_services::explorer_menu::{self, Status};
use serde::Deserialize;
use serde_json::Value;

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

/// The pseudo-setting the UI follows: `SettingChanged { key, value: <status> }` after a change.
pub const SETTING_KEY: &str = "ExplorerMenu";

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "app_explorer_menu_get",
            title: "Explorer right-click entry",
            summary: "Whether Explorer's right-click menu offers \"Open in CodeShellManager\" for this user, read from the registry: supported (Windows only), installed (the MSI's entry exists; a portable copy has none) and shown (the person has not hidden it).",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: explorer_menu_get,
        },
        ToolSpec {
            id: "app_explorer_menu_set",
            title: "Explorer right-click entry…",
            summary: "Show (shown: true) or hide (false) \"Open in CodeShellManager\" in Explorer's right-click menu for the current user. Hiding writes a LegacyDisable value under HKEY_CURRENT_USER\\Software\\Classes; showing removes it. The installer's own machine-wide entry is never touched. Windows only.",
            toolset: Toolset::App,
            effects: Effects::WRITES_FILESYSTEM.union(Effects::IDEMPOTENT),
            input_schema: schema_of::<SetParams>,
            run: explorer_menu_set,
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParams {
    /// Show the entry (true) or hide it (false).
    shown: bool,
}

fn unsupported() -> Status {
    Status { supported: false, installed: false, shown: false, text: explorer_menu::verb_text(Channel::current()).to_string(), location: String::new() }
}

fn describe(s: &Status) -> String {
    if !s.supported {
        return "Explorer's right-click menu entry isn't available on this system.".into();
    }
    let what = format!("\"{}\"", s.text);
    match (s.shown, s.installed) {
        (true, true) => format!("Explorer's right-click menu offers {what}."),
        (true, false) => format!("{what} is not hidden, but this copy was not installed with the installer, so Explorer has no such entry."),
        (false, _) => format!("{what} is hidden from Explorer's right-click menu for you."),
    }
}

fn reply(s: &Status) -> Reply {
    let data = serde_json::to_value(s).map_err(|e| ToolError::Failed(e.to_string()))?;
    Ok(ToolOutput::new(describe(s), data))
}

fn store(ctx: &Ctx<'_>) -> Option<Arc<dyn explorer_menu::Store>> {
    ctx.state.options.explorer_menu.store.clone()
}

fn explorer_menu_get(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let Some(s) = store(ctx) else { return reply(&unsupported()) };
    let status = explorer_menu::status(s.as_ref(), Channel::current()).map_err(|e| ToolError::Failed(format!("could not read {}: {e}", s.location())))?;
    reply(&status)
}

fn explorer_menu_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SetParams = parse_params(params)?;
    let Some(s) = store(ctx) else {
        return Err(ToolError::Refused("Explorer's right-click menu entry isn't available on this system".into()));
    };
    let status = explorer_menu::set(s.as_ref(), Channel::current(), p.shown).map_err(|e| ToolError::Failed(format!("could not change {}: {e}", s.location())))?;
    log::info!("explorer menu: {}", if status.shown { "shown" } else { "hidden" });
    let value = serde_json::to_value(&status).unwrap_or(Value::Null);
    ctx.events.emit(crate::Event::SettingChanged { key: SETTING_KEY.into(), value });
    reply(&status)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn st(shown: bool, installed: bool) -> Status {
        Status { supported: true, installed, shown, text: "Open in CodeShellManager".into(), location: "x".into() }
    }

    #[test]
    fn the_message_says_offered_hidden_or_not_installed() {
        assert!(describe(&st(true, true)).contains("offers"));
        assert!(describe(&st(false, true)).contains("hidden"));
        assert!(describe(&st(true, false)).contains("not installed with the installer"));
        assert!(describe(&unsupported()).contains("isn't available"));
    }
}
