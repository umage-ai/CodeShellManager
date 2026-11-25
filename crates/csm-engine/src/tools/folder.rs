//! `folder_probe`, and the rules `session_new { create_folder }` follows to make a missing folder.
//!
//! The new-session dialog asks `folder_probe` (read-only, on a worker thread: a mapped or
//! sleeping drive can take a while to answer) whether a typed path exists, and if not, which
//! levels would be created. The creation itself is `session_new`'s, behind its
//! file-system-write effect; both use [`plan`], so what the dialog shows is what is made.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        id: "folder_probe",
        title: "Check a folder",
        // Read-only: it only looks (metadata), nothing is created or changed.
        summary: "Whether a local folder (an absolute path) exists, and when it doesn't, whether `session_new` with `create_folder` could make it: the nearest existing folder above it and the missing levels. Never creates anything.",
        toolset: Toolset::Core,
        effects: Effects::READ_ONLY,
        input_schema: schema_of::<ProbeParams>,
        run: folder_probe,
    }]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct ProbeParams {
    /// An absolute local path.
    path: String,
}

/// What making a folder would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Plan {
    /// The folder is already there.
    Exists,
    /// The nearest existing folder above it, and the levels to make below that, outermost first.
    Create { ancestor: PathBuf, missing: Vec<String> },
}

/// Checks `path` and says what `create_dir_all` would make. Blocking file-system reads.
/// Refuses (with the reason) a path that is not absolute, has control characters or quotes, is a
/// device path (`\\?\`, `\\.\`), has `.` or `..`, whose nearest existing ancestor is not a
/// directory (or that has none, which includes a missing drive), or that is itself a file.
pub(crate) fn plan(path: &str) -> Result<Plan, String> {
    let p = path.trim();
    if p.is_empty() {
        return Err("no folder given".into());
    }
    if p.chars().any(|c| c.is_control() || c == '"') {
        return Err("the folder path has control characters or quotes".into());
    }
    let norm = p.replace('/', "\\");
    if norm.starts_with("\\\\?\\") || norm.starts_with("\\\\.\\") {
        return Err("device paths (\\\\?\\ and \\\\.\\) are not accepted".into());
    }
    let path = Path::new(p);
    if !path.is_absolute() {
        return Err("the folder must be an absolute path".into());
    }
    if path.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir)) {
        return Err("the folder path must not contain `.` or `..`".into());
    }
    match std::fs::metadata(path) {
        Ok(m) if m.is_dir() => return Ok(Plan::Exists),
        Ok(_) => return Err("that path is a file, not a folder".into()),
        Err(_) => {}
    }
    for anc in path.ancestors().skip(1) {
        match std::fs::metadata(anc) {
            Ok(m) if m.is_dir() => {
                let rest = path.strip_prefix(anc).map_err(|_| "the folder path could not be split".to_string())?;
                let mut missing = Vec::new();
                for c in rest.components() {
                    match c {
                        Component::Normal(n) => missing.push(n.to_string_lossy().into_owned()),
                        _ => return Err("the folder path is not a plain path".into()),
                    }
                }
                if missing.is_empty() {
                    return Err("the folder could not be checked".into());
                }
                return Ok(Plan::Create { ancestor: anc.to_path_buf(), missing });
            }
            Ok(_) => return Err(format!("{} is a file, so a folder cannot be made inside it", anc.display())),
            Err(_) => {}
        }
    }
    Err("no folder above it exists (a drive is never created)".into())
}

/// Makes the missing levels of `path` (see [`plan`]). `Ok(true)` when something was made.
pub(crate) fn create(path: &str) -> Result<bool, String> {
    match plan(path)? {
        Plan::Exists => Ok(false),
        Plan::Create { .. } => {
            std::fs::create_dir_all(path.trim()).map_err(|e| format!("could not create {}: {e}", path.trim()))?;
            Ok(true)
        }
    }
}

fn folder_probe(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: ProbeParams = parse_params(params)?;
    let Some(reply) = ctx.park() else { return Err(ToolError::Failed("no answer channel".into())) };
    let started = std::thread::Builder::new().name("csm-folder-probe".into()).spawn(move || {
        let data = match plan(&p.path) {
            Ok(Plan::Exists) => json!({"exists": true, "can_create": false}),
            Ok(Plan::Create { ancestor, missing }) => {
                json!({"exists": false, "can_create": true, "ancestor": ancestor.to_string_lossy(), "missing": missing})
            }
            Err(why) => json!({"exists": false, "can_create": false, "reason": why}),
        };
        reply.send(Ok(ToolOutput::new("Checked.", data)));
    });
    match started {
        Ok(_) => Ok(ToolOutput::message("Looking.")),
        Err(e) => Err(ToolError::Failed(format!("could not start the folder check: {e}"))),
    }
}
