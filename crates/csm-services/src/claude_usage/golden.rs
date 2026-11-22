//! Golden tests over ContextCost's fixture (`fixtures/`, synthetic): the same numbers its
//! importer tests expect.

use std::collections::BTreeMap;
use std::path::PathBuf;

use csm_core::claude_usage::{Compaction, UsageRequest, project_name, summarize_session};

use super::{Batch, PriceTable, tail};

const S1: &str = include_str!("fixtures/s1.jsonl");
const S2: &str = include_str!("fixtures/s2.jsonl");
const A1: &str = include_str!("fixtures/s1/subagents/agent-a1.jsonl");

fn fold(text: &str) -> Batch {
    let mut b = Batch::default();
    for l in text.lines() {
        b.push(l);
    }
    b
}

fn priced(b: &Batch, sid: &str, agent: Option<&str>, prices: &PriceTable) -> Vec<UsageRequest> {
    b.requests
        .iter()
        .map(|r| {
            let p = prices.price(&r.model, &r.tokens, r.fast);
            UsageRequest {
                id: r.id.clone(),
                claude_sid: sid.into(),
                agent_id: agent.map(Into::into).or_else(|| r.agent_id.clone()),
                ts_ms: r.ts_ms,
                model: r.model.clone(),
                price_key: p.as_ref().map(|p| p.key.clone()),
                fast: r.fast,
                tokens: r.tokens,
                cost_usd: p.map(|p| p.cost_usd),
            }
        })
        .collect()
}

#[test]
fn the_fixture_folds_to_contextcosts_requests() {
    let s1 = fold(S1);
    let ids: Vec<&str> = s1.requests.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["msg_1", "msg_2", "msg_3"], "msg_1 is two lines, one request");
    let m1 = &s1.requests[0];
    assert_eq!((m1.tokens.input, m1.tokens.output, m1.tokens.cache_write_5m), (100, 50, 1000));
    assert_eq!(s1.compactions.len(), 1);
    assert_eq!(s1.compactions[0].0, 6, "the compaction is line 6");
    assert_eq!((s1.lines, s1.bad_lines, s1.unknown_lines), (8, 0, 0));
    assert_eq!(project_name(s1.cwd.as_deref().unwrap(), "x"), "Demo");

    let s2 = fold(S2);
    assert_eq!(s2.requests.len(), 1);
    assert_eq!(s2.bad_lines, 1, "the broken line is counted, not fatal");
    assert_eq!(project_name(s2.cwd.as_deref().unwrap(), "x"), "Other");

    let a1 = fold(A1);
    assert_eq!(a1.requests[0].agent_id.as_deref(), Some("a1"));
}

#[test]
fn the_fixture_prices_and_summarises() {
    let prices = PriceTable::embedded();
    let mut rows = priced(&fold(S1), "s1", None, &prices);
    rows.extend(priced(&fold(A1), "s1", Some("a1"), &prices));
    let w1 = priced(&fold(S2), "s2", None, &prices);
    assert_eq!(w1[0].price_key.as_deref(), Some("claude-haiku-4-5"), "a snapshot id takes its entry");
    assert!((w1[0].cost_usd.unwrap() - 0.006).abs() < 1e-12);

    let by_id: BTreeMap<&str, &UsageRequest> = rows.iter().map(|r| (r.id.as_str(), r)).collect();
    assert!((by_id["msg_1"].cost_usd.unwrap() - 0.0064).abs() < 1e-12);
    assert!((by_id["msg_2"].cost_usd.unwrap() - 0.00564).abs() < 1e-12);
    assert!((by_id["msg_3"].cost_usd.unwrap() - 0.00012).abs() < 1e-12);
    assert_eq!(by_id["msg_l1"].cost_usd, None, "a local model is unpriced, not $0");

    let comps: Vec<Compaction> = fold(S1)
        .compactions
        .into_iter()
        .map(|(_, c)| Compaction { ts_ms: c.ts_ms, agent_id: c.agent_id, trigger: c.trigger, pre_tokens: c.pre_tokens, post_tokens: c.post_tokens })
        .collect();
    let u = summarize_session(&rows, &comps, 0, "");
    assert_eq!((u.total.requests, u.main.requests, u.subagents.requests), (4, 3, 1));
    assert!((u.main.cost_usd - 0.01216).abs() < 1e-12, "{}", u.main.cost_usd);
    assert_eq!(u.subagents.unpriced_models, ["qwen3.8:27b"]);
    assert_eq!(u.context.as_ref().unwrap().fill_tokens, 5, "msg_3, after the compaction");
    assert_eq!(u.compactions.count, 1);
    assert_eq!(u.compactions.last.as_ref().and_then(|c| c.pre_tokens), Some(1000));
}

#[test]
fn reading_the_fixture_in_passes_gives_the_same_requests() {
    let path: PathBuf = [env!("CARGO_MANIFEST_DIR"), "src", "claude_usage", "fixtures", "s1.jsonl"].iter().collect();
    let mut merged: BTreeMap<String, csm_core::claude_usage::Tokens> = BTreeMap::new();
    let mut offset = 0;
    let mut lines = 0;
    let mut passes = 0;
    loop {
        let mut b = Batch::after(lines);
        let pass = tail::read_lines(&path, offset, 400, |l| b.push(l)).unwrap();
        for r in &b.requests {
            merged.entry(r.id.clone()).or_default().max_with(&r.tokens);
        }
        offset = pass.end;
        lines = b.lines;
        passes += 1;
        if !pass.more {
            break;
        }
    }
    assert!(passes > 2, "the budget split the file");
    let whole = fold(S1);
    assert_eq!(merged.len(), whole.requests.len());
    for r in &whole.requests {
        assert_eq!(merged[&r.id], r.tokens, "{}", r.id);
    }
    assert_eq!(lines, 8);
}


#[test]
fn the_report_fixture_gives_the_report_numbers() {
    use csm_core::usage_report::{Filter, GroupBy, ReportQuery, ReportRequest, build, utc_days};
    const R1: &str = include_str!("fixtures/r1.jsonl");
    const R2: &str = include_str!("fixtures/r2.jsonl");
    let prices = PriceTable::embedded();
    let mut rows: Vec<ReportRequest> = Vec::new();
    for (text, sid, csm) in [(R1, "r1", Some("csm-shop")), (R2, "r2", None)] {
        let b = fold(text);
        let project = b.cwd.as_deref().map(|c| project_name(c, sid));
        rows.extend(
            priced(&b, sid, None, &prices)
                .into_iter()
                .map(|req| ReportRequest { req, csm_session: csm.map(Into::into), project: project.clone() }),
        );
    }
    assert_eq!(rows.len(), 6, "the synthetic message is skipped");
    // 2026-10-01T00:00Z .. 2026-10-03T00:00Z.
    let (from, to) = (1_790_812_800_000, 1_790_985_600_000);
    let q = |group_by| ReportQuery { from_ms: Some(from), to_ms: Some(to), days: utc_days(from, to), group_by, filter: Filter::default() };
    let rates = |r: &UsageRequest| prices.rate_card(&r.model, &r.tokens, r.fast);
    let rep = build(&rows, &q(GroupBy::Session), &rates);
    let t = &rep.total;
    assert_eq!(t.requests, 6);
    assert_eq!((t.tokens.cache_write_5m, t.tokens.cache_write_1h), (6_000, 122_000), "1h writes are kept apart");
    assert_eq!(t.unpriced_models, ["qwen3.8:27b"]);
    // r1: opus 200*4 + 1000*20 + 60000*8 = 500800; 20*4 + 400*20 + 60000*0.2 + 2000*5 = 30080;
    //     20*4 + 300*20 + 62000*8 = 502080; sonnet 3000*2 + 100*10 = 7000.
    // r2: haiku-4-5 5000*1 + 500*5 + 20000*0.1 + 4000*1.25 = 14500.
    let want = (500_800.0 + 30_080.0 + 502_080.0 + 7_000.0 + 14_500.0) / 1e6;
    assert!((t.cost_usd - want).abs() < 1e-9, "{}", t.cost_usd);
    assert!((t.cost_by_class.total() - want).abs() < 1e-9);
    assert!((t.cost_by_class.cache_write_1h - 122_000.0 * 8.0 / 1e6).abs() < 1e-12);
    // Hit rate: reads 80k of prompt 200+60000 + 20+60000+2000 + 20+62000 + 3000 + 5000+20000+4000 + 900.
    let prompt = 217_140.0;
    assert!((t.cache_hit_rate.unwrap() - 80_000.0 / prompt).abs() < 1e-12);
    // The miss: r1_m3 re-wrote 62k (1h) after 2h14m idle.
    assert_eq!(t.miss_events, 1);
    assert_eq!(rep.misses[0].idle_gap_ms, Some((2 * 60 + 14) * 60_000));
    assert!((rep.misses[0].impact_usd - 62_000.0 * 7.8 / 1e6).abs() < 1e-12);
    // Over midnight: two requests on Oct 1, the rest on Oct 2.
    let per_day: Vec<(&str, u64)> = rep.days.iter().map(|d| (d.day.as_str(), d.metrics.requests)).collect();
    assert_eq!(per_day, [("2026-10-01", 2), ("2026-10-02", 4)]);
    assert_eq!(rep.rows[0].key, "csm-shop");
    assert_eq!(rep.rows[0].projects, ["Shop"]);
    assert_eq!(rep.rows[1].key, "r2");
    assert_eq!(rep.rows[1].projects, ["blog"]);
    // Drill into the shop session: its models.
    let mut drill = q(GroupBy::Model);
    drill.filter.session = Some("csm-shop".into());
    let d = build(&rows, &drill, &rates);
    assert_eq!(d.rows.iter().map(|r| r.key.as_str()).collect::<Vec<_>>(), ["claude-opus-5-5", "claude-sonnet-5-5"]);
}
