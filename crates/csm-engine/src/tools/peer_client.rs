//! Connecting this app to another CodeShellManager (`peer_client_*`, `peer_hosts_list`,
//! `peer_host_forget`): the laptop's half of pairing, and the list of hosts it is paired with.
//!
//! Pairing starts only in the app's own window: `peer_client_pair` is refused in [`precheck`] for
//! every other caller (the CLI, MCP clients, a paired peer), before any consent card, exactly like
//! `peer_pair_start`. Reading the progress and cancelling are allowed for everyone. The tools only
//! decide who may ask; the key, the host list and the dialling belong to the
//! [`PeerClientHost`](crate::peer_client::PeerClientHost) the app wired in (`csm-peer`).

use serde::Deserialize;
use serde_json::Value;

use crate::engine::{Ctx, Reply};
use crate::peer_client::PeerClientHost;
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

/// The tools only the person at the app's window may call.
pub const UI_ONLY: [&str; 3] = ["peer_client_pair", "peer_connect_open", "remote_window_open"];

/// Refused for a paired device (`Caller::Peer`) too: the pairing in progress (its check code, its
/// cancel), the office list and its removal are this app's own business, not a device's. An agent
/// or the CLI may still use them (the person's own tools; forgetting an office asks first).
pub const PEER_REFUSED: [&str; 4] = ["peer_client_pair_status", "peer_client_pair_cancel", "peer_hosts_list", "peer_host_forget"];

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "peer_connect_open",
            title: "Connect to another CodeShellManager…",
            summary: "Open the dialog that pairs this app with another CodeShellManager from its `csm://pair#...` link, shows the check code, and lists the paired offices with Connect and Forget. Only the app's own window opens it; refused for every other caller.",
            toolset: Toolset::App,
            effects: Effects::CHANGES_UI_STATE,
            input_schema: empty_schema,
            run: peer_connect_open,
        },
        ToolSpec {
            id: "remote_window_open",
            title: "Open a remote window",
            summary: "Start a window of this app that runs on a paired CodeShellManager (the office) instead of a local engine. It is a separate process with its own layout, focus and theme. Only the app's own window starts one; refused for every other caller.",
            toolset: Toolset::App,
            effects: Effects::CHANGES_UI_STATE.union(Effects::LAUNCHES_PROCESS),
            input_schema: schema_of::<RemoteWindowParams>,
            run: remote_window_open,
        },
        ToolSpec {
            id: "peer_client_pair",
            title: "Pair with a link",
            summary: "Pair this app with another CodeShellManager from its `csm://pair#...` link. It starts in the background and returns at once; `peer_client_pair_status` shows the four-digit check code to compare with the other screen, and the result once the other person has allowed or denied it. Refused for every caller except the app's own window.",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE.union(Effects::ACCESSES_NETWORK).union(Effects::WRITES_FILESYSTEM),
            input_schema: schema_of::<PairParams>,
            run: peer_client_pair,
        },
        ToolSpec {
            id: "peer_client_pair_status",
            title: "Pairing progress",
            summary: "The state of the latest pairing with another CodeShellManager: connecting, the check code to compare, paired, failed (with why) or cancelled. No keys and no link. The check code is shown to the app's own window only.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: peer_client_pair_status,
        },
        ToolSpec {
            id: "peer_client_pair_cancel",
            title: "Cancel pairing",
            summary: "Stop a pairing that waits for the other CodeShellManager to answer. Nothing is stored. Allowed for every caller.",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE.union(Effects::IDEMPOTENT),
            input_schema: empty_schema,
            run: peer_client_pair_cancel,
        },
        ToolSpec {
            id: "peer_hosts_list",
            title: "Paired hosts",
            summary: "The other CodeShellManagers this app is paired with: id, name, address, when paired and when last connected.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: peer_hosts_list,
        },
        ToolSpec {
            id: "peer_host_forget",
            title: "Forget a host",
            summary: "Remove one paired host from this app and the host key stored for it (`host_id`, from `peer_hosts_list`). This app then cannot connect to that host until it is paired again. This app's own device key is kept, since other hosts may still know it.",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE.union(Effects::WRITES_FILESYSTEM),
            input_schema: schema_of::<ForgetParams>,
            run: peer_host_forget,
        },
    ]
}

/// Refused before any consent card: pairing is started by the person in the app, never by an agent,
/// a CLI call or a paired device.
pub(super) fn precheck(id: &str, caller: &Caller) -> Option<ToolError> {
    if matches!(caller, Caller::Peer { .. }) && PEER_REFUSED.contains(&id) {
        return Some(ToolError::Refused("a paired device cannot read or change this app's pairings or its list of offices".into()));
    }
    if !UI_ONLY.contains(&id) || *caller == Caller::Ui {
        return None;
    }
    Some(ToolError::Refused("another CodeShellManager can only be paired from the app's own window: this is refused for the CLI, MCP clients and paired devices".into()))
}

fn client(ctx: &Ctx<'_>) -> std::sync::Arc<dyn PeerClientHost> {
    ctx.state.options.peer_client.clone()
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PairParams {
    /// The `csm://pair#...` link the other CodeShellManager shows.
    link: String,
    /// What this host is called in this app (up to 64 characters). Defaults to its address.
    #[serde(default)]
    name: Option<String>,
}

fn peer_client_pair(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if *ctx.caller != Caller::Ui {
        // Belt and braces: `precheck` already refused it.
        return Err(ToolError::Refused("only the app's own window can pair with another CodeShellManager".into()));
    }
    let p: PairParams = parse_params(params)?;
    let name = p.name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    let s = client(ctx).pair_start(&p.link, name)?;
    Ok(ToolOutput::new("Connecting. Compare the four digits with the other screen.", to_json(&s)?))
}

fn peer_connect_open(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    // The app's window opens its dialog before a call reaches the engine; one that gets here has
    // no window to open.
    if *ctx.caller != Caller::Ui {
        return Err(ToolError::Refused("only the app's own window opens the connect dialog".into()));
    }
    Err(ToolError::Refused("the connect dialog is opened by the app's window, not by a tool call".into()))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RemoteWindowParams {
    /// The paired host's id (from `peer_hosts_list`).
    host_id: String,
}

/// The app's Connect button: the host must be one this app is paired with, then the launcher the
/// host wired in (`StateOptions::remote_window_launcher`) starts the window. A launcher that is
/// not wired refuses, and its message is the reply.
fn remote_window_open(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if *ctx.caller != Caller::Ui {
        return Err(ToolError::Refused("only the app's own window opens a remote window".into()));
    }
    let p: RemoteWindowParams = parse_params(params)?;
    let id = p.host_id.trim();
    if id.is_empty() {
        return Err(ToolError::InvalidParams("give the `host_id` from `peer_hosts_list`".into()));
    }
    if !client(ctx).hosts()?.iter().any(|h| h.host_id == id) {
        return Err(ToolError::NotFound(format!("no paired host `{id}`; `peer_hosts_list` shows the ids")));
    }
    let launch = ctx.state.options.remote_window_launcher.clone();
    launch(id).map_err(ToolError::Failed)?;
    Ok(ToolOutput::new("Opening a remote window.", serde_json::json!({"host_id": id})))
}

fn peer_client_pair_status(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let mut s = client(ctx).status();
    // The check code is for the person's own screen (the app's window). An agent or the CLI sees the
    // state and the outcome, not the digits: a code read from a log would let it approve a pairing.
    if *ctx.caller != Caller::Ui {
        s.check_code = None;
    }
    Ok(ToolOutput::new("Pairing status.", to_json(&s)?))
}

fn peer_client_pair_cancel(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let s = client(ctx).pair_cancel();
    Ok(ToolOutput::new("Pairing cancel requested.", to_json(&s)?))
}

fn peer_hosts_list(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let list = client(ctx).hosts()?;
    Ok(ToolOutput::new(format!("{} paired hosts", list.len()), to_json(&list)?))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ForgetParams {
    /// The host's id (from `peer_hosts_list`).
    host_id: String,
}

fn peer_host_forget(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ForgetParams = parse_params(params)?;
    let id = p.host_id.trim();
    if id.is_empty() {
        return Err(ToolError::InvalidParams("give the `host_id` from `peer_hosts_list`".into()));
    }
    if !client(ctx).forget(id)? {
        return Err(ToolError::NotFound(format!("no paired host `{id}`; `peer_hosts_list` shows the ids")));
    }
    Ok(ToolOutput::new("Host forgotten.", serde_json::json!({"forgotten": id})))
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, ToolError> {
    serde_json::to_value(v).map_err(|e| ToolError::Failed(e.to_string()))
}
