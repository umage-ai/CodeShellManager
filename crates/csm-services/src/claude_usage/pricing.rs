//! API list prices (ContextCost `pricing.js` + `config/prices.json`).
//!
//! - A model id resolves by alias, then exactly, then by the longest entry it extends with a
//!   snapshot suffix (`claude-haiku-4-5-20251001` → `claude-haiku-4-5`). A newer version never
//!   inherits an older entry's price (`claude-fable-5-2` does not fall back to `claude-fable-5`):
//!   such a model is **unpriced** and reported, never priced at $0.
//! - A request whose whole prompt is over `longContext.threshold` uses the long-context rates.
//! - Fast-mode requests on `fastModels` cost `fastMultiplier` times as much.
//!
//! The embedded table is the default; a user file (`usage-prices.json` beside `state.json`)
//! overrides or adds entries ([`PriceTable::with_override`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use csm_core::claude_usage::Tokens;
use csm_core::usage_report::RateCard;

/// The table shipped with the app (USD per million tokens).
pub const DEFAULT_PRICES_JSON: &str = include_str!("prices.json");

/// The file name of the user's override, beside `state.json`.
pub const OVERRIDE_FILE: &str = "usage-prices.json";

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TierRates {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write5m: f64,
    #[serde(default)]
    pub cache_write1h: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LongContext {
    pub threshold: u64,
    #[serde(flatten)]
    pub rates: TierRates,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Rates {
    #[serde(flatten)]
    pub base: TierRates,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context: Option<LongContext>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct PriceTable {
    /// The date the embedded table was checked against the list prices (`YYYY-MM-DD`). Bump it
    /// with every price change; the reports name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default = "one")]
    pub fast_multiplier: f64,
    #[serde(default)]
    pub fast_models: Vec<String>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
    #[serde(default)]
    pub models: BTreeMap<String, Rates>,
}

fn one() -> f64 {
    1.0
}

/// A priced request.
#[derive(Debug, Clone, PartialEq)]
pub struct Priced {
    pub key: String,
    pub cost_usd: f64,
}

/// What the override file did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum OverrideStatus {
    /// No file: the embedded table.
    None,
    /// Applied, with this many model entries.
    Applied(usize),
    /// Unreadable or not a price table: ignored, with the reason.
    Ignored(String),
}

impl PriceTable {
    /// The embedded table. It is checked by a test, so the fallback to an empty table never
    /// happens in a shipped build.
    pub fn embedded() -> PriceTable {
        PriceTable::from_json(DEFAULT_PRICES_JSON).unwrap_or_default()
    }

    pub fn from_json(text: &str) -> Result<PriceTable, String> {
        let mut t: PriceTable = serde_json::from_str(text).map_err(|e| format!("not a price table: {e}"))?;
        t.normalize();
        Ok(t)
    }

    /// Keys lower-cased (model ids compare case-insensitively); non-finite or negative rates are
    /// dropped as zero, a non-positive multiplier is 1.
    fn normalize(&mut self) {
        let fix = |r: &mut TierRates| {
            for v in [&mut r.input, &mut r.output, &mut r.cache_read, &mut r.cache_write5m, &mut r.cache_write1h] {
                if !v.is_finite() || *v < 0.0 {
                    *v = 0.0;
                }
            }
        };
        self.models = std::mem::take(&mut self.models)
            .into_iter()
            .map(|(k, mut v)| {
                fix(&mut v.base);
                if let Some(lc) = v.long_context.as_mut() {
                    fix(&mut lc.rates);
                }
                (k.to_ascii_lowercase(), v)
            })
            .collect();
        self.aliases = std::mem::take(&mut self.aliases).into_iter().map(|(k, v)| (k.to_ascii_lowercase(), v.to_ascii_lowercase())).collect();
        self.fast_models = self.fast_models.iter().map(|m| m.to_ascii_lowercase()).collect();
        if !self.fast_multiplier.is_finite() || self.fast_multiplier <= 0.0 {
            self.fast_multiplier = 1.0;
        }
    }

    /// This table with a user file's entries on top (models and aliases replace or add; the
    /// fast multiplier and fast models replace when the file sets them). A bad file is ignored.
    pub fn with_override(mut self, text: Option<&str>) -> (PriceTable, OverrideStatus) {
        let Some(text) = text else { return (self, OverrideStatus::None) };
        let raw: serde_json::Value = match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => return (self, OverrideStatus::Ignored(format!("not JSON: {e}"))),
        };
        let o = match PriceTable::from_json(text) {
            Ok(o) => o,
            Err(e) => return (self, OverrideStatus::Ignored(e)),
        };
        let n = o.models.len();
        self.models.extend(o.models);
        self.aliases.extend(o.aliases);
        if raw.get("fastMultiplier").is_some() {
            self.fast_multiplier = o.fast_multiplier;
        }
        if raw.get("fastModels").is_some() {
            self.fast_models = o.fast_models;
        }
        if o.source.is_some() {
            self.source = o.source;
        }
        if o.version.is_some() {
            self.version = o.version;
        }
        (self, OverrideStatus::Applied(n))
    }

    /// The entry a model id is priced with, or `None` (unpriced).
    pub fn resolve_key(&self, model: &str) -> Option<String> {
        let m = model.trim().to_ascii_lowercase();
        if m.is_empty() {
            return None;
        }
        if let Some(a) = self.aliases.get(&m) {
            return self.models.contains_key(a).then(|| a.clone());
        }
        if self.models.contains_key(&m) {
            return Some(m);
        }
        // A snapshot suffix inherits the entry; a newer minor version (`-2`, `-12`) does not.
        self.models
            .keys()
            .filter(|k| {
                m.strip_prefix(k.as_str()).and_then(|rest| rest.strip_prefix('-')).is_some_and(|rest| !is_minor_version(rest))
            })
            .max_by_key(|k| k.len())
            .cloned()
    }

    /// The API-equivalent cost of one request, or `None` when its model has no price.
    pub fn price(&self, model: &str, tokens: &Tokens, fast: bool) -> Option<Priced> {
        let (key, r, mul) = self.tier(model, tokens, fast)?;
        let c = (tokens.input as f64 * r.input
            + tokens.output as f64 * r.output
            + tokens.cache_read as f64 * r.cache_read
            + tokens.cache_write_5m as f64 * r.cache_write5m
            + tokens.cache_write_1h as f64 * r.cache_write1h)
            / 1e6;
        Some(Priced { key, cost_usd: c * mul })
    }

    /// The rates one request is priced with (its tier chosen, the fast multiplier beside them),
    /// for the report's per-class and cache numbers. `None` when unpriced.
    pub fn rate_card(&self, model: &str, tokens: &Tokens, fast: bool) -> Option<RateCard> {
        let (_, r, multiplier) = self.tier(model, tokens, fast)?;
        Some(RateCard {
            input: r.input,
            output: r.output,
            cache_read: r.cache_read,
            cache_write_5m: r.cache_write5m,
            cache_write_1h: r.cache_write1h,
            multiplier,
        })
    }

    /// `(price key, tier rates, fast multiplier)`: the long-context tier when the whole prompt is
    /// over its threshold; the multiplier only for a fast request on a fast model.
    fn tier(&self, model: &str, tokens: &Tokens, fast: bool) -> Option<(String, TierRates, f64)> {
        let key = self.resolve_key(model)?;
        let rates = self.models.get(&key)?;
        let r = match rates.long_context {
            Some(lc) if tokens.prompt() > lc.threshold => lc.rates,
            _ => rates.base,
        };
        let mul = if fast && self.fast_models.contains(&key) { self.fast_multiplier } else { 1.0 };
        Some((key, r, mul))
    }

    /// A fingerprint: when it changes, stored costs are recomputed.
    pub fn signature(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }
}

/// `2`, `12`, `2-20261101`: what follows a key when the id is a newer version, not a snapshot.
fn is_minor_version(rest: &str) -> bool {
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    (1..=2).contains(&digits) && rest.as_bytes().get(digits).is_none_or(|b| *b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> PriceTable {
        PriceTable::embedded()
    }

    fn tok(input: u64, output: u64, cr: u64, w5: u64, w1: u64) -> Tokens {
        Tokens { input, output, cache_read: cr, cache_write_5m: w5, cache_write_1h: w1 }
    }

    #[test]
    fn the_embedded_table_parses() {
        let table = PriceTable::from_json(DEFAULT_PRICES_JSON).unwrap();
        assert!(table.models.len() >= 20);
        assert_eq!(table.fast_multiplier, 2.0);
    }

    #[test]
    fn resolve_exact_snapshot_alias_unknown() {
        let t = t();
        assert_eq!(t.resolve_key("claude-opus-5-5").as_deref(), Some("claude-opus-5-5"));
        assert_eq!(t.resolve_key("claude-opus-5").as_deref(), Some("claude-opus-5"));
        assert_eq!(t.resolve_key("claude-haiku-4-5-20251001").as_deref(), Some("claude-haiku-4-5"));
        assert_eq!(t.resolve_key("sonnet").as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(t.resolve_key("Claude-Opus-5-5").as_deref(), Some("claude-opus-5-5"));
        assert_eq!(t.resolve_key("qwen3.8:27b"), None);
        assert_eq!(t.resolve_key("claude-nova-9"), None);
        assert_eq!(t.resolve_key(""), None);
    }

    #[test]
    fn a_newer_version_never_inherits_an_older_price() {
        let t = PriceTable::from_json(r#"{"models": {"claude-fable-5": {}, "claude-haiku-4-5": {}}}"#).unwrap();
        assert_eq!(t.resolve_key("claude-fable-5-2"), None);
        assert_eq!(t.resolve_key("claude-fable-5-2-20261101"), None);
        assert_eq!(t.resolve_key("claude-fable-5-20260801").as_deref(), Some("claude-fable-5"));
        assert_eq!(t.resolve_key("claude-haiku-4-5-20251001").as_deref(), Some("claude-haiku-4-5"));
        let e = self::t();
        assert_eq!(e.resolve_key("claude-fable-5-2").as_deref(), Some("claude-fable-5-2"));
        assert_eq!(e.resolve_key("haiku").as_deref(), Some("claude-haiku-5-5"));
    }

    #[test]
    fn cost_sums_all_five_classes_per_mtok() {
        let t = t();
        let all = tok(1_000_000, 1_000_000, 1_000_000, 1_000_000, 1_000_000);
        let p = t.price("claude-opus-5-5", &all, false).unwrap();
        assert!((p.cost_usd - 37.2).abs() < 1e-9, "{}", p.cost_usd);
        let input_only = tok(1_000_000, 0, 0, 0, 0);
        assert!((t.price("claude-opus-5-5", &input_only, true).unwrap().cost_usd - 8.0).abs() < 1e-9, "fast x2");
        assert!((t.price("claude-sonnet-5-5", &input_only, true).unwrap().cost_usd - 2.0).abs() < 1e-9, "not a fast model");
        assert_eq!(t.price("claude-nova-9", &all, false), None, "unpriced, not $0");
    }

    #[test]
    fn haiku_5_5_switches_to_long_context_rates_over_100k() {
        let t = t();
        let small = tok(1000, 1000, 99_000, 0, 0);
        let c = t.price("claude-haiku-5-5", &small, false).unwrap().cost_usd;
        assert!((c - (1000.0 * 0.1 + 1000.0 * 0.5 + 99_000.0 * 0.01) / 1e6).abs() < 1e-12);
        let big = tok(1000, 1000, 99_001, 0, 0);
        let c = t.price("claude-haiku-5-5", &big, false).unwrap().cost_usd;
        assert!((c - (1000.0 * 0.5 + 1000.0 * 2.5 + 99_001.0 * 0.05) / 1e6).abs() < 1e-12);
    }

    #[test]
    fn the_override_adds_and_replaces_and_a_bad_one_is_ignored() {
        let (t2, st) = t().with_override(Some(r#"{"models": {"Claude-Nova-9": {"input": 1, "output": 2}, "claude-opus-5-5": {"input": 100}}, "aliases": {"nova": "claude-nova-9"}}"#));
        assert_eq!(st, OverrideStatus::Applied(2));
        assert_eq!(t2.resolve_key("nova").as_deref(), Some("claude-nova-9"));
        let p = t2.price("claude-opus-5-5", &tok(1_000_000, 0, 0, 0, 0), false).unwrap();
        assert!((p.cost_usd - 100.0).abs() < 1e-9);
        assert_eq!(t2.fast_multiplier, 2.0, "kept when the file does not set it");
        assert_ne!(t2.signature(), t().signature());
        let (t3, st) = t().with_override(Some("{ nope"));
        assert!(matches!(st, OverrideStatus::Ignored(_)));
        assert_eq!(t3, t());
        let (_, st) = t().with_override(Some(r#"{"models": 5}"#));
        assert!(matches!(st, OverrideStatus::Ignored(_)));
        let (_, st) = t().with_override(None);
        assert_eq!(st, OverrideStatus::None);
    }

    #[test]
    fn the_embedded_table_is_versioned_and_matches_contextcost() {
        let t = t();
        // The table is ContextCost's `config/prices.json` with a version: bump both together.
        assert_eq!(t.version.as_deref(), Some("2026-11-15"));
        assert!(t.source.as_deref().is_some_and(|s| s.contains("checked 2026-11-15")), "the source names the same date");
        let r = |k: &str| t.models[k].base;
        let opus = r("claude-opus-5-5");
        assert_eq!((opus.input, opus.output, opus.cache_read, opus.cache_write5m, opus.cache_write1h), (4.0, 20.0, 0.2, 5.0, 8.0));
        let sonnet = r("claude-sonnet-5-5");
        assert_eq!((sonnet.input, sonnet.output, sonnet.cache_read, sonnet.cache_write5m, sonnet.cache_write1h), (2.0, 10.0, 0.1, 2.5, 4.0));
        let haiku45 = r("claude-haiku-4-5");
        assert_eq!((haiku45.input, haiku45.cache_write1h), (1.0, 2.0));
        assert_eq!(t.models["claude-haiku-5-5"].long_context.map(|l| l.threshold), Some(100_000));
        // Every entry: a 1h write costs more than a 5m write, which costs more than input, which
        // costs more than a cache read (what the cache numbers assume).
        for (k, v) in &t.models {
            for b in std::iter::once(v.base).chain(v.long_context.map(|l| l.rates)) {
                assert!(b.cache_write1h > b.cache_write5m && b.cache_write5m > b.input && b.input > b.cache_read, "{k}");
            }
        }
    }

    #[test]
    fn the_rate_card_is_the_tier_the_cost_used() {
        let t = t();
        let small = tok(1000, 0, 1000, 0, 0);
        let rc = t.rate_card("claude-haiku-5-5", &small, false).unwrap();
        assert_eq!((rc.input, rc.cache_read, rc.multiplier), (0.1, 0.01, 1.0));
        let big = tok(1000, 0, 200_000, 0, 0);
        let rc = t.rate_card("claude-haiku-5-5", &big, false).unwrap();
        assert_eq!((rc.input, rc.cache_write_1h), (0.5, 1.0), "long-context tier");
        let rc = t.rate_card("claude-opus-5-5", &small, true).unwrap();
        assert_eq!(rc.multiplier, 2.0);
        assert_eq!(t.rate_card("claude-sonnet-5-5", &small, true).unwrap().multiplier, 1.0, "not a fast model");
        assert_eq!(t.rate_card("qwen3.8:27b", &small, false), None);
        // The classes priced with the card add up to the cost.
        let all = tok(123, 456, 789, 1011, 1213);
        let rc = t.rate_card("claude-opus-5-5", &all, true).unwrap();
        let by_class = (123.0 * rc.input + 456.0 * rc.output + 789.0 * rc.cache_read + 1011.0 * rc.cache_write_5m + 1213.0 * rc.cache_write_1h) / 1e6 * rc.multiplier;
        assert!((by_class - t.price("claude-opus-5-5", &all, true).unwrap().cost_usd).abs() < 1e-12);
    }

    #[test]
    fn hostile_rates_are_neutralised() {
        let t = PriceTable::from_json(r#"{"fastMultiplier": -3, "models": {"m": {"input": -5, "output": 1e400}}}"#);
        // 1e400 is not valid JSON for f64 in serde_json: the whole file is refused, not trusted.
        assert!(t.is_err() || t.as_ref().is_ok_and(|t| t.models["m"].base.input == 0.0));
        let t = PriceTable::from_json(r#"{"fastMultiplier": -3, "models": {"m": {"input": -5}}}"#).unwrap();
        assert_eq!(t.fast_multiplier, 1.0);
        assert_eq!(t.models["m"].base.input, 0.0);
    }
}
