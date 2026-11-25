//! Remote access: letting another CodeShellManager connect to this one (`peer_*`).
//!
//! Server mode is **off by default** and only the person, in the app, can turn it on. The three
//! tools that open a way in (`peer_server_enable`, `peer_pair_start`, `peer_pair_answer`) are
//! refused in [`precheck`] for every caller except [`Caller::Ui`]: the CLI (an agent can run
//! `csm` from its own shell), MCP clients and paired devices alike. That is a refusal before any
//! consent card, not a question, because a card can be clicked through or asked again until it is.
//! None of this is a `settings_set` key: settings are `state`, which needs no consent and which
//! the CLI may write. Turning things off (`peer_server_disable`, `peer_device_revoke`) is always
//! allowed for the app and the CLI; a caller that is an MCP client or a paired device may only revoke a single device by id (closing one door), because dropping everything is a denial of service.
//!
//! The tools only decide who may ask; the socket, the keys and the device list belong to the
//! [`PeerHost`](crate::peer_host::PeerHost) the app wired in (`csm-peer`).

use std::net::{IpAddr, SocketAddr};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::peer_host::{AuditEvent, EnableRequest, PairAnswer, PeerHost, RelaySetRequest, Revoke};
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

/// The port suggested when the person gives none.
pub const DEFAULT_PORT: u16 = 47_300;

/// The tools only the person at the app's window may call.
pub const UI_ONLY: [&str; 5] = ["peer_server_enable", "peer_pair_start", "peer_pair_answer", "peer_relay_set", "peer_device_trust"];

pub(super) fn specs() -> Vec<ToolSpec> {
    // Declared honestly: turning it on opens a network door to shells, so it is flagged like
    // every other tool that does something the person has not seen (consent would apply to an
    // MCP caller, if the precheck below ever let one through).
    let opens = Effects::MUTATES_STATE.union(Effects::ACCESSES_NETWORK).union(Effects::WRITES_FILESYSTEM).union(Effects::ENABLES_EXTENSION);
    vec![
        ToolSpec {
            id: "peer_status",
            title: "Remote access status",
            summary: "Whether another CodeShellManager can connect to this one: on or off, the address it listens on, an open pairing offer, a device waiting for Allow or Deny, and how many devices are paired and connected. No keys and no pairing link.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: peer_status,
        },
        ToolSpec {
            id: "peer_interfaces",
            title: "Network addresses of this machine",
            summary: "The addresses of this machine that remote access could listen on: adapter name, IP, prefix length, whether it looks like a WireGuard tunnel. Loopback is listed last.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: peer_interfaces,
        },
        ToolSpec {
            id: "peer_relay_status",
            title: "Relay status",
            summary: "Whether remote access is also reachable through a relay, which relay, and whether this machine is connected to it right now. No secrets.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: peer_relay_status,
        },
        ToolSpec {
            id: "peer_relay_set",
            title: "Reach this machine through a relay…",
            summary: "Also let paired devices reach remote access through a relay (`wss://` address), for networks without a VPN. The relay only copies encrypted frames. The operator's enrolment token, when the relay needs one, is kept in the OS keychain, never in a file. The relay runs only while remote access is on. An empty `url` turns the relay off. Refused for every caller except the app's own window.",
            toolset: Toolset::App,
            effects: opens,
            input_schema: schema_of::<RelaySetParams>,
            run: peer_relay_set,
        },
        ToolSpec {
            id: "peer_server_enable",
            title: "Turn on remote access…",
            summary: "Let another CodeShellManager connect to this one, on the one address and port you choose (never all interfaces unless `all_interfaces` is set). A paired device can type into your sessions. Refused for every caller except the app's own window: not the CLI, not MCP, not a paired device.",
            toolset: Toolset::App,
            effects: opens,
            input_schema: schema_of::<EnableParams>,
            run: peer_server_enable,
        },
        ToolSpec {
            id: "peer_server_disable",
            title: "Turn off remote access",
            summary: "Stop listening, close every connected device with a goodbye, close any open pairing offer. Paired devices stay paired (revoke them to delete their keys). Refused for MCP clients and paired devices (the app and the CLI may).",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE.union(Effects::ACCESSES_NETWORK).union(Effects::IDEMPOTENT),
            input_schema: empty_schema,
            run: peer_server_disable,
        },
        ToolSpec {
            id: "peer_pair_start",
            title: "Pair a device…",
            summary: "Open a pairing offer, valid for 10 minutes and one use, and return its link (`csm://pair#...`) for the other CodeShellManager to open. The link is a secret. Refused for every caller except the app's own window.",
            toolset: Toolset::App,
            effects: opens,
            input_schema: empty_schema,
            run: peer_pair_start,
        },
        ToolSpec {
            id: "peer_pair_answer",
            title: "Allow or deny a device",
            summary: "Answer the device waiting to pair: `allow` with the four-digit `check_code` both screens show, or deny. Refused for every caller except the app's own window.",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE.union(Effects::ENABLES_EXTENSION).union(Effects::WRITES_FILESYSTEM),
            input_schema: schema_of::<AnswerParams>,
            run: peer_pair_answer,
        },
        ToolSpec {
            id: "peer_device_list",
            title: "Paired devices",
            summary: "The devices paired with this app: id, name, level, whether trusted (acts without prompts), when paired, when last seen, whether connected now.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: peer_device_list,
        },
        ToolSpec {
            id: "peer_device_trust",
            title: "Trust a device (no prompts)",
            summary: "Trust one paired device (`device_id`, from `peer_device_list`) or stop trusting it (`trusted`: false). A trusted device acts without a consent card; an untrusted one needs the person's approval on this screen before it types into a session or starts a process. Refused for every caller except the app's own window: a device cannot trust itself.",
            toolset: Toolset::App,
            effects: Effects::MUTATES_STATE.union(Effects::ENABLES_EXTENSION).union(Effects::WRITES_FILESYSTEM),
            input_schema: schema_of::<TrustParams>,
            run: peer_device_trust,
        },
        ToolSpec {
            id: "peer_device_revoke",
            title: "Revoke a device",
            summary: "Delete one paired device's key (`id`) or every device's (`all`: true) and close their connections at once. They need a fresh pairing to come back. One device by `id` is open to every caller; `all` is the app's and the CLI's only.",
            toolset: Toolset::App,
            // Not WRITES_FILESYSTEM on purpose (like peer_server_disable): that flag would put an
            // MCP consent card in front of closing a door, and closing must work for every caller.
            // The small private-file write is the tool's own bookkeeping, not a user file.
            effects: Effects::MUTATES_STATE.union(Effects::ACCESSES_NETWORK),
            input_schema: schema_of::<RevokeParams>,
            run: peer_device_revoke,
        },
    ]
}

/// Refused before any consent card, whatever the person would answer: the tools that open a way
/// in belong to the app's own window.
pub(super) fn precheck(id: &str, caller: &Caller, host: &dyn PeerHost) -> Option<ToolError> {
    if !UI_ONLY.contains(&id) || *caller == Caller::Ui {
        return None;
    }
    host.audit(AuditEvent::Refused { tool: id.to_owned(), caller: caller.describe() });
    Some(ToolError::Refused(
        "remote access can only be turned on, and devices only paired, by the person in the app's own window: this is refused for the CLI, MCP clients and paired devices".into(),
    ))
}

/// A peer below Full may only read (levels other than Full are not issued yet; this is the rule
/// the server enforces when they are).
pub(crate) fn level_gate(caller: &Caller, spec: &ToolSpec) -> Option<ToolError> {
    use csm_peer_proto::msg::Level;
    match caller {
        Caller::Peer { level, .. } if !level.allows(Level::Full) && !spec.effects.read_only_hint() => {
            Some(ToolError::Refused(format!("`{}` needs the Full level; this device is paired as {level:?}", spec.id)))
        }
        _ => None,
    }
}

/// Records a paired device's non-read call in the remote-access log.
pub(crate) fn audit_peer_call(host: &dyn PeerHost, caller: &Caller, spec: &ToolSpec, params: &Value) {
    if let Caller::Peer { device_id, name, .. } = caller
        && !spec.effects.read_only_hint()
    {
        let session = params.get("session_id").and_then(Value::as_str).map(|s| s.chars().take(64).collect());
        host.audit(AuditEvent::PeerCall { device_id: device_id.clone(), device_name: name.clone(), tool: spec.id.to_owned(), session, effects: spec.effects });
    }
}

fn host(ctx: &Ctx<'_>) -> std::sync::Arc<dyn PeerHost> {
    ctx.state.options.peer_host.clone()
}

fn peer_status(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let s = host(ctx).status();
    let message = if !s.available {
        "Remote access isn't available here.".to_owned()
    } else if let Some(l) = &s.listening {
        format!("Remote access is on, listening on {l}; {} devices paired, {} connected.", s.devices, s.connected)
    } else if let Some(w) = &s.waiting {
        format!("Remote access is on but not listening yet: {w}")
    } else {
        "Remote access is off.".to_owned()
    };
    Ok(ToolOutput::new(message, to_json(&s)?))
}

fn peer_interfaces(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let list = host(ctx).interfaces();
    Ok(ToolOutput::new(format!("{} addresses", list.len()), to_json(&list)?))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EnableParams {
    /// The IP address of this machine to listen on, for example the WireGuard address `10.8.0.2`.
    address: String,
    /// The port; 47300 when omitted, 0 to let the system choose.
    #[serde(default)]
    port: Option<u16>,
    /// Listen on every interface (0.0.0.0 or ::). Off unless the person chose it under Advanced.
    #[serde(default)]
    all_interfaces: bool,
}

/// The address the person chose, checked: an IP, not a name; no multicast; the unspecified
/// address only with `all_interfaces`.
fn parse_listen(p: &EnableParams) -> Result<SocketAddr, ToolError> {
    let text = p.address.trim().trim_start_matches('[').trim_end_matches(']');
    let ip: IpAddr = text.parse().map_err(|_| ToolError::InvalidParams(format!("`{}` is not an IP address; give the address of this machine to listen on", p.address.trim())))?;
    let addr = SocketAddr::new(ip, p.port.unwrap_or(DEFAULT_PORT));
    // Port 0 is a request for a free port; the check is about the address.
    let probe = SocketAddr::new(ip, if addr.port() == 0 { 1 } else { addr.port() });
    csm_peer_proto::addr::check_bind(&probe, p.all_interfaces).map_err(|e| ToolError::Refused(format!("{e}; choose the address of one interface, or turn on `all_interfaces` on purpose")))?;
    Ok(addr)
}

fn peer_server_enable(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if *ctx.caller != Caller::Ui {
        // Belt and braces: `precheck` already refused it.
        return Err(ToolError::Refused("only the app's own window can turn on remote access".into()));
    }
    let p: EnableParams = parse_params(params)?;
    let addr = parse_listen(&p)?;
    let by = ctx.caller.describe();
    let s = host(ctx).enable(EnableRequest { addr, allow_all_interfaces: p.all_interfaces }, &by)?;
    let at = s.listening.clone().unwrap_or_else(|| addr.to_string());
    Ok(ToolOutput::new(format!("Remote access is on, listening on {at}."), to_json(&s)?))
}

fn peer_server_disable(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    if ctx.caller.is_external() {
        return Err(ToolError::Refused("turning remote access off drops every connected device; do it in the app (or with `csm peer-server-disable`)".into()));
    }
    let by = ctx.caller.describe();
    let s = host(ctx).disable(&by);
    Ok(ToolOutput::new("Remote access is off; every connection is closed.", to_json(&s)?))
}

fn peer_pair_start(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    if *ctx.caller != Caller::Ui {
        return Err(ToolError::Refused("only the app's own window can pair a device".into()));
    }
    let by = ctx.caller.describe();
    let offer = host(ctx).pair_start(&by)?;
    Ok(ToolOutput::new(format!("Pairing is open for {} minutes. Open the link on the other CodeShellManager.", offer.expires_in_secs / 60), to_json(&offer)?))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AnswerParams {
    /// Allow the waiting device (true) or deny it (false).
    allow: bool,
    /// The four digits both screens show. Required to allow.
    #[serde(default)]
    check_code: Option<String>,
    /// `full` (the only level so far).
    #[serde(default)]
    level: Option<String>,
    /// Trust this device: its calls that type into a session or start a process run without a card on this screen. Off unless the person ticks "Trust this device (no prompts)".
    #[serde(default)]
    trusted: bool,
}

fn peer_pair_answer(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if *ctx.caller != Caller::Ui {
        return Err(ToolError::Refused("only the app's own window can answer a pairing".into()));
    }
    let p: AnswerParams = parse_params(params)?;
    let level = p.level.unwrap_or_else(|| "full".into());
    if level != "full" {
        return Err(ToolError::InvalidParams("only the `full` level is available so far".into()));
    }
    let allow = p.allow;
    let by = ctx.caller.describe();
    let s = host(ctx).pair_answer(PairAnswer { allow, check_code: p.check_code, level, trusted: p.trusted }, &by)?;
    Ok(ToolOutput::new(if allow { "Device allowed." } else { "Device denied." }, to_json(&s)?))
}

fn peer_device_list(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let list = host(ctx).devices();
    Ok(ToolOutput::new(format!("{} paired devices", list.len()), to_json(&list)?))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RevokeParams {
    /// One device's id (from `peer_device_list`).
    #[serde(default)]
    id: Option<String>,
    /// Every device.
    #[serde(default)]
    all: bool,
}

fn peer_device_revoke(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: RevokeParams = parse_params(params)?;
    let target = match (p.id, p.all) {
        (Some(id), false) if !id.trim().is_empty() => Revoke::One(id.trim().to_owned()),
        (None, true) => Revoke::All,
        _ => return Err(ToolError::InvalidParams("give either `id` or `all: true`".into())),
    };
    if matches!(target, Revoke::All) && ctx.caller.is_external() {
        return Err(ToolError::Refused("revoking every device is done in the app (or with the CLI); an MCP client or a paired device may revoke one device by `id`".into()));
    }
    let by = ctx.caller.describe();
    let n = host(ctx).revoke(target, &by)?;
    Ok(ToolOutput::new(if n == 1 { "Revoked 1 device.".to_owned() } else { format!("Revoked {n} devices.") }, json!({"revoked": n})))
}

fn to_json<T: serde::Serialize>(v: &T) -> Result<Value, ToolError> {
    serde_json::to_value(v).map_err(|e| ToolError::Failed(e.to_string()))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RelaySetParams {
    /// The relay's address, `wss://relay.example` (or `ws://` to this PC). Empty turns the relay off.
    #[serde(default)]
    url: String,
    /// The operator's enrolment token, when the relay needs one. Kept in the OS keychain. Left out:
    /// the saved token is kept when the address is the same, else removed. Empty: removed.
    #[serde(default)]
    access: Option<String>,
}

fn peer_relay_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if *ctx.caller != Caller::Ui {
        // Belt and braces: `precheck` already refused it.
        return Err(ToolError::Refused("only the app's own window can set the relay".into()));
    }
    let p: RelaySetParams = parse_params(params)?;
    // Left out (`None`) keeps the saved token for the same address; `Some("")` removes it.
    let access = p.access.map(|t| t.trim().to_owned());
    let by = ctx.caller.describe();
    match host(ctx).relay_set(RelaySetRequest { url: p.url.trim().to_owned(), access }, &by)? {
        Some(info) => Ok(ToolOutput::new(format!("Also reachable through {}.", info.url), to_json(&info)?)),
        None => Ok(ToolOutput::new("The relay is off.", Value::Null)),
    }
}

fn peer_relay_status(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    match host(ctx).relay_status() {
        Some(info) => {
            let message = if info.connected { "Connected to the relay." } else { "Not connected to the relay." };
            Ok(ToolOutput::new(message, to_json(&info)?))
        }
        None => Ok(ToolOutput::new("No relay is set.", Value::Null)),
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct TrustParams {
    /// The device's id (from `peer_device_list`).
    device_id: String,
    /// `true`: it acts without prompts. `false`: its typing and process launches need approval here.
    trusted: bool,
}

fn peer_device_trust(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if *ctx.caller != Caller::Ui {
        // Belt and braces: `precheck` already refused it.
        return Err(ToolError::Refused("only the app's own window can trust a device".into()));
    }
    let p: TrustParams = parse_params(params)?;
    let id = p.device_id.trim().to_owned();
    let by = ctx.caller.describe();
    host(ctx).set_device_trusted(&id, p.trusted, &by)?;
    Ok(ToolOutput::new(if p.trusted { "The device is trusted: it acts without prompts." } else { "The device is not trusted: it needs approval here to type or start a process." }, json!({"device_id": id, "trusted": p.trusted})))
}
