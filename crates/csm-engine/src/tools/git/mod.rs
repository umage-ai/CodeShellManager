//! `git` tools: status, fetch, pull, switch, branches, and worktrees (list, new, remove, prune).
//!
//! **Read-mostly, on purpose: no commit, push or merge.** A Claude session is usually editing
//! that same working tree, and a one-click history write from outside its conversation is both
//! a race and a misclick nobody can take back. Pull is `--ff-only --no-rebase`; switch is
//! `--no-guess --`; worktree remove never uses `--force` and deleting its branch uses `-d`.
//!
//! Every git process runs on a worker thread (`csm_services::git::run`) and the reply is parked
//! until it lands; one operation per repository at a time ([`rt::claim`]); after an operation
//! every live session on that repository re-reads its state ([`rt::release`]). Nothing is typed
//! into a terminal: in a Claude session, typed text would land in Claude's prompt.

pub(crate) mod rt;
mod read;

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use csm_core::models::{SessionKind, SessionStatus, ShellSession};
use csm_core::time::Timestamp;
use csm_pty::Pty;
use csm_services::git::{self as g, WorktreeInfo, run, worktrees};
use serde::Deserialize;
use serde_json::{Value, json};

pub(crate) use rt::{GitRt, tick, view_fields};

use super::find;
use crate::engine::{Ctx, ParkedReply, Reply};
use crate::lifecycle::{self, new_id};
use crate::session::now_ms;
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

pub(crate) fn specs() -> Vec<ToolSpec> {
    let read = Effects::IDEMPOTENT.union(Effects::LAUNCHES_PROCESS);
    let write = Effects::LAUNCHES_PROCESS.union(Effects::WRITES_FILESYSTEM);
    let mut v = vec![
        ToolSpec {
            id: "git_status",
            title: "Git status",
            summary: "A session folder's git state, read now: branch, upstream, ahead/behind, the changed files, and which repo it is a worktree of.",
            toolset: Toolset::Git,
            effects: read,
            input_schema: schema_of::<SidParams>,
            run: git_status,
        },
        ToolSpec {
            id: "git_fetch",
            title: "Fetch",
            summary: "git fetch in the session's repository (never prompts for a password; sign in from a terminal once if it fails).",
            toolset: Toolset::Git,
            effects: Effects::LAUNCHES_PROCESS.union(Effects::ACCESSES_NETWORK),
            input_schema: schema_of::<SidParams>,
            run: git_fetch,
        },
        ToolSpec {
            id: "git_pull",
            title: "Pull (fast-forward only)",
            summary: "git pull --ff-only --no-rebase: moves the branch only when that loses nothing; a diverged branch or a conflicting local edit is refused with git's reason.",
            toolset: Toolset::Git,
            effects: write.union(Effects::ACCESSES_NETWORK),
            input_schema: schema_of::<SidParams>,
            run: git_pull,
        },
        ToolSpec {
            id: "git_switch_branch",
            title: "Switch branch",
            summary: "git switch --no-guess to a local branch. A branch checked out in another worktree is refused (open that worktree instead; `git_branches` says which).",
            toolset: Toolset::Git,
            effects: write,
            input_schema: schema_of::<SwitchParams>,
            run: git_switch_branch,
        },
        ToolSpec {
            id: "git_branches",
            title: "Branches",
            summary: "Local branches; for each one checked out in another worktree, that worktree and its session.",
            toolset: Toolset::Git,
            effects: read,
            input_schema: schema_of::<SidParams>,
            run: git_branches,
        },
        ToolSpec {
            id: "worktree_list",
            title: "Worktrees",
            summary: "Every worktree of the session's repository (or, with `folder`, of the repository a folder without a session is in): branch, changes, ahead/behind, the sessions in it, and the `session_new` params to open one there.",
            toolset: Toolset::Git,
            effects: read,
            input_schema: schema_of::<WorktreeListParams>,
            run: worktree_list,
        },
        ToolSpec {
            id: "worktree_new",
            title: "New worktree…",
            summary: "git worktree add for a branch (created from `base` or HEAD when it doesn't exist), next to the main checkout by default; opens a session there with the same command, placed after this one, unless open_session is false.",
            toolset: Toolset::Git,
            effects: write.union(Effects::MUTATES_STATE),
            input_schema: schema_of::<NewWorktreeParams>,
            run: worktree_new,
        },
        ToolSpec {
            id: "worktree_remove",
            title: "Remove worktree…",
            summary: "Remove a linked worktree with git worktree remove (no --force). Refuses the main checkout, a worktree with changes, and one with another repository inside it. Closes the sessions in that folder first (needs force: true when there are any) and waits for them to exit; delete_branch uses branch -d, which keeps an unmerged branch.",
            toolset: Toolset::Git,
            effects: write.union(Effects::MAY_DISCARD_DATA).union(Effects::MUTATES_STATE),
            input_schema: schema_of::<RemoveWorktreeParams>,
            run: worktree_remove,
        },
        ToolSpec {
            id: "worktree_prune",
            title: "Prune missing worktrees",
            summary: "git worktree prune: forgets worktrees whose folder is already gone. Only git's bookkeeping; never a working tree.",
            toolset: Toolset::Git,
            effects: write,
            input_schema: schema_of::<SidParams>,
            run: worktree_prune,
        },
    ];
    v.extend(read::specs());
    v
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SidParams {
    session_id: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct WorktreeListParams {
    /// The session whose repository to list. Give this or `folder`.
    #[serde(default)]
    session_id: Option<String>,
    /// A local folder (an absolute path) whose repository to list, for a folder no session is
    /// in yet (the quick-open's "Also start in sibling worktrees"). Give this or `session_id`.
    #[serde(default)]
    folder: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct SwitchParams {
    session_id: String,
    /// A local branch name (as `git_branches` lists it).
    name: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct NewWorktreeParams {
    session_id: String,
    /// The branch the worktree checks out. Created when it doesn't exist locally.
    branch: String,
    /// What a new branch starts from (default: the current HEAD). Only for a new branch.
    #[serde(default)]
    base: Option<String>,
    /// Where the worktree goes (default: `<main checkout>-<branch>` beside the main checkout).
    #[serde(default)]
    path: Option<String>,
    /// Open a session in it (default true): same kind, command and group, after this session.
    #[serde(default)]
    open_session: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct RemoveWorktreeParams {
    session_id: String,
    /// The worktree to remove (default: the one this session's folder is in).
    #[serde(default)]
    path: Option<String>,
    /// Then delete its branch with `git branch -d` (an unmerged branch is kept).
    #[serde(default)]
    delete_branch: bool,
    /// Close the sessions in that folder. Without it, a worktree with open sessions is refused.
    #[serde(default)]
    force: bool,
}

// ── Shared plumbing ──────────────────────────────────────────────────────────────────────

/// What a git tool needs to know about its session, read on the engine thread.
#[derive(Clone)]
struct Target {
    id: String,
    folder: String,
    root: Option<String>,
}

fn target(ctx: &Ctx<'_>, id: &str) -> Result<Target, ToolError> {
    let i = find(ctx.state, id)?;
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound(format!("no session `{id}`")))?;
    if s.kind == SessionKind::Ssh {
        return Err(ToolError::Refused("git actions need a local or WSL folder; an SSH session's branch is only what it reports (OSC 9001)".into()));
    }
    if s.working_folder.trim().is_empty() {
        return Err(ToolError::Refused("the session has no folder to run git in".into()));
    }
    Ok(Target { id: id.to_owned(), folder: s.working_folder.clone(), root: rt::known_root(ctx.state, id) })
}

fn not_a_repo(folder: &str) -> ToolError {
    ToolError::Refused(format!("{folder} is not in a git repository"))
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), ToolError> {
    std::thread::Builder::new()
        .name(name.into())
        .spawn(f)
        .map(|_| ())
        .map_err(|e| ToolError::Failed(format!("could not start a worker for git: {e}")))
}

fn no_controls(field: &str, v: &str) -> Result<(), ToolError> {
    if v.trim().is_empty() {
        return Err(ToolError::InvalidParams(format!("`{field}` is empty")));
    }
    if v.chars().any(char::is_control) {
        return Err(ToolError::InvalidParams(format!("`{field}` contains control characters")));
    }
    Ok(())
}

/// What an operation's worker produced: the reply, and optionally a step that must run on the
/// engine thread afterwards (opening a session, deleting sleeping ones) before replying.
type After = Box<dyn FnOnce(&mut Ctx<'_>, ToolOutput) -> Reply + Send>;

struct Done {
    out: ToolOutput,
    after: Option<After>,
}

impl From<ToolOutput> for Done {
    fn from(out: ToolOutput) -> Self {
        Done { out, after: None }
    }
}

type Work = Box<dyn FnOnce(&Target, &str) -> Result<Done, ToolError> + Send>;

/// Runs a repository operation: resolve the repo root (worker), claim the repo's busy slot
/// (engine), run `work` (worker), then release the slot, refresh every session on the repo and
/// reply (engine). Parks the call's reply throughout.
fn repo_op(ctx: &mut Ctx<'_>, t: Target, work: Work) -> Reply {
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    let engine = ctx.engine.clone();
    spawn("csm-git-root", move || {
        let root = t.root.clone().or_else(|| run::repo_root(&t.folder));
        let Some(root) = root else {
            reply.send(Err(not_a_repo(&t.folder)));
            return;
        };
        let eng = engine.clone();
        engine.post(Box::new(move |ctx| {
            let key = worktrees::busy_key(Some(&root), Some(&t.folder));
            if !rt::claim(ctx.state, &key) {
                reply.send(Err(busy_error()));
                return;
            }
            let held = key.clone();
            let worker = spawn("csm-git-op", move || {
                let r = work(&t, &root);
                eng.post(Box::new(move |ctx| finish(ctx, &key, r, reply)));
            });
            if let Err(e) = worker {
                // The reply went with the closure that never ran (the caller sees the engine
                // drop it); the slot must not stay claimed.
                rt::release(ctx.state, &held);
                log::error!("git: {e}");
            }
        }));
    })?;
    Ok(ToolOutput::message("running git"))
}

fn busy_error() -> ToolError {
    ToolError::Refused("another git operation is running on this repository; try again when it has finished".into())
}

fn finish(ctx: &mut Ctx<'_>, key: &str, r: Result<Done, ToolError>, reply: ParkedReply) {
    rt::release(ctx.state, key);
    let out = match r {
        Ok(Done { out, after: Some(after) }) => after(ctx, out),
        Ok(Done { out, after: None }) => Ok(out),
        Err(e) => Err(e),
    };
    reply.send(out);
}

fn op_failed(what: &str, r: &g::GitOpResult) -> ToolError {
    ToolError::Failed(run::failure_message(what, &r.output))
}

/// The sessions of every kind that could be in a worktree: (id, name, folder, asleep).
#[derive(Clone)]
struct SessionRef {
    id: String,
    name: String,
    folder: String,
    asleep: bool,
    kind: SessionKind,
}

fn session_refs(ctx: &Ctx<'_>) -> Vec<SessionRef> {
    ctx.state
        .app
        .sessions
        .iter()
        .filter(|s| rt::has_git_folder(s))
        .map(|s| SessionRef {
            id: s.id.clone(),
            name: if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() },
            folder: s.working_folder.clone(),
            asleep: s.is_dormant,
            kind: s.kind,
        })
        .collect()
}

/// Each session paired with the worktree its folder is in (deepest match; folders compared in
/// their real form). On a worker: canonicalising touches the disk.
fn sessions_in<'a>(list: &'a [WorktreeInfo], refs: &[SessionRef]) -> Vec<(SessionRef, &'a WorktreeInfo)> {
    refs.iter().filter_map(|r| worktrees::containing(list, &run::canonical(&r.folder)).map(|w| (r.clone(), w))).collect()
}

/// `session_new` params for a new session in worktree `path`, like `source` (kind, command,
/// group), placed after it.
fn open_params(source: Option<&ShellSession>, path: &str) -> Value {
    let Some(s) = source else {
        return json!({"folder": run::windows_form(path), "focus": true});
    };
    let mut p = json!({
        "command": s.command,
        "args": s.args,
        "after": s.id,
        "focus": true,
    });
    if !s.group_id.is_empty() {
        p["group"] = json!(s.group_id);
    }
    match csm_services::wsl::try_parse_unc_path(path) {
        (Some(distro), linux) if s.kind == SessionKind::Wsl => {
            p["kind"] = json!("wsl");
            let mut w = json!({"distro": distro, "folder": linux});
            if !s.wsl_user.is_empty() {
                w["user"] = json!(s.wsl_user);
            }
            p["wsl"] = w;
        }
        _ => {
            p["kind"] = json!("local");
            p["folder"] = json!(run::windows_form(path));
        }
    }
    p
}

fn status_json(st: &g::GitRepoStatus) -> Value {
    json!({
        "branch": st.branch,
        "detached": st.is_detached,
        "upstream": st.upstream,
        "ahead": st.ahead,
        "behind": st.behind,
        "dirty": st.is_dirty(),
        "change_count": st.changes.len(),
    })
}

// ── Tools ────────────────────────────────────────────────────────────────────────────────

fn git_status(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SidParams = parse_params(params)?;
    let t = target(ctx, &p.session_id)?;
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    let engine = ctx.engine.clone();
    spawn("csm-git-status", move || {
        let status = run::status(&t.folder);
        let Some(st) = status else {
            reply.send(Err(not_a_repo(&t.folder)));
            return;
        };
        let root = t.root.clone().or_else(|| run::repo_root(&t.folder));
        let worktree_of = run::linked_worktree_of(&t.folder, root.as_deref());
        let summary = g::menu_header(st.branch.as_deref(), st.upstream.as_deref(), st.ahead, st.behind, st.changes.len(), worktree_of.as_deref());
        let mut data = status_json(&st);
        data["session_id"] = json!(t.id);
        data["folder"] = json!(t.folder);
        data["repo_root"] = json!(root);
        data["worktree_of"] = json!(worktree_of);
        data["summary"] = json!(summary);
        data["changes"] = st.changes.iter().map(|c| json!({"code": c.code, "path": c.path})).collect();
        engine.post(Box::new(move |ctx| {
            rt::store_status(ctx.state, &t.id, &t.folder, root.as_deref(), Some(st));
            reply.send(Ok(ToolOutput::new(summary, data)));
        }));
    })?;
    Ok(ToolOutput::message("reading git status"))
}

fn git_fetch(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SidParams = parse_params(params)?;
    let t = target(ctx, &p.session_id)?;
    repo_op(
        ctx,
        t,
        Box::new(|t, _root| {
            let r = run::fetch(&t.folder);
            if !r.ok {
                return Err(op_failed("Fetch", &r));
            }
            let st = run::status(&t.folder);
            let msg = match &st {
                Some(s) => g::fetch_summary(s.branch.as_deref(), s.upstream.as_deref(), s.ahead, s.behind),
                None => "Fetched.".to_string(),
            };
            Ok(ToolOutput::new(msg, json!({"session_id": t.id, "output": r.output, "status": st.as_ref().map(status_json)})).into())
        }),
    )
}

fn git_pull(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SidParams = parse_params(params)?;
    let t = target(ctx, &p.session_id)?;
    repo_op(
        ctx,
        t,
        Box::new(|t, _root| {
            let r = run::pull_ff_only(&t.folder);
            if !r.ok {
                return Err(op_failed("Pull (fast-forward only)", &r));
            }
            Ok(ToolOutput::new(g::summarize_pull(&r.output), json!({"session_id": t.id, "output": r.output})).into())
        }),
    )
}

fn git_switch_branch(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SwitchParams = parse_params(params)?;
    no_controls("name", &p.name)?;
    let t = target(ctx, &p.session_id)?;
    let name = p.name;
    repo_op(
        ctx,
        t,
        Box::new(move |t, _root| {
            // git allows a branch in one worktree at a time: say where it is instead of letting
            // git fail with "already used by worktree at ...".
            let list = run::worktree_list(&t.folder);
            let here = run::canonical(&t.folder);
            let current = worktrees::containing(&list, &here);
            if let Some((_, w)) = worktrees::branches_elsewhere(&list, current).into_iter().find(|(b, _)| *b == name) {
                return Err(ToolError::Refused(format!(
                    "`{name}` is checked out in the worktree {} ({}); git allows a branch in one worktree at a time. Focus or open that worktree instead.",
                    worktrees::name(&w.path),
                    w.path
                )));
            }
            let r = run::switch_branch(&t.folder, &name);
            if !r.ok {
                return Err(op_failed(&format!("Switching to {name}"), &r));
            }
            Ok(ToolOutput::new(format!("Switched to {name}."), json!({"session_id": t.id, "branch": name})).into())
        }),
    )
}

fn git_branches(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SidParams = parse_params(params)?;
    let t = target(ctx, &p.session_id)?;
    let refs = session_refs(ctx);
    let source = ctx.state.app.sessions.iter().find(|s| s.id == t.id).cloned();
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    spawn("csm-git-branches", move || {
        let names = match run::branches(&t.folder) {
            Ok(n) => n,
            Err(e) => {
                reply.send(Err(if run::status(&t.folder).is_none() { not_a_repo(&t.folder) } else { ToolError::Failed(e) }));
                return;
            }
        };
        let list = run::worktree_list(&t.folder);
        let here = run::canonical(&t.folder);
        let current = worktrees::containing(&list, &here);
        let current_branch = current.and_then(|w| w.branch.clone()).or_else(|| run::status(&t.folder).and_then(|s| s.branch));
        let elsewhere = worktrees::branches_elsewhere(&list, current);
        let placed = sessions_in(&list, &refs);
        let branches: Vec<Value> = names
            .iter()
            .map(|n| {
                let wt = elsewhere.iter().find(|(b, _)| b == n).map(|(_, w)| {
                    let session = placed.iter().find(|(_, pw)| worktrees::same_path(&pw.path, &w.path)).map(|(r, _)| r);
                    json!({
                        "path": w.path,
                        "name": worktrees::name(&w.path),
                        "session_id": session.map(|s| s.id.clone()),
                        "session_asleep": session.map(|s| s.asleep),
                        "open": open_params(source.as_ref(), &w.path),
                    })
                });
                json!({"name": n, "current": current_branch.as_deref() == Some(n.as_str()), "worktree": wt})
            })
            .collect();
        let msg = format!("{} local branches", branches.len());
        reply.send(Ok(ToolOutput::new(msg, json!({"session_id": t.id, "current": current_branch, "branches": branches}))));
    })?;
    Ok(ToolOutput::message("listing branches"))
}

/// `worktree_list`'s target: a session, or a bare folder (no session id, no known root).
fn list_target(ctx: &Ctx<'_>, p: WorktreeListParams) -> Result<Target, ToolError> {
    match (p.session_id, p.folder) {
        (Some(id), None) => target(ctx, &id),
        (None, Some(folder)) => {
            no_controls("folder", &folder)?;
            let folder = folder.trim().to_owned();
            if !Path::new(&folder).is_absolute() {
                return Err(ToolError::InvalidParams(format!("`folder` must be an absolute path, not `{folder}`")));
            }
            Ok(Target { id: String::new(), folder, root: None })
        }
        _ => Err(ToolError::InvalidParams("give either `session_id` or `folder`".into())),
    }
}

fn worktree_list(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: WorktreeListParams = parse_params(params)?;
    let t = list_target(ctx, p)?;
    let refs = session_refs(ctx);
    let source = if t.id.is_empty() { None } else { ctx.state.app.sessions.iter().find(|s| s.id == t.id).cloned() };
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    spawn("csm-git-worktrees", move || {
        let list = run::worktree_list(&t.folder);
        if list.is_empty() {
            reply.send(Err(not_a_repo(&t.folder)));
            return;
        }
        let here = run::canonical(&t.folder);
        let current = worktrees::containing(&list, &here).map(|w| w.path.clone());
        // One status per worktree, in parallel (each is a process start).
        let statuses: Vec<Option<g::GitRepoStatus>> = std::thread::scope(|sc| {
            let hs: Vec<_> = list.iter().map(|w| sc.spawn(move || if w.is_prunable || w.is_bare { None } else { run::status(&w.path) })).collect();
            hs.into_iter().map(|h| h.join().ok().flatten()).collect()
        });
        let placed = sessions_in(&list, &refs);
        let rows: Vec<Value> = list
            .iter()
            .zip(&statuses)
            .map(|(w, st)| {
                let sessions: Vec<Value> = placed
                    .iter()
                    .filter(|(_, pw)| worktrees::same_path(&pw.path, &w.path))
                    .map(|(r, _)| json!({"id": r.id, "name": r.name, "asleep": r.asleep, "kind": match r.kind { SessionKind::Wsl => "wsl", _ => "local" }}))
                    .collect();
                json!({
                    "path": w.path,
                    "name": worktrees::name(&w.path),
                    "label": worktrees::label(w, st.as_ref()),
                    "branch": w.branch,
                    "detached": w.is_detached,
                    "locked": w.is_locked,
                    "prunable": w.is_prunable,
                    "main": worktrees::is_main(&list, w),
                    "current": current.as_deref().is_some_and(|c| worktrees::same_path(c, &w.path)),
                    "dirty": st.as_ref().is_some_and(g::GitRepoStatus::is_dirty),
                    "ahead": st.as_ref().and_then(|s| s.ahead),
                    "behind": st.as_ref().and_then(|s| s.behind),
                    "sessions": sessions,
                    "open": open_params(source.as_ref(), &w.path),
                })
            })
            .collect();
        let prunable = list.iter().filter(|w| w.is_prunable).count();
        let msg = format!("{} worktrees", rows.len());
        let mut data = json!({"current": current, "prunable": prunable, "worktrees": rows});
        if t.id.is_empty() {
            data["folder"] = json!(t.folder);
        } else {
            data["session_id"] = json!(t.id);
        }
        reply.send(Ok(ToolOutput::new(msg, data)));
    })?;
    Ok(ToolOutput::message("listing worktrees"))
}

fn worktree_prune(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SidParams = parse_params(params)?;
    let t = target(ctx, &p.session_id)?;
    repo_op(
        ctx,
        t,
        Box::new(|t, root| {
            let missing = run::worktree_list(root).iter().filter(|w| w.is_prunable).count();
            let r = run::prune_worktrees(root);
            if !r.ok {
                return Err(op_failed("Pruning worktrees", &r));
            }
            let msg = match missing {
                0 => "No missing worktrees to prune.".to_string(),
                1 => "Pruned 1 missing worktree.".to_string(),
                n => format!("Pruned {n} missing worktrees."),
            };
            Ok(ToolOutput::new(msg, json!({"session_id": t.id, "pruned": missing})).into())
        }),
    )
}

fn worktree_new(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: NewWorktreeParams = parse_params(params)?;
    no_controls("branch", &p.branch)?;
    if p.branch.chars().any(char::is_whitespace) {
        return Err(ToolError::InvalidParams("a branch name may not contain spaces".into()));
    }
    if let Some(b) = &p.base {
        no_controls("base", b)?;
    }
    if let Some(path) = &p.path {
        no_controls("path", path)?;
    }
    if ctx.state.rt.shutting_down {
        return Err(ToolError::Refused("the app is closing".into()));
    }
    let t = target(ctx, &p.session_id)?;
    let open = p.open_session.unwrap_or(true);
    let focus = !ctx.caller.is_external();
    let (branch, base, path) = (p.branch, p.base, p.path);
    repo_op(
        ctx,
        t,
        Box::new(move |t, root| {
            let exists = run::branches(root).map_err(ToolError::Failed)?.contains(&branch);
            if exists && base.is_some() {
                return Err(ToolError::InvalidParams(format!("`{branch}` already exists; `base` only applies to a new branch")));
            }
            let target_path = path.unwrap_or_else(|| run::default_worktree_path(root, &branch));
            let tp = Path::new(&target_path);
            if tp.exists() && std::fs::read_dir(tp).map(|mut d| d.next().is_some()).unwrap_or(true) {
                return Err(ToolError::Refused(format!("{target_path} already exists and is not empty; pick another `path`")));
            }
            let r = run::add_worktree(root, &target_path, &branch, !exists, base.as_deref());
            if !r.ok {
                return Err(op_failed("git worktree add", &r));
            }
            let created = run::canonical(&target_path);
            let repo = worktrees::name(root);
            let data = json!({"session_id": t.id, "path": created, "branch": branch, "created_branch": !exists});
            let msg = format!("Created the worktree {} on {branch}.", worktrees::name(&created));
            let out = ToolOutput::new(msg, data);
            if !open {
                return Ok(out.into());
            }
            let source_id = t.id.clone();
            let after: After = Box::new(move |ctx, out| open_worktree_session(ctx, &source_id, &created, &repo, &branch, focus, out));
            Ok(Done { out, after: Some(after) })
        }),
    )
}

/// Opens a session in a new worktree, cloned from `source_id` (kind, command, args, group,
/// appearance, run commands with fresh ids), placed right after it.
fn open_worktree_session(ctx: &mut Ctx<'_>, source_id: &str, path: &str, repo: &str, branch: &str, focus: bool, mut out: ToolOutput) -> Reply {
    let Some(i) = lifecycle::session_index(ctx.state, source_id) else {
        out.message.push_str(" Its session was closed, so no session was opened.");
        return Ok(out);
    };
    let Some(src) = ctx.state.app.sessions.get(i) else { return Ok(out) };
    let now = Timestamp::from_epoch_ms(now_ms());
    let mut s = src.clone();
    s.id = new_id();
    s.name = format!("{repo} ⎇ {branch}");
    s.is_dormant = false;
    s.status = SessionStatus::Running;
    s.created_at = now.clone();
    s.last_activity_at = now;
    for r in &mut s.run_commands {
        r.id = new_id();
    }
    match (s.kind, csm_services::wsl::try_parse_unc_path(path)) {
        (SessionKind::Wsl, (Some(distro), linux)) => {
            s.wsl_working_folder = linux;
            s.working_folder = csm_services::wsl::to_unc_path(&distro, &s.wsl_working_folder);
        }
        _ => s.working_folder = run::windows_form(path),
    }
    let id = s.id.clone();
    ctx.state.app.sessions.insert(i + 1, s);
    if let Err(msg) = lifecycle::launch(ctx, &id, false) {
        if let Some(j) = lifecycle::session_index(ctx.state, &id) {
            ctx.state.app.sessions.remove(j);
        }
        out.message.push_str(&format!(" The session could not be started: {msg}"));
        return Ok(out);
    }
    if focus {
        lifecycle::focus(ctx, &id)?;
    }
    ctx.state.persist();
    out.message = format!("{} Opened a session in it.", out.message);
    out.data["new_session_id"] = json!(id);
    Ok(out)
}

// ── Remove ───────────────────────────────────────────────────────────────────────────────

/// Checked on a worker: what is removed, from where, and who is in it.
struct RemovePlan {
    key: String,
    main: String,
    path: String,
    branch: Option<String>,
    live: Vec<SessionRef>,
    asleep: Vec<SessionRef>,
}

fn worktree_remove(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: RemoveWorktreeParams = parse_params(params)?;
    if let Some(path) = &p.path {
        no_controls("path", path)?;
    }
    if ctx.state.rt.shutting_down {
        return Err(ToolError::Refused("the app is closing".into()));
    }
    let t = target(ctx, &p.session_id)?;
    let refs = session_refs(ctx);
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    let engine = ctx.engine.clone();
    let (path, delete_branch, force) = (p.path, p.delete_branch, p.force);
    spawn("csm-git-wt-check", move || match plan_remove(&t, path.as_deref(), &refs) {
        Err(e) => reply.send(Err(e)),
        Ok(plan) => {
            let eng = engine.clone();
            engine.post(Box::new(move |ctx| remove_close_sessions(ctx, plan, delete_branch, force, reply, eng)));
        }
    })?;
    Ok(ToolOutput::message("checking the worktree"))
}

/// The refusals, all at click time and all failing closed: the listing must contain the target
/// (it is empty on any git error); not the main checkout; no changes; no other worktree listed
/// inside it; no repository of any kind on disk inside it (an independent clone or another
/// repo's worktree would be deleted just the same; a scan that hits its cap refuses).
fn plan_remove(t: &Target, path: Option<&str>, refs: &[SessionRef]) -> Result<RemovePlan, ToolError> {
    let list = run::worktree_list(&t.folder);
    if list.is_empty() {
        return Err(not_a_repo(&t.folder));
    }
    let want = run::canonical(path.unwrap_or(&t.folder));
    let target = match path {
        Some(_) => list.iter().find(|w| worktrees::same_path(&w.path, &want)),
        None => worktrees::containing(&list, &want),
    }
    .ok_or_else(|| ToolError::Refused(format!("{want} is not a worktree git lists for this repository; nothing was removed")))?;
    if worktrees::is_main(&list, target) || target.is_bare {
        return Err(ToolError::Refused("that is the main checkout; only linked worktrees can be removed".into()));
    }
    if target.is_prunable {
        return Err(ToolError::Refused("that worktree's folder is already gone; use worktree_prune to forget it".into()));
    }
    let nested = worktrees::nested_inside(&list, target);
    if let Some(n) = nested.first() {
        return Err(ToolError::Refused(format!(
            "another worktree ({}) lives inside {}; removing it would delete that one too. Remove the inner one first.",
            n.path, target.path
        )));
    }
    match run::status(&target.path) {
        None => return Err(ToolError::Refused(format!("could not read the status of {}; nothing was removed", target.path))),
        Some(st) if st.is_dirty() => {
            return Err(ToolError::Refused(format!(
                "{} has {} uncommitted change(s); commit, stash or discard them first. Nothing was removed.",
                target.path,
                st.changes.len()
            )));
        }
        Some(_) => {}
    }
    match run::nested_repositories(Path::new(&target.path), run::NESTED_SCAN_MAX_DIRS) {
        None => return Err(ToolError::Refused(format!("{} is too large to check for repositories inside it; nothing was removed", target.path))),
        Some(found) if !found.is_empty() => {
            let first = found.first().map(|p| p.display().to_string()).unwrap_or_default();
            return Err(ToolError::Refused(format!(
                "a repository lives inside {} ({first}); removing the worktree would delete it with it. Nothing was removed.",
                target.path
            )));
        }
        Some(_) => {}
    }
    let main = list.first().map(|w| w.path.clone()).unwrap_or_default();
    let placed = sessions_in(&list, refs);
    let (mut live, mut asleep) = (Vec::new(), Vec::new());
    for (r, w) in placed {
        if worktrees::same_path(&w.path, &target.path) {
            if r.asleep { asleep.push(r) } else { live.push(r) }
        }
    }
    Ok(RemovePlan {
        key: worktrees::busy_key(Some(&main), Some(&t.folder)),
        main,
        path: target.path.clone(),
        branch: target.branch.clone().filter(|_| !target.is_detached),
        live,
        asleep,
    })
}

/// Engine thread: claim the repo, close the folder's live sessions (they hold the folder open on
/// Windows), then wait for their processes on a worker before git removes it.
fn remove_close_sessions(ctx: &mut Ctx<'_>, plan: RemovePlan, delete_branch: bool, force: bool, reply: ParkedReply, engine: crate::Engine) {
    if !plan.live.is_empty() && !force {
        let names: Vec<&str> = plan.live.iter().map(|r| r.name.as_str()).collect();
        reply.send(Err(ToolError::Refused(format!(
            "removing {} closes {} ({}); pass force: true to close them and remove it",
            plan.path,
            if names.len() == 1 { "1 session".to_string() } else { format!("{} sessions", names.len()) },
            names.join(", ")
        ))));
        return;
    }
    if !rt::claim(ctx.state, &plan.key) {
        reply.send(Err(busy_error()));
        return;
    }
    let mut ptys: Vec<Arc<dyn Pty>> = Vec::new();
    let mut closed = Vec::new();
    for r in &plan.live {
        if let Some(p) = ctx.state.rt.sessions.get(&r.id).and_then(|rt| rt.io.pty.get().cloned()) {
            ptys.push(p);
        }
        if lifecycle::close_session(ctx, &r.id, true).is_ok() {
            closed.push(r.name.clone());
        }
    }
    let wait = ctx.state.options.exit_wait + Duration::from_secs(2);
    let key = plan.key.clone();
    let spawned = spawn("csm-git-wt-remove", move || {
        let deadline = Instant::now() + wait;
        for p in &ptys {
            lifecycle::wait_exit(p.as_ref(), deadline.saturating_duration_since(Instant::now()));
        }
        let r = run::remove_worktree(&plan.main, &plan.path);
        let deleted = match (&plan.branch, r.ok && delete_branch) {
            (Some(b), true) => Some((b.clone(), run::delete_merged_branch(&plan.main, b))),
            _ => None,
        };
        engine.post(Box::new(move |ctx| remove_finish(ctx, plan, r, deleted, closed, reply)));
    });
    if spawned.is_err() {
        rt::release(ctx.state, &key);
    }
}

fn remove_finish(
    ctx: &mut Ctx<'_>,
    plan: RemovePlan,
    r: g::GitOpResult,
    deleted: Option<(String, g::GitOpResult)>,
    closed: Vec<String>,
    reply: ParkedReply,
) {
    rt::release(ctx.state, &plan.key);
    if !r.ok {
        let mut msg = run::failure_message(&format!("Removing {}", plan.path), &r.output);
        if !closed.is_empty() {
            msg.push_str(&format!("\n\nIts sessions were closed ({}); Ctrl+Shift+T reopens them.", closed.join(", ")));
        }
        reply.send(Err(ToolError::Failed(msg)));
        return;
    }
    // Sleeping sessions are deleted only now that git removed the folder: they never go through
    // Recently closed, so deleting them first would lose them for good on a refusal.
    for s in &plan.asleep {
        if let Some(i) = lifecycle::session_index(ctx.state, &s.id) {
            ctx.state.app.sessions.remove(i);
            lifecycle::forget_session(ctx.state, &s.id);
        }
    }
    ctx.state.persist();
    let mut msg = format!("Removed the worktree {}.", worktrees::name(&plan.path));
    let mut branch_kept = None;
    if let Some((b, d)) = &deleted {
        if d.ok {
            msg.push_str(&format!(" Deleted the branch {b}."));
        } else {
            msg.push_str(&format!(" Kept the branch {b}: git branch -d refused (it is probably not merged)."));
            branch_kept = Some(d.output.clone());
        }
    }
    reply.send(Ok(ToolOutput::new(
        msg,
        json!({
            "path": plan.path,
            "closed_sessions": closed,
            "deleted_asleep": plan.asleep.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            "branch_deleted": deleted.as_ref().is_some_and(|(_, d)| d.ok),
            "branch_kept_reason": branch_kept,
        }),
    )));
}
