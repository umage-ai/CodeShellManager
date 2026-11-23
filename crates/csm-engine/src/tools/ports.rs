//! SSH port forwards as tools: `session_port_forward` starts one, `session_port_forwards` lists
//! them, `session_port_unforward` stops them. Forwards belong to SSH sessions and live only as
//! long as the session's process (`port_forward.rs` has the I/O; `csm-services::ssh_ports` the
//! rules). Nothing here is persisted.

use csm_core::models::{SessionKind, ShellSession};
use csm_services::ssh_ports::check_port;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::engine::{Ctx, Reply};
use crate::port_forward;
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

use super::find;

pub(super) fn specs() -> Vec<ToolSpec> {
    let t = |id, title, summary, effects, input_schema, run| ToolSpec { id, title, summary, toolset: Toolset::Core, effects, input_schema, run };
    // Starts an ssh process that holds a connection open, and may open a browser.
    let forward_effects = Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK | Effects::MUTATES_STATE;
    vec![
        t(
            "session_port_forward",
            "Forward a port from an SSH host",
            "Start an SSH port forward: a port on an SSH session's remote host (its loopback) answers at 127.0.0.1 on this computer, until the session closes, restarts or the app quits. The local port is the remote one when free, else a free one (or `local_port`). `open: true` opens the address. Refused for local and WSL sessions.",
            forward_effects,
            schema_of::<ForwardParams>,
            forward,
        ),
        t(
            "session_port_forwards",
            "Port forwards",
            "The running SSH port forwards: session, remote port, local port, the ssh process and its address. With `session_id`, only that session's.",
            Effects::READ_ONLY,
            schema_of::<ListParams>,
            list,
        ),
        t(
            "session_port_unforward",
            "Stop port forwards",
            "Stop an SSH session's port forward on `local_port`, or all of them with `all: true`.",
            Effects::MUTATES_STATE,
            schema_of::<UnforwardParams>,
            unforward,
        ),
    ]
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForwardParams {
    /// The SSH session whose remote host has the port.
    session_id: String,
    /// The port on the remote host's loopback, 1 to 65535.
    remote_port: i64,
    /// The port on this computer, 1 to 65535; it must be free. Left out: the remote port when
    /// free here, else a free port.
    #[serde(default)]
    local_port: Option<i64>,
    /// Open the forwarded address in the browser once it is up.
    #[serde(default)]
    open: bool,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ListParams {
    /// Only this session's forwards.
    #[serde(default)]
    session_id: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct UnforwardParams {
    /// The SSH session the forwards belong to.
    session_id: String,
    /// The forward to stop, by its local port on this computer.
    #[serde(default)]
    local_port: Option<i64>,
    /// Stop every forward of the session (instead of `local_port`).
    #[serde(default)]
    all: bool,
}

/// The session, when it is an SSH session (the only kind with a remote host to forward from).
fn ssh_session(ctx: &Ctx<'_>, id: &str) -> Result<ShellSession, ToolError> {
    let i = find(ctx.state, id)?;
    let s = ctx.state.app.sessions.get(i).cloned().ok_or_else(|| ToolError::NotFound(format!("no session `{id}`")))?;
    if s.kind != SessionKind::Ssh {
        let what = if s.kind == SessionKind::Wsl { "a WSL session" } else { "a local session" };
        return Err(ToolError::Refused(format!("port forwards run on SSH sessions; `{}` is {what}, whose ports are on this computer", s.name)));
    }
    Ok(s)
}

fn forward(ctx: &mut Ctx<'_>, params: serde_json::Value) -> Reply {
    let p: ForwardParams = parse_params(params)?;
    let remote = check_port(p.remote_port).map_err(ToolError::InvalidParams)?;
    let requested = p.local_port.map(check_port).transpose().map_err(ToolError::InvalidParams)?;
    let session = ssh_session(ctx, &p.session_id)?;
    port_forward::start_for_session(ctx, &session, remote, requested, p.open)
}

fn list(ctx: &mut Ctx<'_>, params: serde_json::Value) -> Reply {
    let p: ListParams = parse_params(params)?;
    if let Some(id) = p.session_id.as_deref() {
        find(ctx.state, id)?;
    }
    port_forward::prune(&mut ctx.state.rt.forwards);
    let rows: Vec<_> = port_forward::live_views(&ctx.state.rt.forwards)
        .into_iter()
        .filter(|v| p.session_id.as_deref().is_none_or(|id| id == v.session_id))
        .collect();
    let n = rows.len();
    let noun = if n == 1 { "port forward" } else { "port forwards" };
    Ok(ToolOutput::new(format!("{n} {noun}."), json!({"forwards": rows})))
}

fn unforward(ctx: &mut Ctx<'_>, params: serde_json::Value) -> Reply {
    let p: UnforwardParams = parse_params(params)?;
    find(ctx.state, &p.session_id)?;
    let local = p.local_port.map(check_port).transpose().map_err(ToolError::InvalidParams)?;
    let removed = match (local, p.all) {
        (Some(_), true) | (None, false) => {
            return Err(ToolError::InvalidParams("pass local_port, or all: true to stop every forward of the session".into()));
        }
        (Some(port), false) => {
            let owned = ctx.state.rt.forwards.iter().any(|f| f.session_id == p.session_id && f.local_port == port);
            if !owned {
                return Err(ToolError::NotFound(format!("no port forward of this session on local port {port}; session_port_forwards lists them")));
            }
            port_forward::stop_matching(&mut ctx.state.rt.forwards, &p.session_id, Some(port))
        }
        (None, true) => port_forward::stop_matching(&mut ctx.state.rt.forwards, &p.session_id, None),
    };
    if removed > 0 {
        ctx.state.touch();
    }
    let noun = if removed == 1 { "port forward" } else { "port forwards" };
    Ok(ToolOutput::new(format!("Stopped {removed} {noun}."), json!({"removed": removed})))
}
