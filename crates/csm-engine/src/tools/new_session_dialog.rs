//! `session_new_dialog`: open the New session dialog, optionally with a folder filled in.
//!
//! This is what Explorer's "Open in CodeShellManager" entry ends in (`codeshell.exe --open-folder
//! "%V"`, `csm_core::open_folder`): the second process hands the folder to the running app through
//! this tool, or, when none runs, the app calls it once it is up. The tool only *shows* the dialog.
//! It never starts a session and never creates a folder: the person picks the command and confirms
//! with the dialog's own button, which is `session_new` (and `folder_probe` / `create_folder` when
//! the folder has gone in the meantime, exactly as for a path typed into the dialog).
//!
//! The folder is untrusted text from a command line: it is cleaned and refused by the pure rules
//! of `csm_core::open_folder`, and its existence is checked on a worker thread (a sleeping network
//! share can take a while), never on the engine thread.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::events::Event;
use crate::tool::{Caller, Effects, ToolError, ToolOutput, ToolSpec, Toolset, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        id: "session_new_dialog",
        title: "New session dialog…",
        summary: "Bring the window to the front and open the New session dialog, with `folder` filled in when given (an absolute path to a folder that exists; relative, shell, device and URL paths are refused). It only shows the dialog: nothing starts and nothing is created until the person confirms in it. This is what Explorer's \"Open in CodeShellManager\" entry calls.",
        toolset: Toolset::Core,
        effects: Effects::CHANGES_UI_STATE,
        input_schema: schema_of::<DialogParams>,
        run: session_new_dialog,
    }]
}

#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct DialogParams {
    /// An absolute local folder (or a UNC share folder) that exists. Omit for an empty dialog.
    #[serde(default)]
    folder: Option<String>,
}

fn session_new_dialog(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if matches!(ctx.caller, Caller::Peer { .. }) {
        return Err(ToolError::Refused("a paired device cannot open a dialog on this app's window".into()));
    }
    let p: DialogParams = parse_params(params)?;
    let Some(raw) = p.folder else {
        ctx.events.emit(Event::NewSessionRequested { folder: None });
        return Ok(ToolOutput::message("Opened the New session dialog."));
    };
    let folder = match csm_core::open_folder::clean(&raw) {
        Ok(f) => f,
        Err(why) => return Err(refuse(ctx, &why)),
    };
    let Some(reply) = ctx.park() else { return Err(ToolError::Failed("no answer channel".into())) };
    let engine = ctx.engine.clone();
    let quiet = *ctx.caller == Caller::Ui;
    let spawned = std::thread::Builder::new().name("csm-new-dialog".into()).spawn(move || {
        let is_dir = std::fs::metadata(&folder).map(|m| m.is_dir());
        engine.post(Box::new(move |ctx: &mut Ctx<'_>| {
            let answer = match is_dir {
                Ok(true) => {
                    ctx.events.emit(Event::NewSessionRequested { folder: Some(folder.clone()) });
                    Ok(ToolOutput::new(format!("Opened the New session dialog in {folder}."), json!({"folder": folder})))
                }
                Ok(false) => Err(not_shown(ctx, quiet, &format!("{folder} is a file, not a folder"))),
                Err(_) => Err(not_shown(ctx, quiet, &format!("the folder {folder} does not exist or cannot be read"))),
            };
            reply.send(answer);
        }));
    });
    match spawned {
        Ok(_) => Ok(ToolOutput::message("Checking the folder.")),
        Err(e) => Err(ToolError::Failed(format!("could not start the folder check: {e}"))),
    }
}

/// A refusal before any thread: the window comes forward with a toast so the person sees why
/// (the caller may be another process with no window of its own).
fn refuse(ctx: &mut Ctx<'_>, why: &str) -> ToolError {
    let quiet = *ctx.caller == Caller::Ui;
    not_shown(ctx, quiet, why)
}

fn not_shown(ctx: &mut Ctx<'_>, quiet: bool, why: &str) -> ToolError {
    let message = format!("Could not open that folder in a new session: {why}.");
    if !quiet {
        ctx.events.emit(Event::WindowShowRequested);
        ctx.events.emit(Event::Toast { message: message.clone() });
    }
    ToolError::InvalidParams(message)
}
