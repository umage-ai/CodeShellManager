//! What process add-ons contribute to sessions (docs/addons.md, "Process add-ons"): remote
//! entries for quick-open and the palette, a status override and note on the sessions an add-on
//! opened, and secrets in the OS keychain.
//!
//! None of this is specific to one service: a process add-on offers the places it can
//! open a session to (`csm/remotes_changed`), and these tools are how the app, the CLI, MCP and
//! the add-on itself reach them.
//!
//! Secrets are the add-on's own and nobody else's. The namespace is the **caller's** add-on id,
//! never a parameter, so an add-on cannot name another's secrets, and the person, the CLI and MCP
//! clients are refused outright: a secret goes from the keychain to the add-on that stored it and
//! nowhere else. The keychain can block (a locked store, a prompt), so every call runs on a worker
//! thread and answers when it is done.

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use csm_core::addons::RemoteOpen;
use csm_core::addons::protocol::ProcessEffect;
use csm_services::secrets::{self, SecretStore};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::events::Event;
use crate::plugins::{self, PluginMark};
use crate::status::Status;
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

/// Longest annotation, in characters.
const MAX_ANNOTATION: usize = 80;
/// How long `remote_open` waits for an add-on's tool to say what to open (a little over the
/// tool timeout, so the plugin's own timeout answers first).
const OPEN_WAIT: Duration = Duration::from_secs(35);

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "remote_list",
            title: "List remotes",
            summary: "The places enabled process add-ons offer to open a session on (\"Remote… › add-on name › label\"): the add-on, the id to pass to remote_open, a label, a detail line, and whether it opens as given or asks the add-on's tool.",
            toolset: Toolset::Addons,
            effects: Effects::IDEMPOTENT,
            input_schema: crate::tool::empty_schema,
            run: remote_list,
        },
        ToolSpec {
            id: "remote_open",
            title: "Open a remote",
            summary: "Open the session an add-on's remote entry describes (`addon` and `remote_id` from remote_list): an ssh session, or whatever the add-on's own tool answers. Starts a process and may reach the network, so MCP clients are asked first.",
            toolset: Toolset::Addons,
            effects: Effects::MUTATES_STATE | Effects::LAUNCHES_PROCESS | Effects::ACCESSES_NETWORK,
            input_schema: schema_of::<OpenParams>,
            run: remote_open,
        },
        ToolSpec {
            id: "session_annotate",
            title: "Set a session's status note",
            summary: "For an add-on only (through csm/call): set a status override and a short note (for example \"running remotely\") on a session that add-on opened. Both clear when the add-on stops or is disabled. Other callers are refused.",
            toolset: Toolset::Addons,
            effects: Effects::CHANGES_UI_STATE,
            input_schema: schema_of::<AnnotateParams>,
            run: session_annotate,
        },
        ToolSpec {
            id: "addon_secret_set",
            title: "Store an add-on secret",
            summary: "For an add-on only (through csm/call), and only one that declared `stores_secrets`: keep a secret in the OS keychain under the add-on's own name. Never written to state.json. Other callers are refused.",
            toolset: Toolset::Addons,
            effects: Effects::STORES_SECRETS,
            input_schema: schema_of::<SecretSetParams>,
            run: secret_set,
        },
        ToolSpec {
            id: "addon_secret_get",
            title: "Read an add-on secret",
            summary: "For an add-on only (through csm/call), and only one that declared `stores_secrets`: read one of its own secrets from the OS keychain (`value` is null when there is none). Other callers are refused.",
            toolset: Toolset::Addons,
            effects: Effects::STORES_SECRETS,
            input_schema: schema_of::<SecretKeyParams>,
            run: secret_get,
        },
        ToolSpec {
            id: "addon_secret_delete",
            title: "Delete an add-on secret",
            summary: "For an add-on only (through csm/call), and only one that declared `stores_secrets`: remove one of its own secrets from the OS keychain. Other callers are refused.",
            toolset: Toolset::Addons,
            effects: Effects::STORES_SECRETS,
            input_schema: schema_of::<SecretKeyParams>,
            run: secret_delete,
        },
    ]
}

fn remote_list(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let views = plugins::remote_views(&ctx.state.rt.addons);
    Ok(ToolOutput::new(format!("{} remote(s) offered.", views.len()), json!({"remotes": views})))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenParams {
    /// The add-on that offers the remote (from `remote_list`).
    addon: String,
    /// The remote's id (from `remote_list`).
    remote_id: String,
    /// Focus the new session (default: true from the app, false for MCP clients).
    #[serde(default)]
    focus: Option<bool>,
}

fn remote_open(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: OpenParams = parse_params(params)?;
    let remote = ctx
        .state
        .rt
        .addons
        .remotes
        .get(&p.addon)
        .and_then(|l| l.iter().find(|r| r.id == p.remote_id))
        .cloned()
        .ok_or_else(|| ToolError::NotFound(format!("no remote `{}` from add-on `{}`; `remote_list` shows what is offered", p.remote_id, p.addon)))?;
    let focus = p.focus.unwrap_or(!ctx.caller.is_external());
    match remote.open {
        RemoteOpen::Session(spec) => open_spec(ctx, &p.addon, spec, focus),
        RemoteOpen::Tool { name, params } => {
            if ctx.registry.get(&name).is_none() {
                return Err(ToolError::Refused(format!("`{name}` is not running; the add-on `{}` may have stopped", p.addon)));
            }
            let Some(parked) = ctx.park() else { return Err(ToolError::Failed("cannot wait for the add-on here".into())) };
            let (tx, rx) = mpsc::channel::<Reply>();
            let who = crate::plugins::caller_json(ctx.state, ctx.caller);
            ctx.engine.host.dispatch(p.addon.clone(), name.clone(), params, who, tx);
            let (engine, addon) = (ctx.engine.clone(), p.addon);
            let spawned = thread::Builder::new().name("csm-remote-open".into()).spawn(move || {
                let answer = rx.recv_timeout(OPEN_WAIT).unwrap_or_else(|_| Err(ToolError::TimedOut(format!("`{name}` did not say what to open"))));
                engine.post(Box::new(move |ctx: &mut Ctx<'_>| {
                    let reply = answer.and_then(|out| {
                        let spec = out.data.get("session").cloned().filter(Value::is_object).ok_or_else(|| {
                            ToolError::Failed(format!("`{name}` answered without a `session` object in its data, so there is nothing to open"))
                        })?;
                        open_spec(ctx, &addon, spec, focus)
                    });
                    parked.send(reply);
                }));
            });
            if let Err(e) = spawned {
                return Err(ToolError::Failed(format!("could not start a worker thread: {e}")));
            }
            Ok(ToolOutput::new("Asking the add-on.", Value::Null))
        }
    }
}

/// Opens `spec` (the parameters of `session_new`) as a session owned by `addon`.
fn open_spec(ctx: &mut Ctx<'_>, addon: &str, mut spec: Value, focus: bool) -> Reply {
    let Some(obj) = spec.as_object_mut() else { return Err(ToolError::InvalidParams("a remote's session must be an object".into())) };
    obj.insert("focus".into(), Value::Bool(focus));
    super::sessions::session_new_owned(ctx, spec, Some(addon.to_owned()))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct AnnotateParams {
    session_id: String,
    /// `working`, `needs_you_approval`, `needs_you_question`, `done_unseen`, `error` or `idle`;
    /// `""` clears the override. Leave out to keep it as it is.
    #[serde(default)]
    status: Option<String>,
    /// A short note shown beside the session (at most 80 characters); `""` clears it. Leave out
    /// to keep it as it is.
    #[serde(default)]
    annotation: Option<String>,
}

/// The statuses a process add-on may set. Not `starting`, `asleep` or `exited`: those are facts
/// about the process the app itself runs.
fn settable(s: &str) -> Option<Status> {
    Status::from_id(s).filter(|st| !matches!(st, Status::Starting | Status::Asleep | Status::Exited))
}

fn session_annotate(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: AnnotateParams = parse_params(params)?;
    let Some(addon) = plugins::addon_of(ctx.caller).map(str::to_owned) else {
        return Err(ToolError::Refused("only a process add-on can set a status note, and only on a session it opened".into()));
    };
    let owner = ctx.state.app.sessions.iter().find(|s| s.id == p.session_id).map(|s| s.owner_addon.clone());
    match owner {
        None => return Err(ToolError::NotFound(format!("no session `{}`", p.session_id))),
        Some(o) if o != addon => return Err(ToolError::Refused(format!("`{}` did not open session `{}`, so it cannot set its status", addon, p.session_id))),
        Some(_) => {}
    }
    let mut mark = ctx
        .state
        .rt
        .plugin_marks
        .get(&p.session_id)
        .cloned()
        .unwrap_or(PluginMark { addon: addon.clone(), generation: 0, status: None, annotation: None });
    mark.addon.clone_from(&addon);
    mark.generation = ctx.engine.plugin_tool_gen(&addon).unwrap_or(0);
    if let Some(s) = &p.status {
        mark.status = if s.is_empty() {
            None
        } else {
            Some(settable(s).ok_or_else(|| ToolError::InvalidParams(format!("`status` must be working, needs_you_approval, needs_you_question, done_unseen, error, idle, or \"\" (got `{s}`)")))?)
        };
    }
    if let Some(a) = &p.annotation {
        let a = csm_core::addons::manifest::clean_text(a, MAX_ANNOTATION);
        mark.annotation = Some(a).filter(|a| !a.is_empty());
    }
    let cleared = mark.status.is_none() && mark.annotation.is_none();
    if cleared {
        ctx.state.rt.plugin_marks.remove(&p.session_id);
    } else {
        ctx.state.rt.plugin_marks.insert(p.session_id.clone(), mark);
    }
    ctx.state.touch();
    ctx.events.emit(Event::SessionChanged { session_id: p.session_id.clone() });
    Ok(ToolOutput::new(if cleared { "Cleared." } else { "Set." }, json!({"session_id": p.session_id})))
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SecretKeyParams {
    /// Lowercase letters, digits and `. _ -`, up to 64 bytes.
    key: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SecretSetParams {
    /// Lowercase letters, digits and `. _ -`, up to 64 bytes.
    key: String,
    /// The secret, up to 2048 bytes.
    value: String,
}

/// The calling add-on and the store, after the checks every secret call shares.
fn secret_access(ctx: &Ctx<'_>, key: &str) -> Result<(String, std::sync::Arc<dyn SecretStore>), ToolError> {
    let Some(addon) = plugins::addon_of(ctx.caller).map(str::to_owned) else {
        return Err(ToolError::Refused("secrets belong to an add-on: only the add-on itself can read or change its own".into()));
    };
    if !plugins::declared_effects(&ctx.state.rt.addons, &addon).contains(&ProcessEffect::StoresSecrets) {
        return Err(ToolError::Refused(format!("`{addon}` did not declare `stores_secrets` in its [process] effects")));
    }
    if !secrets::valid_key(key) {
        return Err(ToolError::InvalidParams(format!("a secret key is lowercase letters, digits and . _ - (up to {} bytes)", secrets::MAX_KEY_BYTES)));
    }
    Ok((secrets::service_for(&addon), ctx.state.options.secrets.clone()))
}

/// Runs `work` on a worker thread and answers the parked call with its result.
fn on_worker(ctx: &mut Ctx<'_>, work: impl FnOnce() -> Reply + Send + 'static) -> Reply {
    let Some(parked) = ctx.park() else { return Err(ToolError::Failed("cannot wait for the keychain here".into())) };
    let spawned = thread::Builder::new().name("csm-secret".into()).spawn(move || parked.send(work()));
    match spawned {
        Ok(_) => Ok(ToolOutput::new("Working.", Value::Null)),
        Err(e) => Err(ToolError::Failed(format!("could not start a worker thread: {e}"))),
    }
}

fn secret_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SecretSetParams = parse_params(params)?;
    let (service, store) = secret_access(ctx, &p.key)?;
    if p.value.len() > secrets::MAX_VALUE_BYTES {
        return Err(ToolError::InvalidParams(format!("a secret is at most {} bytes", secrets::MAX_VALUE_BYTES)));
    }
    on_worker(ctx, move || {
        store.set(&service, &p.key, &p.value).map_err(ToolError::Failed)?;
        Ok(ToolOutput::new("Stored.", json!({"key": p.key})))
    })
}

fn secret_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SecretKeyParams = parse_params(params)?;
    let (service, store) = secret_access(ctx, &p.key)?;
    on_worker(ctx, move || {
        let value = store.get(&service, &p.key).map_err(ToolError::Failed)?;
        // The value is in `data` for the add-on that asked, and nowhere in the message.
        Ok(ToolOutput::new(if value.is_some() { "Found." } else { "No such secret." }, json!({"key": p.key, "value": value})))
    })
}

fn secret_delete(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SecretKeyParams = parse_params(params)?;
    let (service, store) = secret_access(ctx, &p.key)?;
    on_worker(ctx, move || {
        let removed = store.delete(&service, &p.key).map_err(ToolError::Failed)?;
        Ok(ToolOutput::new(if removed { "Deleted." } else { "No such secret." }, json!({"key": p.key, "removed": removed})))
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn only_working_states_are_settable() {
        for ok in ["working", "needs_you_approval", "needs_you_question", "done_unseen", "error", "idle"] {
            assert!(settable(ok).is_some(), "{ok}");
        }
        for no in ["starting", "asleep", "exited", "", "bogus"] {
            assert!(settable(no).is_none(), "{no}");
        }
    }
}
