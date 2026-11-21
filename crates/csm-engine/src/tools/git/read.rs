//! `git_diff`, `git_show_file` and `git_ls_files`: read-only views of a repository for people,
//! agents and add-ons (the Review add-on calls them through `csm/call`).
//!
//! Argv only, `--no-ext-diff --no-textconv`, paths after `--` (see `csm_services::git::read`).
//! Each runs on a worker thread and parks its reply; none changes the repository. A patch or a
//! file is cut at `max_bytes` (default 512 KiB, at most 4 MiB) and says so in `truncated`.

use std::path::Path;

use csm_services::git::read::{self as r, DiffRequest, DiffSource};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{spawn, target};
use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    let read = Effects::IDEMPOTENT.union(Effects::LAUNCHES_PROCESS);
    vec![
        ToolSpec {
            id: "git_diff",
            title: "Git diff",
            summary: "The changed files and the unified patch of a session's repository (or an absolute `folder`): `working` (working tree against HEAD, plus untracked files), `staged`, or `range` (`base...head`). Read-only: no external diff program, no textconv. The patch is cut at max_bytes (default 512 KiB) and says so.",
            toolset: Toolset::Git,
            effects: read,
            input_schema: schema_of::<DiffParams>,
            run: git_diff,
        },
        ToolSpec {
            id: "git_show_file",
            title: "Show a file",
            summary: "One file's text: at `rev` (HEAD, a sha, `:0` for the index) or, with no rev, from the working tree (a file git ignores is refused). Returns the blob id, whether it is binary, and the text cut at max_bytes (default 512 KiB).",
            toolset: Toolset::Git,
            effects: read,
            input_schema: schema_of::<ShowParams>,
            run: git_show_file,
        },
        ToolSpec {
            id: "git_ls_files",
            title: "List files",
            summary: "The tracked files of a session's repository (or an absolute `folder`), from the index or from the tree of `rev`. At most 100,000 paths.",
            toolset: Toolset::Git,
            effects: read,
            input_schema: schema_of::<LsParams>,
            run: git_ls_files,
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DiffParams {
    /// The session whose folder to read. Give this or `folder`.
    #[serde(default)]
    session_id: Option<String>,
    /// An absolute local folder inside a repository, for a repository no session is in.
    #[serde(default)]
    folder: Option<String>,
    /// `working` (default), `staged` or `range`.
    #[serde(default)]
    source: Option<String>,
    /// For `range`: the base revision (required).
    #[serde(default)]
    base: Option<String>,
    /// For `range`: the head revision (default HEAD).
    #[serde(default)]
    head: Option<String>,
    /// Restrict to these repository-relative paths (at most 64; a renamed file needs both names).
    #[serde(default)]
    paths: Vec<String>,
    /// Lines of context (0 to 20, default 3).
    #[serde(default)]
    context: Option<u32>,
    /// `false` returns the file list without the patch (default true).
    #[serde(default)]
    patch: Option<bool>,
    /// Cut the patch here (default 524288, at most 4194304).
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ShowParams {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    /// `HEAD`, a branch, a sha, or `:0` (the index). Absent: the working tree.
    #[serde(default)]
    rev: Option<String>,
    /// Repository-relative path with forward slashes.
    path: String,
    /// Cut the text here (default 524288, at most 4194304).
    #[serde(default)]
    max_bytes: Option<usize>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct LsParams {
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    folder: Option<String>,
    /// List the tree of this revision instead of the index.
    #[serde(default)]
    rev: Option<String>,
}

/// The folder git runs in: the session's, or an absolute local `folder`.
fn folder_of(ctx: &Ctx<'_>, session_id: Option<&str>, folder: Option<&str>) -> Result<String, ToolError> {
    match (session_id, folder) {
        (Some(_), Some(_)) => Err(ToolError::InvalidParams("give `session_id` or `folder`, not both".into())),
        (Some(id), None) => Ok(target(ctx, id)?.folder),
        (None, Some(f)) => {
            let f = f.trim();
            if f.is_empty() || f.chars().any(char::is_control) {
                return Err(ToolError::InvalidParams("`folder` is empty or has control characters".into()));
            }
            let wsl = csm_services::git::run::is_wsl_folder(f);
            if !wsl && !Path::new(f).is_absolute() {
                return Err(ToolError::InvalidParams("`folder` must be an absolute path".into()));
            }
            Ok(f.to_owned())
        }
        (None, None) => Err(ToolError::InvalidParams("give `session_id` or `folder`".into())),
    }
}

fn bad(e: String) -> ToolError {
    ToolError::InvalidParams(e)
}

fn git_diff(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: DiffParams = parse_params(params)?;
    let folder = folder_of(ctx, p.session_id.as_deref(), p.folder.as_deref())?;
    let source = DiffSource::parse(p.source.as_deref().unwrap_or("working")).ok_or_else(|| bad("`source` is `working`, `staged` or `range`".into()))?;
    if p.paths.len() > r::MAX_PATHS {
        return Err(bad(format!("at most {} paths", r::MAX_PATHS)));
    }
    let paths = p.paths.iter().map(|x| r::check_repo_path(x)).collect::<Result<Vec<_>, _>>().map_err(bad)?;
    for rev in [p.base.as_deref(), p.head.as_deref()].into_iter().flatten() {
        r::check_rev(rev).map_err(bad)?;
    }
    if source == DiffSource::Range && p.base.is_none() {
        return Err(bad("`range` needs `base`".into()));
    }
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    let (context, want_patch, max_bytes) = (p.context.unwrap_or(3).min(20), p.patch.unwrap_or(true), p.max_bytes.unwrap_or(r::DEFAULT_MAX_BYTES));
    let (base, head) = (p.base, p.head);
    spawn("csm-git-diff", move || {
        let req = DiffRequest { source, base: base.as_deref(), head: head.as_deref(), paths: &paths, context, want_patch, max_bytes };
        reply.send(r::diff(&folder, &req).map(diff_output).map_err(ToolError::Failed));
    })?;
    Ok(ToolOutput::message("reading the diff"))
}

fn diff_output(d: r::DiffOut) -> ToolOutput {
    let files: Vec<Value> = d
        .files
        .iter()
        .map(|f| {
            let mut o = json!({"path": f.path, "status": f.status.to_string()});
            if let Some(old) = &f.old_path {
                o["old_path"] = json!(old);
            }
            o
        })
        .collect();
    let msg = format!(
        "{} changed file(s){}{}.",
        d.files.len(),
        if d.untracked.is_empty() { String::new() } else { format!(", {} untracked", d.untracked.len()) },
        if d.patch_truncated { "; the patch was cut" } else { "" }
    );
    ToolOutput::new(
        msg,
        json!({"head": d.head, "files": files, "files_truncated": d.files_truncated, "patch": d.patch, "truncated": d.patch_truncated, "untracked": d.untracked}),
    )
}

fn git_show_file(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ShowParams = parse_params(params)?;
    let folder = folder_of(ctx, p.session_id.as_deref(), p.folder.as_deref())?;
    let path = r::check_repo_path(&p.path).map_err(bad)?;
    if let Some(rev) = p.rev.as_deref()
        && rev != ":0"
    {
        r::check_rev(rev).map_err(bad)?;
    }
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    let (rev, max_bytes) = (p.rev, p.max_bytes.unwrap_or(r::DEFAULT_MAX_BYTES));
    spawn("csm-git-show", move || {
        let out = r::show_file(&folder, rev.as_deref(), &path, max_bytes).map_err(ToolError::Failed).map(|f| {
            let at = rev.as_deref().unwrap_or("the working tree");
            let msg = if f.binary {
                format!("`{path}` at {at} is binary ({} bytes).", f.size)
            } else {
                format!("`{path}` at {at}: {} bytes{}.", f.size, if f.truncated { ", cut" } else { "" })
            };
            ToolOutput::new(msg, json!({"path": path, "rev": rev, "head": r::head_id(&folder), "blob": f.blob, "size": f.size, "binary": f.binary, "truncated": f.truncated, "text": f.text}))
        });
        reply.send(out);
    })?;
    Ok(ToolOutput::message("reading the file"))
}

fn git_ls_files(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: LsParams = parse_params(params)?;
    let folder = folder_of(ctx, p.session_id.as_deref(), p.folder.as_deref())?;
    if let Some(rev) = p.rev.as_deref() {
        r::check_rev(rev).map_err(bad)?;
    }
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("nothing to answer".into()));
    };
    let rev = p.rev;
    spawn("csm-git-ls", move || {
        let out = r::ls_files(&folder, rev.as_deref())
            .map_err(ToolError::Failed)
            .map(|(files, truncated)| ToolOutput::new(format!("{} file(s){}.", files.len(), if truncated { ", the list was cut" } else { "" }), json!({"files": files, "truncated": truncated})));
        reply.send(out);
    })?;
    Ok(ToolOutput::message("listing files"))
}
