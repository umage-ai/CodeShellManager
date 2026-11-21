//! Usage tools.
//!
//! - `usage_stats`: how many sessions each command started and how long they ran (WPF Settings →
//!   Usage). Counted locally in the index (`usage_stats` table) when a session's process starts
//!   and ends.
//! - `usage_session`, `usage_context`, `usage_summary`: Claude Code token and cost analytics from
//!   Claude's own transcripts (a port of ContextCost), read by `crate::usage_tail` into the
//!   index. Costs are **API-equivalent** (what the tokens would cost at API list prices), not
//!   what a subscription bills. Numbers only; nothing leaves the machine.

use std::collections::HashMap;

use jiff::Span;
use jiff::civil::Date;
use jiff::tz::TimeZone;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use csm_core::claude_usage::{self as cu, ContextFill, format_tokens, format_usd, summarize_session};
use csm_index::ClaudeGroupBy;

use crate::engine::{Ctx, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "usage_stats",
            title: "Usage stats",
            summary: "Per command (claude, pwsh, …): how many sessions started, their total running time in seconds, and the last day (UTC) it was used. Counted on this machine only.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: usage_stats,
        },
        ToolSpec {
            id: "usage_session",
            title: "Claude usage of a session",
            summary: "A Claude session's context fill (last main-thread request vs the model's window), API-equivalent cost and tokens (total and today), cache hit ratio, the last cache miss with the idle gap before it, compactions, and subagent cost. Read from Claude's transcripts; numbers only.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<SessionParams>,
            run: usage_session,
        },
        ToolSpec {
            id: "session_subagents",
            title: "Subagents of a Claude session",
            summary: "The subagents a Claude session has started (Task/Agent calls and dynamic-workflow agents): label, type, model, state (running, idle after 5 minutes without output, done), workflow run and phase, tokens and API-equivalent cost, plus workflow runs with per-phase done/total. Read from Claude Code's own transcript folder (an undocumented layout, parsed leniently); numbers and the agents' short descriptions only.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<SessionParams>,
            run: session_subagents,
        },
        ToolSpec {
            id: "usage_context",
            title: "Claude context fill",
            summary: "For every live Claude session: how full its context is (tokens, window, ratio, ok/warn/critical near auto-compact) and its API-equivalent cost so far.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: empty_schema,
            run: usage_context,
        },
        ToolSpec {
            id: "usage_summary",
            title: "Claude cost summary",
            summary: "API-equivalent cost and tokens of Claude sessions between from and to (today, week, month, all, or a YYYY-MM-DD local date; default the last 7 days), grouped by project, session or model. Unpriced models are listed, never counted as $0.",
            toolset: Toolset::App,
            effects: Effects::READ_ONLY,
            input_schema: schema_of::<SummaryParams>,
            run: usage_summary,
        },
    ]
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

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SessionParams {
    session_id: String,
}

#[derive(Deserialize, JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct SummaryParams {
    /// Start: `today`, `week` (the last 7 days), `month` (the last 30 days), `all`, or a local
    /// date `YYYY-MM-DD`. Default `week`.
    #[serde(default)]
    from: Option<String>,
    /// End (inclusive): `today` or a local date `YYYY-MM-DD`. Default: now.
    #[serde(default)]
    to: Option<String>,
    /// `project` (default), `session` or `model`.
    #[serde(default)]
    group_by: Option<GroupBy>,
}

#[derive(Deserialize, JsonSchema, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum GroupBy {
    Project,
    Session,
    Model,
}

const API_NOTE: &str = "API-equivalent: what these tokens would cost at Anthropic API list prices, not what a subscription bills.";

fn index_of(ctx: &Ctx<'_>) -> Result<csm_index::Index, ToolError> {
    ctx.state.rt.index.clone().ok_or_else(|| ToolError::Refused("Claude usage is not kept in this run (there is no index).".into()))
}

fn price_source(ctx: &Ctx<'_>) -> Value {
    match ctx.state.rt.usage.as_ref().and_then(|u| u.prices.get()) {
        Some(p) => json!(p),
        None => json!({"status": "not following transcripts in this run"}),
    }
}

fn display_name(s: &csm_core::models::ShellSession) -> String {
    if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() }
}

/// Local midnight of `day` (the day's first instant, so a DST gap at midnight is handled), as
/// epoch ms. Falls back to UTC midnight if the system time zone can't place it.
fn local_midnight(day: Date) -> i64 {
    day.to_zoned(TimeZone::system()).or_else(|_| day.to_zoned(TimeZone::UTC)).map_or(0, |z| z.timestamp().as_millisecond())
}

fn today() -> Date {
    jiff::Zoned::now().date()
}

/// A YYYY-MM-DD date, strictly.
fn parse_day(s: &str) -> Option<Date> {
    (s.len() == 10).then(|| s.parse::<Date>().ok()).flatten()
}

fn days_before(day: Date, n: i64) -> Date {
    day.checked_sub(Span::new().days(n)).unwrap_or(day)
}

/// The start of a period word or date (pure apart from the local time zone).
fn period_start(word: &str, today: Date) -> Result<i64, ToolError> {
    let day = match word.trim().to_ascii_lowercase().as_str() {
        "today" => today,
        "week" => days_before(today, 6),
        "month" => days_before(today, 29),
        "all" => return Ok(i64::MIN),
        other => parse_day(other).ok_or_else(|| ToolError::InvalidParams(format!("`{word}` is not today, week, month, all or a YYYY-MM-DD date")))?,
    };
    Ok(local_midnight(day))
}

/// The (exclusive) end of a period: the end of that local day.
fn period_end(word: &str, today: Date) -> Result<i64, ToolError> {
    let day = match word.trim().to_ascii_lowercase().as_str() {
        "today" | "now" => today,
        other => parse_day(other).ok_or_else(|| ToolError::InvalidParams(format!("`{word}` is not today or a YYYY-MM-DD date")))?,
    };
    Ok(local_midnight(day.tomorrow().unwrap_or(day)))
}

fn fill_json(c: &ContextFill) -> Value {
    json!({
        "fill_tokens": c.fill_tokens,
        "window_tokens": c.window_tokens,
        "ratio": c.ratio,
        "level": c.level,
        "model": c.model,
        "at_ms": c.at_ms,
        "warn_ratio": cu::WARN_RATIO,
        "critical_ratio": cu::CRITICAL_RATIO,
    })
}

fn usage_session(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SessionParams = parse_params(params)?;
    let i = super::find(ctx.state, &p.session_id)?;
    let s = ctx.state.app.sessions.get(i).cloned().ok_or_else(|| ToolError::NotFound("gone".into()))?;
    if !crate::launch::is_claude_command(&s.command) {
        return Err(ToolError::Refused(format!("{} is not a Claude session; usage is read from Claude's transcripts.", display_name(&s))));
    }
    if s.kind != csm_core::models::SessionKind::Local {
        return Err(ToolError::Refused(format!(
            "{} runs Claude over {}: only local Claude sessions' transcripts are read on this machine.",
            display_name(&s),
            if s.kind == csm_core::models::SessionKind::Wsl { "WSL" } else { "SSH" }
        )));
    }
    let ix = index_of(ctx)?;
    let prices = price_source(ctx);
    let following = ctx.state.rt.usage.is_some();
    let day_start = local_midnight(today());
    let reply = ctx.park().ok_or_else(|| ToolError::Failed("this tool cannot run outside a call".into()))?;
    let id = p.session_id.clone();
    ix.call(
        move |db| Ok((db.claude_requests_for(&id)?, db.claude_compactions_for(&id)?, db.claude_quality_for(&id)?)),
        move |r| {
            reply.send(match r {
                Ok((reqs, comps, quality)) => {
                    let u = summarize_session(&reqs, &comps, day_start, &s.args);
                    let name = display_name(&s);
                    let message = match &u.context {
                        None if !following => "Claude transcripts are not followed in this run.".to_string(),
                        None => format!("No Claude requests recorded for {name} yet."),
                        Some(c) => format!(
                            "{name}: context {} of {} ({:.0}%); {} API-equivalent ({} today).",
                            format_tokens(c.fill_tokens),
                            format_tokens(c.window_tokens),
                            c.ratio * 100.0,
                            format_usd(u.total.cost_usd),
                            format_usd(u.today.cost_usd)
                        ),
                    };
                    let mut data = serde_json::to_value(&u).unwrap_or(Value::Null);
                    if let Some(c) = &u.context {
                        data["context"] = fill_json(c);
                    }
                    data["session_id"] = json!(s.id);
                    data["claude_session_id"] = json!(s.claude_session_id);
                    data["following"] = json!(following);
                    data["data_quality"] = json!(quality);
                    data["prices"] = prices;
                    data["note"] = json!(API_NOTE);
                    Ok(ToolOutput::new(message, data))
                }
                Err(e) => Err(ToolError::Failed(e.to_string())),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

fn usage_context(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    if params.as_object().is_some_and(|o| !o.is_empty()) {
        return Err(ToolError::InvalidParams("this tool takes no params".into()));
    }
    let ix = index_of(ctx)?;
    let live: Vec<(String, String, String)> = ctx
        .state
        .app
        .sessions
        .iter()
        .filter(|s| crate::launch::is_claude_command(&s.command) && ctx.state.rt.sessions.contains_key(&s.id))
        .map(|s| (s.id.clone(), display_name(s), s.args.clone()))
        .collect();
    let reply = ctx.park().ok_or_else(|| ToolError::Failed("this tool cannot run outside a call".into()))?;
    let ids: Vec<String> = live.iter().map(|(id, ..)| id.clone()).collect();
    ix.call(
        move |db| db.claude_live(&ids),
        move |r| {
            reply.send(match r {
                Ok(rows) => {
                    let by: HashMap<&str, &csm_index::ClaudeLive> = rows.iter().map(|r| (r.csm_session.as_str(), r)).collect();
                    let sessions: Vec<Value> = live
                        .iter()
                        .map(|(id, name, args)| {
                            let row = by.get(id.as_str());
                            let fill = row.and_then(|r| {
                                let last = r.last_main.as_ref()?;
                                let window = cu::context_window(&last.model, args, r.largest_main_prompt);
                                Some(ContextFill::new(last.tokens.prompt(), window, last.model.clone(), last.ts_ms))
                            });
                            json!({
                                "session_id": id,
                                "name": name,
                                "context": fill.as_ref().map(fill_json),
                                "requests": row.map_or(0, |r| r.requests),
                                "cost_usd": row.map_or(0.0, |r| r.cost_usd),
                                "unpriced_requests": row.map_or(0, |r| r.unpriced_requests),
                            })
                        })
                        .collect();
                    let with = sessions.iter().filter(|s| !s["context"].is_null()).count();
                    let msg = format!("{} live Claude sessions, {with} with usage recorded.", sessions.len());
                    Ok(ToolOutput::new(msg, json!({"sessions": sessions, "note": API_NOTE})))
                }
                Err(e) => Err(ToolError::Failed(e.to_string())),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

fn usage_summary(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SummaryParams = parse_params(params)?;
    let ix = index_of(ctx)?;
    let today = today();
    let from = period_start(p.from.as_deref().unwrap_or("week"), today)?;
    let to = match &p.to {
        Some(w) => period_end(w, today)?,
        None => i64::MAX,
    };
    if to <= from {
        return Err(ToolError::InvalidParams("`to` is before `from`".into()));
    }
    let by = p.group_by.unwrap_or(GroupBy::Project);
    let names: HashMap<String, String> = ctx.state.app.sessions.iter().map(|s| (s.id.clone(), display_name(s))).collect();
    let reply = ctx.park().ok_or_else(|| ToolError::Failed("this tool cannot run outside a call".into()))?;
    let (group, by_id) = match by {
        GroupBy::Project => (ClaudeGroupBy::Project, "project"),
        GroupBy::Session => (ClaudeGroupBy::Session, "session"),
        GroupBy::Model => (ClaudeGroupBy::Model, "model"),
    };
    ix.call(
        move |db| db.claude_summary(from, to, group),
        move |r| {
            reply.send(match r {
                Ok(rows) => {
                    let mut total = cu::Totals::default();
                    let list: Vec<Value> = rows
                        .iter()
                        .map(|r| {
                            total.requests += r.requests;
                            total.tokens.add(&r.tokens);
                            total.cost_usd += r.cost_usd;
                            total.unpriced_requests += r.unpriced_requests;
                            for m in &r.unpriced_models {
                                if !total.unpriced_models.contains(m) {
                                    total.unpriced_models.push(m.clone());
                                }
                            }
                            let label = match by {
                                GroupBy::Session => names.get(&r.key).cloned().unwrap_or_else(|| format!("closed session {}", r.key)),
                                _ if r.key.is_empty() => "(unknown)".to_owned(),
                                _ => r.key.clone(),
                            };
                            json!({
                                "key": r.key, "label": label, "requests": r.requests, "tokens": r.tokens, "cost_usd": r.cost_usd,
                                "unpriced_requests": r.unpriced_requests, "unpriced_models": r.unpriced_models,
                            })
                        })
                        .collect();
                    let msg = format!("{} API-equivalent across {} {by_id}s ({} requests).", format_usd(total.cost_usd), list.len(), total.requests);
                    Ok(ToolOutput::new(
                        msg,
                        json!({
                            "from_ms": (from != i64::MIN).then_some(from),
                            "to_ms": (to != i64::MAX).then_some(to),
                            "group_by": by_id,
                            "rows": list,
                            "total": total,
                            "note": API_NOTE,
                        }),
                    ))
                }
                Err(e) => Err(ToolError::Failed(e.to_string())),
            });
        },
    );
    Ok(ToolOutput::message(""))
}

fn session_subagents(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: SessionParams = parse_params(params)?;
    let i = super::find(ctx.state, &p.session_id)?;
    let s = ctx.state.app.sessions.get(i).ok_or_else(|| ToolError::NotFound("gone".into()))?;
    let name = display_name(s);
    let following = ctx.state.rt.usage.is_some();
    let report = ctx.state.rt.subagents.get(&p.session_id).map(|r| (**r).clone()).unwrap_or_default();
    let message = if !following {
        "Claude transcripts are not followed in this run.".to_string()
    } else if report.total() == 0 {
        format!("{name} has no subagents.")
    } else {
        format!(
            "{name}: {} subagents ({} running, {} idle, {} done); {} API-equivalent.",
            report.total(),
            report.running,
            report.stale,
            report.done,
            format_usd(report.total_cost_usd)
        )
    };
    let mut data = serde_json::to_value(&report).unwrap_or(Value::Null);
    data["session_id"] = json!(p.session_id);
    data["following"] = json!(following);
    data["note"] = json!(API_NOTE);
    Ok(ToolOutput::new(message, data))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods() {
        let d = jiff::civil::date(2026, 11, 20);
        let start = |w: &str| period_start(w, d).unwrap();
        assert_eq!(start("today"), local_midnight(d));
        assert_eq!(start("WEEK"), local_midnight(jiff::civil::date(2026, 11, 14)));
        assert_eq!(start("month"), local_midnight(jiff::civil::date(2026, 10, 22)));
        assert_eq!(start("all"), i64::MIN);
        assert_eq!(start("2026-01-02"), local_midnight(jiff::civil::date(2026, 1, 2)));
        assert!(period_start("yesterday-ish", d).is_err());
        assert!(period_start("2026-1-2", d).is_err());
        assert!(period_start("2026-02-30", d).is_err());
        assert_eq!(period_end("today", d).unwrap(), local_midnight(jiff::civil::date(2026, 11, 21)));
        assert!(period_end("week", d).is_err());
        assert!(local_midnight(d) < local_midnight(d.tomorrow().unwrap()));
    }
}
