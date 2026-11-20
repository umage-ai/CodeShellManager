//! `runs` tools: a session's run commands (`run_list`, `run_start`, `run_stop`, `run_output`,
//! `run_edit`). The work is in [`crate::runs`].
//!
//! `run_edit` decides what `run_start` (and F5) will execute, so MCP clients may not call it:
//! a client that could rewrite the default command would turn the person's next F5 into its own
//! command line. The app and the CLI (the person) can.

use csm_core::models::{RunCommandItem, RunMode, SessionKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::runs::{self, MAX_RUN_COMMANDS, RunPhase};
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

use super::{clean_name, find};

pub(super) fn specs() -> Vec<ToolSpec> {
    let t = |id, title, summary, effects, input_schema, run| ToolSpec { id, title, summary, toolset: Toolset::Runs, effects, input_schema, run };
    vec![
        t(
            "run_list",
            "Run commands",
            "A session's run commands and the state of each one's last run (running, ok, failed, stopped).",
            Effects::READ_ONLY,
            schema_of::<SessionParams>,
            run_list,
        ),
        t(
            "run_start",
            "Run",
            "Start a run command (default: the session's default) in its own background terminal; never types into the session. Restarts it if it is already running.",
            Effects::LAUNCHES_PROCESS,
            schema_of::<RunParams>,
            run_start,
        ),
        t(
            "run_stop",
            "Stop run",
            "Stop a running run command (default: the default one) and everything it started.",
            Effects::MAY_DISCARD_DATA,
            schema_of::<RunParams>,
            run_stop,
        ),
        t(
            "run_output",
            "Run output",
            "The ANSI-stripped output of a run command's last run (kept up to 1 MB; `max_chars` returns the tail).",
            Effects::from_bits_truncate(Effects::IDEMPOTENT.bits() | Effects::READS_SESSION_CONTENT.bits()),
            schema_of::<OutputParams>,
            run_output,
        ),
        t(
            "run_edit",
            "Edit run commands…",
            "Replace a session's run commands (label, command line, mode process/powershell, post-run URL, default), or `detect: true` to take the project-type template. App and CLI only.",
            Effects::MUTATES_STATE,
            schema_of::<EditParams>,
            run_edit,
        ),
    ]
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SessionParams {
    session_id: String,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunParams {
    session_id: String,
    /// A run command's id (or label). Default: the session's default run command.
    run_id: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OutputParams {
    session_id: String,
    run_id: Option<String>,
    /// Return only the last this-many characters (default 200 000).
    max_chars: Option<u32>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ModeParam {
    /// `cmd /c` on Windows, the login shell elsewhere.
    Process,
    /// `pwsh -EncodedCommand` (pipes, `$env:`, cmdlets).
    Powershell,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ItemParam {
    /// Keep an existing command's id; new ones get one.
    #[serde(default)]
    id: Option<String>,
    label: String,
    command_line: String,
    #[serde(default)]
    is_default: bool,
    #[serde(default)]
    mode: Option<ModeParam>,
    /// Opened in the browser after the run exits 0. Absolute http/https only.
    #[serde(default)]
    post_run_url: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditParams {
    session_id: String,
    /// The new list, in order. Exactly one ends up the default (the last marked, else the first).
    #[serde(default)]
    runs: Vec<Value>,
    /// Replace the list with the project-type template of the session's folder instead.
    #[serde(default)]
    detect: bool,
}

fn list_json(ctx: &Ctx<'_>, sid: &str) -> Result<Value, ToolError> {
    let i = find(ctx.state, sid)?;
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound(format!("no session `{sid}`")))?;
    Ok(json!(runs::views(ctx.state, s)))
}

fn run_list(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SessionParams = parse_params(params)?;
    let data = list_json(ctx, &p.session_id)?;
    let n = data.as_array().map_or(0, Vec::len);
    Ok(ToolOutput::new(format!("{n} run commands"), data))
}

fn item_for(ctx: &Ctx<'_>, p: &RunParams) -> Result<(csm_core::models::ShellSession, RunCommandItem), ToolError> {
    let i = find(ctx.state, &p.session_id)?;
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound(format!("no session `{}`", p.session_id)))?;
    if s.run_commands.is_empty() {
        return Err(ToolError::NotFound("this session has no run commands; add some with run_edit (or the Runs tab)".into()));
    }
    let item = runs::pick(s, p.run_id.as_deref())
        .ok_or_else(|| ToolError::NotFound(format!("no run command `{}`; run_list shows them", p.run_id.clone().unwrap_or_default())))?;
    Ok((s.clone(), item.clone()))
}

fn run_start(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: RunParams = parse_params(params)?;
    if ctx.state.rt.shutting_down {
        return Err(ToolError::Refused("the app is closing".into()));
    }
    let (s, item) = item_for(ctx, &p)?;
    if s.is_dormant {
        return Err(ToolError::Refused(format!("{} is asleep; wake it first", s.name)));
    }
    if let Some(e) = runs::command_line_error(&item.command_line) {
        return Err(ToolError::Refused(format!("{}: {e}", item.label)));
    }
    runs::start(ctx, &s, &item);
    Ok(ToolOutput::new(format!("Started {} in its own background terminal.", item.label), json!({"session_id": s.id, "run_id": item.id})))
}

fn run_stop(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: RunParams = parse_params(params)?;
    let (s, item) = item_for(ctx, &p)?;
    let was = runs::stop(ctx.state, &s.id, &item.id);
    let msg = if was { format!("Stopped {}.", item.label) } else { format!("{} is not running.", item.label) };
    Ok(ToolOutput::new(msg, json!({"session_id": s.id, "run_id": item.id, "was_running": was})))
}

fn run_output(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: OutputParams = parse_params(params)?;
    let (s, item) = item_for(ctx, &RunParams { session_id: p.session_id, run_id: p.run_id })?;
    let inst = ctx.state.rt.runs.get(&s.id).and_then(|v| v.iter().find(|r| r.item_id == item.id));
    let Some(r) = inst else {
        return Ok(ToolOutput::new(format!("{} has not run yet.", item.label), json!({"run_id": item.id, "phase": RunPhase::Idle, "output": ""})));
    };
    let max = usize::try_from(p.max_chars.unwrap_or(200_000)).unwrap_or(usize::MAX).max(1);
    let text = r.output.tail(max);
    Ok(ToolOutput::new(
        format!("{} — {} characters.", item.label, text.chars().count()),
        json!({"run_id": item.id, "phase": r.phase, "exit_code": r.exit_code, "output": text}),
    ))
}

fn parse_item(v: Value, index: usize) -> Result<RunCommandItem, ToolError> {
    let p: ItemParam = serde_json::from_value(v).map_err(|e| ToolError::InvalidParams(format!("runs[{index}]: {e}")))?;
    let label = clean_name(&p.label, 80);
    if label.is_empty() {
        return Err(ToolError::InvalidParams(format!("runs[{index}]: give it a label")));
    }
    if let Some(e) = runs::command_line_error(&p.command_line) {
        return Err(ToolError::InvalidParams(format!("{label}: {e}")));
    }
    let url = p.post_run_url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
    if let Some(u) = &url
        && runs::launchable_url(u).is_none()
    {
        return Err(ToolError::InvalidParams(format!("{label}: the post-run URL must be an absolute http:// or https:// address")));
    }
    Ok(RunCommandItem {
        id: p.id.filter(|i| !i.trim().is_empty() && i.len() <= 64 && !i.chars().any(char::is_control)).unwrap_or_default(),
        label,
        command_line: p.command_line.trim().to_owned(),
        is_default: p.is_default,
        mode: match p.mode {
            Some(ModeParam::Powershell) => RunMode::PowerShell,
            _ => RunMode::Process,
        },
        post_run_url: url,
    })
}

fn run_edit(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if matches!(ctx.caller, Caller::External { .. }) {
        return Err(ToolError::Refused("run commands decide what F5 runs; edit them in the app (or with the csm CLI)".into()));
    }
    let p: EditParams = parse_params(params)?;
    let i = find(ctx.state, &p.session_id)?;
    if p.detect {
        let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound("no such session".into()))?;
        if s.kind == SessionKind::Ssh {
            return Err(ToolError::Refused("SSH sessions have no local folder to detect a project type from".into()));
        }
        let (folder, sid, engine) = (s.working_folder.clone(), s.id.clone(), ctx.engine.clone());
        let reply = ctx.park();
        let spawned = std::thread::Builder::new().name("csm-run-detect".into()).spawn(move || {
            let found = runs::templates::seed_for_folder(&folder);
            engine.post(Box::new(move |ctx| {
                let r = match found {
                    None => Err(ToolError::NotFound("no project type recognised in the session's folder (dotnet, cargo, node, python, make)".into())),
                    Some((kind, items)) => match ctx.state.app.sessions.iter_mut().find(|s| s.id == sid) {
                        None => Err(ToolError::NotFound("the session was closed".into())),
                        Some(s) => {
                            s.run_commands = items;
                            ctx.state.persist();
                            Ok(ToolOutput::new(format!("Added the {kind} run commands."), json!({"template": kind})))
                        }
                    },
                };
                crate::lifecycle::after_step(ctx.state, ctx.engine);
                if let Some(reply) = reply {
                    reply.send(r);
                }
            }));
        });
        return match spawned {
            Ok(_) => Ok(ToolOutput::message("Detecting.")),
            Err(e) => Err(ToolError::Failed(format!("could not start a worker: {e}"))),
        };
    }
    if p.runs.len() > MAX_RUN_COMMANDS {
        return Err(ToolError::InvalidParams(format!("at most {MAX_RUN_COMMANDS} run commands")));
    }
    let mut items = p.runs.into_iter().enumerate().map(|(n, v)| parse_item(v, n)).collect::<Result<Vec<_>, _>>()?;
    let mut seen = std::collections::HashSet::new();
    for it in &mut items {
        if it.id.is_empty() || !seen.insert(it.id.clone()) {
            it.id = crate::lifecycle::new_id();
            seen.insert(it.id.clone());
        }
    }
    RunCommandItem::ensure_single_default(&mut items);
    let s = ctx.state.app.sessions.get_mut(i).ok_or_else(|| ToolError::NotFound("no such session".into()))?;
    let sid = s.id.clone();
    // Runs of removed commands are stopped (their item is gone, so nothing could stop them later).
    let keep: Vec<String> = items.iter().map(|x| x.id.clone()).collect();
    s.run_commands = items;
    let removed: Vec<String> =
        ctx.state.rt.runs.get(&sid).map(|v| v.iter().filter(|r| !keep.contains(&r.item_id)).map(|r| r.item_id.clone()).collect()).unwrap_or_default();
    for r in &removed {
        runs::stop(ctx.state, &sid, r);
    }
    if let Some(v) = ctx.state.rt.runs.get_mut(&sid) {
        v.retain(|r| keep.contains(&r.item_id));
    }
    ctx.state.persist();
    let data = list_json(ctx, &sid)?;
    Ok(ToolOutput::new(format!("Saved {} run commands.", keep.len()), data))
}
