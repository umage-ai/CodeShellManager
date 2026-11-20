//! `usage_stats`: how many sessions each command started and how long they ran (WPF Settings →
//! Usage). Counted locally in the search index's database (`usage_stats` table) when a
//! session's process starts and ends; nothing leaves the machine.

use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![ToolSpec {
        id: "usage_stats",
        title: "Usage stats",
        summary: "Per command (claude, pwsh, …): how many sessions started, their total running time in seconds, and the last day (UTC) it was used. Counted on this machine only.",
        toolset: Toolset::App,
        effects: Effects::READ_ONLY,
        input_schema: empty_schema,
        run: usage_stats,
    }]
}

fn usage_stats(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let ix = ctx.state.rt.index.clone().ok_or_else(|| ToolError::Refused("usage stats are not kept in this run (there is no index).".into()))?;
    let reply = ctx.park().ok_or_else(|| ToolError::Failed("this tool cannot run outside a call".into()))?;
    ix.call(
        |db| db.usage_stats(),
        move |r| {
            reply.send(match r {
                Ok(rows) => {
                    let sessions: i64 = rows.iter().map(|r| r.sessions).sum();
                    let msg = match rows.len() {
                        0 => "No sessions counted yet.".to_string(),
                        n => format!("{sessions} sessions across {n} commands."),
                    };
                    Ok(ToolOutput::new(msg, json!({"commands": rows})))
                }
                Err(e) => Err(ToolError::Failed(e.to_string())),
            });
        },
    );
    Ok(ToolOutput::message(""))
}
