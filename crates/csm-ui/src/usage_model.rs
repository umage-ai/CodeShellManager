//! What the Usage tab, the sidebar's fill chips and Settings → Usage show: Claude token and
//! cost numbers from the usage tools' **replies** (`usage_session`, `usage_context`,
//! `usage_summary`). The backend keeps them here and puts them into
//! [`crate::model::UiModel::usage`]; the screens read it and submit the tools (polled, never
//! waited for). The demo fills it from a fixture.

use std::collections::BTreeMap;

use serde_json::Value;

use csm_core::claude_usage::{ContextFill, SessionUsage, Tokens};

use crate::backend::Call;
use crate::model::SessionId;

/// The usage tool ids whose replies land here.
pub const TOOLS: [&str; 3] = ["usage_session", "usage_context", "usage_summary"];

/// The Usage tab's numbers for one session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SessionUsageView {
    pub usage: SessionUsage,
    /// The tool's sentence (or why there is nothing yet).
    pub message: String,
    /// Transcript lines that could not be read.
    pub bad_lines: u64,
    /// The reader follows transcripts in this run.
    pub following: bool,
}

/// A sidebar row's chip.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageChip {
    pub context: Option<ContextFill>,
    pub cost_usd: f64,
    pub unpriced_requests: u64,
}

/// One row of a summary (a project, by default).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummaryRow {
    pub label: String,
    pub cost_usd: f64,
    pub requests: u64,
    pub tokens: u64,
    pub unpriced_requests: u64,
}

/// `usage_summary` for one period.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummaryView {
    pub rows: Vec<SummaryRow>,
    pub total_cost: f64,
    pub unpriced_models: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct UsageDetails {
    /// The real engine: the screens poll the tools. The demo's fixture is static.
    pub live: bool,
    pub sessions: BTreeMap<SessionId, SessionUsageView>,
    /// Why a session has no numbers (not a Claude session, no index in this run).
    pub unavailable: BTreeMap<SessionId, String>,
    pub chips: BTreeMap<SessionId, UsageChip>,
    /// By period (`today`, `week`).
    pub summary: BTreeMap<String, SummaryView>,
    pub summary_error: Option<String>,
}

fn f(v: &Value) -> f64 {
    v.as_f64().filter(|x| x.is_finite()).unwrap_or(0.0)
}

fn n(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn context_of(v: &Value) -> Option<ContextFill> {
    if v.is_null() {
        return None;
    }
    serde_json::from_value(v.clone()).ok()
}

impl UsageDetails {
    /// Follows a usage tool's reply. `None` when the call is not a usage tool. Errors are kept
    /// for the screens (a periodic poll must not toast).
    pub fn on_reply(&mut self, call: &Call, reply: &csm_engine::Reply) -> Option<()> {
        if !TOOLS.contains(&call.id.as_str()) {
            return None;
        }
        match (call.id.as_str(), reply) {
            ("usage_session", r) => {
                let sid = call.params["session_id"].as_str()?.to_owned();
                match r {
                    Ok(o) => {
                        self.unavailable.remove(&sid);
                        let mut usage: SessionUsage = serde_json::from_value(o.data.clone()).unwrap_or_default();
                        usage.context = context_of(&o.data["context"]);
                        self.sessions.insert(
                            sid,
                            SessionUsageView {
                                usage,
                                message: o.message.clone(),
                                bad_lines: n(&o.data["data_quality"]["bad_lines"]),
                                following: o.data["following"].as_bool().unwrap_or(false),
                            },
                        );
                    }
                    Err(e) => {
                        self.unavailable.insert(sid, e.to_string());
                    }
                }
            }
            ("usage_context", Ok(o)) => {
                self.chips.clear();
                for s in o.data["sessions"].as_array().into_iter().flatten() {
                    let Some(id) = s["session_id"].as_str() else { continue };
                    let chip = UsageChip { context: context_of(&s["context"]), cost_usd: f(&s["cost_usd"]), unpriced_requests: n(&s["unpriced_requests"]) };
                    if chip.context.is_some() || chip.cost_usd > 0.0 {
                        self.chips.insert(id.to_owned(), chip);
                    }
                }
            }
            ("usage_summary", r) => {
                let period = call.params["from"].as_str().unwrap_or("week").to_owned();
                match r {
                    Ok(o) => {
                        self.summary_error = None;
                        let rows = o.data["rows"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|r| {
                                let t: Tokens = serde_json::from_value(r["tokens"].clone()).unwrap_or_default();
                                SummaryRow {
                                    label: r["label"].as_str().unwrap_or_default().to_owned(),
                                    cost_usd: f(&r["cost_usd"]),
                                    requests: n(&r["requests"]),
                                    tokens: t.total(),
                                    unpriced_requests: n(&r["unpriced_requests"]),
                                }
                            })
                            .collect();
                        let unpriced_models =
                            o.data["total"]["unpriced_models"].as_array().into_iter().flatten().filter_map(|m| m.as_str().map(str::to_owned)).collect();
                        self.summary.insert(period, SummaryView { rows, total_cost: f(&o.data["total"]["cost_usd"]), unpriced_models });
                    }
                    Err(e) => self.summary_error = Some(e.to_string()),
                }
            }
            // A failed `usage_context` keeps the last chips.
            _ => {}
        }
        Some(())
    }
}

/// The demo's numbers: a few Claude sessions at different fills.
pub fn demo_details() -> UsageDetails {
    use csm_core::claude_usage::{CacheMiss, Compaction, Compactions, Totals};
    let fill = |tokens: u64, window: u64| Some(ContextFill::new(tokens, window, "claude-opus-5-5".into(), Some(1_795_513_000_000)));
    let totals = |requests: u64, cost: f64, input: u64, read: u64, write: u64, output: u64| Totals {
        requests,
        tokens: Tokens { input, output, cache_read: read, cache_write_5m: write, cache_write_1h: 0 },
        cost_usd: cost,
        unpriced_requests: 0,
        unpriced_models: Vec::new(),
    };
    let mut d = UsageDetails::default();
    let web = SessionUsage {
        context: fill(148_200, 200_000),
        total: Totals { unpriced_requests: 3, unpriced_models: vec!["qwen3.8:27b".into()], ..totals(212, 18.42, 41_000, 9_800_000, 610_000, 182_000) },
        today: totals(64, 4.87, 9_000, 2_900_000, 190_000, 51_000),
        main: totals(180, 15.10, 36_000, 8_700_000, 520_000, 160_000),
        subagents: Totals { unpriced_requests: 3, unpriced_models: vec!["qwen3.8:27b".into()], ..totals(32, 3.32, 5_000, 1_100_000, 90_000, 22_000) },
        cache_hit_ratio: Some(0.93),
        last_cache_miss: Some(CacheMiss {
            at_ms: Some(1_795_512_400_000),
            idle_gap_ms: Some(7 * 60_000 + 12_000),
            cache_write: 96_400,
            cache_read: 0,
            previous_prompt: 95_100,
        }),
        compactions: Compactions {
            count: 2,
            last: Some(Compaction { ts_ms: Some(1_795_509_000_000), trigger: Some("auto".into()), pre_tokens: Some(167_000), post_tokens: Some(21_400), ..Compaction::default() }),
        },
    };
    let chip = |u: &SessionUsage| UsageChip { context: u.context.clone(), cost_usd: u.total.cost_usd, unpriced_requests: u.total.unpriced_requests };
    d.chips.insert("web".into(), chip(&web));
    d.sessions.insert(
        "web".into(),
        SessionUsageView { message: "Odulodo.Web: context 148k of 200k (74%); $18.42 API-equivalent ($4.87 today).".into(), usage: web, bad_lines: 0, following: true },
    );
    let odulodo = SessionUsage {
        context: fill(38_900, 200_000),
        total: totals(41, 2.06, 8_000, 1_200_000, 140_000, 30_000),
        today: totals(41, 2.06, 8_000, 1_200_000, 140_000, 30_000),
        main: totals(41, 2.06, 8_000, 1_200_000, 140_000, 30_000),
        cache_hit_ratio: Some(0.88),
        ..SessionUsage::default()
    };
    d.chips.insert("odulodo".into(), chip(&odulodo));
    d.sessions.insert("odulodo".into(), SessionUsageView { usage: odulodo, following: true, ..SessionUsageView::default() });
    d.chips.insert("rs".into(), UsageChip { context: fill(176_500, 200_000), cost_usd: 31.7, unpriced_requests: 0 });
    d.chips.insert("csm".into(), UsageChip { context: fill(412_000, 1_000_000), cost_usd: 9.15, unpriced_requests: 0 });
    let row = |label: &str, cost: f64, requests: u64| SummaryRow { label: label.into(), cost_usd: cost, requests, tokens: requests * 61_000, unpriced_requests: 0 };
    d.summary.insert(
        "today".into(),
        SummaryView { rows: vec![row("umage/CodeShellManager-rs", 7.9, 88), row("GitHub/Odulodo.Web", 4.87, 64), row("GitHub/Odulodo", 2.06, 41)], total_cost: 14.83, unpriced_models: Vec::new() },
    );
    d.summary.insert(
        "week".into(),
        SummaryView {
            rows: vec![
                row("umage/CodeShellManager-rs", 31.7, 402),
                row("GitHub/Odulodo.Web", 18.42, 212),
                row("GitHub/CodeShellManager", 9.15, 120),
                row("GitHub/Odulodo", 2.06, 41),
            ],
            total_cost: 61.33,
            unpriced_models: vec!["qwen3.8:27b".into()],
        },
    );
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_engine::{ToolError, ToolOutput};
    use serde_json::json;

    fn call(id: &str, params: Value) -> Call {
        Call { id: id.into(), params }
    }

    #[test]
    fn replies_fill_the_details_and_errors_are_kept_not_toasted() {
        let mut d = UsageDetails::default();
        assert_eq!(d.on_reply(&call("git_status", json!({})), &Ok(ToolOutput::message(""))), None, "not a usage tool");
        let out = ToolOutput::new(
            "x: context 30k of 200k",
            json!({
                "context": {"fill_tokens": 30_000, "window_tokens": 200_000, "ratio": 0.15, "level": "ok", "model": "m", "at_ms": 5, "warn_ratio": 0.7},
                "total": {"requests": 3, "tokens": {"input": 1, "output": 2, "cache_read": 3, "cache_write_5m": 4, "cache_write_1h": 0}, "cost_usd": 1.5, "unpriced_requests": 0, "unpriced_models": []},
                "today": {"requests": 1, "tokens": {"input": 0, "output": 0, "cache_read": 0, "cache_write_5m": 0, "cache_write_1h": 0}, "cost_usd": 0.5, "unpriced_requests": 0, "unpriced_models": []},
                "data_quality": {"bad_lines": 2}, "following": true
            }),
        );
        assert_eq!(d.on_reply(&call("usage_session", json!({"session_id": "s"})), &Ok(out)), Some(()));
        let v = &d.sessions["s"];
        assert_eq!(v.usage.context.as_ref().unwrap().fill_tokens, 30_000);
        assert_eq!(v.usage.total.requests, 3);
        assert_eq!(v.bad_lines, 2);
        assert!(v.following);
        d.on_reply(&call("usage_session", json!({"session_id": "p"})), &Err(ToolError::Refused("not a Claude session".into())));
        assert!(d.unavailable["p"].contains("not a Claude session"));

        let ctx = ToolOutput::new(
            "",
            json!({"sessions": [
                {"session_id": "s", "context": {"fill_tokens": 160_000, "window_tokens": 200_000, "ratio": 0.8, "level": "warn", "model": "m", "at_ms": null}, "cost_usd": 2.0, "unpriced_requests": 0},
                {"session_id": "empty", "context": null, "cost_usd": 0.0},
                {"bad": true}
            ]}),
        );
        d.on_reply(&call("usage_context", json!({})), &Ok(ctx));
        assert_eq!(d.chips.len(), 1, "sessions with nothing recorded get no chip");
        assert_eq!(d.chips["s"].context.as_ref().unwrap().level, csm_core::claude_usage::FillLevel::Warn);
        d.on_reply(&call("usage_context", json!({})), &Err(ToolError::Failed("x".into())));
        assert_eq!(d.chips.len(), 1, "a failed poll keeps the last chips");

        let sum = ToolOutput::new(
            "",
            json!({"rows": [{"label": "umage/app", "cost_usd": 3.0, "requests": 4, "tokens": {"input": 10, "output": 5, "cache_read": 0, "cache_write_5m": 0, "cache_write_1h": 0}}],
                   "total": {"cost_usd": 3.0, "unpriced_models": ["qwen"]}}),
        );
        d.on_reply(&call("usage_summary", json!({"from": "today", "group_by": "project"})), &Ok(sum));
        let s = &d.summary["today"];
        assert_eq!((s.rows[0].label.as_str(), s.rows[0].tokens, s.total_cost), ("umage/app", 15, 3.0));
        assert_eq!(s.unpriced_models, ["qwen"]);
        d.on_reply(&call("usage_summary", json!({"from": "week"})), &Err(ToolError::Refused("no index".into())));
        assert_eq!(d.summary_error.as_deref(), Some("no index"));
    }

    #[test]
    fn hostile_replies_do_not_panic() {
        let mut d = UsageDetails::default();
        for data in [json!(null), json!([]), json!({"context": "x", "total": 5}), json!({"sessions": "nope", "rows": {}})] {
            for id in TOOLS {
                d.on_reply(&call(id, json!({"session_id": "s"})), &Ok(ToolOutput::new("", data.clone())));
            }
        }
        d.on_reply(&call("usage_session", json!({})), &Ok(ToolOutput::new("", json!({}))));
    }

    #[test]
    fn the_demo_has_usage_for_its_claude_sessions() {
        let d = demo_details();
        assert!(d.sessions.contains_key("web"));
        assert!(!d.live);
        assert!(d.chips.values().any(|c| c.context.as_ref().is_some_and(|x| x.level == csm_core::claude_usage::FillLevel::Critical)));
    }
}
