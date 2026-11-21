//! The add-on tools (docs/addons.md, toolset `addons`): list, reload, enable, disable, open the
//! folder, and add a pack's run template to a session.
//!
//! Enabling is the consent point. The UI shows the add-on's contributions (and, for a process
//! add-on, its declared effects) and then calls `addon_enable` with the `version` it showed; the
//! engine refuses if the file now says something else. `addon_enable` declares
//! `ENABLES_EXTENSION`, so an MCP client gets a consent card for it, like any other tool that
//! would run code the person has not seen.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::addons::{self, AddonState};
use crate::engine::{Ctx, Reply};
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};
use crate::tools::{find, open};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "addon_list",
            title: "List add-ons",
            summary: "The add-ons in the add-ons folder: each one's name, version, kind, state (loaded, disabled or error) and why, what it contributes (profiles, run templates, slash presets, palette commands) and what was skipped.",
            toolset: Toolset::Addons,
            effects: Effects::IDEMPOTENT,
            input_schema: crate::tool::empty_schema,
            run: addon_list,
        },
        ToolSpec {
            id: "addon_reload",
            title: "Reload add-ons",
            summary: "Read the add-ons folder again. Enabled add-ons whose addon.toml changed are off until enabled again. Answers once the folder is read.",
            toolset: Toolset::Addons,
            effects: Effects::MUTATES_STATE,
            input_schema: crate::tool::empty_schema,
            run: addon_reload,
        },
        ToolSpec {
            id: "addon_enable",
            title: "Enable an add-on",
            summary: "Turn an add-on on. `version` and `digest` must be the ones the person was shown (from addon_list); the engine refuses if the file has changed since. Asks MCP clients first: an add-on can add commands, profiles and process effects.",
            toolset: Toolset::Addons,
            effects: Effects::MUTATES_STATE | Effects::ENABLES_EXTENSION,
            input_schema: schema_of::<EnableParams>,
            run: addon_enable,
        },
        ToolSpec {
            id: "addon_disable",
            title: "Disable an add-on",
            summary: "Turn an add-on off. Its contributions go away; enabling it again asks for consent again.",
            toolset: Toolset::Addons,
            effects: Effects::MUTATES_STATE,
            input_schema: schema_of::<IdParams>,
            run: addon_disable,
        },
        ToolSpec {
            id: "addon_open_folder",
            title: "Open add-ons folder",
            summary: "Open the add-ons folder in the file manager (created if it is missing). Not available for a --clean or isolated run.",
            toolset: Toolset::Addons,
            effects: Effects::LAUNCHES_PROCESS | Effects::WRITES_FILESYSTEM,
            input_schema: crate::tool::empty_schema,
            run: addon_open_folder,
        },
        ToolSpec {
            id: "addon_run_apply",
            title: "Add a pack's run command",
            summary: "Add one of an enabled add-on's run templates (`addon-id.name`, from addon_list) to a session's Runs. Refused for MCP clients, like run_edit: run commands decide what F5 runs.",
            toolset: Toolset::Addons,
            effects: Effects::MUTATES_STATE,
            input_schema: schema_of::<ApplyRunParams>,
            run: addon_run_apply,
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct IdParams {
    /// The add-on id (its folder name).
    id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EnableParams {
    /// The add-on id (its folder name).
    id: String,
    /// The version the person was shown in the consent dialog (`addon_list` gives it).
    version: String,
    /// The digest of the `addon.toml` the person was shown (`addon_list` gives it). The engine
    /// refuses if the file has changed since, even when the version did not.
    digest: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ApplyRunParams {
    session_id: String,
    /// The run template's id, `addon-id.name`.
    template: String,
}

fn addon_list(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let cat = &ctx.state.rt.addons;
    let list = addons::summaries(cat);
    let enabled = list.iter().filter(|a| a.state == AddonState::Loaded).count();
    let msg = if cat.root.is_none() {
        "Add-ons are off in this run (--clean or an isolated run has no add-ons folder).".to_owned()
    } else {
        format!("{} add-on(s) found, {enabled} enabled.", list.len())
    };
    Ok(ToolOutput::new(
        msg,
        json!({
            "folder": cat.root.as_ref().map(|r| r.display().to_string()),
            "scanning": cat.scanning,
            "scan_error": cat.scan_error,
            "addons": list,
        }),
    ))
}

fn addon_reload(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    if let Some(reply) = ctx.park() {
        ctx.state.rt.addons.waiters.push(reply);
    }
    addons::reload(ctx);
    Ok(ToolOutput::message("Reloading add-ons."))
}

fn addon_enable(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: EnableParams = parse_params(params)?;
    let e = addons::entry(&ctx.state.rt.addons, &p.id).ok_or_else(|| ToolError::NotFound(format!("no add-on `{}`; `addon_list` shows them", p.id)))?;
    let parsed = e.parsed.clone().ok_or_else(|| ToolError::Refused(format!("`{}` did not load: {}", p.id, e.message.clone().unwrap_or_default())))?;
    let digest = e.digest.clone().unwrap_or_default();
    let now = parsed.meta.version.to_string();
    if now != p.version {
        return Err(ToolError::Refused(format!("`{}` is now version {now}, not {}; review it again before enabling", p.id, p.version)));
    }
    if digest.is_empty() {
        return Err(ToolError::Refused(format!("`{}` has no readable manifest", p.id)));
    }
    if digest != p.digest {
        return Err(ToolError::Refused(format!("`{}` changed since it was shown; `addon_reload` and review it again before enabling", p.id)));
    }
    ctx.state.app.settings.addon_approvals.insert(p.id.clone(), digest);
    ctx.state.persist();
    addons::refresh_all(ctx);
    Ok(ToolOutput::new(format!("Enabled {} {now}.", parsed.meta.name), json!({"id": p.id, "version": now})))
}

fn addon_disable(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: IdParams = parse_params(params)?;
    if addons::entry(&ctx.state.rt.addons, &p.id).is_none() {
        return Err(ToolError::NotFound(format!("no add-on `{}`; `addon_list` shows them", p.id)));
    }
    let was = ctx.state.app.settings.addon_approvals.remove(&p.id).is_some();
    ctx.state.persist();
    addons::refresh_all(ctx);
    let msg = if was { format!("Disabled {}.", p.id) } else { format!("{} was not enabled.", p.id) };
    Ok(ToolOutput::new(msg, json!({"id": p.id})))
}

fn addon_open_folder(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let Some(root) = ctx.state.rt.addons.root.clone() else {
        return Err(ToolError::Refused("there is no add-ons folder in this run (--clean or an isolated run)".into()));
    };
    let reply = ctx.park();
    let spawned = std::thread::Builder::new().name("csm-addon-folder".into()).spawn(move || {
        let r = match csm_services::addons::ensure_dir(&root) {
            Err(e) => Err(ToolError::Failed(format!("could not create {}: {e}", root.display()))),
            Ok(()) => {
                let target = root.display().to_string();
                open::open_folder_now(&target).map(|()| ToolOutput::new(format!("Opened {target}."), json!({"folder": target}))).map_err(ToolError::Failed)
            }
        };
        if let Some(reply) = reply {
            reply.send(r);
        }
    });
    match spawned {
        Ok(_) => Ok(ToolOutput::message("Opening the add-ons folder.")),
        Err(e) => Err(ToolError::Failed(format!("could not start a worker: {e}"))),
    }
}

fn addon_run_apply(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if matches!(ctx.caller, Caller::External { .. }) {
        return Err(ToolError::Refused("run commands decide what F5 runs; add an add-on's run command in the app".into()));
    }
    let p: ApplyRunParams = parse_params(params)?;
    let (addon, _) = p.template.split_once('.').ok_or_else(|| ToolError::InvalidParams(format!("`{}` is not `addon-id.name`", p.template)))?;
    let parsed = addons::enabled_addon(&ctx.state.rt.addons, addon)?;
    let i = find(ctx.state, &p.session_id)?;
    let count = ctx.state.app.sessions.get(i).map_or(0, |s| s.run_commands.len());
    if count >= 50 {
        return Err(ToolError::Refused("this session already has 50 run commands; remove one first".into()));
    }
    let name = p.template.split_once('.').map_or(String::new(), |(_, n)| n.to_owned());
    let qualified = format!("{addon}.{name}");
    let item = addons::run_item_for(&parsed, &name, crate::lifecycle::new_id(), count == 0).map_err(ToolError::Refused)?;
    let Some(s) = ctx.state.app.sessions.get_mut(i) else {
        return Err(ToolError::NotFound("no such session".into()));
    };
    let label = item.label.clone();
    s.run_commands.push(item);
    csm_core::models::RunCommandItem::ensure_single_default(&mut s.run_commands);
    ctx.state.persist();
    Ok(ToolOutput::new(format!("Added {label} to the session's runs."), json!({"session_id": p.session_id, "template": qualified})))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn enable_declares_the_consent_effect_and_disable_does_not() {
        let specs = specs();
        let enable = specs.iter().find(|s| s.id == "addon_enable").unwrap();
        assert!(enable.effects.needs_consent());
        let disable = specs.iter().find(|s| s.id == "addon_disable").unwrap();
        assert!(!disable.effects.needs_consent());
    }
}
