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

