//! `state_snapshots`: the rolling `state.json` snapshots the saver keeps (issue #166), so a
//! person (or an agent helping them) can find the generation to go back to. Restoring is a file
//! copy done with the app closed (copy a snapshot over `state.json`); there is no live restore,
//! because the running app would overwrite the file with its in-memory state on the next save.

use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        id: "state_snapshots",
        title: "State snapshots",
        summary: "The rolling snapshots of state.json (one an hour for a day, one a day for a week), newest first, with their files and the folder. To go back to one, quit the app and copy it over state.json.",
        toolset: Toolset::App,
        effects: Effects::READ_ONLY,
        input_schema: empty_schema,
        run: state_snapshots,
    }]
}

fn state_snapshots(ctx: &mut Ctx<'_>, _params: Value) -> Reply {
    let Some(path) = ctx.state.options.save_path.clone() else {
        return Ok(ToolOutput::new("No state is saved in this run (isolated), so there are no snapshots.", json!({"snapshots": []})));
    };
    let Some(reply) = ctx.park() else { return Ok(ToolOutput::message("No answer channel.")) };
    // Listing a folder is disk I/O: never on the engine thread.
    let started = std::thread::Builder::new().name("csm-snapshots".into()).spawn(move || {
        let list: Vec<Value> = crate::persist::list_snapshots(&path)
            .into_iter()
            .map(|(ms, p)| {
                let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                json!({"taken": csm_core::time::Timestamp::from_epoch_ms(ms).as_str(), "taken_ms": ms, "file": p.display().to_string(), "bytes": size})
            })
            .collect();
        let message = match list.len() {
            0 => "No snapshots yet (the first is taken on the first save).".to_owned(),
            1 => "1 snapshot.".to_owned(),
            n => format!("{n} snapshots."),
        };
        let folder = crate::persist::snapshots_dir(&path).display().to_string();
        reply.send(Ok(ToolOutput::new(message, json!({"folder": folder, "state_file": path.display().to_string(), "snapshots": list}))));
    });
    match started {
        Ok(_) => Ok(ToolOutput::message("Looking.")),
        Err(e) => Err(ToolError::Failed(format!("could not list the snapshots: {e}"))),
    }
}
