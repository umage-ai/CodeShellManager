//! Claude Code usage (schema 3): per-request token numbers and costs read from Claude's
//! transcripts, the read position of each transcript, and compactions. Numbers only: no
//! prompt, reply, tool output or title is stored.

use rusqlite::{OptionalExtension, Row, params};
use serde::Serialize;

use csm_core::claude_usage::{Compaction, Tokens, UsageRequest};

use crate::db::{Db, IndexError};

/// Schema version 3: the Claude usage tables.
pub(crate) const V3: &str = "
CREATE TABLE IF NOT EXISTS claude_files (
    path TEXT PRIMARY KEY,
    claude_sid TEXT NOT NULL,
    agent_id TEXT,
    csm_session TEXT,
    project TEXT,
    size INTEGER NOT NULL DEFAULT 0,
    mtime_ms INTEGER NOT NULL DEFAULT 0,
    offset INTEGER NOT NULL DEFAULT 0,
    head BLOB,
    lines INTEGER NOT NULL DEFAULT 0,
    bad_lines INTEGER NOT NULL DEFAULT 0,
    unknown_lines INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS claude_requests (
    id TEXT PRIMARY KEY,
    claude_sid TEXT NOT NULL,
    agent_id TEXT,
    csm_session TEXT,
    project TEXT,
    ts INTEGER,
    model TEXT NOT NULL,
    price_key TEXT,
    fast INTEGER NOT NULL DEFAULT 0,
    input INTEGER NOT NULL DEFAULT 0,
    output INTEGER NOT NULL DEFAULT 0,
    cache_read INTEGER NOT NULL DEFAULT 0,
    cache_w5 INTEGER NOT NULL DEFAULT 0,
    cache_w1h INTEGER NOT NULL DEFAULT 0,
    cost REAL,
    file TEXT
);
CREATE INDEX IF NOT EXISTS claude_requests_session ON claude_requests(csm_session, ts);
CREATE INDEX IF NOT EXISTS claude_requests_ts ON claude_requests(ts);
CREATE TABLE IF NOT EXISTS claude_compactions (
    file TEXT NOT NULL,
    line INTEGER NOT NULL,
    claude_sid TEXT NOT NULL,
    agent_id TEXT,
    csm_session TEXT,
    ts INTEGER,
    trigger TEXT,
    pre_tokens INTEGER,
    post_tokens INTEGER,
    PRIMARY KEY (file, line)
);
CREATE INDEX IF NOT EXISTS claude_compactions_session ON claude_compactions(csm_session);
CREATE TABLE IF NOT EXISTS claude_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
";

/// Where reading a transcript got to, and what it is.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ClaudeFile {
    pub path: String,
    pub claude_sid: String,
    /// The subagent this transcript is (`None`: the main thread).
    pub agent_id: Option<String>,
    /// The CSM session it is linked to.
    pub csm_session: Option<String>,
    pub project: Option<String>,
    pub size: u64,
    pub mtime_ms: i64,
    /// Bytes read (complete lines only).
    pub offset: u64,
    /// The file's first bytes, to notice it was rewritten.
    #[serde(skip)]
    pub head: Vec<u8>,
    pub lines: u64,
    pub bad_lines: u64,
    pub unknown_lines: u64,
}

/// One pass over one transcript, to store in one transaction.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeIngest {
    /// The file's state after the pass.
    pub file: ClaudeFile,
    /// The file was rewritten: forget what was stored from it first.
    pub reset: bool,
    pub requests: Vec<UsageRequest>,
    /// `(line number, compaction)`.
    pub compactions: Vec<(u64, Compaction)>,
}

/// How [`Db::claude_summary`] groups.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeGroupBy {
    Project,
    Session,
    Model,
}

/// One group of [`Db::claude_summary`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ClaudeSummaryRow {
    pub key: String,
    pub requests: u64,
    pub tokens: Tokens,
    pub cost_usd: f64,
    pub unpriced_requests: u64,
    pub unpriced_models: Vec<String>,
}

/// The data-quality counters of a session's transcripts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct ClaudeQuality {
    pub files: u64,
    pub lines: u64,
    pub bad_lines: u64,
    pub unknown_lines: u64,
}

/// The newest main-thread request of a session plus its totals ([`Db::claude_live`]).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ClaudeLive {
    pub csm_session: String,
    pub last_main: Option<UsageRequest>,
    pub largest_main_prompt: u64,
    pub requests: u64,
    pub cost_usd: f64,
    pub unpriced_requests: u64,
}

fn u(v: i64) -> u64 {
    u64::try_from(v).unwrap_or(0)
}

fn i(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

const REQ_COLS: &str = "id, claude_sid, agent_id, ts, model, price_key, fast, input, output, cache_read, cache_w5, cache_w1h, cost";

fn request_of(r: &Row<'_>) -> rusqlite::Result<UsageRequest> {
    Ok(UsageRequest {
        id: r.get(0)?,
        claude_sid: r.get(1)?,
        agent_id: r.get(2)?,
        ts_ms: r.get(3)?,
        model: r.get(4)?,
        price_key: r.get(5)?,
        fast: r.get::<_, i64>(6)? != 0,
        tokens: Tokens {
            input: u(r.get(7)?),
            output: u(r.get(8)?),
            cache_read: u(r.get(9)?),
            cache_write_5m: u(r.get(10)?),
            cache_write_1h: u(r.get(11)?),
        },
        cost_usd: r.get(12)?,
    })
}

impl Db {
    /// The stored read position of a transcript.
    pub fn claude_file(&self, path: &str) -> Result<Option<ClaudeFile>, IndexError> {
        Ok(self
            .conn
            .query_row(
                "SELECT path, claude_sid, agent_id, csm_session, project, size, mtime_ms, offset, head, lines, bad_lines, unknown_lines
                 FROM claude_files WHERE path = ?1",
                [path],
                |r| {
                    Ok(ClaudeFile {
                        path: r.get(0)?,
                        claude_sid: r.get(1)?,
                        agent_id: r.get(2)?,
                        csm_session: r.get(3)?,
                        project: r.get(4)?,
                        size: u(r.get(5)?),
                        mtime_ms: r.get(6)?,
                        offset: u(r.get(7)?),
                        head: r.get::<_, Option<Vec<u8>>>(8)?.unwrap_or_default(),
                        lines: u(r.get(9)?),
                        bad_lines: u(r.get(10)?),
                        unknown_lines: u(r.get(11)?),
                    })
                },
            )
            .optional()?)
    }

    /// Stores one pass. A request already stored (the same `message.id`, from an earlier pass or
    /// another file) keeps the maximum of each token field and is priced again with `price`;
    /// it keeps the session, file and project it was first stored with.
    pub fn claude_ingest(&mut self, ing: &ClaudeIngest, price: &dyn Fn(&UsageRequest) -> (Option<String>, Option<f64>)) -> Result<(), IndexError> {
        let tx = self.conn.transaction()?;
        let f = &ing.file;
        if ing.reset {
            tx.execute("DELETE FROM claude_requests WHERE file = ?1", [&f.path])?;
            tx.execute("DELETE FROM claude_compactions WHERE file = ?1", [&f.path])?;
        }
        {
            let mut get = tx.prepare_cached(&format!("SELECT {REQ_COLS} FROM claude_requests WHERE id = ?1"))?;
            let mut ins = tx.prepare_cached(
                "INSERT INTO claude_requests (id, claude_sid, agent_id, csm_session, project, ts, model, price_key, fast, input, output, cache_read, cache_w5, cache_w1h, cost, file)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            )?;
            let mut upd = tx.prepare_cached(
                "UPDATE claude_requests SET input = ?2, output = ?3, cache_read = ?4, cache_w5 = ?5, cache_w1h = ?6, fast = ?7, price_key = ?8, cost = ?9,
                 ts = COALESCE(ts, ?10) WHERE id = ?1",
            )?;
            for r in &ing.requests {
                let old = get.query_row([&r.id], request_of).optional()?;
                match old {
                    Some(mut o) => {
                        o.tokens.max_with(&r.tokens);
                        o.fast |= r.fast;
                        let (key, cost) = price(&o);
                        let t = o.tokens;
                        upd.execute(params![
                            o.id,
                            i(t.input),
                            i(t.output),
                            i(t.cache_read),
                            i(t.cache_write_5m),
                            i(t.cache_write_1h),
                            o.fast,
                            key,
                            cost,
                            r.ts_ms
                        ])?;
                    }
                    None => {
                        let (key, cost) = price(r);
                        let t = r.tokens;
                        ins.execute(params![
                            r.id,
                            r.claude_sid,
                            r.agent_id,
                            f.csm_session,
                            f.project,
                            r.ts_ms,
                            r.model,
                            key,
                            r.fast,
                            i(t.input),
                            i(t.output),
                            i(t.cache_read),
                            i(t.cache_write_5m),
                            i(t.cache_write_1h),
                            cost,
                            f.path
                        ])?;
                    }
                }
            }
            let mut comp = tx.prepare_cached(
                "INSERT OR REPLACE INTO claude_compactions (file, line, claude_sid, agent_id, csm_session, ts, trigger, pre_tokens, post_tokens)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for (line, c) in &ing.compactions {
                comp.execute(params![
                    f.path,
                    i(*line),
                    f.claude_sid,
                    c.agent_id.as_ref().or(f.agent_id.as_ref()),
                    f.csm_session,
                    c.ts_ms,
                    c.trigger,
                    c.pre_tokens.map(i),
                    c.post_tokens.map(i)
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO claude_files (path, claude_sid, agent_id, csm_session, project, size, mtime_ms, offset, head, lines, bad_lines, unknown_lines)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(path) DO UPDATE SET claude_sid = excluded.claude_sid, agent_id = excluded.agent_id, csm_session = excluded.csm_session,
               project = excluded.project, size = excluded.size, mtime_ms = excluded.mtime_ms, offset = excluded.offset, head = excluded.head,
               lines = excluded.lines, bad_lines = excluded.bad_lines, unknown_lines = excluded.unknown_lines",
            params![
                f.path,
                f.claude_sid,
                f.agent_id,
                f.csm_session,
                f.project,
                i(f.size),
                f.mtime_ms,
                i(f.offset),
                f.head,
                i(f.lines),
                i(f.bad_lines),
                i(f.unknown_lines)
            ],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every request stored for a CSM session (main thread and subagents), oldest first.
    pub fn claude_requests_for(&self, csm_session: &str) -> Result<Vec<UsageRequest>, IndexError> {
        let mut st = self.conn.prepare(&format!("SELECT {REQ_COLS} FROM claude_requests WHERE csm_session = ?1 ORDER BY ts, id"))?;
        let rows = st.query_map([csm_session], request_of)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The compactions of a CSM session's transcripts, oldest first.
    pub fn claude_compactions_for(&self, csm_session: &str) -> Result<Vec<Compaction>, IndexError> {
        let mut st = self.conn.prepare(
            "SELECT ts, agent_id, trigger, pre_tokens, post_tokens FROM claude_compactions WHERE csm_session = ?1 ORDER BY ts, file, line",
        )?;
        let rows = st.query_map([csm_session], |r| {
            Ok(Compaction {
                ts_ms: r.get(0)?,
                agent_id: r.get(1)?,
                trigger: r.get(2)?,
                pre_tokens: r.get::<_, Option<i64>>(3)?.map(u),
                post_tokens: r.get::<_, Option<i64>>(4)?.map(u),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// The data-quality counters of a CSM session's transcripts.
    pub fn claude_quality_for(&self, csm_session: &str) -> Result<ClaudeQuality, IndexError> {
        Ok(self.conn.query_row(
            "SELECT COUNT(*), COALESCE(SUM(lines), 0), COALESCE(SUM(bad_lines), 0), COALESCE(SUM(unknown_lines), 0) FROM claude_files WHERE csm_session = ?1",
            [csm_session],
            |r| Ok(ClaudeQuality { files: u(r.get(0)?), lines: u(r.get(1)?), bad_lines: u(r.get(2)?), unknown_lines: u(r.get(3)?) }),
        )?)
    }

    /// For each session: its newest main-thread request, the largest main-thread prompt (a 1M
    /// window tell), and its totals. Sessions without requests are left out.
    pub fn claude_live(&self, csm_sessions: &[String]) -> Result<Vec<ClaudeLive>, IndexError> {
        let mut last = self.conn.prepare_cached(&format!(
            "SELECT {REQ_COLS} FROM claude_requests WHERE csm_session = ?1 AND agent_id IS NULL ORDER BY ts DESC, id DESC LIMIT 1"
        ))?;
        let mut sums = self.conn.prepare_cached(
            "SELECT COUNT(*), COALESCE(SUM(cost), 0), SUM(cost IS NULL),
                    COALESCE(MAX(CASE WHEN agent_id IS NULL THEN input + cache_read + cache_w5 + cache_w1h END), 0)
             FROM claude_requests WHERE csm_session = ?1",
        )?;
        let mut out = Vec::new();
        for s in csm_sessions {
            let (n, cost, unpriced, largest): (i64, f64, Option<i64>, i64) = sums.query_row([s], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            if n == 0 {
                continue;
            }
            out.push(ClaudeLive {
                csm_session: s.clone(),
                last_main: last.query_row([s], request_of).optional()?,
                largest_main_prompt: u(largest),
                requests: u(n),
                cost_usd: cost,
                unpriced_requests: u(unpriced.unwrap_or(0)),
            });
        }
        Ok(out)
    }

    /// Requests with `from_ms <= ts < to_ms`, grouped, most expensive first.
    pub fn claude_summary(&self, from_ms: i64, to_ms: i64, by: ClaudeGroupBy) -> Result<Vec<ClaudeSummaryRow>, IndexError> {
        let key = match by {
            ClaudeGroupBy::Project => "COALESCE(project, '')",
            ClaudeGroupBy::Session => "COALESCE(csm_session, claude_sid)",
            ClaudeGroupBy::Model => "COALESCE(price_key, model)",
        };
        let mut st = self.conn.prepare(&format!(
            "SELECT {key} AS k, COUNT(*), SUM(input), SUM(output), SUM(cache_read), SUM(cache_w5), SUM(cache_w1h), COALESCE(SUM(cost), 0),
                    SUM(cost IS NULL), GROUP_CONCAT(DISTINCT CASE WHEN cost IS NULL THEN model END)
             FROM claude_requests WHERE ts >= ?1 AND ts < ?2 GROUP BY k ORDER BY COALESCE(SUM(cost), 0) DESC, k"
        ))?;
        let rows = st.query_map(params![from_ms, to_ms], |r| {
            let models: Option<String> = r.get(9)?;
            Ok(ClaudeSummaryRow {
                key: r.get(0)?,
                requests: u(r.get(1)?),
                tokens: Tokens {
                    input: u(r.get::<_, Option<i64>>(2)?.unwrap_or(0)),
                    output: u(r.get::<_, Option<i64>>(3)?.unwrap_or(0)),
                    cache_read: u(r.get::<_, Option<i64>>(4)?.unwrap_or(0)),
                    cache_write_5m: u(r.get::<_, Option<i64>>(5)?.unwrap_or(0)),
                    cache_write_1h: u(r.get::<_, Option<i64>>(6)?.unwrap_or(0)),
                },
                cost_usd: r.get(7)?,
                unpriced_requests: u(r.get::<_, Option<i64>>(8)?.unwrap_or(0)),
                unpriced_models: models.map(|m| m.split(',').map(str::to_owned).collect()).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn claude_meta(&self, key: &str) -> Result<Option<String>, IndexError> {
        Ok(self.conn.query_row("SELECT value FROM claude_meta WHERE key = ?1", [key], |r| r.get(0)).optional()?)
    }

    pub fn claude_set_meta(&mut self, key: &str, value: &str) -> Result<(), IndexError> {
        self.conn.execute("INSERT INTO claude_meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value])?;
        Ok(())
    }

    /// Prices every stored request again (the price table changed). Returns how many.
    pub fn claude_reprice(&mut self, price: &dyn Fn(&UsageRequest) -> (Option<String>, Option<f64>)) -> Result<usize, IndexError> {
        let tx = self.conn.transaction()?;
        let rows: Vec<UsageRequest> = {
            let mut st = tx.prepare(&format!("SELECT {REQ_COLS} FROM claude_requests"))?;
            st.query_map([], request_of)?.collect::<Result<Vec<_>, _>>()?
        };
        {
            let mut upd = tx.prepare("UPDATE claude_requests SET price_key = ?2, cost = ?3 WHERE id = ?1")?;
            for r in &rows {
                let (key, cost) = price(r);
                upd.execute(params![r.id, key, cost])?;
            }
        }
        tx.commit()?;
        Ok(rows.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Db {
        Db::open(None).unwrap()
    }

    fn req(id: &str, ts: i64, agent: Option<&str>, input: u64, model: &str) -> UsageRequest {
        UsageRequest {
            id: id.into(),
            claude_sid: "c1".into(),
            agent_id: agent.map(Into::into),
            ts_ms: Some(ts),
            model: model.into(),
            tokens: Tokens { input, output: 10, ..Tokens::default() },
            ..UsageRequest::default()
        }
    }

    /// $1 per input token for `claude-*`, unpriced otherwise.
    fn price(r: &UsageRequest) -> (Option<String>, Option<f64>) {
        if r.model.starts_with("claude") { (Some(r.model.clone()), Some(r.tokens.input as f64)) } else { (None, None) }
    }

    fn file(path: &str, session: &str, offset: u64) -> ClaudeFile {
        ClaudeFile {
            path: path.into(),
            claude_sid: "c1".into(),
            csm_session: Some(session.into()),
            project: Some("umage/app".into()),
            offset,
            head: b"{\"type\"".to_vec(),
            lines: 3,
            bad_lines: 1,
            ..ClaudeFile::default()
        }
    }

    #[test]
    fn ingest_stores_numbers_and_the_read_position() {
        let mut db = mem();
        let ing = ClaudeIngest {
            file: file("/t/c1.jsonl", "s1", 300),
            requests: vec![req("m1", 1000, None, 5, "claude-x"), req("m2", 2000, Some("a1"), 7, "qwen")],
            compactions: vec![(3, Compaction { ts_ms: Some(1500), pre_tokens: Some(100), post_tokens: Some(10), ..Compaction::default() })],
            reset: false,
        };
        db.claude_ingest(&ing, &price).unwrap();
        let f = db.claude_file("/t/c1.jsonl").unwrap().unwrap();
        assert_eq!((f.offset, f.lines, f.bad_lines, f.head.as_slice()), (300, 3, 1, b"{\"type\"".as_slice()));
        let rows = db.claude_requests_for("s1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].cost_usd, Some(5.0));
        assert_eq!(rows[1].cost_usd, None, "unpriced stays NULL, never 0");
        assert_eq!(rows[1].agent_id.as_deref(), Some("a1"));
        assert_eq!(db.claude_compactions_for("s1").unwrap().len(), 1);
        assert_eq!(db.claude_quality_for("s1").unwrap(), ClaudeQuality { files: 1, lines: 3, bad_lines: 1, unknown_lines: 0 });
        assert_eq!(db.claude_file("/nope").unwrap(), None);
    }

    #[test]
    fn a_request_seen_again_keeps_the_max_per_field_and_is_repriced() {
        let mut db = mem();
        let mut ing = ClaudeIngest { file: file("/t/c1.jsonl", "s1", 10), requests: vec![req("m1", 1000, None, 5, "claude-x")], ..ClaudeIngest::default() };
        db.claude_ingest(&ing, &price).unwrap();
        let mut again = req("m1", 1000, None, 3, "claude-x");
        again.tokens.output = 99;
        ing.requests = vec![again];
        // From a forked copy in another file, linked to another session: still one request.
        ing.file = file("/t/fork.jsonl", "s2", 10);
        db.claude_ingest(&ing, &price).unwrap();
        let rows = db.claude_requests_for("s1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!((rows[0].tokens.input, rows[0].tokens.output, rows[0].cost_usd), (5, 99, Some(5.0)));
        assert!(db.claude_requests_for("s2").unwrap().is_empty(), "counted once, where it was first seen");
    }

    #[test]
    fn a_reset_forgets_what_the_file_stored() {
        let mut db = mem();
        let ing = ClaudeIngest {
            file: file("/t/c1.jsonl", "s1", 10),
            requests: vec![req("m1", 1000, None, 5, "claude-x")],
            compactions: vec![(1, Compaction::default())],
            reset: false,
        };
        db.claude_ingest(&ing, &price).unwrap();
        let again = ClaudeIngest { file: file("/t/c1.jsonl", "s1", 5), requests: vec![req("m9", 1000, None, 1, "claude-x")], compactions: vec![], reset: true };
        db.claude_ingest(&again, &price).unwrap();
        let ids: Vec<String> = db.claude_requests_for("s1").unwrap().into_iter().map(|r| r.id).collect();
        assert_eq!(ids, ["m9"]);
        assert!(db.claude_compactions_for("s1").unwrap().is_empty());
    }

    #[test]
    fn live_and_summary_and_reprice() {
        let mut db = mem();
        let mut a = file("/t/a.jsonl", "s1", 1);
        a.project = Some("p/one".into());
        db.claude_ingest(
            &ClaudeIngest { file: a, requests: vec![req("m1", 1000, None, 5, "claude-x"), req("m2", 3000, None, 7, "claude-x"), req("m3", 2000, Some("a"), 50, "qwen")], ..ClaudeIngest::default() },
            &price,
        )
        .unwrap();
        let mut b = file("/t/b.jsonl", "s2", 1);
        b.project = Some("p/two".into());
        db.claude_ingest(&ClaudeIngest { file: b, requests: vec![req("n1", 5000, None, 100, "claude-y")], ..ClaudeIngest::default() }, &price).unwrap();

        let live = db.claude_live(&["s1".into(), "s2".into(), "nobody".into()]).unwrap();
        assert_eq!(live.len(), 2);
        assert_eq!(live[0].last_main.as_ref().unwrap().id, "m2", "the newest main-thread request");
        assert_eq!((live[0].requests, live[0].cost_usd, live[0].unpriced_requests, live[0].largest_main_prompt), (3, 12.0, 1, 7));

        let by_project = db.claude_summary(0, 10_000, ClaudeGroupBy::Project).unwrap();
        assert_eq!(by_project.iter().map(|r| (r.key.as_str(), r.cost_usd)).collect::<Vec<_>>(), [("p/two", 100.0), ("p/one", 12.0)]);
        assert_eq!(by_project[1].unpriced_models, ["qwen"]);
        let window = db.claude_summary(1500, 4000, ClaudeGroupBy::Session).unwrap();
        assert_eq!(window.len(), 1);
        assert_eq!((window[0].key.as_str(), window[0].requests), ("s1", 2));
        let models = db.claude_summary(0, 10_000, ClaudeGroupBy::Model).unwrap();
        assert!(models.iter().any(|r| r.key == "qwen" && r.unpriced_requests == 1));

        // A new table prices qwen too.
        let n = db.claude_reprice(&|r| (Some(r.model.clone()), Some(1.0))).unwrap();
        assert_eq!(n, 4);
        assert_eq!(db.claude_live(&["s1".into()]).unwrap()[0].unpriced_requests, 0);
        db.claude_set_meta("price_sig", "abc").unwrap();
        assert_eq!(db.claude_meta("price_sig").unwrap().as_deref(), Some("abc"));
        assert_eq!(db.claude_meta("none").unwrap(), None);
    }

    #[test]
    fn a_version_2_file_gains_the_tables_and_keeps_its_data() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("i.db");
        {
            let c = rusqlite::Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE project_notes (folder_path TEXT PRIMARY KEY, content TEXT NOT NULL, updated_at INTEGER NOT NULL);
                 INSERT INTO project_notes VALUES ('c:\\x', 'kept', 1);",
            )
            .unwrap();
            c.execute_batch(crate::usage::V2).unwrap();
            c.pragma_update(None, "user_version", 2).unwrap();
        }
        let mut db = Db::open(Some(&p)).unwrap();
        assert_eq!(db.schema_version(), 3);
        db.claude_ingest(&ClaudeIngest { file: file("/t/a.jsonl", "s1", 1), ..ClaudeIngest::default() }, &price).unwrap();
        let notes: i64 = db.conn.query_row("SELECT COUNT(*) FROM project_notes", [], |r| r.get(0)).unwrap();
        assert_eq!(notes, 1);
    }
}
