//! "Open folder" and "Open with…" (editors and IDEs): `open_folder`, `open_with`,
//! `open_with_list`, `open_with_set`, `open_with_detect`, plus the one-time seeding of the list.
//!
//! Every rule is in `csm-services::open_with` (pure): templates are split first and placeholders
//! substituted inside their own token, nothing option-shaped, programs absolute and not a shell,
//! not under `%SystemRoot%`, batch arguments with cmd metacharacters refused. This module does
//! the I/O those rules ask about — always on a worker, never the engine or UI thread — and
//! launches the program with **argv** (`crate::proc`, no shell, `CREATE_NO_WINDOW`).
//!
//! **Open folder** (`FolderOpener`): only an existing DIRECTORY is ever opened (ShellExecute or
//! `open` on a *file* runs it, and a folder comes from state.json and imports). On Windows the
//! path is normalised to backslashes and handed to `explorer.exe` as its only argument, always
//! quoted: measured on Windows 11 (asking the Shell which folder each new window showed), a
//! forward-slash path and an UNQUOTED path with a comma both opened Documents instead, while the
//! quoted backslash form opened the right folder (comma, spaces, drive root). macOS uses
//! `/usr/bin/open`, Linux `xdg-open`.
//!
//! `open_with_set` decides what runs on a click, so MCP clients are refused (as `settings_set`
//! refuses `OpenWith`); the CLI and the app may.

use std::path::{Path, PathBuf};
use std::time::Duration;

use csm_core::models::{OpenWithEntry as CoreEntry, SessionKind, ShellSession};
use csm_services::open_with::detect::{self, DetectEnv, Platform};
use csm_services::open_with::{self as ow, OpenWithContext, OpenWithEntry as SvcEntry, Rules};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

use super::find;

pub(super) fn specs() -> Vec<ToolSpec> {
    let t = |id, title, summary, effects, input_schema, run| ToolSpec { id, title, summary, toolset: Toolset::App, effects, input_schema, run };
    let state_launch = Effects::from_bits_truncate(Effects::MUTATES_STATE.bits() | Effects::LAUNCHES_PROCESS.bits());
    vec![
        t(
            "open_folder",
            "Open folder",
            "Open the session's folder in the file manager (existing folders only; local and WSL sessions).",
            Effects::LAUNCHES_PROCESS,
            schema_of::<SessionParams>,
            open_folder,
        ),
        t(
            "open_with",
            "Open with…",
            "Open the session's folder in an editor from the Open with list (default: the default entry). `entry` is an entry id or label.",
            Effects::LAUNCHES_PROCESS,
            schema_of::<OpenWithParams>,
            open_with,
        ),
        t(
            "open_url",
            "Open link",
            "Open an absolute http or https URL in the default browser (a link clicked in a terminal). Other schemes, relative URLs and URLs with spaces or quotes are refused; no shell is involved.",
            Effects::LAUNCHES_PROCESS,
            schema_of::<UrlParams>,
            open_url,
        ),
        t(
            "open_with_list",
            "Open with: editors",
            "The Open with list; with `session_id`, also the exact command each entry would run for that session.",
            Effects::READ_ONLY,
            schema_of::<ListParams>,
            open_with_list,
        ),
        t(
            "open_with_set",
            "Edit the Open with list…",
            "Replace the Open with list (label, program, argument templates per session kind, default). Checked and cleaned; app and CLI only.",
            Effects::MUTATES_STATE,
            schema_of::<SetParams>,
            open_with_set,
        ),
        t(
            "open_with_detect",
            "Detect installed editors",
            "Look for installed editors and IDEs and add the ones not listed yet.",
            state_launch,
            empty_schema,
            open_with_detect,
        ),
        t(
            "open_with_pending",
            "Editors from CodeShellManager 0.x",
            "The Open with entries imported from CodeShellManager 0.x that wait for the person's answer, each with the program and arguments it would run. open_with_pending_answer adds or discards them.",
            Effects::READ_ONLY,
            empty_schema,
            open_with_pending,
        ),
        t(
            "open_with_pending_answer",
            "Use the editors from CodeShellManager 0.x…",
            "Answer the pending Open with entries from CodeShellManager 0.x (open_with_pending lists them): adopt_open_with: true adds the ones not listed yet (0.x's default becomes the default), false discards them. App and CLI only.",
            Effects::MUTATES_STATE,
            schema_of::<PendingAnswerParams>,
            open_with_pending_answer,
        ),
    ]
}

// ── Model conversion ─────────────────────────────────────────────────────────────────────

fn to_svc(e: &CoreEntry) -> SvcEntry {
    SvcEntry {
        id: e.id.clone(),
        label: e.label.clone(),
        executable: e.executable.clone(),
        local_args: e.local_args.clone(),
        wsl_args: e.wsl_args.clone(),
        ssh_args: e.ssh_args.clone(),
        is_default: e.is_default,
    }
}

fn to_core(e: SvcEntry) -> CoreEntry {
    CoreEntry {
        id: e.id,
        label: e.label,
        executable: e.executable,
        local_args: e.local_args,
        wsl_args: e.wsl_args,
        ssh_args: e.ssh_args,
        is_default: e.is_default,
    }
}

fn svc_kind(k: SessionKind) -> csm_services::launch::SessionKind {
    match k {
        SessionKind::Local => csm_services::launch::SessionKind::Local,
        SessionKind::Ssh => csm_services::launch::SessionKind::Ssh,
        SessionKind::Wsl => csm_services::launch::SessionKind::Wsl,
    }
}

fn context_of(s: &ShellSession) -> OpenWithContext {
    OpenWithContext {
        kind: svc_kind(s.kind),
        folder: s.working_folder.trim().to_owned(),
        distro: s.wsl_distro.clone(),
        linux_path: s.wsl_working_folder.clone(),
        wsl_user: s.wsl_user.clone(),
        user: s.ssh_user.clone(),
        host: s.ssh_host.clone(),
        port: s.ssh_port,
        remote_folder: s.ssh_remote_folder.clone(),
    }
}

/// The program's file name (shown beside every menu item, so a label can't hide what runs).
pub fn program_name(path: &str) -> String {
    path.rsplit(['\\', '/']).next().unwrap_or(path).to_owned()
}

/// One entry as the menus show it for one session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenWithTarget {
    pub entry_id: String,
    pub label: String,
    /// The program's file name.
    pub program: String,
    pub is_default: bool,
    /// The exact command this session would run (or the program and why it can't).
    pub command: String,
}

/// The entries that apply to a session's kind, with their resolved commands (pure).
pub(crate) fn targets(list: &[CoreEntry], s: &ShellSession) -> Vec<OpenWithTarget> {
    let ctx = context_of(s);
    let kind = ctx.kind;
    let svc: Vec<SvcEntry> = list.iter().map(to_svc).collect();
    let default_id = ow::default_for(&svc, kind).map(|e| e.id.clone());
    svc.iter()
        .filter(|e| e.applies_to(kind))
        .map(|e| OpenWithTarget {
            entry_id: e.id.clone(),
            label: e.label.clone(),
            program: program_name(&e.executable),
            is_default: default_id.as_deref() == Some(e.id.as_str()),
            command: ow::describe_for(e, &ctx, Rules::native()),
        })
        .collect()
}

/// The whole list as the editor shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OpenWithView {
    pub id: String,
    pub label: String,
    pub executable: String,
    pub program: String,
    pub local_args: Option<String>,
    pub wsl_args: Option<String>,
    pub ssh_args: Option<String>,
    pub is_default: bool,
}

pub(crate) fn list_view(list: &[CoreEntry]) -> Vec<OpenWithView> {
    list.iter()
        .map(|e| OpenWithView {
            id: e.id.clone(),
            label: e.label.clone(),
            executable: e.executable.clone(),
            program: program_name(&e.executable),
            local_args: e.local_args.clone(),
            wsl_args: e.wsl_args.clone(),
            ssh_args: e.ssh_args.clone(),
            is_default: e.is_default,
        })
        .collect()
}

// ── Start-up: sanitise the loaded list, seed it once ─────────────────────────────────────

/// The list from state.json is untrusted: clean it (pure), then, when it was never seeded,
/// detect editors on a worker. `OpenWithSeeded` is set only when something was found, so a
/// machine with nothing installed picks up an editor installed later.
pub(crate) fn on_start(ctx: &mut Ctx<'_>) {
    let loaded: Vec<Option<SvcEntry>> = ctx.state.app.settings.open_with.iter().map(|e| Some(to_svc(e))).collect();
    if !loaded.is_empty() {
        let clean = ow::sanitize_for(&loaded, true, Rules::native(), &mut |m| log::warn!("open with: {m}"));
        let clean: Vec<CoreEntry> = clean.into_iter().map(to_core).collect();
        if clean != ctx.state.app.settings.open_with {
            ctx.state.app.settings.open_with = clean;
            ctx.state.persist();
        }
    }
    if ctx.state.options.seed_editors && !ctx.state.options.clean && !ctx.state.app.settings.open_with_seeded {
        let engine = ctx.engine.clone();
        let _ = std::thread::Builder::new().name("csm-detect-editors".into()).spawn(move || {
            let found = detect::detect(&RealEnv);
            engine.post(Box::new(move |ctx| {
                let added = merge_detected(ctx, found);
                if added > 0 {
                    log::info!("open with: seeded {added} editors");
                }
            }));
        });
    }
}

/// Adds detected entries whose program isn't listed yet. Returns how many were added.
fn merge_detected(ctx: &mut Ctx<'_>, found: Vec<SvcEntry>) -> usize {
    let list = &mut ctx.state.app.settings.open_with;
    let mut added = 0;
    for e in found {
        let same = |x: &CoreEntry| x.executable.eq_ignore_ascii_case(&e.executable) && x.local_args == e.local_args;
        if !list.iter().any(same) {
            let mut c = to_core(e);
            c.is_default = c.is_default && list.is_empty();
            list.push(c);
            added += 1;
        }
    }
    CoreEntry::ensure_single_default(list);
    if added > 0 || !list.is_empty() {
        ctx.state.app.settings.open_with_seeded = true;
    }
    ctx.state.persist();
    added
}

// ── Tools ────────────────────────────────────────────────────────────────────────────────

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SessionParams {
    session_id: String,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct UrlParams {
    /// An absolute `http://` or `https://` URL.
    url: String,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct OpenWithParams {
    session_id: String,
    /// An entry id or label (case-insensitive). Default: the default entry for the session's kind.
    entry: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ListParams {
    /// Also resolve each entry's command for this session.
    session_id: Option<String>,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct EntryParam {
    /// Keep an existing entry's id; new entries get one.
    #[serde(default)]
    id: Option<String>,
    label: String,
    /// Absolute path to the program (never a shell).
    executable: String,
    /// Argument template for local sessions, e.g. `{folder}`. Blank: not offered there.
    #[serde(default)]
    local_args: Option<String>,
    /// For WSL sessions: `{folder}` (the `\\wsl$` mirror), `{distro}`, `{linuxPath}`.
    #[serde(default)]
    wsl_args: Option<String>,
    /// For SSH sessions: `{user}`, `{host}`, `{userAtHost}`, `{port}`, `{remoteFolder}`.
    #[serde(default)]
    ssh_args: Option<String>,
    #[serde(default)]
    is_default: bool,
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SetParams {
    entries: Vec<EntryParam>,
}

fn session_clone(ctx: &Ctx<'_>, id: &str) -> Result<ShellSession, ToolError> {
    let i = find(ctx.state, id)?;
    ctx.state.app.sessions.get(i).cloned().ok_or_else(|| ToolError::NotFound(format!("no session `{id}`")))
}

/// Runs `work` on a worker and answers the call with its result.
fn on_worker(ctx: &mut Ctx<'_>, name: &str, work: impl FnOnce() -> Reply + Send + 'static) -> Reply {
    let reply = ctx.park();
    let spawned = std::thread::Builder::new().name(name.to_owned()).spawn(move || {
        let r = work();
        if let Some(reply) = reply {
            reply.send(r);
        }
    });
    match spawned {
        Ok(_) => Ok(ToolOutput::message("Working.")),
        Err(e) => Err(ToolError::Failed(format!("could not start a worker: {e}"))),
    }
}

fn open_folder(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SessionParams = parse_params(params)?;
    let s = session_clone(ctx, &p.session_id)?;
    if s.kind == SessionKind::Ssh {
        return Err(ToolError::Refused("an SSH session's folder is on the remote host; there is no local folder to open".into()));
    }
    let folder = s.working_folder.trim().to_owned();
    if folder.is_empty() {
        return Err(ToolError::Refused("this session has no folder".into()));
    }
    on_worker(ctx, "csm-open-folder", move || {
        let Some(target) = folder_target(&folder) else {
            return Err(ToolError::Failed(format!("{folder} is not an existing folder")));
        };
        open_folder_now(&target).map_err(ToolError::Failed)?;
        Ok(ToolOutput::new(format!("Opened {target}."), json!({"folder": target})))
    })
}

/// A link from a terminal (or any caller): only what [`crate::runs::launchable_url`] accepts
/// (absolute http/https, no whitespace, quotes or shell metacharacters) reaches the opener, as
/// one argument, never through a shell.
fn open_url(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: UrlParams = parse_params(params)?;
    let Some(url) = crate::runs::launchable_url(&p.url) else {
        let shown: String = p.url.chars().filter(|c| !c.is_control()).take(200).collect();
        return Err(ToolError::Refused(format!("only absolute http and https links are opened, not `{shown}`")));
    };
    (ctx.state.options.url_opener)(url.clone());
    Ok(ToolOutput::new(format!("Opening {url} in the browser."), json!({"url": url})))
}

fn open_with(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: OpenWithParams = parse_params(params)?;
    let s = session_clone(ctx, &p.session_id)?;
    let ctxv = context_of(&s);
    let list: Vec<SvcEntry> = ctx.state.app.settings.open_with.iter().map(to_svc).collect();
    let entry = match p.entry.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        Some(want) => list
            .iter()
            .find(|e| e.id == want || e.label.eq_ignore_ascii_case(want))
            .cloned()
            .ok_or_else(|| ToolError::NotFound(format!("no Open with entry `{want}`; open_with_list shows them")))?,
        None => ow::default_for(&list, ctxv.kind).cloned().ok_or_else(|| {
            ToolError::NotFound("no editor in the Open with list applies to this session; add one (open_with_detect or the editor)".into())
        })?,
    };
    if !entry.applies_to(ctxv.kind) {
        return Err(ToolError::Refused(format!("\"{}\" has no arguments for {} sessions", entry.label, ow::kind_name(ctxv.kind))));
    }
    on_worker(ctx, "csm-open-with", move || {
        let host = RealHost;
        let cmd = if Rules::native() == Rules::Windows {
            ow::prepare(&entry, &ctxv, &host).map_err(ToolError::Failed)?
        } else {
            prepare_posix(&entry, &ctxv).map_err(ToolError::Failed)?
        };
        let shown = cmd.display();
        spawn_detached(Path::new(&cmd.executable), &cmd.arguments, None)
            .map_err(|e| ToolError::Failed(format!("could not start {}: {e}", entry.label)))?;
        Ok(ToolOutput::new(format!("Opening in {}.", entry.label), json!({"entry": entry.id, "label": entry.label, "command": shown})))
    })
}

fn open_with_list(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ListParams = parse_params(params)?;
    let list = &ctx.state.app.settings.open_with;
    let mut data = json!({"entries": list_view(list), "seeded": ctx.state.app.settings.open_with_seeded});
    if let Some(id) = p.session_id {
        let s = session_clone(ctx, &id)?;
        data["for_session"] = json!(targets(&ctx.state.app.settings.open_with, &s));
    }
    Ok(ToolOutput::new(format!("{} editors", ctx.state.app.settings.open_with.len()), data))
}

fn open_with_set(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if matches!(ctx.caller, Caller::External { .. }) {
        return Err(ToolError::Refused("the Open with list decides what runs on a click; edit it in the app".into()));
    }
    let p: SetParams = parse_params(params)?;
    if p.entries.len() > ow::MAX_ENTRIES {
        return Err(ToolError::InvalidParams(format!("at most {} entries", ow::MAX_ENTRIES)));
    }
    let raw: Vec<SvcEntry> = p
        .entries
        .into_iter()
        .map(|e| SvcEntry {
            id: e.id.unwrap_or_default(),
            label: e.label,
            executable: e.executable.trim().to_owned(),
            local_args: e.local_args.filter(|a| !a.trim().is_empty()),
            wsl_args: e.wsl_args.filter(|a| !a.trim().is_empty()),
            ssh_args: e.ssh_args.filter(|a| !a.trim().is_empty()),
            is_default: e.is_default,
        })
        .collect();
    // Say exactly what is wrong instead of silently dropping it (sanitize drops).
    for e in &raw {
        let name = if e.label.trim().is_empty() { program_name(&e.executable) } else { e.label.clone() };
        if let Some(err) = ow::validate_executable_for(&e.executable, true, Rules::native()) {
            return Err(ToolError::InvalidParams(format!("{name}: {err}")));
        }
        if e.local_args.is_none() && e.wsl_args.is_none() && e.ssh_args.is_none() {
            return Err(ToolError::InvalidParams(format!("{name}: give arguments for at least one session kind, e.g. {{folder}}")));
        }
        for (kind, t) in [(SessionKind::Local, &e.local_args), (SessionKind::Wsl, &e.wsl_args), (SessionKind::Ssh, &e.ssh_args)] {
            if let Some(t) = t
                && let Some(err) = ow::validate_template(t, svc_kind(kind))
            {
                return Err(ToolError::InvalidParams(format!("{name} ({} arguments): {err}", ow::kind_name(svc_kind(kind)))));
            }
        }
    }
    // The real path of each program (8.3 names, links, network drives, a renamed shell) is a
    // disk question: on a worker, then applied on the engine thread.
    let engine = ctx.engine.clone();
    let reply = ctx.park();
    let spawned = std::thread::Builder::new().name("csm-open-with-set".into()).spawn(move || {
        let problems: Vec<String> = raw
            .iter()
            .filter_map(|e| {
                on_disk_error(&e.executable)
                    .map(|err| format!("{}: {err}", if e.label.trim().is_empty() { program_name(&e.executable) } else { e.label.clone() }))
            })
            .collect();
        engine.post(Box::new(move |ctx| {
            let r = if let Some(first) = problems.first() {
                Err(ToolError::InvalidParams(first.clone()))
            } else {
                let wrapped: Vec<Option<SvcEntry>> = raw.into_iter().map(Some).collect();
                let clean: Vec<CoreEntry> =
                    ow::sanitize_for(&wrapped, true, Rules::native(), &mut |m| log::warn!("open with: {m}")).into_iter().map(to_core).collect();
                let n = clean.len();
                ctx.state.app.settings.open_with = clean;
                ctx.state.app.settings.open_with_seeded = true;
                ctx.state.persist();
                Ok(ToolOutput::new(format!("Saved {n} editors."), json!({"entries": list_view(&ctx.state.app.settings.open_with)})))
            };
            crate::lifecycle::after_step(ctx.state, ctx.engine);
            if let Some(reply) = reply {
                reply.send(r);
            }
        }));
    });
    match spawned {
        Ok(_) => Ok(ToolOutput::message("Saving.")),
        Err(e) => Err(ToolError::Failed(format!("could not start a worker: {e}"))),
    }
}

fn open_with_detect(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let engine = ctx.engine.clone();
    let reply = ctx.park();
    let spawned = std::thread::Builder::new().name("csm-detect-editors".into()).spawn(move || {
        let found = detect::detect(&RealEnv);
        let total = found.len();
        engine.post(Box::new(move |ctx| {
            let added = merge_detected(ctx, found);
            crate::lifecycle::after_step(ctx.state, ctx.engine);
            let msg = match (total, added) {
                (0, _) => "No editors found.".to_owned(),
                (_, 0) => format!("Found {total} editors; all were already listed."),
                (_, n) => format!("Added {n} editors."),
            };
            if let Some(reply) = reply {
                reply
                    .send(Ok(ToolOutput::new(msg, json!({"found": total, "added": added, "entries": list_view(&ctx.state.app.settings.open_with)}))));
            }
        }));
    });
    match spawned {
        Ok(_) => Ok(ToolOutput::message("Detecting.")),
        Err(e) => Err(ToolError::Failed(format!("could not start a worker: {e}"))),
    }
}

// ── Open with entries from CodeShellManager 0.x (`state_import_wpf`) ─────────────────────

/// The pending answer tool decides what runs on a click: refused for MCP clients before any
/// consent card (`tools::precheck`).
pub(super) fn precheck(id: &str, caller: &Caller) -> Option<ToolError> {
    (id == "open_with_pending_answer" && caller.is_external())
        .then(|| ToolError::Refused("the Open with list decides what runs on a click; answer in the app".into()))
}

/// True when `e` launches the same program with the same local arguments as one in `list`.
fn listed(list: &[CoreEntry], e: &CoreEntry) -> bool {
    list.iter().any(|x| x.executable.eq_ignore_ascii_case(&e.executable) && x.local_args == e.local_args)
}

/// Offers an imported (already sanitised) Open with list for consent: the entries not listed
/// yet become `PendingOpenWith`, replacing an earlier offer. Returns how many wait. Nothing is
/// added to the list itself.
pub(crate) fn offer_pending(ctx: &mut Ctx<'_>, imported: Vec<CoreEntry>) -> usize {
    let ours = &ctx.state.app.settings.open_with;
    let mut pending: Vec<CoreEntry> = Vec::new();
    for e in imported {
        if !listed(ours, &e) && !listed(&pending, &e) {
            pending.push(e);
        }
    }
    let n = pending.len();
    if n > 0 {
        ctx.state.app.pending_open_with = pending;
    }
    n
}

fn open_with_pending(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let list = &ctx.state.app.pending_open_with;
    let review = if list.is_empty() { String::new() } else { describe_for_review(list) };
    Ok(ToolOutput::new(
        if list.is_empty() { "No editors from CodeShellManager 0.x are waiting.".to_owned() } else { format!("{} editors wait for your answer.", list.len()) },
        json!({"entries": list_view(list), "review": review}),
    ))
}

#[derive(Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct PendingAnswerParams {
    /// true: add the pending editors to the Open with list. false: discard them.
    adopt_open_with: bool,
}

fn open_with_pending_answer(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if ctx.caller.is_external() {
        return Err(ToolError::Refused("the Open with list decides what runs on a click; answer in the app".into()));
    }
    let p: PendingAnswerParams = parse_params(params)?;
    let pending = std::mem::take(&mut ctx.state.app.pending_open_with);
    if pending.is_empty() {
        return Ok(ToolOutput::new("No editors from CodeShellManager 0.x were waiting.", json!({"added": 0, "discarded": 0})));
    }
    if !p.adopt_open_with {
        let n = pending.len();
        ctx.state.persist();
        return Ok(ToolOutput::new(format!("Discarded {n} editors from CodeShellManager 0.x."), json!({"added": 0, "discarded": n})));
    }
    // Checked again (pure rules; the programs were checked on disk at import time).
    let wrapped: Vec<Option<SvcEntry>> = pending.iter().map(|e| Some(to_svc(e))).collect();
    let clean: Vec<CoreEntry> =
        ow::sanitize_for(&wrapped, true, Rules::native(), &mut |m| log::warn!("open with, from 0.x: {m}")).into_iter().map(to_core).collect();
    let list = &mut ctx.state.app.settings.open_with;
    let mut added = 0;
    let mut new_default: Option<String> = None;
    for e in clean {
        if list.len() >= ow::MAX_ENTRIES {
            break;
        }
        if e.is_default {
            new_default = Some(e.id.clone());
        }
        if !listed(list, &e) {
            let mut e = e;
            if e.id.trim().is_empty() || list.iter().any(|x| x.id == e.id) {
                e.id = format!("wpf-{}", list.len() + added + 1);
            }
            if e.is_default {
                new_default = Some(e.id.clone());
            }
            list.push(e);
            added += 1;
        }
    }
    // The person chose to use their 0.x editors: its default is the default here too.
    if let Some(id) = new_default
        && list.iter().any(|x| x.id == id)
    {
        for x in list.iter_mut() {
            x.is_default = x.id == id;
        }
    }
    CoreEntry::ensure_single_default(list);
    ctx.state.app.settings.open_with_seeded = true;
    ctx.state.persist();
    Ok(ToolOutput::new(
        format!("Added {added} editors from CodeShellManager 0.x."),
        json!({"added": added, "discarded": 0, "entries": list_view(&ctx.state.app.settings.open_with)}),
    ))
}

// ── Folder opening (FolderOpener) ────────────────────────────────────────────────────────

/// The folder to hand to the file manager, or `None` when it must not be opened: not absolute,
/// or not an existing DIRECTORY. Windows: backslashes, no trailing separator except a drive root
/// (the form every Windows tool accepts; git prints forward slashes). Touches the disk.
pub fn folder_target(folder: &str) -> Option<String> {
    let f = folder.trim();
    if f.is_empty() || f.chars().any(char::is_control) || f.contains('"') {
        return None;
    }
    let normal = normalise_folder(f, cfg!(windows))?;
    Path::new(&normal).is_dir().then_some(normal)
}

/// The pure part of [`folder_target`].
pub fn normalise_folder(f: &str, windows: bool) -> Option<String> {
    if windows {
        if !csm_services::winpath::is_fully_qualified(f) {
            return None;
        }
        let full = csm_services::winpath::normalize_full(&f.replace('/', "\\"));
        let trimmed = full.trim_end_matches('\\');
        Some(if trimmed.ends_with(':') { format!("{trimmed}\\") } else { trimmed.to_owned() })
    } else {
        if !f.starts_with('/') || f.split('/').any(|s| s == "..") {
            return None;
        }
        let t = f.trim_end_matches('/');
        Some(if t.is_empty() { "/".to_owned() } else { t.to_owned() })
    }
}

/// The file manager's argv for an already-validated folder: the program and, on Windows, the
/// raw (always quoted) argument.
pub fn folder_command(target: &str, platform: Platform) -> (PathBuf, Vec<String>, Option<String>) {
    match platform {
        Platform::Windows => (system32_or_windows("explorer.exe", false), Vec::new(), Some(format!("\"{target}\""))),
        Platform::Mac => (PathBuf::from("/usr/bin/open"), vec![target.to_owned()], None),
        Platform::Linux => (PathBuf::from("xdg-open"), vec![target.to_owned()], None),
    }
}

pub(crate) fn open_folder_now(target: &str) -> Result<(), String> {
    let (program, args, raw) = folder_command(target, Platform::native());
    spawn_detached(&program, &args, raw.as_deref()).map_err(|e| format!("could not open the folder: {e}"))
}

/// `%SystemRoot%\System32\<exe>` (or `%SystemRoot%\<exe>`), by full path.
fn system32_or_windows(exe: &str, system32: bool) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    if system32 { root.join("System32").join(exe) } else { root.join(exe) }
}

/// Opens an already-checked `http(s)` URL in the default browser, on a worker. Windows:
/// `rundll32 url.dll,FileProtocolHandler <url>` by full path (no shell; the URL is one argument
/// and contains no space or quote); macOS `open`; Linux `xdg-open`.
pub(crate) fn open_url_detached(url: String) {
    let _ = std::thread::Builder::new().name("csm-open-url".into()).spawn(move || {
        let r = match Platform::native() {
            Platform::Windows => {
                spawn_detached(&system32_or_windows("rundll32.exe", true), &["url.dll,FileProtocolHandler".to_owned(), url.clone()], None)
            }
            Platform::Mac => spawn_detached(Path::new("/usr/bin/open"), std::slice::from_ref(&url), None),
            Platform::Linux => spawn_detached(Path::new("xdg-open"), std::slice::from_ref(&url), None),
        };
        if let Err(e) = r {
            log::warn!("post-run URL '{url}': {e}");
        }
    });
}

fn spawn_detached(program: &Path, args: &[String], raw: Option<&str>) -> std::io::Result<()> {
    crate::proc::spawn_detached(program, args, raw)
}

// ── The real machine, for the pure rules ─────────────────────────────────────────────────

/// The real path through `std::fs::canonicalize` (8.3 names, junctions, symlinks, subst), with
/// the `\\?\` prefix removed; `\\?\UNC\server\share` becomes `\\server\share`.
fn final_path(path: &str) -> Option<String> {
    let real = std::fs::canonicalize(path).ok()?;
    let s = real.to_string_lossy().into_owned();
    Some(if let Some(unc) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(local) = s.strip_prefix(r"\\?\") {
        local.to_owned()
    } else {
        s
    })
}

struct RealHost;

impl ow::DiskProbe for RealHost {
    fn drive_is_network(&self, root: &str) -> bool {
        // A mapped or subst'd network drive canonicalises to a UNC path.
        !root.is_empty() && final_path(root).is_some_and(|p| p.starts_with(r"\\"))
    }
    fn file_exists(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }
    fn final_path(&self, path: &str) -> Option<String> {
        final_path(path)
    }
    fn version_names(&self, path: &str) -> Vec<String> {
        pe_version_names(path)
    }
}

impl ow::OpenWithHost for RealHost {
    fn probe_wsl_home(&self, distro: &str, user: &str) -> Option<String> {
        wsl_home(distro, user)
    }
    fn folder_target(&self, folder: &str) -> Option<String> {
        folder_target(folder)
    }
}

/// `$HOME` in a WSL distro for a user (the shared, cached `wsl_probe`). May boot the distro:
/// workers only.
fn wsl_home(distro: &str, user: &str) -> Option<String> {
    crate::wsl_probe::home(distro, user)
}

/// POSIX: the same order as `prepare` — pure rules first, then the program on disk (real path,
/// not a script), then the folder.
fn prepare_posix(entry: &SvcEntry, ctx: &OpenWithContext) -> Result<ow::OpenWithCommand, String> {
    ow::build_for(entry, ctx, Rules::Posix)?;
    if let Some(e) = on_disk_error(&entry.executable) {
        return Err(e);
    }
    if !Path::new(&entry.executable).is_file() {
        return Err(format!("{}: program not found at {}.", entry.label, entry.executable));
    }
    let mut c = ctx.clone();
    if entry.args_for(ctx.kind).is_some_and(|t| t.contains("{folder")) {
        c.folder = folder_target(&ctx.folder).ok_or_else(|| format!("The session's folder doesn't exist: {}", ctx.folder))?;
    }
    ow::build_for(entry, &c, Rules::Posix)
}

/// The on-disk program checks for the platform: Windows `validate_executable_on_disk` (real
/// path, network drive, renamed shell); POSIX the real path's rules and a `#!` script check.
fn on_disk_error(exe: &str) -> Option<String> {
    if Rules::native() == Rules::Windows {
        return ow::validate_executable_on_disk(exe, true, &RealHost);
    }
    let real = final_path(exe)?;
    if real != exe
        && let Some(e) = ow::validate_executable_posix(&real)
    {
        return Some(e);
    }
    let mut head = [0u8; 2];
    let is_script = std::fs::File::open(&real).and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head)).is_ok() && &head == b"#!";
    is_script.then(|| format!("{} is a script, not a program — point at the editor's binary.", program_name(exe)))
}

// ── Imports (`state_import`) ─────────────────────────────────────────────────────────────

/// An Open with list from a backup file (C# `OpenWithService.SanitizeImported`): stricter than
/// state.json, no batch files, and every program checked at its real path on disk. Touches the
/// disk: workers only. Only a filter: the caller still shows the list and asks before adopting.
pub(crate) fn sanitize_imported(list: &[CoreEntry]) -> Vec<CoreEntry> {
    let wrapped: Vec<Option<SvcEntry>> = list.iter().map(|e| Some(to_svc(e))).collect();
    let mut log = |m: &str| log::warn!("import, open with: {m}");
    let clean = if Rules::native() == Rules::Windows {
        ow::sanitize_imported(&wrapped, &RealHost, &mut log)
    } else {
        // As on Windows, a program that isn't there (yet) passes: it fails at launch.
        ow::sanitize_for(&wrapped, false, Rules::Posix, &mut log).into_iter().filter(|e| on_disk_error(&e.executable).is_none()).collect()
    };
    clean.into_iter().map(to_core).collect()
}

/// True when two lists would launch the same things.
pub(crate) fn same_commands(a: &[CoreEntry], b: &[CoreEntry]) -> bool {
    let (a, b): (Vec<SvcEntry>, Vec<SvcEntry>) = (a.iter().map(to_svc).collect(), b.iter().map(to_svc).collect());
    ow::same_commands(&a, &b)
}

/// Every entry spelled out (program and argument templates), for the import confirmation.
pub(crate) fn describe_for_review(list: &[CoreEntry]) -> String {
    let v: Vec<SvcEntry> = list.iter().map(to_svc).collect();
    ow::describe_for_review(&v)
}

/// The PE version resource's `OriginalFilename` and `InternalName`, read from the `.rsrc`
/// section only (bounded reads; any malformed header gives an empty list).
fn pe_version_names(path: &str) -> Vec<String> {
    read_rsrc(path).map(|b| csm_services_pe::version_names(&b)).unwrap_or_default()
}

fn read_rsrc(path: &str) -> Option<Vec<u8>> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let mut head = vec![0u8; 4096];
    let n = f.read(&mut head).ok()?;
    head.truncate(n);
    let (offset, size) = csm_services_pe::rsrc_location(&head)?;
    let size = size.min(16 * 1024 * 1024);
    f.seek(SeekFrom::Start(u64::from(offset))).ok()?;
    let mut buf = vec![0u8; usize::try_from(size).ok()?];
    f.read_exact(&mut buf).ok()?;
    Some(buf)
}

/// Just enough PE parsing to find the version strings: the section table, then a scan of the
/// resource section for the UTF-16 keys. Pure over bytes; every read is bounds-checked.
mod csm_services_pe {
    fn u16_at(b: &[u8], i: usize) -> Option<u16> {
        Some(u16::from_le_bytes([*b.get(i)?, *b.get(i + 1)?]))
    }

    fn u32_at(b: &[u8], i: usize) -> Option<u32> {
        Some(u32::from_le_bytes([*b.get(i)?, *b.get(i + 1)?, *b.get(i + 2)?, *b.get(i + 3)?]))
    }

    /// `(file offset, size)` of the `.rsrc` section, from the first 4 KiB of the file.
    pub fn rsrc_location(head: &[u8]) -> Option<(u32, u32)> {
        if head.get(..2)? != b"MZ" {
            return None;
        }
        let pe = usize::try_from(u32_at(head, 0x3C)?).ok()?;
        if head.get(pe..pe + 4)? != b"PE\0\0" {
            return None;
        }
        let sections = usize::from(u16_at(head, pe + 6)?);
        let opt_size = usize::from(u16_at(head, pe + 20)?);
        let table = pe + 24 + opt_size;
        (0..sections.min(96)).find_map(|i| {
            let s = table + i * 40;
            let name = head.get(s..s + 8)?;
            if name.starts_with(b".rsrc") {
                let size = u32_at(head, s + 16)?;
                let off = u32_at(head, s + 20)?;
                Some((off, size))
            } else {
                None
            }
        })
    }

    /// The values after the UTF-16 keys `OriginalFilename` / `InternalName` in a version
    /// resource (`StringFileInfo` entries: key, NUL, padding to 4 bytes, value, NUL).
    pub fn version_names(rsrc: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        for key in ["OriginalFilename", "InternalName"] {
            let pat: Vec<u8> = key.encode_utf16().chain(std::iter::once(0)).flat_map(u16::to_le_bytes).collect();
            let mut from = 0;
            while let Some(pos) = rsrc.get(from..).and_then(|h| h.windows(pat.len()).position(|w| w == pat.as_slice())) {
                let start = from + pos + pat.len();
                let mut i = start;
                // Padding to a 32-bit boundary (relative to the file, close enough for a scan).
                while i < rsrc.len() && i < start + 4 && rsrc.get(i..i + 2) == Some(&[0, 0]) {
                    i += 2;
                }
                let mut units = Vec::new();
                while let Some(u) = u16_at(rsrc, i) {
                    if u == 0 || units.len() >= 260 {
                        break;
                    }
                    units.push(u);
                    i += 2;
                }
                let v = String::from_utf16_lossy(&units);
                if !v.trim().is_empty() && !out.contains(&v) {
                    out.push(v);
                }
                from = start;
            }
        }
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn version_strings_are_found_in_a_resource_blob() {
            let mut b = vec![1u8, 2, 3];
            for (k, v) in [("OriginalFilename", "PowerShell.EXE.MUI"), ("InternalName", "POWERSHELL")] {
                b.extend(k.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
                b.extend([0, 0]);
                b.extend(v.encode_utf16().chain([0]).flat_map(u16::to_le_bytes));
            }
            assert_eq!(version_names(&b), ["PowerShell.EXE.MUI", "POWERSHELL"]);
            assert!(version_names(b"garbage").is_empty());
            assert!(rsrc_location(b"MZ").is_none());
            assert!(rsrc_location(&[]).is_none());
        }

        #[test]
        #[cfg(windows)]
        fn a_real_system_exe_reports_its_original_name() {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
            let names = super::super::pe_version_names(&format!(r"{root}\System32\cmd.exe"));
            assert!(names.iter().any(|n| n.to_lowercase().starts_with("cmd")), "{names:?}");
        }
    }
}

/// The real machine for detection: environment, files, the registry's App Paths (through
/// `reg.exe query`, by full path), vswhere (by full path, 5 s cap).
struct RealEnv;

impl DetectEnv for RealEnv {
    fn platform(&self) -> Platform {
        Platform::native()
    }
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }
    fn is_file(&self, path: &str) -> bool {
        Path::new(path).is_file()
    }
    fn is_dir(&self, path: &str) -> bool {
        Path::new(path).is_dir()
    }
    fn list(&self, dir: &str) -> Vec<String> {
        std::fs::read_dir(dir)
            .map(|r| r.filter_map(Result::ok).take(2000).map(|e| e.path().to_string_lossy().into_owned()).collect())
            .unwrap_or_default()
    }
    fn read_text(&self, path: &str) -> Option<String> {
        let m = std::fs::metadata(path).ok()?;
        if m.len() > 256 * 1024 {
            return None;
        }
        std::fs::read(path).ok().map(|b| String::from_utf8_lossy(&b).into_owned())
    }
    fn modified(&self, path: &str) -> Option<u64> {
        let t = std::fs::metadata(path).ok()?.modified().ok()?;
        t.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs())
    }
    fn app_path(&self, exe_name: &str) -> Option<String> {
        if !cfg!(windows) || exe_name.contains(['\\', '/', '"']) {
            return None;
        }
        for hive in ["HKCU", "HKLM"] {
            let key = format!(r"{hive}\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\{exe_name}");
            if let Some(out) = crate::proc::capture_ok(system32_or_windows("reg.exe", true), ["query", key.as_str(), "/ve"], Duration::from_secs(3))
                && let Some(v) = parse_reg_default(&out)
            {
                let expanded = expand_env(&v);
                if Path::new(&expanded).is_file() {
                    return Some(expanded);
                }
            }
        }
        None
    }
    fn vswhere_json(&self) -> Option<String> {
        if !cfg!(windows) {
            return None;
        }
        let pf86 = std::env::var_os("ProgramFiles(x86)")?;
        let vswhere = PathBuf::from(pf86).join("Microsoft Visual Studio").join("Installer").join("vswhere.exe");
        if !vswhere.is_file() {
            return None;
        }
        crate::proc::capture_ok(vswhere, ["-all", "-prerelease", "-format", "json", "-utf8", "-nologo"], Duration::from_secs(5))
    }
    fn real_path(&self, path: &str) -> Option<String> {
        final_path(path)
    }
}

/// The `(Default)` value from `reg query <key> /ve` output.
pub fn parse_reg_default(out: &str) -> Option<String> {
    out.lines().find_map(|l| {
        let l = l.trim();
        let rest = l.strip_prefix("(Default)").or_else(|| l.strip_prefix("(Standard)"))?;
        let rest = rest.trim_start();
        let rest = rest.strip_prefix("REG_EXPAND_SZ").or_else(|| rest.strip_prefix("REG_SZ"))?;
        let v = rest.trim().trim_matches('"').to_owned();
        (!v.is_empty()).then_some(v)
    })
}

/// `%VAR%` expansion (App Paths values are often `REG_EXPAND_SZ`).
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(rest.get(..i).unwrap_or(""));
        let after = rest.get(i + 1..).unwrap_or("");
        match after.find('%') {
            Some(j) => {
                let name = after.get(..j).unwrap_or("");
                match std::env::var(name) {
                    Ok(v) if !name.is_empty() => out.push_str(&v),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = after.get(j + 1..).unwrap_or("");
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_are_normalised_like_folder_opener() {
        assert_eq!(normalise_folder("C:/repos/wt-feat", true).as_deref(), Some(r"C:\repos\wt-feat"));
        assert_eq!(normalise_folder(r"C:\repos\a,b\", true).as_deref(), Some(r"C:\repos\a,b"));
        assert_eq!(normalise_folder(r"C:\", true).as_deref(), Some(r"C:\"));
        assert_eq!(normalise_folder("C:/", true).as_deref(), Some(r"C:\"));
        assert_eq!(normalise_folder(r"C:\a\..\b", true).as_deref(), Some(r"C:\b"));
        assert_eq!(normalise_folder("relative", true), None);
        assert_eq!(normalise_folder(r"C:relative", true), None);
        assert_eq!(normalise_folder("/home/u/x/", false).as_deref(), Some("/home/u/x"));
        assert_eq!(normalise_folder("/", false).as_deref(), Some("/"));
        assert_eq!(normalise_folder("x/y", false), None);
        assert_eq!(normalise_folder("/a/../etc", false), None);
    }

    #[test]
    fn explorer_gets_the_folder_as_one_quoted_argument() {
        let (prog, args, raw) = folder_command(r"C:\repos\a,b & c %PATH%", Platform::Windows);
        assert!(prog.ends_with("explorer.exe"));
        assert!(args.is_empty());
        assert_eq!(raw.as_deref(), Some(r#""C:\repos\a,b & c %PATH%""#));
        let (prog, args, raw) = folder_command("/Users/u/a b", Platform::Mac);
        assert_eq!(prog, PathBuf::from("/usr/bin/open"));
        assert_eq!(args, ["/Users/u/a b"]);
        assert!(raw.is_none());
    }

    #[test]
    fn folder_target_accepts_only_existing_directories() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("a,b c");
        std::fs::create_dir(&sub).unwrap();
        let file = dir.path().join("run-me.exe");
        std::fs::write(&file, "").unwrap();
        let s = sub.display().to_string();
        assert!(folder_target(&s).is_some());
        assert!(folder_target(&format!("{s}/")).is_some());
        if cfg!(windows) {
            assert_eq!(folder_target(&s.replace('\\', "/")).as_deref(), Some(s.as_str()), "forward slashes become backslashes");
        }
        assert_eq!(folder_target(&file.display().to_string()), None, "a file would be run, never opened");
        assert_eq!(folder_target(&dir.path().join("missing").display().to_string()), None);
        assert_eq!(folder_target("relative\\path"), None);
        assert_eq!(folder_target(""), None);
        assert_eq!(folder_target("C:\\x\ny"), None);
    }

    #[test]
    fn reg_query_default_values() {
        let out = "\r\nHKEY_CURRENT_USER\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\App Paths\\Zed.exe\r\n    (Default)    REG_SZ    C:\\Users\\u\\AppData\\Local\\Programs\\Zed\\Zed.exe\r\n\r\n";
        assert_eq!(parse_reg_default(out).as_deref(), Some(r"C:\Users\u\AppData\Local\Programs\Zed\Zed.exe"));
        assert_eq!(parse_reg_default("    (Default)    REG_EXPAND_SZ    \"%ProgramFiles%\\x.exe\""), Some(r"%ProgramFiles%\x.exe".to_owned()));
        assert_eq!(parse_reg_default("    (Default)    REG_SZ    "), None);
        assert_eq!(parse_reg_default("ERROR: nothing"), None);
        assert_eq!(expand_env("%NO_SUCH_VAR_CSM%\\x"), "%NO_SUCH_VAR_CSM%\\x");
    }

    #[test]
    fn targets_resolve_the_command_per_session_and_skip_other_kinds() {
        let list = vec![
            CoreEntry {
                id: "vs".into(),
                label: "VS Code".into(),
                executable: r"C:\Program Files\Microsoft VS Code\Code.exe".into(),
                local_args: Some("{folder}".into()),
                ssh_args: Some(ow::VSCODE_SSH_ARGS.into()),
                is_default: true,
                ..Default::default()
            },
            CoreEntry {
                id: "np".into(),
                label: "Notepad++".into(),
                executable: r"C:\npp\notepad++.exe".into(),
                local_args: Some("-openFoldersAsWorkspace {folder}".into()),
                ..Default::default()
            },
        ];
        let s = ShellSession { id: "s".into(), working_folder: r"C:\src\a b".into(), ..Default::default() };
        let t = targets(&list, &s);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].program, "Code.exe");
        assert!(t[0].is_default && !t[1].is_default);
        if cfg!(windows) {
            assert_eq!(t[0].command, r#""C:\Program Files\Microsoft VS Code\Code.exe" "C:\src\a b""#);
        }
        let ssh =
            ShellSession { id: "r".into(), kind: SessionKind::Ssh, ssh_host: "box".into(), ssh_remote_folder: "/srv".into(), ..Default::default() };
        let t = targets(&list, &ssh);
        assert_eq!(t.len(), 1, "Notepad++ has no SSH form");
        assert_eq!(t[0].entry_id, "vs");
    }
}
