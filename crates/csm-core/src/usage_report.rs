//! The Claude usage report (`usage_report`): tokens, API-equivalent cost and cache numbers over a
//! time range, per day and per group (session, project, model or day), with drill-down filters.
//! Pure: callers pass the stored requests, the local day boundaries and a rate lookup.
//!
//! Definitions (ContextCost's "How the numbers are computed", `queries.js`):
//! - **prompt tokens** = `input + cache_read + cache_write_5m + cache_write_1h`.
//! - **cache hit rate** = `cache_read / prompt tokens` (ContextCost `hitRate`).
//! - **cache savings** = `cache_read x (input rate - cache-read rate)`: what the cached tokens
//!   would have cost as plain input (ContextCost `cacheSavings`).
//! - **cache write premium** = `w5 x (5m rate - input rate) + w1h x (1h rate - input rate)`
//!   (ContextCost `cacheWritePremium`). Net saved by caching = savings - premium.
//! - **cache-miss cost** = `input x (input rate - cache-read rate) + w5 x (5m rate - cache-read
//!   rate) + w1h x (1h rate - cache-read rate)`: what the prompt tokens that were *not* read from
//!   the cache cost above what they would have cost as cache reads. ContextCost prices a cache
//!   miss's `impact` the same way (writes vs reads) for its miss events only; both are reported.
//! - **cache-miss event** (ContextCost): a request after the first of its stream (one
//!   conversation's main thread, or one subagent) with `cache writes > 20,000` and `cache_read <
//!   0.5 x` the previous request's prompt. Its **impact** = `w5 x (5m rate - cache-read rate) +
//!   w1h x (1h rate - cache-read rate)`.
//! - The savings, premium and miss numbers use the request's tier rates (the long-context tier
//!   when its prompt is over the threshold) without the fast-mode multiplier, as ContextCost
//!   does; the cost per token class includes it, so the classes add up to the cost.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::claude_usage::{Tokens, UsageRequest, is_cache_miss};

/// The miss events a report lists (the costliest).
pub const MISS_LIST: usize = 25;
/// A report lists at most this many groups (the costliest); `truncated` says when there were more.
pub const MAX_ROWS: usize = 500;
/// How far before the range the requests are read, so the first request in range has its
/// predecessor for the miss test.
pub const LOOKBACK_MS: i64 = 24 * 3_600_000;

/// One stored request with what the report groups it by.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReportRequest {
    pub req: UsageRequest,
    /// The CSM session its transcript is linked to (`None`: a conversation CSM didn't run, or
    /// one read from history).
    pub csm_session: Option<String>,
    pub project: Option<String>,
}

impl ReportRequest {
    /// The session group key: the CSM session, else Claude's conversation id.
    pub fn session_key(&self) -> &str {
        self.csm_session.as_deref().unwrap_or(&self.req.claude_sid)
    }

    /// The model group key: the price entry, else the model id.
    pub fn model_key(&self) -> &str {
        self.req.price_key.as_deref().unwrap_or(&self.req.model)
    }

    pub fn project_key(&self) -> &str {
        self.project.as_deref().unwrap_or_default()
    }
}

/// A request's rates (USD per million tokens), the tier it falls in already chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct RateCard {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    /// The fast-mode multiplier that applies to this request (1 when it doesn't).
    pub multiplier: f64,
}

/// Cost per token class (USD, with the fast multiplier).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClassCost {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
}

impl ClassCost {
    pub fn total(&self) -> f64 {
        self.input + self.output + self.cache_read + self.cache_write_5m + self.cache_write_1h
    }

    fn add(&mut self, o: &ClassCost) {
        self.input += o.input;
        self.output += o.output;
        self.cache_read += o.cache_read;
        self.cache_write_5m += o.cache_write_5m;
        self.cache_write_1h += o.cache_write_1h;
    }
}

/// Sums over a set of requests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metrics {
    pub requests: u64,
    pub tokens: Tokens,
    /// API-equivalent cost of the priced requests.
    pub cost_usd: f64,
    pub cost_by_class: ClassCost,
    /// Requests whose model has no price: counted, never costed as $0.
    pub unpriced_requests: u64,
    pub unpriced_models: Vec<String>,
    /// `cache_read / prompt tokens`; `None` when nothing was sent.
    pub cache_hit_rate: Option<f64>,
    pub cache_savings_usd: f64,
    pub cache_write_premium_usd: f64,
    pub cache_miss_cost_usd: f64,
    pub miss_events: u64,
    /// The summed impact of the miss events.
    pub miss_event_cost_usd: f64,
}

impl Metrics {
    fn add(&mut self, r: &UsageRequest, rates: Option<&RateCard>, miss: Option<f64>) {
        self.requests += 1;
        self.tokens.add(&r.tokens);
        match r.cost_usd {
            Some(c) if c.is_finite() => self.cost_usd += c,
            _ => {
                self.unpriced_requests += 1;
                if !self.unpriced_models.contains(&r.model) {
                    self.unpriced_models.push(r.model.clone());
                }
            }
        }
        if let (Some(rc), Some(_)) = (rates, r.cost_usd.filter(|c| c.is_finite())) {
            let t = &r.tokens;
            let m = |n: u64, rate: f64| n as f64 * rate / 1e6;
            let mul = if rc.multiplier.is_finite() && rc.multiplier > 0.0 { rc.multiplier } else { 1.0 };
            self.cost_by_class.add(&ClassCost {
                input: m(t.input, rc.input) * mul,
                output: m(t.output, rc.output) * mul,
                cache_read: m(t.cache_read, rc.cache_read) * mul,
                cache_write_5m: m(t.cache_write_5m, rc.cache_write_5m) * mul,
                cache_write_1h: m(t.cache_write_1h, rc.cache_write_1h) * mul,
            });
            self.cache_savings_usd += m(t.cache_read, rc.input - rc.cache_read);
            self.cache_write_premium_usd += m(t.cache_write_5m, rc.cache_write_5m - rc.input) + m(t.cache_write_1h, rc.cache_write_1h - rc.input);
            self.cache_miss_cost_usd += m(t.input, rc.input - rc.cache_read)
                + m(t.cache_write_5m, rc.cache_write_5m - rc.cache_read)
                + m(t.cache_write_1h, rc.cache_write_1h - rc.cache_read);
        }
        if let Some(impact) = miss {
            self.miss_events += 1;
            self.miss_event_cost_usd += impact;
        }
    }

    fn finish(&mut self) {
        let p = self.tokens.prompt();
        self.cache_hit_rate = (p > 0).then(|| self.tokens.cache_read as f64 / p as f64);
        self.unpriced_models.sort();
    }

    /// Saved by caching: savings minus the write premium (ContextCost's "Saved by caching").
    pub fn net_cache_savings_usd(&self) -> f64 {
        self.cache_savings_usd - self.cache_write_premium_usd
    }
}

/// What the report groups by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroupBy {
    #[default]
    Session,
    Project,
    Model,
    Day,
}

impl GroupBy {
    pub fn id(self) -> &'static str {
        match self {
            GroupBy::Session => "session",
            GroupBy::Project => "project",
            GroupBy::Model => "model",
            GroupBy::Day => "day",
        }
    }
}

/// Drill-down: only the requests of this session, model and/or project (group keys).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filter {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

impl Filter {
    pub fn matches(&self, r: &ReportRequest) -> bool {
        self.session.as_deref().is_none_or(|s| s == r.session_key())
            && self.model.as_deref().is_none_or(|m| m == r.model_key())
            && self.project.as_deref().is_none_or(|p| p == r.project_key())
    }
}

/// A local day: its `YYYY-MM-DD` and the epoch ms it starts at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Day {
    pub day: String,
    pub start_ms: i64,
}

/// What to report.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ReportQuery {
    /// Inclusive start; `None`: everything, including requests without a time.
    pub from_ms: Option<i64>,
    /// Exclusive end; `None`: no end.
    pub to_ms: Option<i64>,
    /// The local days of the range, oldest first (from the caller, who knows the time zone).
    /// A request belongs to the last day that starts at or before it.
    pub days: Vec<Day>,
    pub group_by: GroupBy,
    pub filter: Filter,
}

/// One day of the series.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DayRow {
    pub day: String,
    pub start_ms: i64,
    pub metrics: Metrics,
}

/// One group.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GroupRow {
    pub key: String,
    /// What to show (the caller names sessions; the key otherwise).
    pub label: String,
    /// A session group: its CSM session (when linked) and Claude conversation ids.
    pub csm_session: Option<String>,
    pub claude_sids: Vec<String>,
    /// The projects and models in the group (most expensive first).
    pub projects: Vec<String>,
    pub models: Vec<String>,
    pub sessions: u64,
    pub first_ms: Option<i64>,
    pub last_ms: Option<i64>,
    pub metrics: Metrics,
}

/// A cache-miss event (ContextCost's "Cache misses" list).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MissEvent {
    pub at_ms: Option<i64>,
    pub session_key: String,
    pub project: String,
    pub model: String,
    pub subagent: bool,
    pub cache_write: u64,
    pub cache_read: u64,
    pub previous_prompt: u64,
    /// Time since the stream's previous request.
    pub idle_gap_ms: Option<i64>,
    /// Re-writing vs reading those tokens (USD); 0 when the model is unpriced.
    pub impact_usd: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub group_by: GroupBy,
    pub filter: Filter,
    pub total: Metrics,
    /// Distinct sessions with requests in the range (after the filter).
    pub sessions: u64,
    pub days: Vec<DayRow>,
    pub rows: Vec<GroupRow>,
    /// More groups than [`MAX_ROWS`]: the cheapest were left out.
    pub truncated: bool,
    /// The costliest miss events, at most [`MISS_LIST`].
    pub misses: Vec<MissEvent>,
}

#[derive(Default)]
struct GroupAcc {
    row: GroupRow,
    sessions: BTreeSet<String>,
    sids: BTreeSet<String>,
    projects: HashMap<String, f64>,
    models: HashMap<String, f64>,
}

fn ranked(m: HashMap<String, f64>) -> Vec<String> {
    let mut v: Vec<(String, f64)> = m.into_iter().collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.into_iter().map(|(k, _)| k).collect()
}

/// The day a time falls in (index into `days`), `None` before the first day.
fn day_index(days: &[Day], ts: i64) -> Option<usize> {
    days.partition_point(|d| d.start_ms <= ts).checked_sub(1)
}

fn in_range(q: &ReportQuery, ts: Option<i64>) -> bool {
    match ts {
        Some(t) => q.from_ms.is_none_or(|f| t >= f) && q.to_ms.is_none_or(|e| t < e),
        None => q.from_ms.is_none(),
    }
}

/// Builds the report. `rows` may hold requests outside the range (the look-back for the miss
/// test); `rates` gives a request's tier rates, `None` for an unpriced one.
pub fn build(rows: &[ReportRequest], q: &ReportQuery, rates: &dyn Fn(&UsageRequest) -> Option<RateCard>) -> Report {
    // Streams in time order, for the miss test (ContextCost: LAG over a file's requests).
    let mut order: Vec<&ReportRequest> = rows.iter().collect();
    order.sort_by(|a, b| {
        (a.req.claude_sid.as_str(), a.req.agent_id.as_deref(), a.req.ts_ms.unwrap_or(i64::MIN), a.req.id.as_str()).cmp(&(
            b.req.claude_sid.as_str(),
            b.req.agent_id.as_deref(),
            b.req.ts_ms.unwrap_or(i64::MIN),
            b.req.id.as_str(),
        ))
    });
    let mut report = Report { from_ms: q.from_ms, to_ms: q.to_ms, group_by: q.group_by, filter: q.filter.clone(), ..Report::default() };
    let mut days: Vec<DayRow> = q.days.iter().map(|d| DayRow { day: d.day.clone(), start_ms: d.start_ms, metrics: Metrics::default() }).collect();
    let mut groups: BTreeMap<String, GroupAcc> = BTreeMap::new();
    let mut sessions: BTreeSet<&str> = BTreeSet::new();
    let mut misses: Vec<MissEvent> = Vec::new();
    let mut prev: Option<&ReportRequest> = None;
    for r in order {
        let same_stream = prev.is_some_and(|p| p.req.claude_sid == r.req.claude_sid && p.req.agent_id == r.req.agent_id);
        let previous = if same_stream { prev } else { None };
        prev = Some(r);
        if !in_range(q, r.req.ts_ms) || !q.filter.matches(r) {
            continue;
        }
        let rc = rates(&r.req);
        let miss = previous.filter(|p| is_cache_miss(&p.req.tokens, &r.req.tokens)).map(|p| {
            let t = &r.req.tokens;
            let impact = rc.map_or(0.0, |rc| {
                (t.cache_write_5m as f64 * (rc.cache_write_5m - rc.cache_read) + t.cache_write_1h as f64 * (rc.cache_write_1h - rc.cache_read)) / 1e6
            });
            (p, impact)
        });
        if let Some((p, impact)) = miss {
            misses.push(MissEvent {
                at_ms: r.req.ts_ms,
                session_key: r.session_key().to_owned(),
                project: r.project_key().to_owned(),
                model: r.model_key().to_owned(),
                subagent: r.req.agent_id.is_some(),
                cache_write: r.req.tokens.cache_write(),
                cache_read: r.req.tokens.cache_read,
                previous_prompt: p.req.tokens.prompt(),
                idle_gap_ms: r.req.ts_ms.zip(p.req.ts_ms).map(|(a, b)| a.saturating_sub(b)),
                impact_usd: impact,
            });
        }
        let impact = miss.map(|(_, i)| i);
        report.total.add(&r.req, rc.as_ref(), impact);
        sessions.insert(r.session_key());
        let day = r.req.ts_ms.and_then(|t| day_index(&q.days, t));
        if let Some(d) = day.and_then(|i| days.get_mut(i)) {
            d.metrics.add(&r.req, rc.as_ref(), impact);
        }
        let key = match q.group_by {
            GroupBy::Session => r.session_key().to_owned(),
            GroupBy::Project => r.project_key().to_owned(),
            GroupBy::Model => r.model_key().to_owned(),
            GroupBy::Day => match day.and_then(|i| q.days.get(i)) {
                Some(d) => d.day.clone(),
                None => continue,
            },
        };
        let g = groups.entry(key).or_default();
        g.row.metrics.add(&r.req, rc.as_ref(), impact);
        if q.group_by == GroupBy::Session && g.row.csm_session.is_none() {
            g.row.csm_session.clone_from(&r.csm_session);
        }
        g.sessions.insert(r.session_key().to_owned());
        g.sids.insert(r.req.claude_sid.clone());
        let cost = r.req.cost_usd.filter(|c| c.is_finite()).unwrap_or(0.0);
        *g.projects.entry(r.project_key().to_owned()).or_default() += cost;
        *g.models.entry(r.model_key().to_owned()).or_default() += cost;
        if let Some(t) = r.req.ts_ms {
            g.row.first_ms = Some(g.row.first_ms.map_or(t, |f| f.min(t)));
            g.row.last_ms = Some(g.row.last_ms.map_or(t, |l| l.max(t)));
        }
    }
    report.total.finish();
    report.sessions = sessions.len() as u64;
    for d in &mut days {
        d.metrics.finish();
    }
    report.days = days;
    let mut rows: Vec<GroupRow> = groups
        .into_iter()
        .map(|(key, g)| {
            let mut row = g.row;
            row.label.clone_from(&key);
            row.key = key;
            row.sessions = g.sessions.len() as u64;
            row.claude_sids = g.sids.into_iter().collect();
            row.projects = ranked(g.projects);
            row.models = ranked(g.models);
            row.metrics.finish();
            row
        })
        .collect();
    if q.group_by == GroupBy::Day {
        rows.sort_by(|a, b| b.key.cmp(&a.key));
    } else {
        rows.sort_by(|a, b| b.metrics.cost_usd.total_cmp(&a.metrics.cost_usd).then_with(|| a.key.cmp(&b.key)));
    }
    report.truncated = rows.len() > MAX_ROWS;
    rows.truncate(MAX_ROWS);
    report.rows = rows;
    misses.sort_by(|a, b| b.impact_usd.total_cmp(&a.impact_usd).then_with(|| b.cache_write.cmp(&a.cache_write)));
    misses.truncate(MISS_LIST);
    report.misses = misses;
    report
}

/// UTC days from `from_ms` to `to_ms` (the demo's and the tests' stand-in for local days).
pub fn utc_days(from_ms: i64, to_ms: i64) -> Vec<Day> {
    const DAY: i64 = 86_400_000;
    let mut out = Vec::new();
    let mut d = from_ms.div_euclid(DAY) * DAY;
    while d < to_ms && out.len() < 10_000 {
        let ts = crate::time::Timestamp::from_epoch_ms(d);
        out.push(Day { day: ts.as_str().get(..10).unwrap_or_default().to_owned(), start_ms: d });
        d += DAY;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const H: i64 = 3_600_000;
    /// 2026-11-20T00:00:00Z.
    const D20: i64 = 1_795_132_800_000;

    fn rates_of(r: &UsageRequest) -> Option<RateCard> {
        match r.price_key.as_deref() {
            // claude-opus-5-5's list prices.
            Some("opus") => Some(RateCard { input: 4.0, output: 20.0, cache_read: 0.2, cache_write_5m: 5.0, cache_write_1h: 8.0, multiplier: 1.0 }),
            Some("sonnet") => Some(RateCard { input: 2.0, output: 10.0, cache_read: 0.1, cache_write_5m: 2.5, cache_write_1h: 4.0, multiplier: 1.0 }),
            _ => None,
        }
    }

    fn cost(model: &str, t: &Tokens) -> Option<f64> {
        let rc = rates_of(&UsageRequest { price_key: Some(model.into()), ..UsageRequest::default() })?;
        Some(
            (t.input as f64 * rc.input
                + t.output as f64 * rc.output
                + t.cache_read as f64 * rc.cache_read
                + t.cache_write_5m as f64 * rc.cache_write_5m
                + t.cache_write_1h as f64 * rc.cache_write_1h)
                / 1e6,
        )
    }

    struct R<'a> {
        id: &'a str,
        sid: &'a str,
        agent: Option<&'a str>,
        csm: Option<&'a str>,
        project: &'a str,
        ts: i64,
        model: &'a str,
        t: Tokens,
    }

    fn row(r: R<'_>) -> ReportRequest {
        let priced = rates_of(&UsageRequest { price_key: Some(r.model.into()), ..UsageRequest::default() }).is_some();
        ReportRequest {
            req: UsageRequest {
                id: r.id.into(),
                claude_sid: r.sid.into(),
                agent_id: r.agent.map(Into::into),
                ts_ms: Some(r.ts),
                model: if priced { format!("claude-{}", r.model) } else { r.model.into() },
                price_key: priced.then(|| r.model.into()),
                fast: false,
                tokens: r.t,
                cost_usd: cost(r.model, &r.t),
            },
            csm_session: r.csm.map(Into::into),
            project: Some(r.project.into()),
        }
    }

    fn tk(input: u64, output: u64, cr: u64, w5: u64, w1h: u64) -> Tokens {
        Tokens { input, output, cache_read: cr, cache_write_5m: w5, cache_write_1h: w1h }
    }

    /// Two sessions, two models, 1h cache writes, a session that runs over midnight, a
    /// subagent, an unpriced local model and a miss after an idle gap.
    fn fixture() -> Vec<ReportRequest> {
        vec![
            // Session A (linked to CSM session "csm-a"), project shop, Opus; 23:00 → 01:00.
            row(R { id: "a1", sid: "A", agent: None, csm: Some("csm-a"), project: "shop", ts: D20 + 23 * H, model: "opus", t: tk(100, 1000, 0, 0, 50_000) }),
            row(R { id: "a2", sid: "A", agent: None, csm: Some("csm-a"), project: "shop", ts: D20 + 23 * H + 60_000, model: "opus", t: tk(10, 500, 50_000, 1_000, 0) }),
            // Over midnight, 2 h idle: the 1h cache expired; everything is written again.
            row(R { id: "a3", sid: "A", agent: None, csm: Some("csm-a"), project: "shop", ts: D20 + 25 * H, model: "opus", t: tk(10, 200, 0, 0, 51_000) }),
            // A subagent of A on Sonnet: its own stream (no miss against the main thread).
            row(R { id: "a-s1", sid: "A", agent: Some("ag1"), csm: Some("csm-a"), project: "shop", ts: D20 + 25 * H + 1, model: "sonnet", t: tk(5_000, 300, 0, 30_000, 0) }),
            // Session B (not run by CSM), project blog, Sonnet, and an unpriced local model.
            row(R { id: "b1", sid: "B", agent: None, csm: None, project: "blog", ts: D20 + 10 * H, model: "sonnet", t: tk(1_000, 100, 9_000, 0, 0) }),
            row(R { id: "b2", sid: "B", agent: None, csm: None, project: "blog", ts: D20 + 11 * H, model: "qwen3.8:27b", t: tk(700, 0, 0, 0, 0) }),
            // Before the range (look-back): not counted.
            row(R { id: "old", sid: "B", agent: None, csm: None, project: "blog", ts: D20 - 2 * H, model: "sonnet", t: tk(1, 1, 0, 0, 0) }),
        ]
    }

    fn q(group_by: GroupBy) -> ReportQuery {
        ReportQuery { from_ms: Some(D20), to_ms: Some(D20 + 48 * H), days: utc_days(D20, D20 + 48 * H), group_by, filter: Filter::default() }
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn totals_split_tokens_by_class_and_price_each_class() {
        let r = build(&fixture(), &q(GroupBy::Session), &rates_of);
        let t = &r.total;
        assert_eq!(t.requests, 6, "the look-back request is not counted");
        assert_eq!(t.tokens, tk(6_820, 2_100, 59_000, 31_000, 101_000));
        assert_eq!(t.unpriced_requests, 1);
        assert_eq!(t.unpriced_models, ["qwen3.8:27b"]);
        assert_eq!(r.sessions, 2);
        // Classes add up to the cost (no fast requests here).
        assert!(close(t.cost_by_class.total(), t.cost_usd), "{} vs {}", t.cost_by_class.total(), t.cost_usd);
        // 1h writes: 50k + 51k on Opus at $8.
        assert!(close(t.cost_by_class.cache_write_1h, 101_000.0 * 8.0 / 1e6));
        // 5m writes: 1k Opus at $5 + 30k Sonnet at $2.5.
        assert!(close(t.cost_by_class.cache_write_5m, (1_000.0 * 5.0 + 30_000.0 * 2.5) / 1e6));
    }

    #[test]
    fn cache_hit_rate_savings_premium_and_miss_cost_follow_contextcost() {
        let r = build(&fixture(), &q(GroupBy::Session), &rates_of);
        let t = &r.total;
        let prompt = (6_820 + 59_000 + 31_000 + 101_000) as f64;
        assert!(close(t.cache_hit_rate.unwrap(), 59_000.0 / prompt));
        // Savings: reads x (input - read): Opus 50k x 3.8, Sonnet 9k x 1.9.
        assert!(close(t.cache_savings_usd, (50_000.0 * 3.8 + 9_000.0 * 1.9) / 1e6));
        // Premium: 5m x (5m - input), 1h x (1h - input).
        let premium = (1_000.0 * 1.0 + 101_000.0 * 4.0 + 30_000.0 * 0.5) / 1e6;
        assert!(close(t.cache_write_premium_usd, premium));
        assert!(close(t.net_cache_savings_usd(), t.cache_savings_usd - premium));
        // Miss cost: priced input and writes above the read rate (the unpriced 700 are left out).
        let opus = (120.0 * 3.8 + 1_000.0 * 4.8 + 101_000.0 * 7.8) / 1e6;
        let sonnet = (6_000.0 * 1.9 + 30_000.0 * 2.4) / 1e6;
        assert!(close(t.cache_miss_cost_usd, opus + sonnet), "{}", t.cache_miss_cost_usd);
    }

    #[test]
    fn a_miss_after_an_idle_gap_is_an_event_with_its_impact() {
        let r = build(&fixture(), &q(GroupBy::Session), &rates_of);
        assert_eq!(r.total.miss_events, 1, "a3 only: the subagent's first request starts its own stream");
        let m = &r.misses[0];
        assert_eq!(m.at_ms, Some(D20 + 25 * H));
        assert_eq!(m.idle_gap_ms, Some(2 * H - 60_000));
        assert_eq!((m.cache_write, m.cache_read, m.previous_prompt), (51_000, 0, 51_010));
        assert_eq!((m.session_key.as_str(), m.model.as_str(), m.subagent), ("csm-a", "opus", false));
        assert!(close(m.impact_usd, 51_000.0 * (8.0 - 0.2) / 1e6));
        assert!(close(r.total.miss_event_cost_usd, m.impact_usd));
    }

    #[test]
    fn a_session_over_midnight_is_split_between_its_days() {
        let r = build(&fixture(), &q(GroupBy::Session), &rates_of);
        assert_eq!(r.days.len(), 2);
        assert_eq!(r.days[0].day, "2026-11-20");
        assert_eq!(r.days[0].metrics.requests, 4, "a1, a2, b1, b2");
        assert_eq!(r.days[1].day, "2026-11-21");
        assert_eq!(r.days[1].metrics.requests, 2, "a3 and the subagent, after midnight");
        assert!(close(r.days.iter().map(|d| d.metrics.cost_usd).sum::<f64>(), r.total.cost_usd));
        let by_day = build(&fixture(), &q(GroupBy::Day), &rates_of);
        let keys: Vec<&str> = by_day.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, ["2026-11-21", "2026-11-20"], "newest day first");
    }

    #[test]
    fn groups_by_session_project_and_model_most_expensive_first() {
        let s = build(&fixture(), &q(GroupBy::Session), &rates_of);
        assert_eq!(s.rows[0].key, "csm-a", "a linked session is keyed by its CSM session");
        assert_eq!(s.rows[0].csm_session.as_deref(), Some("csm-a"));
        assert_eq!(s.rows[0].claude_sids, ["A"]);
        assert_eq!(s.rows[0].models, ["opus", "sonnet"], "costliest model first");
        assert_eq!(s.rows[0].metrics.requests, 4);
        assert_eq!((s.rows[0].first_ms, s.rows[0].last_ms), (Some(D20 + 23 * H), Some(D20 + 25 * H + 1)));
        assert_eq!(s.rows[1].key, "B", "an unlinked conversation is keyed by its id");
        assert_eq!(s.rows[1].metrics.unpriced_requests, 1);
        let p = build(&fixture(), &q(GroupBy::Project), &rates_of);
        assert_eq!(p.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["shop", "blog"]);
        let m = build(&fixture(), &q(GroupBy::Model), &rates_of);
        assert_eq!(m.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["opus", "sonnet", "qwen3.8:27b"]);
        assert_eq!(m.rows[1].sessions, 2, "Sonnet ran in A's subagent and in B");
        let total: f64 = m.rows.iter().map(|r| r.metrics.cost_usd).sum();
        assert!(close(total, m.total.cost_usd));
    }

    #[test]
    fn drill_down_filters_keep_the_miss_test_on_whole_streams() {
        // A session: its models and days.
        let mut query = q(GroupBy::Model);
        query.filter.session = Some("csm-a".into());
        let r = build(&fixture(), &query, &rates_of);
        assert_eq!(r.total.requests, 4);
        assert_eq!(r.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["opus", "sonnet"]);
        assert_eq!(r.days[1].metrics.requests, 2);
        assert_eq!(r.total.miss_events, 1);
        // A model: its sessions.
        let mut query = q(GroupBy::Session);
        query.filter.model = Some("sonnet".into());
        let r = build(&fixture(), &query, &rates_of);
        assert_eq!(r.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["csm-a", "B"]);
        assert_eq!(r.total.miss_events, 0);
        // A project.
        let mut query = q(GroupBy::Session);
        query.filter.project = Some("blog".into());
        assert_eq!(build(&fixture(), &query, &rates_of).total.requests, 2);
    }

    #[test]
    fn the_range_and_requests_without_a_time() {
        let mut rows = fixture();
        rows.push(ReportRequest { req: UsageRequest { id: "nt".into(), claude_sid: "C".into(), model: "x".into(), ..UsageRequest::default() }, ..ReportRequest::default() });
        let r = build(&rows, &q(GroupBy::Session), &rates_of);
        assert_eq!(r.total.requests, 6, "no time: only in an unbounded range");
        let all = ReportQuery { days: utc_days(D20 - 2 * H, D20 + 48 * H), ..ReportQuery::default() };
        let r = build(&rows, &all, &rates_of);
        assert_eq!(r.total.requests, 8);
        assert_eq!(r.days.iter().map(|d| d.metrics.requests).sum::<u64>(), 7, "the timeless request is in no day");
        assert_eq!(r.days[0].day, "2026-11-19");
    }

    #[test]
    fn fast_requests_cost_more_per_class_but_not_in_the_cache_numbers() {
        let mut r = row(R { id: "f", sid: "F", agent: None, csm: None, project: "p", ts: D20, model: "opus", t: tk(1_000_000, 0, 1_000_000, 0, 0) });
        r.req.fast = true;
        r.req.cost_usd = r.req.cost_usd.map(|c| c * 2.0);
        let fast = |u: &UsageRequest| rates_of(u).map(|rc| RateCard { multiplier: 2.0, ..rc });
        let rep = build(&[r], &q(GroupBy::Session), &fast);
        assert!(close(rep.total.cost_by_class.total(), rep.total.cost_usd));
        assert!(close(rep.total.cost_by_class.input, 8.0));
        assert!(close(rep.total.cache_savings_usd, 3.8), "savings at list rates");
    }

    #[test]
    fn empty_and_hostile_input() {
        let r = build(&[], &q(GroupBy::Model), &rates_of);
        assert_eq!(r.total, Metrics::default());
        assert!(r.rows.is_empty() && r.misses.is_empty());
        assert_eq!(r.days.len(), 2);
        let weird = ReportRequest {
            req: UsageRequest { id: "w".into(), ts_ms: Some(i64::MIN), cost_usd: Some(f64::NAN), tokens: tk(u64::MAX, u64::MAX, u64::MAX, 0, 0), ..UsageRequest::default() },
            ..ReportRequest::default()
        };
        let r = build(&[weird.clone(), weird], &ReportQuery::default(), &|_| Some(RateCard::default()));
        assert_eq!(r.total.requests, 2);
        assert_eq!(r.total.unpriced_requests, 2, "a NaN cost is not a cost");
        assert!(utc_days(10, 0).is_empty());
        assert_eq!(utc_days(D20, D20 + 1).len(), 1);
    }

    #[test]
    fn the_report_round_trips_as_json() {
        let r = build(&fixture(), &q(GroupBy::Session), &rates_of);
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["group_by"], "session");
        assert!(v["total"]["cache_hit_rate"].as_f64().is_some());
        let back: Report = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }
}
