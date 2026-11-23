//! Storage: how big the search index is, and keeping it from growing without end
//! (`csm_index::Db::prune`, docs on `csm_index::retention`).
//!
//! The engine prunes on its own a minute after start and then every hour, in a run that keeps an
//! index on disk: the output of sessions that are gone (not in the session list and not seen for
//! [`CLOSED_GRACE`]), the oldest lines over `IndexMaxMb`, and Claude usage rows older than
//! `ClaudeUsageRetentionDays`. `storage_clean` does the same at once. All of it runs on the index
//! thread.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema};

/// A session not in the list keeps its output this long after it was last seen.
pub const CLOSED_GRACE: Duration = Duration::from_secs(30 * 24 * 3600);
/// The first automatic prune, after the engine starts (restore is done by then).
const FIRST_PRUNE: Duration = Duration::from_secs(60);
/// Between automatic prunes.
const PRUNE_EVERY: Duration = Duration::from_secs(3600);

pub(crate) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "storage_info",
            title: "Storage use",
            summary: "How much disk the search index uses: file size, free space inside it, terminal output (bytes, rows, sessions), Claude usage rows and status rows, with the limits that keep it in check (IndexMaxMb, ClaudeUsageRetentionDays, the 30-day grace for closed sessions).",
            toolset: Toolset::Core,
            effects: Effects::IDEMPOTENT,
            input_schema: empty_schema,
            run: storage_info,
        },
        ToolSpec {
            id: "storage_clean",
            title: "Clean up storage now",
            summary: "Prune the search index now, as the hourly clean-up does: output of sessions that are gone and were not seen for 30 days, the oldest output over IndexMaxMb, Claude usage rows older than ClaudeUsageRetentionDays; then give the freed space back to the disk.",
            toolset: Toolset::Core,
            effects: Effects::from_bits_truncate(Effects::MUTATES_STATE.bits() | Effects::MAY_DISCARD_DATA.bits()),
            input_schema: empty_schema,
            run: storage_clean,
        },
    ]
}

/// What may be removed now, from the settings and the session list.
fn policy(ctx: &Ctx<'_>, now_ms: i64) -> csm_index::RetentionPolicy {
    let s = &ctx.state.app.settings;
    let keep: HashSet<String> = ctx.state.app.sessions.iter().map(|x| x.id.clone()).collect();
    let days = i64::from(s.claude_usage_retention_days.max(0));
    csm_index::RetentionPolicy {
        keep_sessions: keep,
        closed_grace_ms: i64::try_from(CLOSED_GRACE.as_millis()).unwrap_or(i64::MAX),
        max_output_bytes: i64::from(s.index_max_mb.max(1)) * 1024 * 1024,
        claude_before_ms: (days > 0).then(|| now_ms.saturating_sub(days * 86_400_000)),
    }
}

fn index(ctx: &Ctx<'_>) -> Result<csm_index::Index, ToolError> {
    ctx.state.rt.index.clone().ok_or_else(|| ToolError::Refused("there is no search index in this run".into()))
}

fn limits(ctx: &Ctx<'_>) -> Value {
    let s = &ctx.state.app.settings;
    json!({
        "index_max_mb": s.index_max_mb,
        "claude_usage_retention_days": s.claude_usage_retention_days,
        "closed_session_grace_days": CLOSED_GRACE.as_secs() / 86_400,
    })
}

fn storage_info(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let ix = index(ctx)?;
    let limits = limits(ctx);
    let reply = ctx.park().ok_or_else(|| ToolError::Failed("this tool cannot run outside a call".into()))?;
    ix.call(
        |db| db.storage(),
        move |r| {
            reply.send(match r {
                Ok(info) => {
                    let mb = |b: i64| b as f64 / (1024.0 * 1024.0);
                    let msg = format!(
                        "Search index: {:.0} MB on disk ({:.0} MB of terminal output in {} sessions, {} Claude usage rows).",
                        mb(info.file_bytes),
                        mb(info.output_bytes),
                        info.output_sessions,
                        info.claude_rows
                    );
                    Ok(ToolOutput::new(msg, json!({"index": info, "limits": limits})))
                }
                Err(e) => Err(ToolError::Failed(e.to_string())),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

fn storage_clean(ctx: &mut Ctx<'_>, _p: Value) -> Reply {
    let ix = index(ctx)?;
    let policy = policy(ctx, crate::session::now_ms());
    let reply = ctx.park().ok_or_else(|| ToolError::Failed("this tool cannot run outside a call".into()))?;
    ctx.state.rt.storage_next_prune = Some(Instant::now() + PRUNE_EVERY);
    ix.call(
        move |db| db.prune(&policy, crate::session::now_ms()),
        move |r| {
            reply.send(match r {
                Ok(rep) => {
                    let mb = |b: i64| b as f64 / (1024.0 * 1024.0);
                    let msg = format!(
                        "Cleaned up: {} closed sessions and {:.0} MB of old output removed, {} old Claude usage rows; {:.0} MB given back to the disk.",
                        rep.sessions_dropped,
                        mb(rep.output_bytes_deleted),
                        rep.claude_rows_deleted,
                        mb(rep.file_bytes_freed)
                    );
                    Ok(ToolOutput::new(msg, json!(rep)))
                }
                Err(e) => Err(ToolError::Failed(e.to_string())),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

/// From the engine tick: the automatic prune, a minute after start and then hourly, only in a
/// run that keeps its index on disk (never under `--clean` or an isolated run).
pub(crate) fn tick(ctx: &mut Ctx<'_>) {
    if !ctx.state.options.saves() {
        return;
    }
    let Some(ix) = ctx.state.rt.index.clone() else { return };
    let now = Instant::now();
    match ctx.state.rt.storage_next_prune {
        // The first tick schedules the first prune.
        None => {
            ctx.state.rt.storage_next_prune = Some(now + FIRST_PRUNE);
            return;
        }
        Some(next) if now < next => return,
        Some(_) => ctx.state.rt.storage_next_prune = Some(now + PRUNE_EVERY),
    }
    let policy = policy(ctx, crate::session::now_ms());
    ix.call(
        move |db| db.prune(&policy, crate::session::now_ms()),
        |r| match r {
            Ok(rep) if rep.sessions_dropped > 0 || rep.output_rows_deleted > 0 || rep.claude_rows_deleted > 0 || rep.converted => {
                log::info!("storage: pruned the index: {rep:?}");
            }
            Ok(_) => {}
            Err(e) => log::warn!("storage: could not prune the index: {e}"),
        },
    );
}
