//! `app` tools: info, settings, recently closed, importing the WPF app's state, quitting, and
//! the toolset switches (filtered by the MCP layer; no-ops here).

use std::path::PathBuf;

use csm_core::models::{AppSettings, SessionStatus, UI_ZOOM_RANGE};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::engine::{Ctx, Reply};
use crate::lifecycle;
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    let t = |id, title, summary, toolset, effects, input_schema, run| ToolSpec { id, title, summary, toolset, effects, input_schema, run };
    vec![
        t(
            "app_info",
            "About",
            "Version, the ConPTY in use, where the state is kept, session counts.",
            Toolset::App,
            Effects::READ_ONLY,
            empty_schema,
            app_info,
        ),
        t(
            "settings_get",
            "Settings",
            "All settings (or one, by key). Keys as in state.json (PascalCase) or snake_case. Secrets are not returned.",
            Toolset::App,
            Effects::READ_ONLY,
            schema_of::<GetParams>,
            settings_get,
        ),
        t(
            "settings_set",
            "Change a setting…",
            "Set one setting by key. Settings that decide what runs (default command, launch commands, editors, API key, ConPTY) can only be changed in the app.",
            Toolset::App,
            Effects::MUTATES_STATE,
            schema_of::<SetParams>,
            settings_set,
        ),
        t(
            "recently_closed_list",
            "Recently closed",
            "Closed sessions that can be reopened, newest first (index 0).",
            Toolset::App,
            Effects::READ_ONLY,
            empty_schema,
            recently_closed_list,
        ),
        t(
            "session_recent",
            "Recent sessions",
            "The sessions used most recently (up to 10, live or asleep, newest first), without the focused one: the first is the session used before it. Each has its status and how long ago it was used (5m, 3h, 2d). `session_focus` goes to one (waking it).",
            Toolset::Core,
            Effects::READ_ONLY,
            empty_schema,
            session_recent,
        ),
        t(
            "state_import_wpf",
            "Import from CodeShellManager 0.x…",
            "Import sessions and groups from the WPF app's state.json (default: its usual location). Imported sessions arrive asleep; existing ones are kept.",
            Toolset::App,
            Effects::MUTATES_STATE,
            schema_of::<ImportParams>,
            state_import_wpf,
        ),
        t(
            "app_quit",
            "Quit",
            "Close every session (Claude ones get time to exit cleanly), save, then signal the app to exit. Sessions restore next time. force: true skips the exit waits (also while a quit is already running).",
            Toolset::App,
            Effects::MAY_DISCARD_DATA,
            schema_of::<QuitParams>,
            app_quit,
        ),
        t(
            "window_show",
            "Show the window",
            "Bring the app's window to the front, restoring it if it is minimized (the tray icon's Show window).",
            Toolset::App,
            Effects::UI.union(Effects::IDEMPOTENT),
            empty_schema,
            window_show,
        ),
        t(
            "notification_test",
            "Send a test notification",
            "Show a sample OS notification now, to check that notifications reach you (sent even with notifications off; the sound follows ShowNotificationSound).",
            Toolset::App,
            Effects::UI,
            empty_schema,
            notification_test,
        ),
        t(
            "grant_list",
            "MCP grants",
            "Clients allowed to call a tool without asking (\"Always for this client\" on a consent card).",
            Toolset::App,
            Effects::READ_ONLY,
            empty_schema,
            grant_list,
        ),
        t(
            "grant_revoke",
            "Revoke an MCP grant",
            "Make a client ask again before calling a tool (or, without `tool`, before calling any tool).",
            Toolset::App,
            Effects::MUTATES_STATE.union(Effects::IDEMPOTENT),
            schema_of::<RevokeParams>,
            grant_revoke,
        ),
        t(
            "toolset_enable",
            "Enable toolset",
            "Offer a toolset's tools to this MCP client (core is always on).",
            Toolset::Core,
            Effects::IDEMPOTENT,
            schema_of::<ToolsetParams>,
            toolset_switch,
        ),
        t(
            "toolset_disable",
            "Disable toolset",
            "Stop offering a toolset's tools to this MCP client.",
            Toolset::Core,
            Effects::IDEMPOTENT,
            schema_of::<ToolsetParams>,
            toolset_switch,
        ),
    ]
}

/// Settings an MCP client may not change: they decide what runs, or are secrets.
/// (`ClaudeCommands` is text the app types into sessions; `PassSessionNameToClaude` changes every
/// Claude command line, and a Claude CLI without `--name` would then refuse to start.)
const APP_ONLY_KEYS: [&str; 9] = [
    "AnthropicApiKey",
    "DefaultCommand",
    "DefaultWorkingFolder",
    "LaunchCommands",
    "OpenWith",
    "OpenWithSeeded",
    "UseBundledConPty",
    "ClaudeCommands",
    "PassSessionNameToClaude",
];
const SECRET_KEYS: [&str; 1] = ["AnthropicApiKey"];

/// `snake_case` → `PascalCase` (keys are PascalCase in state.json).
fn pascal(key: &str) -> String {
    if !key.contains('_') && key.chars().next().is_some_and(char::is_uppercase) {
        return key.to_owned();
    }
    key.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut c = p.chars();
            match c.next() {
                Some(f) => f.to_uppercase().chain(c).collect::<String>(),
                None => String::new(),
            }
        })
        .collect()
}

/// Every setting, including the Rust app's own ones that `state.json` leaves out while they are
/// at their defaults (so a file the WPF app reads back stays as it wrote it).
fn full_settings(s: &AppSettings) -> Result<Value, serde_json::Error> {
    let mut v = serde_json::to_value(s)?;
    if let Value::Object(m) = &mut v {
        m.entry("OfferMcpToClaude").or_insert(json!(s.offer_mcp_to_claude));
        m.entry("TerminalPadding").or_insert(json!(s.terminal_padding));
        m.entry("TerminalFontPoints").or_insert(json!(s.terminal_font_points));
        m.entry("ClaudeCommands").or_insert(json!(s.claude_commands));
        m.entry("AllowOsc52Clipboard").or_insert(json!(s.allow_osc52_clipboard));
        m.entry("UiZoomPercent").or_insert(json!(s.ui_zoom_percent));
        m.entry("CloseToTray").or_insert(json!(s.close_to_tray));
        m.entry("PassSessionNameToClaude").or_insert(json!(s.pass_session_name_to_claude));
        m.entry("ClaudeUsageHistory").or_insert(json!(s.claude_usage_history));
    }
    Ok(v)
}

fn settings_map(s: &AppSettings) -> Result<Map<String, Value>, ToolError> {
    match full_settings(s) {
        Ok(Value::Object(mut m)) => {
            for k in SECRET_KEYS {
                if let Some(v) = m.get_mut(k) {
                    let set = v.as_str().is_some_and(|x| !x.is_empty());
                    *v = Value::String(if set { "(set)".into() } else { String::new() });
                }
            }
            Ok(m)
        }
        _ => Err(ToolError::Failed("could not read the settings".into())),
    }
}

fn app_info(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let live = ctx.state.rt.sessions.len();
    let total = ctx.state.app.sessions.len();
    let path = ctx.state.options.save_path.as_ref().map(|p| p.display().to_string());
    let data = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "conpty": ctx.state.rt.conpty.get().cloned(),
        "state_path": path,
        "saving": ctx.state.options.saves(),
        "clean": ctx.state.options.clean,
        "sessions": total,
        "live": live,
        "groups": ctx.state.app.groups.len(),
    });
    Ok(ToolOutput::new(format!("CodeShellManager {} — {live} of {total} sessions running.", env!("CARGO_PKG_VERSION")), data))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct GetParams {
    key: Option<String>,
}

fn settings_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: GetParams = parse_params(params)?;
    let m = settings_map(&ctx.state.app.settings)?;
    match p.key {
        None => Ok(ToolOutput::new(format!("{} settings", m.len()), Value::Object(m))),
        Some(k) => {
            let key = pascal(&k);
            let v = m.get(&key).cloned().ok_or_else(|| ToolError::NotFound(format!("no setting `{k}`")))?;
            Ok(ToolOutput::new(format!("{key} = {v}"), json!({"key": key, "value": v})))
        }
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParams {
    key: String,
    value: Value,
}

fn settings_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SetParams = parse_params(params)?;
    let key = pascal(&p.key);
    if key == "AddonApprovals" {
        return Err(ToolError::Refused("add-ons are enabled and disabled with addon_enable and addon_disable (the person sees what they contribute first)".into()));
    }
    if ctx.caller.is_external() && APP_ONLY_KEYS.contains(&key.as_str()) {
        return Err(ToolError::Refused(format!("`{key}` decides what runs; change it in the app's settings")));
    }
    let Ok(Value::Object(mut m)) = full_settings(&ctx.state.app.settings) else {
        return Err(ToolError::Failed("could not read the settings".into()));
    };
    if !m.contains_key(&key) {
        return Err(ToolError::NotFound(format!("no setting `{}`; settings_get lists them", p.key)));
    }
    m.insert(key.clone(), p.value.clone());
    let updated: AppSettings =
        serde_json::from_value(Value::Object(m)).map_err(|e| ToolError::InvalidParams(format!("`{key}` cannot be {}: {e}", p.value)))?;
    if key == "UiZoomPercent" && !UI_ZOOM_RANGE.contains(&updated.ui_zoom_percent) {
        return Err(ToolError::InvalidParams(format!("UiZoomPercent is a percentage from {} to {}", UI_ZOOM_RANGE.start(), UI_ZOOM_RANGE.end())));
    }
    ctx.state.app.settings = updated;
    ctx.state.persist();
    // Events reach every IPC client: a secret's value never goes out.
    // Masked as `settings_get` masks it: "(set)", or "" once cleared.
    let shown = if SECRET_KEYS.contains(&key.as_str()) {
        json!(if p.value.as_str().is_some_and(|x| !x.is_empty()) { "(set)" } else { "" })
    } else {
        p.value.clone()
    };
    ctx.events.emit(crate::Event::SettingChanged { key: key.clone(), value: shown });
    let restart = matches!(key.as_str(), "UseBundledConPty");
    let msg = if restart { format!("{key} saved; it takes effect when the app restarts.") } else { format!("{key} saved.") };
    Ok(ToolOutput::new(msg, json!({"key": key, "value": p.value})))
}

/// The Recent menu's list (Ctrl+Shift+E), ordered by `csm_core::recent_sessions`.
fn session_recent(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let snap = super::current(ctx);
    let now = crate::session::now_ms();
    let picked = csm_core::recent_sessions::pick_default(&ctx.state.app.sessions, snap.focused.as_deref());
    let list: Vec<Value> = picked
        .iter()
        .filter_map(|s| snap.session(&s.id).map(|v| (s, v)))
        .map(|(s, v)| {
            json!({
                "session_id": v.id,
                "name": v.name,
                "folder": v.folder_short,
                "status": v.status,
                "asleep": v.asleep,
                "last_used_ms": if s.last_activity_at.is_never() { None } else { s.last_activity_at.epoch_ms() },
                "ago": csm_core::recent_sessions::ago_for(&s.last_activity_at, now),
            })
        })
        .collect();
    Ok(ToolOutput::new(format!("{} recent", list.len()), json!(list)))
}

fn recently_closed_list(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let list: Vec<Value> = ctx
        .state
        .app
        .recently_closed
        .iter()
        .enumerate()
        .map(|(i, e)| {
            json!({
                "index": i,
                "name": e.name,
                "subtitle": e.subtitle(),
                "command": e.command,
                "folder": e.working_folder,
                "closed_at": e.closed_at.as_str(),
            })
        })
        .collect();
    Ok(ToolOutput::new(format!("{} recently closed", list.len()), json!(list)))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ImportParams {
    /// Path to the WPF app's state.json. Default: %AppData%\CodeShellManager\state.json.
    path: Option<String>,
}

fn state_import_wpf(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ImportParams = parse_params(params)?;
    let path = match p.path.filter(|s| !s.trim().is_empty()) {
        Some(s) => PathBuf::from(s.trim()),
        None => crate::persist::wpf_state_path().ok_or_else(|| ToolError::NotFound("no default location on this platform; pass `path`".into()))?,
    };
    let reply = ctx.park();
    let engine = ctx.engine.clone();
    let worker = move || {
        let result = std::fs::read(&path).map_err(|e| format!("could not read {}: {e}", path.display())).and_then(|b| {
            csm_core::state::parse(&String::from_utf8_lossy(&b)).map_err(|e| format!("{} is not a CodeShellManager state file: {e}", path.display()))
        });
        engine.post(Box::new(move |ctx| {
            let r = match result {
                Err(msg) => Err(ToolError::Failed(msg)),
                Ok(imported) => Ok(merge(ctx, imported)),
            };
            crate::lifecycle::after_step(ctx.state, ctx.engine);
            if let Some(reply) = reply {
                reply.send(r);
            }
        }));
    };
    std::thread::Builder::new().name("csm-import".into()).spawn(worker).map_err(|e| ToolError::Failed(format!("could not start the import: {e}")))?;
    Ok(ToolOutput::message("Importing."))
}

/// Adds the imported groups and sessions that are not here yet (by id). Sessions arrive asleep.
fn merge(ctx: &mut Ctx<'_>, mut imported: csm_core::models::AppState) -> ToolOutput {
    let mut groups = 0;
    for g in imported.groups.drain(..) {
        if !g.id.trim().is_empty() && !ctx.state.app.groups.iter().any(|x| x.id == g.id) {
            ctx.state.app.groups.push(g);
            groups += 1;
        }
    }
    let mut sessions = 0;
    for mut s in imported.sessions.drain(..) {
        if s.id.trim().is_empty() || ctx.state.app.sessions.iter().any(|x| x.id == s.id) {
            continue;
        }
        s.is_dormant = true;
        s.status = SessionStatus::Idle;
        if !s.group_id.is_empty() && !ctx.state.app.groups.iter().any(|g| g.id == s.group_id) {
            s.group_id.clear();
        }
        ctx.state.app.sessions.push(s);
        sessions += 1;
    }
    for (i, g) in ctx.state.app.groups.iter_mut().enumerate() {
        g.sort_order = i32::try_from(i).unwrap_or(i32::MAX);
    }
    ctx.state.persist();
    ToolOutput::new(format!("Imported {sessions} sessions (asleep) and {groups} groups."), json!({"sessions": sessions, "groups": groups}))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct QuitParams {
    /// Stop waiting for sessions to exit on their own: everything still running is stopped now.
    /// Also works while a quit is already running (the shutdown board's "Force close now").
    force: bool,
}

fn app_quit(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: QuitParams = if params.is_null() { QuitParams::default() } else { parse_params(params)? };
    if matches!(ctx.caller, Caller::External { .. }) {
        // Allowed (it needs consent), but say what happens.
        log::info!("quit requested by an MCP client");
    }
    let reply = ctx.park();
    lifecycle::quit(ctx, p.force, reply);
    Ok(ToolOutput::message("Quitting."))
}

fn window_show(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    ctx.events.emit(crate::Event::WindowShowRequested);
    Ok(ToolOutput::message("Showing the window."))
}

fn notification_test(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    ctx.events.emit(crate::Event::NotificationTestRequested);
    Ok(ToolOutput::message("Sent a test notification. If none appeared, check the system's notification settings for this app."))
}

fn grant_list(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let list: Vec<Value> = ctx.state.app.mcp_grants.iter().map(|g| json!({"client": g.client, "tool": g.tool})).collect();
    Ok(ToolOutput::new(format!("{} grants", list.len()), json!(list)))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RevokeParams {
    /// The client name, as `grant_list` shows it.
    client: String,
    /// One tool id; omit to revoke every grant this client has.
    #[serde(default)]
    tool: Option<String>,
}

fn grant_revoke(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: RevokeParams = parse_params(params)?;
    let before = ctx.state.app.mcp_grants.len();
    ctx.state.app.mcp_grants.retain(|g| !(g.client == p.client && p.tool.as_ref().is_none_or(|t| &g.tool == t)));
    let removed = before - ctx.state.app.mcp_grants.len();
    if removed > 0 {
        ctx.state.persist();
    }
    let what = p.tool.as_deref().map_or_else(|| "any tool".to_string(), |t| format!("`{t}`"));
    let msg = if removed == 0 { format!("{} had no grant for {what}.", p.client) } else { format!("{} asks again before calling {what}.", p.client) };
    Ok(ToolOutput::new(msg, json!({"client": p.client, "tool": p.tool, "removed": removed})))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ToolsetParams {
    /// `groups`, `layout`, `runs`, `git`, `notes`, `search` or `app`.
    toolset: String,
}

fn toolset_switch(_ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ToolsetParams = parse_params(params)?;
    let t = Toolset::from_id(&p.toolset).ok_or_else(|| ToolError::InvalidParams(format!("no toolset `{}`", p.toolset)))?;
    // The MCP layer keeps the per-client set; the app and CLI always see every tool.
    Ok(ToolOutput::new(format!("{} toolset noted.", t.title()), json!({"toolset": t.id()})))
}

#[cfg(test)]
mod tests {
    use super::pascal;

    #[test]
    fn keys() {
        assert_eq!(pascal("use_bundled_con_pty"), "UseBundledConPty");
        assert_eq!(pascal("ShowGitBranch"), "ShowGitBranch");
        assert_eq!(pascal("theme"), "Theme");
    }
}
