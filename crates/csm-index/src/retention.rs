//! Keeping the index from growing without end.
//!
//! The output of a session is capped per session ([`crate::IndexOptions::max_bytes_per_session`]),
//! but before this every session ever created kept its output (and its search index) for good.
//! [`Db::prune`] removes what nothing needs any more:
//!
//! - **Gone sessions.** A session that is not kept (the engine passes the ids it still has, open
//!   or in Recently closed) and has not been seen for [`RetentionPolicy::closed_grace_ms`] loses its
//!   output, its `sessions` row and its `session_history` row.
//! - **A total cap.** While the output of all sessions together is over
//!   [`RetentionPolicy::max_output_bytes`], the oldest lines go first, whichever session they
//!   belong to.
//! - **Claude usage rows** older than [`RetentionPolicy::claude_before_ms`]. The read positions in
//!   `claude_files` stay, so a transcript is not read again and the rows do not come back.
//!
//! Then the file is given back to the disk: the database uses `auto_vacuum = INCREMENTAL`, and
//! the freed pages are released. A database made before that setting existed is converted once
//! with a `VACUUM`, which runs after the pruning, so it rewrites at most the capped content.
//! Everything here runs on the index thread, never on the UI's.

use std::collections::HashSet;

use rusqlite::params;

use crate::{Db, IndexError};

/// What [`Db::prune`] may remove.
#[derive(Debug, Clone)]
pub struct RetentionPolicy {
    /// Sessions whose data stays whatever their age: the engine's sessions and Recently closed.
    pub keep_sessions: HashSet<String>,
    /// A session not kept goes once it has not been seen for this long.
    pub closed_grace_ms: i64,
    /// The output of all sessions together is kept under this many bytes (oldest lines go first).
    pub max_output_bytes: i64,
    /// Claude usage rows older than this go; `None` keeps them all.
    pub claude_before_ms: Option<i64>,
}

/// What [`Db::prune`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct PruneReport {
    pub sessions_dropped: usize,
    pub output_rows_deleted: usize,
    pub output_bytes_deleted: i64,
    pub claude_rows_deleted: usize,
    /// Bytes given back to the disk by the vacuum.
    pub file_bytes_freed: i64,
    /// The database was converted to incremental vacuum (a one-time `VACUUM`).
    pub converted: bool,
}

/// The sizes [`Db::storage`] reports.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct StorageInfo {
    pub file_bytes: i64,
    pub free_bytes: i64,
    pub output_bytes: i64,
    pub output_rows: i64,
    pub output_sessions: i64,
    pub claude_rows: i64,
    pub status_rows: i64,
}

/// Rows deleted per statement while trimming to the total cap (keeps each step short).
const CAP_CHUNK_ROWS: i64 = 5_000;

impl Db {
    /// Sizes of the index's parts.
    pub fn storage(&self) -> Result<StorageInfo, IndexError> {
        let one = |sql: &str| -> i64 { self.conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
        let page = one("PRAGMA page_size");
        Ok(StorageInfo {
            file_bytes: one("PRAGMA page_count") * page,
            free_bytes: one("PRAGMA freelist_count") * page,
            output_bytes: one("SELECT COALESCE(SUM(length(text)), 0) FROM output"),
            output_rows: one("SELECT COUNT(*) FROM output"),
            output_sessions: one("SELECT COUNT(DISTINCT session_id) FROM output"),
            claude_rows: one("SELECT COUNT(*) FROM claude_requests"),
            status_rows: one("SELECT COUNT(*) FROM status_log"),
        })
    }

    /// Removes what the policy says nothing needs, then gives the space back (module docs).
    pub fn prune(&mut self, policy: &RetentionPolicy, now_ms: i64) -> Result<PruneReport, IndexError> {
        let mut report = PruneReport::default();
        let before = self.storage()?.file_bytes;
        let cutoff = now_ms.saturating_sub(policy.closed_grace_ms.max(0));

        let tx = self.conn.transaction()?;
        // Every session the index knows, with the last time it was seen anywhere.
        let known: Vec<(String, i64)> = {
            let mut st = tx.prepare(
                "SELECT id, MAX(seen) FROM (
                     SELECT session_id AS id, last_seen AS seen FROM sessions
                     UNION ALL SELECT session_id, MAX(ts) FROM output GROUP BY session_id
                     UNION ALL SELECT session_id, updated_at FROM session_history
                 ) GROUP BY id",
            )?;
            let rows = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0))))?;
            rows.filter_map(Result::ok).collect()
        };
        for (id, seen) in known {
            if policy.keep_sessions.contains(&id) || seen >= cutoff {
                continue;
            }
            let (rows, bytes): (i64, i64) =
                tx.query_row("SELECT COUNT(*), COALESCE(SUM(length(text)), 0) FROM output WHERE session_id = ?1", [&id], |r| Ok((r.get(0)?, r.get(1)?)))?;
            tx.execute("DELETE FROM output WHERE session_id = ?1", [&id])?;
            tx.execute("DELETE FROM sessions WHERE session_id = ?1", [&id])?;
            tx.execute("DELETE FROM session_history WHERE session_id = ?1", [&id])?;
            report.sessions_dropped += 1;
            report.output_rows_deleted += usize::try_from(rows).unwrap_or(0);
            report.output_bytes_deleted += bytes;
        }

        // The total cap: the oldest lines go first, in short steps.
        if policy.max_output_bytes > 0 {
            let mut total: i64 = tx.query_row("SELECT COALESCE(SUM(length(text)), 0) FROM output", [], |r| r.get(0))?;
            while total > policy.max_output_bytes {
                // The oldest rows, just enough of them (at most a chunk per step).
                let excess = total - policy.max_output_bytes;
                let (mut last_id, mut rows, mut bytes) = (0i64, 0i64, 0i64);
                {
                    let mut st = tx.prepare("SELECT id, length(text) FROM output ORDER BY id ASC LIMIT ?1")?;
                    let mut it = st.query([CAP_CHUNK_ROWS])?;
                    while let Some(r) = it.next()? {
                        last_id = r.get(0)?;
                        rows += 1;
                        bytes += r.get::<_, i64>(1)?;
                        if bytes >= excess {
                            break;
                        }
                    }
                }
                if rows == 0 {
                    break;
                }
                tx.execute("DELETE FROM output WHERE id <= ?1", params![last_id])?;
                total -= bytes;
                report.output_rows_deleted += usize::try_from(rows).unwrap_or(0);
                report.output_bytes_deleted += bytes;
            }
        }

        // Old Claude usage rows (their files' read positions stay, so they are not read again).
        if let Some(before_ms) = policy.claude_before_ms {
            report.claude_rows_deleted = tx.execute("DELETE FROM claude_requests WHERE ts IS NOT NULL AND ts < ?1", [before_ms])?;
            tx.execute("DELETE FROM claude_compactions WHERE ts IS NOT NULL AND ts < ?1", [before_ms])?;
        }
        tx.commit()?;

        report.converted = self.ensure_incremental_vacuum()?;
        if !report.converted {
            self.conn.execute_batch("PRAGMA incremental_vacuum;")?;
        }
        report.file_bytes_freed = (before - self.storage()?.file_bytes).max(0);
        Ok(report)
    }

    /// Turns on incremental vacuum for a database made without it (one `VACUUM`, which rewrites
    /// the file); returns whether it converted. A new database has it from its first table.
    fn ensure_incremental_vacuum(&mut self) -> Result<bool, IndexError> {
        let mode: i64 = self.conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0))?;
        if mode == 2 {
            return Ok(false);
        }
        self.conn.execute_batch("PRAGMA auto_vacuum = INCREMENTAL; VACUUM;")?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: i64 = 86_400_000;
    const NOW: i64 = 1_800_000_000_000;

    fn policy(keep: &[&str], max: i64) -> RetentionPolicy {
        RetentionPolicy {
            keep_sessions: keep.iter().map(|s| (*s).to_owned()).collect(),
            closed_grace_ms: 30 * DAY,
            max_output_bytes: max,
            claude_before_ms: None,
        }
    }

    fn feed(db: &mut Db, sid: &str, ts: i64, n: usize) {
        // One batch (one row) per line, as a busy session's flushes are.
        for i in 0..n {
            db.insert_output(sid, ts, &[format!("{sid} line {i} {}", "x".repeat(80))], 1 << 30).unwrap();
        }
    }

    fn output_of(db: &Db, sid: &str) -> i64 {
        db.conn.query_row("SELECT COUNT(*) FROM output WHERE session_id = ?1", [sid], |r| r.get(0)).unwrap()
    }

    #[test]
    fn gone_sessions_lose_their_output_after_the_grace_period_kept_ones_never() {
        let mut db = Db::open(None).unwrap();
        feed(&mut db, "open-old", NOW - 90 * DAY, 10);
        feed(&mut db, "closed-old", NOW - 90 * DAY, 10);
        feed(&mut db, "closed-recent", NOW - 2 * DAY, 10);
        db.save_history("closed-old", "x", "C:\\x", "{}", NOW - 90 * DAY).unwrap();
        let r = db.prune(&policy(&["open-old"], 0), NOW).unwrap();
        assert_eq!(r.sessions_dropped, 1, "{r:?}");
        assert_eq!(output_of(&db, "closed-old"), 0);
        assert_eq!(output_of(&db, "open-old"), 10, "a kept session keeps its output whatever its age");
        assert_eq!(output_of(&db, "closed-recent"), 10, "a recently seen one stays searchable for the grace period");
        let hist: i64 = db.conn.query_row("SELECT COUNT(*) FROM session_history WHERE session_id = 'closed-old'", [], |r| r.get(0)).unwrap();
        assert_eq!(hist, 0, "its history row goes too");
        // The search index follows (its delete trigger).
        assert!(db.search_output("\"closed-old\"", 5, None).unwrap().is_empty());
        assert!(!db.search_output("\"open-old\"", 5, None).unwrap().is_empty());
    }

    #[test]
    fn the_total_cap_removes_the_oldest_lines_first() {
        let mut db = Db::open(None).unwrap();
        feed(&mut db, "a", NOW - DAY, 200);
        feed(&mut db, "b", NOW, 200);
        let total = db.storage().unwrap().output_bytes;
        let r = db.prune(&policy(&["a", "b"], total / 2), NOW).unwrap();
        assert!(r.output_bytes_deleted >= total / 2, "{r:?}");
        assert!(db.storage().unwrap().output_bytes <= total / 2);
        assert!(output_of(&db, "a") < output_of(&db, "b"), "the older session's lines went first");
        assert_eq!(output_of(&db, "b"), 200, "the newest lines are all kept");
    }

    #[test]
    fn old_claude_rows_go_and_the_file_is_vacuumed() {
        let mut db = Db::open(None).unwrap();
        for (i, ts) in [NOW - 500 * DAY, NOW - 10 * DAY].into_iter().enumerate() {
            db.conn
                .execute("INSERT INTO claude_requests(id, claude_sid, model, ts) VALUES (?1, 's', 'm', ?2)", params![format!("r{i}"), ts])
                .unwrap();
        }
        let mut p = policy(&[], 0);
        p.claude_before_ms = Some(NOW - 400 * DAY);
        let r = db.prune(&p, NOW).unwrap();
        assert_eq!(r.claude_rows_deleted, 1);
        assert_eq!(db.storage().unwrap().claude_rows, 1);
        let mode: i64 = db.conn.query_row("PRAGMA auto_vacuum", [], |r| r.get(0)).unwrap();
        assert_eq!(mode, 2, "incremental vacuum is on after the first prune");
        // A second prune does not convert again.
        assert!(!db.prune(&p, NOW).unwrap().converted);
    }

    #[test]
    fn pruning_an_empty_or_odd_index_is_harmless() {
        let mut db = Db::open(None).unwrap();
        assert_eq!(db.prune(&policy(&[], 1), NOW).unwrap().sessions_dropped, 0);
        // A negative grace or a zero cap changes nothing it should not.
        feed(&mut db, "a", NOW, 3);
        let mut p = policy(&["a"], 0);
        p.closed_grace_ms = -5;
        db.prune(&p, NOW).unwrap();
        assert_eq!(output_of(&db, "a"), 3);
    }
}
