//! `project_docs_list` and `project_doc_get`: the project docs (`README.md`, `CLAUDE.md`,
//! `AGENTS.md`, `CLAUDE.local.md`, `GEMINI.md`, `.claude/CLAUDE.md`) of a session's folder and of
//! the repository it is in, for the inspector's Docs tab, the CLI and MCP.
//!
//! Read-only: nothing is written and no process starts. The disk is read on a worker thread and
//! the reply is parked until it lands (`csm_services::project_docs`). `project_doc_get` reads only
//! a file the list names, compared by canonical path, so a `file` with `../` in it, a link or any
//! other path is refused.

use csm_core::models::SessionKind;
use csm_services::project_docs::{self as pd, DocEntry, DocError, DocList};
use serde::Deserialize;
use serde_json::{Value, json};

use super::find;
use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "project_docs_list",
            title: "Project docs",
            summary: "The project docs of a session's folder (or an absolute `path`): README.md, CLAUDE.md, AGENTS.md, CLAUDE.local.md, GEMINI.md and .claude/CLAUDE.md, matched ignoring case, in that order; MCP clients name a `session_id`; the folder is never a network path. Also those at the top of the git repository when that is another folder. Name, relative and absolute path, size and modified time of each, and the one to show first.",
            toolset: Toolset::Notes,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<ListParams>,
            run: project_docs_list,
        },
        ToolSpec {
            id: "project_doc_get",
            title: "Read a project doc",
            summary: "The text of one project doc (`file`: its relative path, absolute path or name from project_docs_list). Anything that is not one of the listed docs is refused. Links are never followed. Cut at 1 MiB (`truncated`); a file that is not UTF-8 is decoded lossily (`lossy`).",
            toolset: Toolset::Notes,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<GetParams>,
            run: project_doc_get,
        },
    ]
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct ListParams {
    /// The session whose folder to look in. Give this or `path`.
    session_id: Option<String>,
    /// An absolute local folder (not a network path; MCP clients give `session_id` instead). Give this or `session_id`.
    path: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct GetParams {
    /// The session whose folder to look in. Give this or `path`.
    #[serde(default)]
    session_id: Option<String>,
    /// An absolute local folder (not a network path; MCP clients give `session_id` instead). Give this or `session_id`.
    #[serde(default)]
    path: Option<String>,
    /// The doc: its `rel`, `path` or `name` as project_docs_list gives them.
    file: String,
}

/// The folder to look in: the session's, or an absolute `path`.
fn folder_of(ctx: &Ctx<'_>, session_id: Option<&str>, path: Option<&str>) -> Result<String, ToolError> {
    // An MCP client or peer reads only the folders of sessions the person has open; it does not
    // get to name a folder (SEC-13).
    if ctx.caller.is_external() && path.is_some() {
        return Err(ToolError::Refused("name a `session_id`: project docs are read from a session's folder, not from a path an MCP client picks".into()));
    }
    match (session_id, path) {
        (Some(_), Some(_)) => Err(ToolError::InvalidParams("give `session_id` or `path`, not both".into())),
        (None, None) => Err(ToolError::InvalidParams("give `session_id` or `path`".into())),
        (Some(id), None) => {
            let i = find(ctx.state, id)?;
            let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound(format!("no session `{id}`")))?;
            if s.kind == SessionKind::Ssh {
                return Err(ToolError::Refused("project docs are read from a local or WSL folder; an SSH session's folder is on another machine".into()));
            }
            if s.working_folder.trim().is_empty() {
                return Err(ToolError::Refused("this session has no folder".into()));
            }
            Ok(s.working_folder.clone())
        }
        (None, Some(p)) => {
            let p = p.trim();
            if p.is_empty() || p.chars().any(char::is_control) || !std::path::Path::new(p).is_absolute() {
                return Err(ToolError::InvalidParams("`path` must be an absolute folder".into()));
            }
            Ok(p.to_owned())
        }
    }
}

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> Result<(), ToolError> {
    std::thread::Builder::new().name(name.into()).spawn(f).map(|_| ()).map_err(|e| ToolError::Failed(format!("could not start a worker: {e}")))
}

fn doc_error(e: DocError) -> ToolError {
    match e {
        DocError::NotListed(m) => ToolError::Refused(m),
        DocError::BadFolder(m) => ToolError::Refused(m),
        DocError::Io(m) => ToolError::Failed(m),
    }
}

fn entry_json(e: &DocEntry) -> Value {
    json!({"name": e.name, "rel": e.rel, "path": e.path, "scope": e.scope, "size": e.size, "modified_ms": e.modified_ms})
}

fn list_output(folder: &str, l: &DocList) -> ToolOutput {
    let msg = if l.entries.is_empty() {
        format!("No project docs in {folder} (looked for {}).", pd::NAMES.join(", "))
    } else {
        format!("{} project doc(s): {}.", l.entries.len(), l.entries.iter().map(|e| e.rel.as_str()).collect::<Vec<_>>().join(", "))
    };
    ToolOutput::new(
        msg,
        json!({
            "folder": folder,
            "repo_root": l.repo_root,
            "docs": l.entries.iter().map(entry_json).collect::<Vec<_>>(),
            "default": l.default_rel(),
            "looked_for": pd::NAMES,
        }),
    )
}

fn project_docs_list(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ListParams = parse_params(params)?;
    let folder = folder_of(ctx, p.session_id.as_deref(), p.path.as_deref())?;
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("this tool cannot run outside a call".into()));
    };
    spawn("csm-docs-list", move || {
        reply.send(pd::list(&folder).map(|l| list_output(&folder, &l)).map_err(doc_error));
    })?;
    Ok(ToolOutput::message("Looking for project docs."))
}

fn project_doc_get(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: GetParams = parse_params(params)?;
    let folder = folder_of(ctx, p.session_id.as_deref(), p.path.as_deref())?;
    let file = p.file;
    let Some(reply) = ctx.park() else {
        return Err(ToolError::Failed("this tool cannot run outside a call".into()));
    };
    spawn("csm-docs-get", move || {
        let r = pd::read(&folder, &file).map_err(doc_error).map(|t| {
            let mut msg = format!("{} ({} bytes)", t.entry.rel, t.entry.size);
            if t.truncated {
                msg.push_str(&format!("; cut at {} KiB", pd::MAX_BYTES / 1024));
            }
            if t.lossy {
                msg.push_str("; not UTF-8, bad bytes replaced");
            }
            let mut data = entry_json(&t.entry);
            data["text"] = json!(t.text);
            data["truncated"] = json!(t.truncated);
            data["lossy"] = json!(t.lossy);
            ToolOutput::new(format!("{msg}."), data)
        });
        reply.send(r);
    })?;
    Ok(ToolOutput::message("Reading the doc."))
}
