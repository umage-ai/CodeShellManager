//! The status log: when each session entered each status (`working`, `needs_you_question`,
//! `idle`, …), for a later timeline view of the Board.
//!
//! One row per span: `(session_id, status, start_ms)`; a span ends where the session's next row
//! starts. A status that lasted less than [`FLICKER_MS`] is merged away (the next status takes
//! over its start, and a bounce back to the status before it continues that span), so a status
//! that flickers while a program redraws does not fill the table. Rows older than
//! [`RETENTION_MS`] are deleted each time the index opens.

use rusqlite::{OptionalExtension, params};
use serde::Serialize;

use crate::db::{Db, IndexError};

/// Schema version 4: the status log.
pub(crate) const V4: &str = "
CREATE TABLE IF NOT EXISTS status_log (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    status TEXT NOT NULL,
    start_ms INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS status_log_session ON status_log(session_id, start_ms);
CREATE INDEX IF NOT EXISTS status_log_start ON status_log(start_ms);
";

/// A status shorter than this is a flicker and is merged into the next one.
pub const FLICKER_MS: i64 = 2_000;
/// How long rows are kept: 30 days.
pub const RETENTION_MS: i64 = 30 * 24 * 3_600_000;
/// Longest session id or status stored (both come from the engine, but are capped anyway).
const MAX_FIELD: usize = 128;

/// One span of one status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StatusSpan {
    pub session_id: String,
    pub status: String,
    pub start_ms: i64,
}

fn field(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(MAX_FIELD).collect()
}

impl Db {
    /// Records that `session_id` entered `status` at `ts` (see the module docs for the merging).
    pub fn status_log_record(&mut self, session_id: &str, status: &str, ts: i64) -> Result<(), IndexError> {
        let (sid, status) = (field(session_id), field(status));
        let tx = self.conn.transaction()?;
        let last: Option<(i64, String, i64)> = tx
            .query_row(
                "SELECT id, status, start_ms FROM status_log WHERE session_id = ?1 ORDER BY start_ms DESC, id DESC LIMIT 1",
                [&sid],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        match last {
            Some((_, s, _)) if s == status => {}
            Some((id, _, start)) if ts.saturating_sub(start) < FLICKER_MS => {
                // The last status was a flicker: this one takes over its start...
                tx.execute("UPDATE status_log SET status = ?1 WHERE id = ?2", params![status, id])?;
                // ...and when that returns to the status before it, that span simply goes on.
                let before: Option<String> = tx
                    .query_row(
                        "SELECT status FROM status_log WHERE session_id = ?1 AND id <> ?2 AND start_ms <= ?3 ORDER BY start_ms DESC, id DESC LIMIT 1",
                        params![sid, id, start],
                        |r| r.get(0),
                    )
                    .optional()?;
                if before.as_deref() == Some(status.as_str()) {
                    tx.execute("DELETE FROM status_log WHERE id = ?1", [id])?;
                }
            }
            _ => {
                tx.execute("INSERT INTO status_log (session_id, status, start_ms) VALUES (?1, ?2, ?3)", params![sid, status, ts])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Deletes rows that started before `before_ms`; returns how many.
    pub fn status_log_prune(&mut self, before_ms: i64) -> Result<usize, IndexError> {
        Ok(self.conn.execute("DELETE FROM status_log WHERE start_ms < ?1", [before_ms])?)
    }

    /// Spans since `since_ms`, oldest first; one session's, or every session's.
    pub fn status_log(&self, session_id: Option<&str>, since_ms: i64) -> Result<Vec<StatusSpan>, IndexError> {
        let map = |r: &rusqlite::Row<'_>| Ok(StatusSpan { session_id: r.get(0)?, status: r.get(1)?, start_ms: r.get(2)? });
        let rows = match session_id {
            Some(sid) => {
                let mut st = self.conn.prepare(
                    "SELECT session_id, status, start_ms FROM status_log WHERE session_id = ?1 AND start_ms >= ?2 ORDER BY start_ms, id",
                )?;
                st.query_map(params![sid, since_ms], map)?.collect::<Result<Vec<_>, _>>()?
            }
            None => {
                let mut st = self.conn.prepare("SELECT session_id, status, start_ms FROM status_log WHERE start_ms >= ?1 ORDER BY start_ms, id")?;
                st.query_map([since_ms], map)?.collect::<Result<Vec<_>, _>>()?
            }
        };
        Ok(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: i64 = 1_800_000_000_000;

    fn spans(db: &Db, sid: &str) -> Vec<(String, i64)> {
        db.status_log(Some(sid), 0).unwrap().into_iter().map(|s| (s.status, s.start_ms)).collect()
    }

    fn v(list: &[(&str, i64)]) -> Vec<(String, i64)> {
        list.iter().map(|(s, t)| ((*s).to_owned(), *t)).collect()
    }

    #[test]
    fn transitions_are_recorded_and_repeats_ignored() {
        let mut db = Db::open(None).unwrap();
        db.status_log_record("a", "starting", T).unwrap();
        db.status_log_record("a", "working", T + 5_000).unwrap();
        db.status_log_record("a", "working", T + 9_000).unwrap();
        db.status_log_record("b", "idle", T + 6_000).unwrap();
        db.status_log_record("a", "idle", T + 60_000).unwrap();
        assert_eq!(spans(&db, "a"), v(&[("starting", T), ("working", T + 5_000), ("idle", T + 60_000)]));
        assert_eq!(db.status_log(None, T + 5_000).unwrap().len(), 3, "since filters, all sessions");
    }

    #[test]
    fn flicker_under_two_seconds_is_merged() {
        let mut db = Db::open(None).unwrap();
        db.status_log_record("a", "working", T).unwrap();
        // working → idle for 0.5 s → working: one working span.
        db.status_log_record("a", "idle", T + 10_000).unwrap();
        db.status_log_record("a", "working", T + 10_500).unwrap();
        assert_eq!(spans(&db, "a"), v(&[("working", T)]));
        // working → needs_you for 1 s → done: done takes over the flicker's start.
        db.status_log_record("a", "needs_you_question", T + 20_000).unwrap();
        db.status_log_record("a", "done_unseen", T + 21_000).unwrap();
        assert_eq!(spans(&db, "a"), v(&[("working", T), ("done_unseen", T + 20_000)]));
        // Exactly two seconds is a real span.
        db.status_log_record("a", "idle", T + 23_000).unwrap();
        db.status_log_record("a", "working", T + 25_000).unwrap();
        assert_eq!(spans(&db, "a").len(), 4);
    }

    #[test]
    fn prune_keeps_the_last_thirty_days() {
        let mut db = Db::open(None).unwrap();
        db.status_log_record("a", "working", T - RETENTION_MS - 60_000).unwrap();
        db.status_log_record("a", "idle", T - RETENTION_MS + 1).unwrap();
        assert_eq!(db.status_log_prune(T - RETENTION_MS).unwrap(), 1);
        assert_eq!(spans(&db, "a"), v(&[("idle", T - RETENTION_MS + 1)]));
    }

    #[test]
    fn a_version_3_file_gains_the_log_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("i.db");
        {
            let db = Db::open(Some(&p)).unwrap();
            // Back to what a v3 build wrote: no status log.
            db.conn.execute_batch("DROP TABLE status_log;").unwrap();
            db.conn.pragma_update(None, "user_version", 3).unwrap();
        }
        let mut db = Db::open(Some(&p)).unwrap();
        assert_eq!(db.schema_version(), 4);
        db.status_log_record("a", "idle", T).unwrap();
        assert_eq!(spans(&db, "a"), v(&[("idle", T)]));
    }
}
