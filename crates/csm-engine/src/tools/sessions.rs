//! `core` tools: sessions and the inbox.

use std::time::{Duration, Instant};

use csm_core::models::{RunCommandItem, SessionKind, SessionStatus, ShellSession};
use csm_core::program_status_records::ProgramStatusRecords;
use csm_core::time::Timestamp;
use csm_term::input::{self, Untrusted};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{clean_name, current, find, find_group};
use crate::engine::{Ctx, Reply};
use crate::lifecycle::{self, new_id};
use crate::session::{Phase, now_ms};
use crate::snapshot;
use crate::state::Wait;
use crate::status::{Status, WaitUntil};
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

/// Longest text `session_send_text` accepts (1 MiB).
const MAX_SEND_BYTES: usize = 1 << 20;
const MAX_NAME: usize = 200;
/// `session_wait` upper bound.
const MAX_WAIT_S: u64 = 600;

pub(super) fn specs() -> Vec<ToolSpec> {
    let state_launch = Effects::MUTATES_STATE.union(Effects::LAUNCHES_PROCESS);
    vec![
        ToolSpec {
            id: "session_list",
            title: "List sessions",
            summary: "Every session in sidebar order: id, name, kind, folder, group, status (working, needs_you_approval, needs_you_question, done_unseen, error, idle, asleep, starting, exited), branch.",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<ListParams>,
            run: session_list,
        },
        ToolSpec {
            id: "session_get",
            title: "Session details",
            summary: "One session in full: its launch settings and its status, including the program-status message.",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<IdParams>,
            run: session_get,
        },
        ToolSpec {
            id: "session_new",
            title: "New session…",
            summary: "Open a session: a folder and a command (default: the configured one, usually claude), locally, over SSH or in WSL. It starts when its pane is measured, or after 1.5 s headless.",
            toolset: Toolset::Core,
            effects: state_launch,
            input_schema: schema_of::<NewParams>,
            run: session_new,
        },
        ToolSpec {
            id: "session_focus",
            title: "Focus session",
            summary: "Bring a session to the front (switches the rail tab to its group, wakes it when asleep) and mark its results seen.",
            toolset: Toolset::Core,
            effects: Effects::UI.union(Effects::LAUNCHES_PROCESS),
            input_schema: schema_of::<IdParams>,
            run: session_focus,
        },
        ToolSpec {
            id: "session_rename",
            title: "Rename session…",
            summary: "Give a session a new name.",
            toolset: Toolset::Core,
            effects: Effects::MUTATES_STATE,
            input_schema: schema_of::<RenameParams>,
            run: session_rename,
        },
        ToolSpec {
            id: "session_edit",
            title: "Edit session…",
            summary: "Change a session's folder, command, arguments, kind or connection. Changes that need a new process apply at the next start, or now with restart: true.",
            toolset: Toolset::Core,
            effects: state_launch.union(Effects::MAY_DISCARD_DATA),
            input_schema: schema_of::<EditParams>,
            run: session_edit,
        },
        ToolSpec {
            id: "session_close",
            title: "Close session",
            summary: "End a session's process and remove it (it goes to Recently closed). Refuses a busy session unless force is true.",
            toolset: Toolset::Core,
            effects: Effects::MAY_DISCARD_DATA.union(Effects::MUTATES_STATE),
            input_schema: schema_of::<CloseParams>,
            run: session_close,
        },
        ToolSpec {
            id: "session_sleep",
            title: "Sleep session",
            summary: "End a session's process but keep it in the sidebar, with its settings and place, to wake later.",
            toolset: Toolset::Core,
            effects: Effects::MAY_DISCARD_DATA.union(Effects::MUTATES_STATE),
            input_schema: schema_of::<IdParams>,
            run: session_sleep,
        },
        ToolSpec {
            id: "session_wake",
            title: "Wake session",
            summary: "Start a sleeping session again (a Claude session resumes its conversation).",
            toolset: Toolset::Core,
            effects: state_launch,
            input_schema: schema_of::<IdParams>,
            run: session_wake,
        },
        ToolSpec {
            id: "session_restart",
            title: "Restart session",
            summary: "Close and reopen a session in place (to pick up a new CLI build). Waits for Claude to exit first; refused while sessions are being restored or another restart runs. Replies when it has started again.",
            toolset: Toolset::Core,
            effects: Effects::MAY_DISCARD_DATA.union(Effects::LAUNCHES_PROCESS),
            input_schema: schema_of::<IdParams>,
            run: session_restart,
        },
        ToolSpec {
            id: "session_reopen_closed",
            title: "Reopen closed session",
            summary: "Reopen a recently closed session (index 0 = the last one closed).",
            toolset: Toolset::Core,
            effects: state_launch,
            input_schema: schema_of::<ReopenParams>,
            run: session_reopen_closed,
        },
        ToolSpec {
            id: "session_send_text",
            title: "Send text to session",
            summary: "Paste text into a session (bracketed paste, never typed); submit: true presses Enter after it. Refused into the caller's own session, and while the session is working unless force is true.",
            toolset: Toolset::Core,
            effects: Effects::SENDS_INPUT,
            input_schema: schema_of::<SendParams>,
            run: session_send_text,
        },
        ToolSpec {
            id: "session_read_screen",
            title: "Read screen",
            summary: "The session's visible screen as plain text.",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY.union(Effects::READS_SESSION_CONTENT),
            input_schema: schema_of::<IdParams>,
            run: session_read_screen,
        },
        ToolSpec {
            id: "session_read_output",
            title: "Read output",
            summary: "The last lines (default 200, at most 2000) of a session's scrollback and screen, as plain text.",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY.union(Effects::READS_SESSION_CONTENT),
            input_schema: schema_of::<ReadOutputParams>,
            run: session_read_output,
        },
        ToolSpec {
            id: "session_wait",
            title: "Wait for session",
            summary: "Wait until a session is idle, done, needs you, has exited, or changes status at all (until), up to timeout_s (default 60, max 600).",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<WaitParams>,
            run: session_wait,
        },
        ToolSpec {
            id: "session_status_clear",
            title: "Clear status",
            summary: "Forget a session's reported status (for a status stuck after a program died mid-turn); the screen heuristic takes over again.",
            toolset: Toolset::Core,
            effects: Effects::UI,
            input_schema: schema_of::<IdParams>,
            run: session_status_clear,
        },
        ToolSpec {
            id: "inbox_list",
            title: "Needs you",
            summary: "Sessions waiting for you (oldest first), then finished or failed turns you have not seen.",
            toolset: Toolset::Core,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: inbox_list,
        },
        ToolSpec {
            id: "inbox_next",
            title: "Next that needs you",
            summary: "Focus the next session in the inbox.",
            toolset: Toolset::Core,
            effects: Effects::UI,
            input_schema: empty_schema,
            run: inbox_next,
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdParams {
    session_id: String,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ListParams {
    /// Only this group (`""` = ungrouped).
    group: Option<String>,
    /// Only this status (see `session_list`'s summary for the values).
    status: Option<String>,
    /// Include sleeping sessions (default true).
    include_asleep: Option<bool>,
}

fn session_list(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ListParams = parse_params(params)?;
    let status = match p.status.as_deref() {
        None => None,
        Some(s) => Some(Status::from_id(s).ok_or_else(|| ToolError::InvalidParams(format!("no status `{s}`")))?),
    };
    let snap = current(ctx);
    let list: Vec<&snapshot::SessionView> = snap
        .sessions
        .iter()
        .filter(|s| p.group.as_ref().is_none_or(|g| &s.group_id == g))
        .filter(|s| status.is_none_or(|st| s.status == st))
        .filter(|s| p.include_asleep.unwrap_or(true) || !s.asleep)
        .collect();
    Ok(ToolOutput::new(format!("{} sessions", list.len()), json!(list)))
}

fn session_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    let i = find(ctx.state, &p.session_id)?;
    let snap = current(ctx);
    let view = snap.session(&p.session_id).cloned();
    let Some(s) = ctx.state.app.sessions.get(i) else { return Err(ToolError::NotFound("gone".into())) };
    let rt = ctx.state.rt.sessions.get(&p.session_id);
    let program = rt.map(|r| {
        let sum = r.records.summarize();
        json!({
            "records": r.records.count(),
            "state": sum.state.map(|s| format!("{s:?}").to_lowercase()),
            "kind": format!("{:?}", sum.kind).to_lowercase(),
            "progress": sum.progress,
            "message": sum.message,
            "app": sum.app,
        })
    });
    let busy = lifecycle::busy_reason(ctx.state, &p.session_id);
    let data = json!({
        "session": view,
        "config": {
            "command": s.command, "args": s.args, "folder": s.working_folder,
            "kind": view.as_ref().map(|v| v.kind),
            "ssh": {"user": s.ssh_user, "host": s.ssh_host, "port": s.ssh_port, "remote_folder": s.ssh_remote_folder},
            "wsl": {"distro": s.wsl_distro, "user": s.wsl_user, "folder": s.wsl_working_folder},
            "color_override": s.color_override,
            "created_at": s.created_at.as_str(),
        },
        "program_status": program,
        "busy": busy,
        "cwd": rt.and_then(|r| r.cwd.clone()),
    });
    let name = view.as_ref().map(|v| v.name.clone()).unwrap_or_default();
    Ok(ToolOutput::new(format!("{name}: {}", view.as_ref().map_or("?", |v| v.status.label())), data))
}

#[derive(Deserialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum KindParam {
    Local,
    Ssh,
    Wsl,
}

#[derive(Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
struct SshParams {
    host: String,
    #[serde(default)]
    user: Option<String>,
    /// Default 22.
    #[serde(default)]
    port: Option<i32>,
    /// Folder on the remote host to start in.
    #[serde(default)]
    remote_folder: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Clone)]
#[serde(deny_unknown_fields)]
struct WslParams {
    /// As `wsl -l -q` lists it, e.g. `Ubuntu`.
    distro: String,
    #[serde(default)]
    user: Option<String>,
    /// Linux folder; empty = the user's home.
    #[serde(default)]
    folder: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct NewParams {
    /// Local folder to start in (local sessions). Default: the configured default folder.
    folder: Option<String>,
    /// Program to run (default: the configured default command, usually `claude`).
    command: Option<String>,
    /// Arguments, as typed on a command line.
    args: Option<String>,
    name: Option<String>,
    /// Group id to put it in.
    group: Option<String>,
    /// Inferred from `ssh` / `wsl` when omitted.
    kind: Option<KindParam>,
    ssh: Option<SshParams>,
    wsl: Option<WslParams>,
    /// Focus it (default: true from the app, false for MCP clients).
    focus: Option<bool>,
    /// Place it right after this session in the sidebar.
    after: Option<String>,
}

fn validate_port(port: Option<i32>) -> Result<i32, ToolError> {
    match port {
        None => Ok(22),
        Some(p) if (1..=65535).contains(&p) => Ok(p),
        Some(p) => Err(ToolError::InvalidParams(format!("port {p} is not between 1 and 65535"))),
    }
}

fn no_controls(field: &str, v: &str) -> Result<(), ToolError> {
    if v.chars().any(char::is_control) {
        return Err(ToolError::InvalidParams(format!("`{field}` contains control characters")));
    }
    Ok(())
}

/// Applies the connection fields (kind, ssh, wsl, folder) to `s`, keeping the WSL UNC mirror in
/// step with the Linux folder.
fn apply_connection(
    s: &mut ShellSession,
    kind: KindParam,
    folder: Option<String>,
    ssh: Option<SshParams>,
    wsl: Option<WslParams>,
) -> Result<(), ToolError> {
    match kind {
        KindParam::Local => {
            s.kind = SessionKind::Local;
            if let Some(f) = folder {
                no_controls("folder", &f)?;
                s.working_folder = f.trim().to_owned();
            }
        }
        KindParam::Ssh => {
            s.kind = SessionKind::Ssh;
            if let Some(x) = ssh {
                for (k, v) in [("ssh.host", &x.host), ("ssh.user", &x.user.clone().unwrap_or_default())] {
                    no_controls(k, v)?;
                    if v.chars().any(char::is_whitespace) {
                        return Err(ToolError::InvalidParams(format!("`{k}` may not contain spaces")));
                    }
                }
                s.ssh_host = x.host.trim().to_owned();
                s.ssh_user = x.user.unwrap_or_default().trim().to_owned();
                s.ssh_port = validate_port(x.port)?;
                let rf = x.remote_folder.unwrap_or_default();
                no_controls("ssh.remote_folder", &rf)?;
                s.ssh_remote_folder = rf;
                s.working_folder.clear();
            }
            if s.ssh_host.trim().is_empty() {
                return Err(ToolError::InvalidParams("an SSH session needs `ssh.host`".into()));
            }
        }
        KindParam::Wsl => {
            if !cfg!(windows) {
                return Err(ToolError::InvalidParams("WSL sessions run on Windows only".into()));
            }
            s.kind = SessionKind::Wsl;
            if let Some(x) = wsl {
                for (k, v) in [("wsl.distro", &x.distro), ("wsl.user", &x.user.clone().unwrap_or_default())] {
                    no_controls(k, v)?;
                    if v.chars().any(char::is_whitespace) || v.starts_with('-') {
                        return Err(ToolError::InvalidParams(format!("`{k}` is not a valid name")));
                    }
                }
                s.wsl_distro = x.distro.trim().to_owned();
                s.wsl_user = x.user.unwrap_or_default().trim().to_owned();
                let f = x.folder.unwrap_or_default();
                no_controls("wsl.folder", &f)?;
                s.wsl_working_folder = f.trim().to_owned();
            }
            if s.wsl_distro.is_empty() {
                return Err(ToolError::InvalidParams("a WSL session needs `wsl.distro`".into()));
            }
            s.working_folder = csm_services::wsl::to_unc_path(&s.wsl_distro, &s.wsl_working_folder);
        }
    }
    Ok(())
}

fn session_new(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NewParams = parse_params(params)?;
    if ctx.state.rt.shutting_down {
        return Err(ToolError::Refused("the app is closing".into()));
    }
    let kind = p.kind.unwrap_or(if p.ssh.is_some() {
        KindParam::Ssh
    } else if p.wsl.is_some() {
        KindParam::Wsl
    } else {
        KindParam::Local
    });
    if let Some(g) = &p.group {
        find_group(ctx.state, g)?;
    }
    if let Some(a) = &p.after {
        find(ctx.state, a)?;
    }
    let settings = &ctx.state.app.settings;
    let command = p.command.clone().filter(|c| !c.trim().is_empty()).unwrap_or_else(|| settings.default_command.clone());
    no_controls("command", &command)?;
    let args = p.args.clone().unwrap_or_default();
    no_controls("args", &args)?;
    let folder = match kind {
        KindParam::Local => {
            let f = p.folder.clone().filter(|f| !f.trim().is_empty()).unwrap_or_else(|| settings.default_working_folder.clone());
            if f.trim().is_empty() {
                return Err(ToolError::InvalidParams("a local session needs `folder`".into()));
            }
            Some(f)
        }
        _ => None,
    };
    let now = Timestamp::from_epoch_ms(now_ms());
    let mut s = ShellSession {
        id: new_id(),
        command: command.trim().to_owned(),
        args: args.trim().to_owned(),
        group_id: p.group.clone().unwrap_or_default(),
        status: SessionStatus::Running,
        created_at: now.clone(),
        last_activity_at: now,
        ..Default::default()
    };
    apply_connection(&mut s, kind, folder, p.ssh, p.wsl)?;
    s.name = match &p.name {
        Some(n) if !clean_name(n, MAX_NAME).is_empty() => clean_name(n, MAX_NAME),
        _ => s.default_display_name(),
    };
    let id = s.id.clone();
    let at = p.after.as_deref().and_then(|a| lifecycle::session_index(ctx.state, a)).map(|i| i + 1);
    match at {
        Some(i) if i <= ctx.state.app.sessions.len() => ctx.state.app.sessions.insert(i, s),
        _ => ctx.state.app.sessions.push(s),
    }
    if let Err(msg) = lifecycle::launch(ctx, &id, false) {
        if let Some(i) = lifecycle::session_index(ctx.state, &id) {
            ctx.state.app.sessions.remove(i);
        }
        return Err(ToolError::Failed(format!("The session could not be started: {msg}")));
    }
    if p.focus.unwrap_or(!ctx.caller.is_external()) {
        lifecycle::focus(ctx, &id)?;
    }
    ctx.state.persist();
    let view = snapshot::session_view(ctx.state, ctx.state.app.sessions.iter().find(|x| x.id == id).ok_or_else(|| ToolError::Failed("lost".into()))?);
    Ok(ToolOutput::new(format!("Starting {} in {}.", view.command, view.name), json!(view)))
}

fn session_focus(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    find(ctx.state, &p.session_id)?;
    lifecycle::focus(ctx, &p.session_id)
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RenameParams {
    session_id: String,
    name: String,
}

fn session_rename(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: RenameParams = parse_params(params)?;
    let i = find(ctx.state, &p.session_id)?;
    let name = clean_name(&p.name, MAX_NAME);
    if name.is_empty() {
        return Err(ToolError::InvalidParams("the name is empty".into()));
    }
    if let Some(s) = ctx.state.app.sessions.get_mut(i) {
        s.name = name.clone();
    }
    ctx.state.persist();
    Ok(ToolOutput::new(format!("Renamed to {name}."), json!({"session_id": p.session_id, "name": name})))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditParams {
    session_id: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Option<String>,
    #[serde(default)]
    kind: Option<KindParam>,
    #[serde(default)]
    ssh: Option<SshParams>,
    #[serde(default)]
    wsl: Option<WslParams>,
    /// Restart now when the change needs a new process (default false: it applies at the next
    /// start).
    #[serde(default)]
    restart: bool,
}

fn launch_fields(s: &ShellSession) -> (SessionKind, String, String, String, String, String, i32, String, String, String, String) {
    (
        s.kind,
        s.command.clone(),
        s.args.clone(),
        s.working_folder.trim_end_matches(['/', '\\']).to_lowercase(),
        s.ssh_user.clone(),
        s.ssh_host.clone(),
        s.ssh_port,
        s.ssh_remote_folder.clone(),
        s.wsl_distro.clone(),
        s.wsl_user.clone(),
        s.wsl_working_folder.clone(),
    )
}

fn session_edit(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: EditParams = parse_params(params)?;
    let i = find(ctx.state, &p.session_id)?;
    let Some(old) = ctx.state.app.sessions.get(i).cloned() else { return Err(ToolError::NotFound("gone".into())) };
    let mut s = old.clone();
    if let Some(c) = &p.command {
        no_controls("command", c)?;
        if c.trim().is_empty() {
            return Err(ToolError::InvalidParams("the command is empty".into()));
        }
        s.command = c.trim().to_owned();
    }
    if let Some(a) = &p.args {
        no_controls("args", a)?;
        s.args = a.trim().to_owned();
    }
    let kind = p.kind.unwrap_or(match (s.kind, &p.ssh, &p.wsl) {
        (_, Some(_), _) => KindParam::Ssh,
        (_, _, Some(_)) => KindParam::Wsl,
        (SessionKind::Local, ..) => KindParam::Local,
        (SessionKind::Ssh, ..) => KindParam::Ssh,
        (SessionKind::Wsl, ..) => KindParam::Wsl,
    });
    apply_connection(&mut s, kind, p.folder.clone(), p.ssh.clone(), p.wsl.clone())?;
    if let Some(n) = &p.name {
        let n = clean_name(n, MAX_NAME);
        if !n.is_empty() {
            s.name = n;
        }
    }
    let relaunch = launch_fields(&old) != launch_fields(&s);
    let any = s != old;
    let name = s.name.clone();
    if let Some(slot) = ctx.state.app.sessions.get_mut(i) {
        *slot = s;
    }
    ctx.state.persist();
    let live = !old.is_dormant && ctx.state.rt.sessions.contains_key(&p.session_id);
    let data = json!({"session_id": p.session_id, "changed": any, "requires_restart": relaunch && live});
    if relaunch && live && p.restart {
        if let Some(err) = lifecycle::restart_refusal(ctx.state) {
            return Err(ToolError::Refused(format!("Saved, but not restarted: {err}")));
        }
        let reply = ctx.park();
        lifecycle::restart_many(ctx, vec![p.session_id.clone()], reply);
        return Ok(ToolOutput::new("Saved; restarting.", data));
    }
    let message = match (any, relaunch && live) {
        (false, _) => format!("Nothing changed in {name}."),
        (true, true) => format!("Saved {name}. The change applies when it restarts (pass restart: true to do it now)."),
        (true, false) => format!("Saved {name}."),
    };
    Ok(ToolOutput::new(message, data))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CloseParams {
    session_id: String,
    /// Close even when it is busy (working, waiting mid-task, or printing).
    #[serde(default)]
    force: bool,
}

fn session_close(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: CloseParams = parse_params(params)?;
    lifecycle::close_session(ctx, &p.session_id, p.force)
}

fn session_sleep(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    lifecycle::sleep_session(ctx, &p.session_id)
}

fn session_wake(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    lifecycle::wake_session(ctx, &p.session_id, None)
}

fn session_restart(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    let i = find(ctx.state, &p.session_id)?;
    if ctx.state.app.sessions.get(i).is_some_and(|s| s.is_dormant) {
        return Err(ToolError::Refused("the session is asleep; wake it instead".into()));
    }
    if let Some(err) = lifecycle::restart_refusal(ctx.state) {
        return Err(err);
    }
    let reply = ctx.park();
    lifecycle::restart_many(ctx, vec![p.session_id], reply);
    Ok(ToolOutput::message("Restarting."))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ReopenParams {
    /// Position in Recently closed; 0 (default) is the newest.
    index: usize,
}

fn session_reopen_closed(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ReopenParams = parse_params(params)?;
    if ctx.state.rt.shutting_down {
        return Err(ToolError::Refused("the app is closing".into()));
    }
    if p.index >= ctx.state.app.recently_closed.len() {
        return Err(ToolError::NotFound(format!(
            "nothing at position {} in Recently closed ({} entries)",
            p.index,
            ctx.state.app.recently_closed.len()
        )));
    }
    let e = ctx.state.app.recently_closed.remove(p.index);
    let now = Timestamp::from_epoch_ms(now_ms());
    let group = if ctx.state.app.groups.iter().any(|g| g.id == e.group_id) { e.group_id.clone() } else { String::new() };
    let s = ShellSession {
        id: new_id(),
        name: e.name.clone(),
        working_folder: e.working_folder.clone(),
        command: e.command.clone(),
        args: e.args.clone(),
        group_id: group,
        color_override: e.color_override.clone(),
        status: SessionStatus::Running,
        created_at: now.clone(),
        last_activity_at: now,
        kind: e.kind,
        ssh_user: e.ssh_user.clone(),
        ssh_host: e.ssh_host.clone(),
        ssh_port: e.ssh_port,
        ssh_remote_folder: e.ssh_remote_folder.clone(),
        wsl_distro: e.wsl_distro.clone(),
        wsl_user: e.wsl_user.clone(),
        wsl_working_folder: e.wsl_working_folder.clone(),
        profile_font_family: e.profile_font_family.clone(),
        profile_font_size: e.profile_font_size,
        profile_font_weight: e.profile_font_weight.clone(),
        profile_font_ligatures: e.profile_font_ligatures,
        profile_cursor_shape: e.profile_cursor_shape.clone(),
        profile_cursor_blink: e.profile_cursor_blink,
        profile_padding: e.profile_padding.clone(),
        profile_background_opacity: e.profile_background_opacity,
        profile_retro_effect: e.profile_retro_effect,
        profile_color_scheme_json: e.profile_color_scheme_json.clone(),
        run_commands: e
            .run_commands
            .iter()
            .map(|r| RunCommandItem { id: new_id(), ..r.clone() })
            .collect(),
        ..Default::default()
    };
    let id = s.id.clone();
    ctx.state.app.sessions.push(s);
    if let Err(msg) = lifecycle::launch(ctx, &id, false) {
        // Keep it, asleep and editable, rather than losing the entry.
        if let Some(i) = lifecycle::session_index(ctx.state, &id)
            && let Some(s) = ctx.state.app.sessions.get_mut(i)
        {
            s.is_dormant = true;
        }
        ctx.state.persist();
        return Err(ToolError::Failed(format!("Reopened, but it could not start: {msg}. It is asleep.")));
    }
    if !ctx.caller.is_external() {
        lifecycle::focus(ctx, &id)?;
    }
    ctx.state.persist();
    Ok(ToolOutput::new(format!("Reopened {}.", e.name), json!({"session_id": id})))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SendParams {
    session_id: String,
    /// Pasted as-is (bracketed paste); control characters other than tab and newline are removed.
    text: String,
    /// Press Enter after the text (sent separately).
    #[serde(default)]
    submit: bool,
    /// Send even while the session is working mid-turn.
    #[serde(default)]
    force: bool,
}

fn session_send_text(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SendParams = parse_params(params)?;
    if let Caller::External { session_id: Some(own), .. } = ctx.caller
        && *own == p.session_id
    {
        return Err(ToolError::Refused("a session cannot send input to itself".into()));
    }
    if p.text.len() > MAX_SEND_BYTES {
        return Err(ToolError::InvalidParams("the text is longer than 1 MiB".into()));
    }
    let i = find(ctx.state, &p.session_id)?;
    let asleep = ctx.state.app.sessions.get(i).is_some_and(|s| s.is_dormant);
    let Some(rt) = ctx.state.rt.sessions.get(&p.session_id).filter(|_| !asleep) else {
        return Err(ToolError::Refused("the session is asleep; wake it first".into()));
    };
    match rt.phase {
        Phase::Starting => {
            return Err(ToolError::Refused(
                "the session is still starting; wait for it (session_wait until idle) and try again".into(),
            ));
        }
        Phase::Exited { .. } => return Err(ToolError::Refused("the session's process has exited; restart it first".into())),
        Phase::Live => {}
    }
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound("gone".into()))?;
    let view = snapshot::session_view(ctx.state, s);
    if view.status.is_mid_turn() && !p.force {
        return Err(ToolError::Refused(format!(
            "{} is working mid-turn; input now would land in the middle of it. Wait (session_wait until idle) or pass force: true.",
            view.name
        )));
    }
    let io = rt.io.clone();
    let bytes = input::paste(Untrusted(&p.text), io.mode());
    if !io.write(&bytes) {
        return Err(ToolError::Failed("the session is not accepting input".into()));
    }
    if p.submit {
        // Enter is its own write, after the paste, so it submits rather than becoming pasted text.
        let spawned = std::thread::Builder::new().name("csm-submit".into()).spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            io.write(b"\r");
        });
        if spawned.is_err() {
            return Err(ToolError::Failed("could not send Enter".into()));
        }
    }
    Ok(ToolOutput::new(
        format!("Sent {} characters to {}{}.", p.text.chars().count(), view.name, if p.submit { " and pressed Enter" } else { "" }),
        json!({"session_id": p.session_id}),
    ))
}

fn terminal_of(ctx: &Ctx<'_>, id: &str) -> Result<std::sync::Arc<crate::session::SessionIo>, ToolError> {
    find(ctx.state, id)?;
    ctx.state
        .rt
        .sessions
        .get(id)
        .map(|rt| rt.io.clone())
        .ok_or_else(|| ToolError::Refused("the session is asleep; there is no screen to read".into()))
}

/// Reads the terminal on a worker (it may wait for the PTY reader) and answers from there.
fn read_on_worker(ctx: &mut Ctx<'_>, id: &str, f: impl FnOnce(&mut csm_term::Terminal) -> String + Send + 'static) -> Reply {
    let io = terminal_of(ctx, id)?;
    let Some(reply) = ctx.park() else {
        let text = f(&mut io.lock_terminal());
        return Ok(ToolOutput::new(format!("{} lines", text.lines().count()), json!({"text": text})));
    };
    let sid = id.to_owned();
    let spawned = std::thread::Builder::new().name("csm-read".into()).spawn(move || {
        let text = f(&mut io.lock_terminal());
        reply.send(Ok(ToolOutput::new(format!("{} lines", text.lines().count()), json!({"session_id": sid, "text": text}))));
    });
    match spawned {
        Ok(_) => Ok(ToolOutput::message("reading")),
        Err(e) => Err(ToolError::Failed(format!("could not read the terminal: {e}"))),
    }
}

fn session_read_screen(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    read_on_worker(ctx, &p.session_id, crate::screen::screen)
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ReadOutputParams {
    session_id: String,
    /// How many lines from the end (default 200, at most 2000).
    #[serde(default)]
    lines: Option<usize>,
}

fn session_read_output(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ReadOutputParams = parse_params(params)?;
    let n = p.lines.unwrap_or(200);
    if n > crate::screen::MAX_OUTPUT_LINES {
        return Err(ToolError::InvalidParams(format!("at most {} lines", crate::screen::MAX_OUTPUT_LINES)));
    }
    read_on_worker(ctx, &p.session_id, move |t| crate::screen::tail(t, n))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WaitParams {
    session_id: String,
    until: WaitUntil,
    /// Seconds (default 60, at most 600).
    #[serde(default)]
    timeout_s: Option<f64>,
}

fn session_wait(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: WaitParams = parse_params(params)?;
    let i = find(ctx.state, &p.session_id)?;
    let secs = p.timeout_s.unwrap_or(60.0);
    if !secs.is_finite() || secs < 0.0 || secs > MAX_WAIT_S as f64 {
        return Err(ToolError::InvalidParams(format!("timeout_s must be between 0 and {MAX_WAIT_S}")));
    }
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound("gone".into()))?;
    let view = snapshot::session_view(ctx.state, s);
    if p.until != WaitUntil::AnyChange && p.until.reached(view.status, view.status) {
        return Ok(lifecycle::wait_output(&view));
    }
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    ctx.state.rt.waits.push(Wait {
        session_id: p.session_id,
        until: p.until,
        deadline: Instant::now() + Duration::from_secs_f64(secs),
        reply,
        start: view.status,
    });
    // The snapshot may already show a newer status than the one we compared with.
    ctx.state.touch();
    Ok(ToolOutput::message("waiting"))
}

fn session_status_clear(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    find(ctx.state, &p.session_id)?;
    if let Some(rt) = ctx.state.rt.sessions.get_mut(&p.session_id) {
        rt.records = ProgramStatusRecords::new();
        rt.heuristic = None;
        rt.heuristic_message = None;
        rt.rearm();
    }
    ctx.state.touch();
    Ok(ToolOutput::new("Status cleared.", json!({"session_id": p.session_id})))
}

fn inbox_list(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let snap = current(ctx);
    let items: Vec<&snapshot::SessionView> = snap.inbox.iter().filter_map(|id| snap.session(id)).collect();
    let waiting = items.iter().filter(|s| s.status.needs_you()).count();
    Ok(ToolOutput::new(format!("{waiting} need you, {} unseen results", items.len() - waiting), json!(items)))
}

fn inbox_next(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let snap = current(ctx);
    let next = snap
        .inbox
        .iter()
        .find(|id| snap.focused.as_ref() != Some(*id))
        .or_else(|| snap.inbox.first())
        .cloned();
    match next {
        Some(id) => lifecycle::focus(ctx, &id),
        None => Ok(ToolOutput::message("Nothing needs you.")),
    }
}
