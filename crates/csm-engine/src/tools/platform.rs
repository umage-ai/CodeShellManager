//! Platform probes as tools: today `wsl_distros`, for the new-session panel's distro picker.

use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        id: "wsl_distros",
        title: "WSL distributions",
        // Declared read-only although it starts `wsl.exe`: the argv is fixed (`-l -v`), nothing
        // the caller sends reaches it, and the answer is a list of names. Asking for consent to
        // fill a picker would be noise.
        summary: "The installed WSL distributions (name, version, state, default), default first. Empty when WSL is not installed or this is not Windows.",
        toolset: Toolset::App,
        effects: Effects::READ_ONLY,
        input_schema: empty_schema,
        run: wsl_distros,
    }]
}

fn wsl_distros(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let Some(reply) = ctx.park() else { return Ok(ToolOutput::message("No answer channel.")) };
    // `wsl.exe` can boot a stopped distro VM: never on the engine thread.
    let started = std::thread::Builder::new().name("csm-wsl-distros".into()).spawn(move || {
        let list: Vec<Value> = crate::wsl_probe::distros()
            .into_iter()
            .map(|d| json!({"name": d.name, "version": d.version, "state": d.state, "default": d.is_default}))
            .collect();
        let message = match list.len() {
            0 => "No WSL distributions found.".to_owned(),
            1 => "1 WSL distribution.".to_owned(),
            n => format!("{n} WSL distributions."),
        };
        reply.send(Ok(ToolOutput::new(message, Value::Array(list))));
    });
    match started {
        Ok(_) => Ok(ToolOutput::message("Looking.")),
        Err(e) => Err(ToolError::Failed(format!("could not start the WSL probe: {e}"))),
    }
}
