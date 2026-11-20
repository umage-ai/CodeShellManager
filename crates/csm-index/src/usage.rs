//! Usage stats (WPF `SearchService` usage_stats): how many sessions each command started, how
//! long they ran, and the last day it was used. Counted locally, per command and UTC day; never
//! sent anywhere.

use rusqlite::params;
use serde::Serialize;

use crate::db::{Db, IndexError};

/// Schema version 2: the usage table (the WPF table's shape).
pub(crate) const V2: &str = "
CREATE TABLE IF NOT EXISTS usage_stats (
    id INTEGER PRIMARY KEY,
    command TEXT NOT NULL,
    date TEXT NOT NULL,
    session_count INTEGER NOT NULL DEFAULT 0,
    total_seconds INTEGER NOT NULL DEFAULT 0,
    UNIQUE(command, date)
);
";

/// One command's totals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UsageStat {
    /// The command's program, lower-cased (`claude`, `pwsh`).
    pub command: String,
    pub sessions: i64,
    pub total_seconds: i64,
    /// The last UTC day (`YYYY-MM-DD`) a session of it started or ran.
    pub last_used: String,
}

/// The key a launch command is counted under (C# `NormalizeCommandName`): its first word,
/// lower-cased; `(unknown)` when there is none. Control characters are dropped and the key is
/// capped, since the command came from a state file.
pub fn usage_command_key(command: &str) -> String {
    let head = command.trim_start().split(' ').next().unwrap_or_default();
    let key: String = head.trim().chars().filter(|c| !c.is_control()).take(120).collect::<String>().to_lowercase();
    if key.is_empty() { "(unknown)".to_owned() } else { key }
}

/// `YYYY-MM-DD` (UTC) of an epoch-ms time.
pub fn utc_day(epoch_ms: i64) -> String {
    let ts = csm_core::time::Timestamp::from_epoch_ms(epoch_ms);
    ts.as_str().get(..10).unwrap_or("1970-01-01").to_owned()
}

impl Db {
    /// Counts one session start of `command` on `day`.
    pub fn usage_record_start(&mut self, command: &str, day: &str) -> Result<(), IndexError> {
        self.conn.execute(
            "INSERT INTO usage_stats (command, date, session_count, total_seconds) VALUES (?1, ?2, 1, 0)
             ON CONFLICT(command, date) DO UPDATE SET session_count = session_count + 1",
            params![usage_command_key(command), day],
        )?;
        Ok(())
    }

    /// Adds `seconds` of running time to `command` on `day`. Nothing for a non-positive value.
    pub fn usage_record_seconds(&mut self, command: &str, day: &str, seconds: i64) -> Result<(), IndexError> {
        if seconds <= 0 {
            return Ok(());
        }
        self.conn.execute(
            "INSERT INTO usage_stats (command, date, session_count, total_seconds) VALUES (?1, ?2, 0, ?3)
             ON CONFLICT(command, date) DO UPDATE SET total_seconds = total_seconds + excluded.total_seconds",
            params![usage_command_key(command), day, seconds],
        )?;
        Ok(())
    }

    /// Totals per command, most sessions first (then by name).
    pub fn usage_stats(&self) -> Result<Vec<UsageStat>, IndexError> {
        let mut st = self.conn.prepare(
            "SELECT command, SUM(session_count), SUM(total_seconds), MAX(date) FROM usage_stats
             GROUP BY command ORDER BY SUM(session_count) DESC, command ASC",
        )?;
        let rows = st.query_map([], |r| {
            Ok(UsageStat {
                command: r.get(0)?,
                sessions: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                total_seconds: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                last_used: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Db {
        Db::open(None).unwrap()
    }

    #[test]
    fn command_keys_follow_the_wpf_rule() {
        for (input, want) in [
            ("claude --continue", "claude"),
            ("CLAUDE", "claude"),
            ("pwsh", "pwsh"),
            ("pwsh -NoLogo", "pwsh"),
            ("", "(unknown)"),
            ("   ", "(unknown)"),
            ("  codex  --full-auto", "codex"),
            ("evil\u{1b}[2J", "evil[2j"),
        ] {
            assert_eq!(usage_command_key(input), want, "{input:?}");
        }
    }

    #[test]
    fn starts_are_counted_per_command() {
        let mut db = mem();
        db.usage_record_start("claude", "2026-11-19").unwrap();
        db.usage_record_start("claude --resume abc", "2026-11-19").unwrap();
        db.usage_record_start("pwsh", "2026-11-19").unwrap();
        let s = db.usage_stats().unwrap();
        assert_eq!(s.iter().find(|x| x.command == "claude").unwrap().sessions, 2);
        assert_eq!(s.iter().find(|x| x.command == "pwsh").unwrap().sessions, 1);
    }

    #[test]
    fn durations_accumulate_and_non_positive_ones_are_ignored() {
        let mut db = mem();
        db.usage_record_start("claude", "2026-11-19").unwrap();
        db.usage_record_seconds("claude", "2026-11-19", 120).unwrap();
        db.usage_record_seconds("claude", "2026-11-20", 60).unwrap();
        db.usage_record_seconds("claude", "2026-11-20", 0).unwrap();
        db.usage_record_seconds("claude", "2026-11-20", -10).unwrap();
        let s = db.usage_stats().unwrap();
        assert_eq!(s.len(), 1);
        assert_eq!((s[0].sessions, s[0].total_seconds), (1, 180));
        assert_eq!(s[0].last_used, "2026-11-20", "the newest day it ran");
    }

    #[test]
    fn an_empty_database_has_no_stats() {
        assert!(mem().usage_stats().unwrap().is_empty());
    }

    #[test]
    fn ordered_by_sessions_descending() {
        let mut db = mem();
        for c in ["claude", "claude", "claude", "pwsh", "bash", "bash"] {
            db.usage_record_start(c, "2026-11-19").unwrap();
        }
        let s = db.usage_stats().unwrap();
        let got: Vec<(&str, i64)> = s.iter().map(|x| (x.command.as_str(), x.sessions)).collect();
        assert_eq!(got, [("claude", 3), ("bash", 2), ("pwsh", 1)]);
    }

    #[test]
    fn utc_day_is_the_date_part() {
        assert_eq!(utc_day(1_700_000_000_000), "2023-11-14");
        assert_eq!(utc_day(0), "1970-01-01");
    }

    #[test]
    fn a_version_1_file_gains_the_table_on_open() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("i.db");
        {
            let c = rusqlite::Connection::open(&p).unwrap();
            c.execute_batch("CREATE TABLE project_notes (folder_path TEXT PRIMARY KEY, content TEXT NOT NULL, updated_at INTEGER NOT NULL);")
                .unwrap();
            c.pragma_update(None, "user_version", 1).unwrap();
        }
        let mut db = Db::open(Some(&p)).unwrap();
        assert_eq!(db.schema_version(), crate::SCHEMA_VERSION);
        db.usage_record_start("claude", "2026-11-19").unwrap();
        assert_eq!(db.usage_stats().unwrap().len(), 1);
    }
}
