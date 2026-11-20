//! Claude Code token and cost analytics: the pure part (ported from ContextCost's `parse.js`,
//! `queries.js` and `project.js`).
//!
//! The numbers come from Claude's own transcripts (`message.usage` on assistant lines), one row
//! per API request after de-duplication by `message.id`. Costs are **API-equivalent**: what the
//! same tokens would cost at the first-party API list prices, not what a subscription bills.
//! Nothing here reads a file or a clock: callers pass the rows and "now".
//!
//! Definitions (ContextCost's "How the numbers are computed"):
//! - prompt tokens = `input + cache_read + cache_write_5m + cache_write_1h`; the context fill of
//!   a request is its prompt size.
//! - cache hit ratio = `cache_read / prompt tokens`.
//! - a cache miss is a request after the first of its stream with `cache writes > 20,000` and
//!   `cache_read < 0.5 x` the previous request's prompt; it is reported with the idle gap before
//!   it, because a long gap usually means the cache TTL expired.

use serde::{Deserialize, Serialize};

/// The context window when nothing says otherwise.
pub const DEFAULT_WINDOW: u64 = 200_000;
/// The window of a `[1m]` model.
pub const LONG_WINDOW: u64 = 1_000_000;
/// Fill ratio where the gauge turns amber: auto-compact is getting close.
pub const WARN_RATIO: f64 = 0.70;
/// Fill ratio where the gauge turns red: Claude Code auto-compacts at about 80-85 % of the window.
pub const CRITICAL_RATIO: f64 = 0.85;
/// Cache writes above this (with few reads) make a request a cache miss.
pub const MISS_WRITE_MIN: u64 = 20_000;

/// One request's tokens, per class.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tokens {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
}

impl Tokens {
    /// What the request sent: input plus cache reads and writes (its context fill).
    pub fn prompt(&self) -> u64 {
        self.input.saturating_add(self.cache_read).saturating_add(self.cache_write())
    }

    pub fn cache_write(&self) -> u64 {
        self.cache_write_5m.saturating_add(self.cache_write_1h)
    }

    pub fn total(&self) -> u64 {
        self.prompt().saturating_add(self.output)
    }

    /// Field-wise maximum: one response is often written as several transcript lines, each with
    /// the usage so far (ContextCost keeps the maximum per field).
    pub fn max_with(&mut self, o: &Tokens) {
        self.input = self.input.max(o.input);
        self.output = self.output.max(o.output);
        self.cache_read = self.cache_read.max(o.cache_read);
        self.cache_write_5m = self.cache_write_5m.max(o.cache_write_5m);
        self.cache_write_1h = self.cache_write_1h.max(o.cache_write_1h);
    }

    pub fn add(&mut self, o: &Tokens) {
        self.input = self.input.saturating_add(o.input);
        self.output = self.output.saturating_add(o.output);
        self.cache_read = self.cache_read.saturating_add(o.cache_read);
        self.cache_write_5m = self.cache_write_5m.saturating_add(o.cache_write_5m);
        self.cache_write_1h = self.cache_write_1h.saturating_add(o.cache_write_1h);
    }
}

/// One API request (an assistant message with usage), numbers only: never content.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct UsageRequest {
    /// `message.id` (or the line's `requestId`): the de-duplication key.
    pub id: String,
    /// Claude's conversation id (the transcript's file name).
    pub claude_sid: String,
    /// `None` for the main thread; the subagent's id otherwise.
    pub agent_id: Option<String>,
    /// Epoch ms; `None` when the line had no readable timestamp.
    pub ts_ms: Option<i64>,
    /// The model id as the transcript names it.
    pub model: String,
    /// The price table entry it was priced with; `None` = unpriced.
    pub price_key: Option<String>,
    /// A fast-mode request (`usage.speed == "fast"`).
    pub fast: bool,
    pub tokens: Tokens,
    /// API-equivalent cost in USD; `None` = unpriced (never silently 0).
    pub cost_usd: Option<f64>,
}

/// A `system/compact_boundary` line.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Compaction {
    pub ts_ms: Option<i64>,
    pub agent_id: Option<String>,
    /// `auto` or `manual`.
    pub trigger: Option<String>,
    pub pre_tokens: Option<u64>,
    pub post_tokens: Option<u64>,
}

/// Sums over a set of requests.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Totals {
    pub requests: u64,
    pub tokens: Tokens,
    /// Sum over the priced requests.
    pub cost_usd: f64,
    /// Requests whose model has no price: their cost is unknown, not zero.
    pub unpriced_requests: u64,
    pub unpriced_models: Vec<String>,
}

impl Totals {
    pub fn add(&mut self, r: &UsageRequest) {
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
    }

    /// `cache_read / prompt tokens`, `None` when nothing was sent.
    pub fn cache_hit_ratio(&self) -> Option<f64> {
        let p = self.tokens.prompt();
        (p > 0).then(|| self.tokens.cache_read as f64 / p as f64)
    }
}

/// How full the context is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FillLevel {
    Ok,
    /// Past [`WARN_RATIO`].
    Warn,
    /// Past [`CRITICAL_RATIO`]: auto-compact is near.
    Critical,
}

pub fn fill_level(ratio: f64) -> FillLevel {
    if !ratio.is_finite() || ratio < WARN_RATIO {
        FillLevel::Ok
    } else if ratio < CRITICAL_RATIO {
        FillLevel::Warn
    } else {
        FillLevel::Critical
    }
}

/// The context window: 1M when the model id or the session's arguments name a `[1m]` model,
/// or when a request already sent more than 200k (only a 1M window allows that); else 200k.
pub fn context_window(model: &str, args: &str, largest_prompt: u64) -> u64 {
    let long = |s: &str| s.to_ascii_lowercase().contains("[1m]");
    if long(model) || long(args) || largest_prompt > DEFAULT_WINDOW { LONG_WINDOW } else { DEFAULT_WINDOW }
}

/// The context fill of the last main-thread request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ContextFill {
    pub fill_tokens: u64,
    pub window_tokens: u64,
    pub ratio: f64,
    pub level: FillLevel,
    pub model: String,
    pub at_ms: Option<i64>,
}

impl ContextFill {
    pub fn new(fill: u64, window: u64, model: String, at_ms: Option<i64>) -> ContextFill {
        let ratio = if window == 0 { 0.0 } else { fill as f64 / window as f64 };
        ContextFill { fill_tokens: fill, window_tokens: window, ratio, level: fill_level(ratio), model, at_ms }
    }
}

/// The last cache miss of the main thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheMiss {
    pub at_ms: Option<i64>,
    /// Time since the previous request; `None` when either has no timestamp.
    pub idle_gap_ms: Option<i64>,
    pub cache_write: u64,
    pub cache_read: u64,
    pub previous_prompt: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Compactions {
    pub count: u64,
    pub last: Option<Compaction>,
}

/// Everything `usage_session` reports for one session.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionUsage {
    pub context: Option<ContextFill>,
    pub total: Totals,
    pub today: Totals,
    pub main: Totals,
    pub subagents: Totals,
    pub cache_hit_ratio: Option<f64>,
    pub last_cache_miss: Option<CacheMiss>,
    pub compactions: Compactions,
}

/// Rows sorted by time, main thread first on ties. Pure.
pub fn sort_requests(rows: &mut [UsageRequest]) {
    rows.sort_by(|a, b| a.ts_ms.unwrap_or(i64::MIN).cmp(&b.ts_ms.unwrap_or(i64::MIN)).then_with(|| a.id.cmp(&b.id)));
}

/// Is `cur` a cache miss after `prev` (same stream)?
pub fn is_cache_miss(prev: &Tokens, cur: &Tokens) -> bool {
    cur.cache_write() > MISS_WRITE_MIN && (cur.cache_read as f64) < 0.5 * prev.prompt() as f64
}

/// Summarises one session's requests (any order) and compactions. `day_start_ms` is the start
/// of "today" (local midnight, from the caller); `args` are the session's launch arguments (a
/// `--model ...[1m]` there means a 1M window).
pub fn summarize_session(requests: &[UsageRequest], compactions: &[Compaction], day_start_ms: i64, args: &str) -> SessionUsage {
    let mut rows: Vec<&UsageRequest> = requests.iter().collect();
    rows.sort_by(|a, b| a.ts_ms.unwrap_or(i64::MIN).cmp(&b.ts_ms.unwrap_or(i64::MIN)).then_with(|| a.id.cmp(&b.id)));
    let mut out = SessionUsage::default();
    let mut prev_main: Option<&UsageRequest> = None;
    let mut largest_main = 0u64;
    for r in &rows {
        out.total.add(r);
        if r.ts_ms.is_some_and(|t| t >= day_start_ms) {
            out.today.add(r);
        }
        if r.agent_id.is_some() {
            out.subagents.add(r);
            continue;
        }
        out.main.add(r);
        largest_main = largest_main.max(r.tokens.prompt());
        if let Some(p) = prev_main
            && is_cache_miss(&p.tokens, &r.tokens)
        {
            out.last_cache_miss = Some(CacheMiss {
                at_ms: r.ts_ms,
                idle_gap_ms: r.ts_ms.zip(p.ts_ms).map(|(a, b)| a.saturating_sub(b)),
                cache_write: r.tokens.cache_write(),
                cache_read: r.tokens.cache_read,
                previous_prompt: p.tokens.prompt(),
            });
        }
        prev_main = Some(r);
    }
    if let Some(last) = prev_main {
        let window = context_window(&last.model, args, largest_main);
        out.context = Some(ContextFill::new(last.tokens.prompt(), window, last.model.clone(), last.ts_ms));
    }
    out.cache_hit_ratio = out.total.cache_hit_ratio();
    let mut comps: Vec<&Compaction> = compactions.iter().filter(|c| c.agent_id.is_none()).collect();
    comps.sort_by_key(|c| c.ts_ms.unwrap_or(i64::MIN));
    out.compactions = Compactions { count: comps.len() as u64, last: comps.last().map(|c| (*c).clone()) };
    out
}

/// The parent folders that say nothing about a project (ContextCost's `genericParentDirs`).
pub const GENERIC_PARENT_DIRS: [&str; 12] = ["projects", "github", "repos", "src", "source", "code", "dev", "work", "users", "home", "c", "d"];

/// A project name from a working folder (ContextCost `projectName`): the folder and its parent
/// (`umage/CodeShellManager`), the folder alone when the parent is generic (`C:\Projects\Demo` →
/// `Demo`); a path inside `.claude` / `.worktrees` / `worktrees` counts as the folder above it.
pub fn project_name(cwd: &str, fallback: &str) -> String {
    let mut parts: Vec<&str> = cwd.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
    if let Some(cut) = parts.iter().position(|p| matches!(p.to_ascii_lowercase().as_str(), ".claude" | ".worktrees" | "worktrees"))
        && cut > 0
    {
        parts.truncate(cut);
    }
    parts.retain(|p| !(p.len() == 2 && p.ends_with(':') && p.chars().next().is_some_and(|c| c.is_ascii_alphabetic())));
    let Some(last) = parts.last() else { return fallback.to_owned() };
    match parts.len().checked_sub(2).and_then(|i| parts.get(i)) {
        Some(parent) if !GENERIC_PARENT_DIRS.contains(&parent.to_ascii_lowercase().as_str()) => format!("{parent}/{last}"),
        _ => (*last).to_owned(),
    }
}

/// `$1.23`, `$0.004`, `$12.3k`: short enough for a chip.
pub fn format_usd(v: f64) -> String {
    if !v.is_finite() || v <= 0.0 {
        return "$0".to_owned();
    }
    if v >= 10_000.0 {
        format!("${:.1}k", v / 1000.0)
    } else if v >= 100.0 {
        format!("${v:.0}")
    } else if v >= 0.01 {
        format!("${v:.2}")
    } else {
        format!("${v:.3}")
    }
}

/// `850`, `12.3k`, `1.20M`.
pub fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1e6)
    } else if n >= 10_000 {
        format!("{:.0}k", n as f64 / 1e3)
    } else if n >= 1_000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

/// `45s`, `12m`, `3h 05m`, `2d 4h`.
pub fn format_gap(ms: i64) -> String {
    let s = (ms / 1000).max(0);
    match (s / 86_400, (s / 3600) % 24, (s / 60) % 60) {
        (0, 0, 0) => format!("{s}s"),
        (0, 0, m) => format!("{m}m"),
        (0, h, m) => format!("{h}h {m:02}m"),
        (d, h, _) => format!("{d}d {h}h"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(id: &str, ts: i64, agent: Option<&str>, t: Tokens, cost: Option<f64>) -> UsageRequest {
        UsageRequest {
            id: id.into(),
            claude_sid: "s1".into(),
            agent_id: agent.map(Into::into),
            ts_ms: Some(ts),
            model: "claude-opus-5-5".into(),
            price_key: cost.map(|_| "claude-opus-5-5".into()),
            fast: false,
            tokens: t,
            cost_usd: cost,
        }
    }

    fn tk(input: u64, output: u64, cr: u64, w5: u64) -> Tokens {
        Tokens { input, output, cache_read: cr, cache_write_5m: w5, cache_write_1h: 0 }
    }

    #[test]
    fn tokens_prompt_total_and_max() {
        let mut a = Tokens { input: 10, output: 20, cache_read: 30, cache_write_5m: 15, cache_write_1h: 25 };
        assert_eq!(a.prompt(), 80);
        assert_eq!(a.total(), 100);
        a.max_with(&Tokens { input: 5, output: 50, ..Tokens::default() });
        assert_eq!((a.input, a.output), (10, 50));
    }

    #[test]
    fn session_summary_matches_the_contextcost_fixture() {
        // s1 of ContextCost's fixture: msg_1, msg_2, compaction, msg_3, plus a subagent request.
        let rows = vec![
            req("msg_1", 1_000, None, tk(100, 50, 0, 1000), Some(0.01)),
            req("msg_2", 61_000, None, tk(10, 20, 1000, 1000), Some(0.02)),
            req("msg_3", 181_000, None, tk(5, 5, 0, 0), Some(0.001)),
            req("msg_l1", 31_000, Some("a1"), tk(1000, 0, 0, 0), None),
        ];
        let comps = vec![Compaction { ts_ms: Some(120_000), trigger: Some("auto".into()), pre_tokens: Some(1000), post_tokens: Some(100), ..Compaction::default() }];
        let u = summarize_session(&rows, &comps, 60_000, "");
        assert_eq!(u.total.requests, 4);
        assert_eq!(u.main.requests, 3);
        assert_eq!(u.subagents.requests, 1);
        assert_eq!(u.subagents.unpriced_requests, 1, "unpriced is counted, never $0");
        assert_eq!(u.subagents.unpriced_models, ["claude-opus-5-5"]);
        assert!((u.total.cost_usd - 0.031).abs() < 1e-9);
        assert_eq!(u.today.requests, 2, "msg_2 and msg_3 are after the day start");
        let c = u.context.unwrap();
        assert_eq!((c.fill_tokens, c.window_tokens, c.level), (5, DEFAULT_WINDOW, FillLevel::Ok));
        assert_eq!(c.at_ms, Some(181_000));
        assert_eq!(u.compactions.count, 1);
        assert_eq!(u.compactions.last.unwrap().post_tokens, Some(100));
        // cache read 1000 of prompt (100+1000) + (10+1000+1000) + 5 + 1000
        let hit = u.cache_hit_ratio.unwrap();
        assert!((hit - 1000.0 / 4115.0).abs() < 1e-9, "{hit}");
        assert!(u.last_cache_miss.is_none(), "writes under 20k are no miss");
    }

    #[test]
    fn a_cache_miss_is_reported_with_its_idle_gap() {
        let rows = vec![
            req("a", 0, None, tk(10, 10, 0, 50_000), Some(1.0)),
            req("b", 60_000, None, tk(10, 10, 50_000, 500), Some(1.0)),
            // Six minutes idle: the 5-minute cache expired and everything is written again.
            req("c", 420_000, None, tk(10, 10, 0, 51_000), Some(1.0)),
            req("d", 430_000, None, tk(10, 10, 51_000, 100), Some(1.0)),
        ];
        let u = summarize_session(&rows, &[], 0, "");
        let m = u.last_cache_miss.unwrap();
        assert_eq!(m.at_ms, Some(420_000));
        assert_eq!(m.idle_gap_ms, Some(360_000));
        assert_eq!((m.cache_write, m.cache_read, m.previous_prompt), (51_000, 0, 50_510));
    }

    #[test]
    fn subagent_requests_never_set_the_main_context_fill() {
        let rows = vec![req("m", 0, None, tk(1000, 1, 0, 0), Some(0.1)), req("s", 10, Some("x"), tk(150_000, 1, 0, 0), Some(0.1))];
        let u = summarize_session(&rows, &[], 0, "");
        assert_eq!(u.context.unwrap().fill_tokens, 1000);
        assert!((u.subagents.cost_usd - 0.1).abs() < 1e-12);
    }

    #[test]
    fn windows_and_levels() {
        assert_eq!(context_window("claude-opus-5-5", "", 10), DEFAULT_WINDOW);
        assert_eq!(context_window("claude-opus-5-5[1m]", "", 10), LONG_WINDOW);
        assert_eq!(context_window("claude-opus-5-5", "--model opus[1M]", 10), LONG_WINDOW);
        assert_eq!(context_window("x", "", 250_000), LONG_WINDOW, "over 200k can only be a 1M window");
        assert_eq!(fill_level(0.5), FillLevel::Ok);
        assert_eq!(fill_level(0.75), FillLevel::Warn);
        assert_eq!(fill_level(0.9), FillLevel::Critical);
        assert_eq!(fill_level(f64::NAN), FillLevel::Ok);
        let c = ContextFill::new(170_000, 200_000, "m".into(), None);
        assert_eq!(c.level, FillLevel::Critical);
        assert_eq!(ContextFill::new(1, 0, "m".into(), None).ratio, 0.0);
    }

    #[test]
    fn empty_input_is_an_empty_summary() {
        let u = summarize_session(&[], &[], 0, "");
        assert_eq!(u, SessionUsage::default());
    }

    #[test]
    fn project_names_follow_contextcost() {
        assert_eq!(project_name(r"C:\Projects\Demo", "x"), "Demo");
        assert_eq!(project_name(r"C:\Github\umage\CodeShellManager", "x"), "umage/CodeShellManager");
        assert_eq!(project_name("/home/me/src/app", "x"), "app", "src is a generic parent");
        assert_eq!(project_name("/home/me/shop", "x"), "me/shop");
        assert_eq!(project_name(r"C:\Github\umage\repo\.claude\worktrees\feat", "x"), "umage/repo");
        assert_eq!(project_name(r"C:\", "fallback"), "fallback");
        assert_eq!(project_name("", "fallback"), "fallback");
        assert_eq!(project_name("Demo", "x"), "Demo");
    }

    #[test]
    fn formats() {
        assert_eq!(format_usd(0.0), "$0");
        assert_eq!(format_usd(0.0042), "$0.004");
        assert_eq!(format_usd(0.42), "$0.42");
        assert_eq!(format_usd(3.456), "$3.46");
        assert_eq!(format_usd(123.4), "$123");
        assert_eq!(format_usd(12_345.0), "$12.3k");
        assert_eq!(format_usd(f64::NAN), "$0");
        assert_eq!(format_tokens(850), "850");
        assert_eq!(format_tokens(1_234), "1.2k");
        assert_eq!(format_tokens(123_456), "123k");
        assert_eq!(format_tokens(1_200_000), "1.20M");
        assert_eq!(format_gap(45_000), "45s");
        assert_eq!(format_gap(12 * 60_000), "12m");
        assert_eq!(format_gap(3 * 3_600_000 + 5 * 60_000), "3h 05m");
        assert_eq!(format_gap(2 * 86_400_000 + 4 * 3_600_000), "2d 4h");
    }
}
