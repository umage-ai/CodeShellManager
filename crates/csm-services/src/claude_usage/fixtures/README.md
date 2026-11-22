Synthetic transcripts for the golden tests, line for line ContextCost's `test/fixture.js`
(`buildFixture`): no real conversation content, ids or paths.

`r1.jsonl` and `r2.jsonl` are the usage report's fixture (`golden.rs`): a conversation that runs
over midnight (UTC) with 1h cache writes, a cache miss after a two-hour gap and two models, and
one on a snapshot-suffixed Haiku with an unpriced local model and a broken line.
