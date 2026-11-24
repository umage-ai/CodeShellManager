//! `runs` tools: a session's run commands (`run_list`, `run_start`, `run_restart`, `run_stop`, `run_output`,
//! `run_edit`, `run_set`, `run_remove`). The work is in [`crate::runs`].
//!
//! The editing tools decide what `run_start` (and F5) will execute, so they declare
//! `LAUNCHES_PROCESS`: an MCP client gets a consent card (which shows the params, so the command
//! lines) before its edit is applied. A client that could rewrite the default command silently
//! would turn the person's next F5 into its own command line. The app and the CLI (the person)
//! are not asked.

use csm_core::models::{RunCommandItem, RunMode, SessionKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::runs::templates::Seed;
use crate::runs::{self, MAX_RUN_COMMANDS, RunPhase};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

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
            "Start a run command (default: the session's default) in its own background terminal; never types into the session. If it is already running, does nothing and says so (run_restart restarts it).",
            Effects::LAUNCHES_PROCESS,
            schema_of::<RunParams>,
            run_start,
        ),
        t(
            "run_restart",
            "Restart run",
            "Stop a run command (default: the default one) if it is running, with everything it started, and start it again. Discards the running process, so MCP clients are asked first.",
            Effects::from_bits_truncate(Effects::LAUNCHES_PROCESS.bits() | Effects::MAY_DISCARD_DATA.bits()),
            schema_of::<RunParams>,
            run_restart,
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
            "Replace a session's run commands (label, command line, mode process/powershell, URL, default), `detect: true` to take the project-type template instead, or `import: true` to add the template's commands that aren't in the list yet (a Node project's package.json scripts). Asks MCP clients first: it decides what F5 runs.",
            EDIT_EFFECTS,
            schema_of::<EditParams>,
            run_edit,
        ),
        t(
            "run_set",
            "Set a run command",
            "Add one run command, or update the one whose id or label matches (`id`, else `label`); the rest of the list is kept. Asks MCP clients first: it decides what F5 runs.",
            Effects::from_bits_truncate(Effects::MUTATES_STATE.bits() | Effects::LAUNCHES_PROCESS.bits()),
            schema_of::<SetParams>,
            run_set,
        ),
        t(
            "run_remove",
            "Remove a run command",
            "Remove one run command (by id or label) and stop its run. If it was the default, the first remaining one becomes the default. Asks MCP clients first.",
            EDIT_EFFECTS,
            schema_of::<RunIdParams>,
            run_remove,
        ),
    ]
}

/// Editing can stop runs (of removed commands) and decides what F5 starts.
const EDIT_EFFECTS: Effects =
    Effects::from_bits_truncate(Effects::MUTATES_STATE.bits() | Effects::LAUNCHES_PROCESS.bits() | Effects::MAY_DISCARD_DATA.bits());

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
    /// Opened in the browser once a loopback server starts listening there, or after the run
    /// exits 0. Absolute http/https only.
    #[serde(default)]
    post_run_url: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EditParams {
    session_id: String,
    /// The new list, in order. Exactly one ends up the default (the last marked, else the first).
    #[serde(default)]
    runs: Option<Vec<Value>>,
    /// Replace the list with the project-type template of the session's folder instead.
    #[serde(default)]
    detect: bool,
    /// Add the folder's template commands whose command line isn't in the list yet (a Node
    /// project: its package.json scripts), keeping the existing commands and default.
    #[serde(default)]
    import: bool,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParams {
    session_id: String,
    /// The id or label of the command to update. Without it, the command with this `label` is
    /// updated, else a new one is added.
    #[serde(default)]
    id: Option<String>,
    /// Required for a new command.
    #[serde(default)]
    label: Option<String>,
    /// One line (chain commands with &&). Required for a new command.
    #[serde(default)]
    command_line: Option<String>,
    /// Make it the default (the Run button and F5).
    #[serde(default)]
    is_default: Option<bool>,
    #[serde(default)]
    mode: Option<ModeParam>,
    /// Opened in the browser once a loopback server starts listening there, or after the run
    /// exits 0. Absolute http/https only; "" removes it.
    #[serde(default)]
    post_run_url: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RunIdParams {
    session_id: String,
    /// A run command's id (or label).
    run_id: String,
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
    launch(ctx, params, false)
}

fn run_restart(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    launch(ctx, params, true)
}

/// Checks and launches; `restart` decides what an already running run does.
fn launch(ctx: &mut Ctx<'_>, params: Value, restart: bool) -> Reply {
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
    let phase = ctx.state.rt.runs.get(&s.id).and_then(|v| v.iter().find(|r| r.item_id == item.id)).map(|r| r.phase);
    let running = phase.is_some_and(RunPhase::is_running);
    if running && !restart {
        return Ok(ToolOutput::new(
            format!("{} is already running; nothing was changed. run_restart restarts it, run_stop stops it.", item.label),
            json!({"session_id": s.id, "run_id": item.id, "already_running": true, "phase": phase}),
        ));
    }
    runs::start(ctx, &s, &item);
    let msg = if running { format!("Restarted {}.", item.label) } else { format!("Started {} in its own background terminal.", item.label) };
    Ok(ToolOutput::new(msg, json!({"session_id": s.id, "run_id": item.id, "already_running": false, "restarted": running})))
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
    let item = RunCommandItem {
        id: p.id.filter(|i| valid_id(i)).unwrap_or_default(),
        label: p.label,
        command_line: p.command_line,
        is_default: p.is_default,
        mode: mode_of(p.mode),
        post_run_url: p.post_run_url,
    };
    validate(item).map_err(|e| ToolError::InvalidParams(format!("runs[{index}]: {e}")))
}

fn valid_id(i: &str) -> bool {
    !i.trim().is_empty() && i.len() <= 64 && !i.chars().any(char::is_control)
}

fn mode_of(m: Option<ModeParam>) -> RunMode {
    match m {
        Some(ModeParam::Powershell) => RunMode::PowerShell,
        _ => RunMode::Process,
    }
}

/// The checks every editing tool applies: a label, a one-line command line, an http(s) URL.
/// Returns the cleaned item.
fn validate(mut item: RunCommandItem) -> Result<RunCommandItem, String> {
    let label = clean_name(&item.label, 80);
    if label.is_empty() {
        return Err("give it a label".into());
    }
    if let Some(e) = runs::command_line_error(&item.command_line) {
        return Err(format!("{label}: {e}"));
    }
    let url = item.post_run_url.take().map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
    if let Some(u) = &url
        && runs::launchable_url(u).is_none()
    {
        return Err(format!("{label}: the URL must be an absolute http:// or https:// address"));
    }
    item.label = label;
    item.command_line = item.command_line.trim().to_owned();
    item.post_run_url = url;
    Ok(item)
}

/// Stores `items` as the session's list (one default), stops runs whose command is gone, saves.
fn store(ctx: &mut Ctx<'_>, i: usize, mut items: Vec<RunCommandItem>) -> Result<String, ToolError> {
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
    Ok(sid)
}

/// Adds `seed`'s commands whose command line isn't in `list` yet, after the existing ones and
/// within [`MAX_RUN_COMMANDS`] (in template order; the template's default is kept first when room
/// is short and the list was empty). The existing default stays; an empty list takes the
/// template's. Returns (added, left out).
pub(crate) fn merge_import(list: &mut Vec<RunCommandItem>, seed: Seed) -> (usize, usize) {
    let had = !list.is_empty();
    let fresh: Vec<RunCommandItem> =
        seed.items.into_iter().filter(|t| !list.iter().any(|e| e.command_line.trim() == t.command_line.trim())).collect();
    let room = MAX_RUN_COMMANDS.saturating_sub(list.len());
    let left_out = seed.left_out + fresh.len().saturating_sub(room);
    let mut added = 0;
    for mut t in fresh.into_iter().take(room) {
        if had {
            t.is_default = false;
        }
        list.push(t);
        added += 1;
    }
    RunCommandItem::ensure_single_default(list);
    (added, left_out)
}

fn left_out_note(n: usize) -> String {
    if n == 0 { String::new() } else { format!(" {n} left out (at most {MAX_RUN_COMMANDS} per session).") }
}

fn run_edit(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: EditParams = parse_params(params)?;
    if usize::from(p.runs.is_some()) + usize::from(p.detect) + usize::from(p.import) > 1 {
        return Err(ToolError::InvalidParams("give one of `runs`, `detect: true` or `import: true`".into()));
    }
    let i = find(ctx.state, &p.session_id)?;
    if p.detect || p.import {
        let import = p.import;
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
                    Some(seed) => match ctx.state.app.sessions.iter().position(|s| s.id == sid) {
                        None => Err(ToolError::NotFound("the session was closed".into())),
                        Some(i) => apply_seed(ctx, i, seed, import),
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
    let list = p.runs.unwrap_or_default();
    if list.len() > MAX_RUN_COMMANDS {
        return Err(ToolError::InvalidParams(format!("at most {MAX_RUN_COMMANDS} run commands")));
    }
    let items = list.into_iter().enumerate().map(|(n, v)| parse_item(v, n)).collect::<Result<Vec<_>, _>>()?;
    let n = items.len();
    let sid = store(ctx, i, items)?;
    let data = list_json(ctx, &sid)?;
    Ok(ToolOutput::new(format!("Saved {n} run commands."), data))
}

/// `detect` (replace the list with the template) or `import` (add what's missing).
fn apply_seed(ctx: &mut Ctx<'_>, i: usize, seed: Seed, import: bool) -> Reply {
    let kind = seed.kind;
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound("no such session".into()))?;
    let (items, added, left_out) = if import {
        let mut list = s.run_commands.clone();
        let (added, left_out) = merge_import(&mut list, seed);
        (list, added, left_out)
    } else {
        let n = seed.items.len();
        (seed.items, n, seed.left_out)
    };
    let sid = store(ctx, i, items)?;
    let list = list_json(ctx, &sid)?;
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let msg = match (import, added) {
        (true, 0) => format!("Nothing to add: the {kind} commands are all in the list already.{}", left_out_note(left_out)),
        (true, n) => format!("Added {n} {kind} run command{}.{}", plural(n), left_out_note(left_out)),
        (false, n) => format!("Added the {kind} run commands ({n}).{}", left_out_note(left_out)),
    };
    Ok(ToolOutput::new(msg, json!({"template": kind, "added": added, "left_out": left_out, "runs": list})))
}

fn run_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SetParams = parse_params(params)?;
    let i = find(ctx.state, &p.session_id)?;
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound("no such session".into()))?;
    let mut list = s.run_commands.clone();
    let position = |list: &[RunCommandItem], key: &str| list.iter().position(|x| x.id == key || x.label.eq_ignore_ascii_case(key.trim()));
    let at = match (p.id.as_deref().filter(|k| !k.trim().is_empty()), p.label.as_deref()) {
        (Some(key), _) => Some(position(&list, key).ok_or_else(|| ToolError::NotFound(format!("no run command `{key}`; run_list shows them")))?),
        (None, Some(label)) => Some(clean_name(label, 80)).filter(|l| !l.is_empty()).and_then(|l| position(&list, &l)),
        (None, None) => None,
    };
    let existing = at.and_then(|n| list.get(n).cloned());
    let added = existing.is_none();
    let item = match existing {
        Some(mut it) => {
            if let Some(l) = p.label {
                it.label = l;
            }
            if let Some(c) = p.command_line {
                it.command_line = c;
            }
            if let Some(d) = p.is_default {
                it.is_default = d;
            }
            if let Some(m) = p.mode {
                it.mode = mode_of(Some(m));
            }
            if let Some(u) = p.post_run_url {
                it.post_run_url = Some(u);
            }
            it
        }
        None => {
            if list.len() >= MAX_RUN_COMMANDS {
                return Err(ToolError::InvalidParams(format!("at most {MAX_RUN_COMMANDS} run commands; remove one first")));
            }
            let (Some(label), Some(command_line)) = (p.label, p.command_line) else {
                return Err(ToolError::InvalidParams("a new run command needs a `label` and a `command_line`".into()));
            };
            RunCommandItem {
                id: crate::lifecycle::new_id(),
                label,
                command_line,
                is_default: p.is_default.unwrap_or(list.is_empty()),
                mode: mode_of(p.mode),
                post_run_url: p.post_run_url,
            }
        }
    };
    let item = validate(item).map_err(ToolError::InvalidParams)?;
    let (id, label, make_default) = (item.id.clone(), item.label.clone(), item.is_default);
    match at.filter(|_| !added).and_then(|n| list.get_mut(n)) {
        Some(slot) => *slot = item,
        None => list.push(item),
    }
    if make_default {
        // This one wins, wherever it sits in the list.
        for x in &mut list {
            x.is_default = x.id == id;
        }
    }
    let sid = store(ctx, i, list)?;
    let data = list_json(ctx, &sid)?;
    let verb = if added { "Added" } else { "Updated" };
    Ok(ToolOutput::new(format!("{verb} {label}."), json!({"id": id, "added": added, "runs": data})))
}

fn run_remove(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: RunIdParams = parse_params(params)?;
    let i = find(ctx.state, &p.session_id)?;
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound("no such session".into()))?;
    let mut list = s.run_commands.clone();
    let key = p.run_id.trim();
    let n = list
        .iter()
        .position(|x| x.id == key || x.label.eq_ignore_ascii_case(key))
        .ok_or_else(|| ToolError::NotFound(format!("no run command `{key}`; run_list shows them")))?;
    let gone = list.remove(n);
    let sid = store(ctx, i, list)?;
    let data = list_json(ctx, &sid)?;
    Ok(ToolOutput::new(format!("Removed {}.", gone.label), json!({"id": gone.id, "runs": data})))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runs::templates::{Seed, monorepo_package_json, node_scripts};

    fn item(label: &str, cmd: &str, default: bool) -> RunCommandItem {
        RunCommandItem { id: label.into(), label: label.into(), command_line: cmd.into(), is_default: default, ..Default::default() }
    }

    #[test]
    fn import_adds_only_missing_commands_and_keeps_the_default() {
        let (items, left_out) = node_scripts("npm", r#"{"scripts":{"dev":"vite","build":"vite build","lint":"eslint ."}}"#).unwrap();
        let mut list = vec![item("Mine", "npm run build", false), item("Serve", "python -m http.server", true)];
        let (added, lo) = merge_import(&mut list, Seed { kind: "node", items, left_out });
        assert_eq!((added, lo), (2, 0));
        assert_eq!(
            list.iter().map(|i| i.command_line.as_str()).collect::<Vec<_>>(),
            ["npm run build", "python -m http.server", "npm run dev", "npm run lint"]
        );
        assert_eq!(list.iter().filter(|i| i.is_default).map(|i| i.label.as_str()).collect::<Vec<_>>(), ["Serve"], "the person's default stays");
        assert_eq!(list[0].label, "Mine", "edits kept");
        // Importing again adds nothing.
        let (items, left_out) = node_scripts("npm", r#"{"scripts":{"dev":"vite","build":"vite build","lint":"eslint ."}}"#).unwrap();
        assert_eq!(merge_import(&mut list, Seed { kind: "node", items, left_out }), (0, 0));
        assert_eq!(list.len(), 4);
    }

    #[test]
    fn importing_a_big_monorepo_respects_the_cap_and_counts_what_is_left_out() {
        let seed = || {
            let (items, left_out) = node_scripts("pnpm", &monorepo_package_json()).unwrap();
            Seed { kind: "node", items, left_out }
        };
        // Into an empty list: the template as is, `dev` the default.
        let mut empty = Vec::new();
        assert_eq!(merge_import(&mut empty, seed()), (MAX_RUN_COMMANDS, 41));
        assert_eq!(empty.iter().filter(|i| i.is_default).map(|i| i.label.as_str()).collect::<Vec<_>>(), ["dev"]);
        // Into a list that already has 45, two of them template commands: 5 room.
        let mut list: Vec<RunCommandItem> = (0..43).map(|n| item(&format!("own{n}"), &format!("echo {n}"), n == 7)).collect();
        list.push(item("b", "pnpm run build", false));
        list.push(item("w", "pnpm run build:web", false));
        let (added, left_out) = merge_import(&mut list, seed());
        assert_eq!(added, 5);
        assert_eq!(left_out, 41 + (MAX_RUN_COMMANDS - 2 - 5), "the cap's own count plus what didn't fit");
        assert_eq!(list.len(), MAX_RUN_COMMANDS);
        let cmds: Vec<&str> = list.iter().map(|i| i.command_line.as_str()).collect();
        assert_eq!(&cmds[45..], ["pnpm run test:e2e:ci", "pnpm run pkg0:build", "pnpm run pkg1:build", "pnpm run pkg2:build", "pnpm run pkg3:build"]);
        let mut uniq = cmds.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), cmds.len(), "no duplicates");
        assert_eq!(list.iter().filter(|i| i.is_default).map(|i| i.label.as_str()).collect::<Vec<_>>(), ["own7"]);
        // Full: nothing more is added.
        assert_eq!(merge_import(&mut list, seed()).0, 0);
    }
}
