//! The SQLite side. [`Db`] is owned by the DB thread ([`crate::Index`]); it is public so tests
//! (and anything that wants a synchronous in-process index) can use it directly.

use std::fmt;
use std::path::Path;

use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, params_from_iter};
use serde::Serialize;

use crate::clean::normalize_folder;

/// The schema version this build writes. Migrations run in order from the file's version.
pub const SCHEMA_VERSION: i64 = 3;

const MARK_START: char = '\u{1}';
const MARK_END: char = '\u{2}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexError {
    /// A message that can be shown as is (bad query syntax, a folder that cannot hold notes).
    Message(String),
    /// The database failed.
    Db(String),
    /// The index thread is gone or too busy.
    Unavailable(String),
}

impl fmt::Display for IndexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IndexError::Message(m) | IndexError::Db(m) | IndexError::Unavailable(m) => f.write_str(m),
        }
    }
}

impl std::error::Error for IndexError {}

impl From<rusqlite::Error> for IndexError {
    fn from(e: rusqlite::Error) -> Self {
        IndexError::Db(format!("the search index failed: {e}"))
    }
}

/// One matching chunk of terminal output.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct OutputHit {
    pub session_id: String,
    /// The name stored when the session started (the engine replaces it with the live name).
    pub session_name: String,
    pub folder: String,
    /// When the matching text was printed, epoch ms.
    pub ts: i64,
    /// A short excerpt with no markup.
    pub excerpt: String,
    /// Character ranges `[start, end)` of the matched terms inside `excerpt`.
    pub matches: Vec<[usize; 2]>,
    /// A config snapshot exists, so the session can be reopened if it is gone.
    pub reopenable: bool,
}

/// One matching note.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteHit {
    pub folder: String,
    pub updated_at: i64,
    pub excerpt: String,
    pub matches: Vec<[usize; 2]>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct NoteRow {
    pub folder: String,
    pub text: String,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SchemaStats {
    pub schema_version: i64,
    pub output_rows: i64,
    pub output_bytes: i64,
    pub notes: i64,
    pub sessions: i64,
}

pub struct Db {
    pub(crate) conn: Connection,
}

/// The FTS5 expression for user text taken literally: one quoted phrase.
pub fn fts_phrase(q: &str) -> String {
    let clean: String = q.chars().filter(|c| !c.is_control()).collect();
    format!("\"{}\"", clean.trim().replace('"', "\"\""))
}

impl Db {
    /// Opens (creating if needed) and migrates. `None` = in memory.
    pub fn open(path: Option<&Path>) -> Result<Db, IndexError> {
        let conn = match path {
            Some(p) => {
                if let Some(dir) = p.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                Connection::open(p)?
            }
            None => Connection::open_in_memory()?,
        };
        let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
        if path.is_some() {
            let _: Result<String, _> = conn.query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0));
            let _ = conn.pragma_update(None, "synchronous", "NORMAL");
        }
        let mut db = Db { conn };
        db.migrate()?;
        Ok(db)
    }

    fn migrate(&mut self) -> Result<(), IndexError> {
        let v: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if v > SCHEMA_VERSION {
            return Err(IndexError::Db(format!("the index was written by a newer version (schema {v}); this one reads {SCHEMA_VERSION}")));
        }
        if v < 1 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(V1)?;
            tx.pragma_update(None, "user_version", 1)?;
            tx.commit()?;
        }
        if v < 2 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(crate::usage::V2)?;
            tx.pragma_update(None, "user_version", 2)?;
            tx.commit()?;
        }
        if v < 3 {
            let tx = self.conn.transaction()?;
            tx.execute_batch(crate::claude::V3)?;
            tx.pragma_update(None, "user_version", 3)?;
            tx.commit()?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> i64 {
        self.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap_or(0)
    }

    // ── output ──────────────────────────────────────────────────────────────────────────

    /// Inserts cleaned lines of one session as rows of at most 32 KiB, then trims the
    /// session to `max_bytes` (oldest rows first).
    pub fn insert_output(&mut self, session_id: &str, ts: i64, lines: &[String], max_bytes: u64) -> Result<(), IndexError> {
        if lines.is_empty() {
            return Ok(());
        }
        const ROW_BYTES: usize = 32 * 1024;
        let tx = self.conn.transaction()?;
        let mut added: i64 = 0;
        {
            let mut ins = tx.prepare_cached("INSERT INTO output(session_id, ts, text) VALUES (?1, ?2, ?3)")?;
            let mut cur = String::new();
            for l in lines {
                if !cur.is_empty() && cur.len() + l.len() + 1 > ROW_BYTES {
                    added += i64::try_from(cur.len()).unwrap_or(i64::MAX);
                    ins.execute(params![session_id, ts, cur])?;
                    cur.clear();
                }
                if !cur.is_empty() {
                    cur.push('\n');
                }
                cur.push_str(l);
            }
            if !cur.is_empty() {
                added += i64::try_from(cur.len()).unwrap_or(i64::MAX);
                ins.execute(params![session_id, ts, cur])?;
            }
        }
        tx.execute(
            "INSERT INTO sessions(session_id, name, folder, group_id, first_seen, last_seen, bytes) VALUES (?1, '', '', '', ?2, ?2, ?3)
             ON CONFLICT(session_id) DO UPDATE SET bytes = bytes + excluded.bytes, last_seen = excluded.last_seen",
            params![session_id, ts, added],
        )?;
        let bytes: i64 = tx.query_row("SELECT bytes FROM sessions WHERE session_id = ?1", [session_id], |r| r.get(0))?;
        let cap = i64::try_from(max_bytes).unwrap_or(i64::MAX);
        if bytes > cap {
            let freed = trim(&tx, session_id, bytes - cap)?;
            tx.execute("UPDATE sessions SET bytes = MAX(0, bytes - ?2) WHERE session_id = ?1", params![session_id, freed])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_session(&mut self, id: &str, name: &str, folder: &str, group_id: &str, now: i64) -> Result<(), IndexError> {
        self.conn.execute(
            "INSERT INTO sessions(session_id, name, folder, group_id, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(session_id) DO UPDATE SET name = excluded.name, folder = excluded.folder, group_id = excluded.group_id, last_seen = excluded.last_seen",
            params![id, name, folder, group_id, now],
        )?;
        Ok(())
    }

    /// Full-text search. `match_expr` is an FTS5 expression (use [`fts_phrase`] for user text).
    /// `session_ids`: only these sessions (`Some(empty)` matches nothing).
    pub fn search_output(&self, match_expr: &str, limit: usize, session_ids: Option<&[String]>) -> Result<Vec<OutputHit>, IndexError> {
        if session_ids.is_some_and(<[String]>::is_empty) {
            return Ok(Vec::new());
        }
        let mut sql = String::from(
            "SELECT o.session_id, COALESCE(s.name, ''), COALESCE(s.folder, ''), o.ts,
                    snippet(output_fts, 0, char(1), char(2), '…', 24),
                    EXISTS(SELECT 1 FROM session_history h WHERE h.session_id = o.session_id)
             FROM output_fts JOIN output o ON o.id = output_fts.rowid
             LEFT JOIN sessions s ON s.session_id = o.session_id
             WHERE output_fts MATCH ?1",
        );
        let mut args: Vec<Value> = vec![Value::Text(match_expr.to_owned())];
        if let Some(ids) = session_ids {
            sql.push_str(" AND o.session_id IN (");
            for (i, id) in ids.iter().enumerate() {
                if i > 0 {
                    sql.push(',');
                }
                sql.push('?');
                args.push(Value::Text(id.clone()));
            }
            sql.push(')');
        }
        sql.push_str(" ORDER BY o.ts DESC, o.id DESC LIMIT ?");
        args.push(Value::Integer(i64::try_from(limit).unwrap_or(500)));
        let run = || -> rusqlite::Result<Vec<OutputHit>> {
            let mut st = self.conn.prepare(&sql)?;
            let rows = st.query_map(params_from_iter(args.iter()), |r| {
                let (excerpt, matches) = split_marks(&r.get::<_, String>(4)?);
                Ok(OutputHit {
                    session_id: r.get(0)?,
                    session_name: r.get(1)?,
                    folder: r.get(2)?,
                    ts: r.get(3)?,
                    excerpt,
                    matches,
                    reopenable: r.get(5)?,
                })
            })?;
            rows.collect()
        };
        run().map_err(fts_error)
    }

    // ── session history ─────────────────────────────────────────────────────────────────

    pub fn save_history(&mut self, session_id: &str, name: &str, folder: &str, snapshot_json: &str, now: i64) -> Result<(), IndexError> {
        self.conn.execute(
            "INSERT INTO session_history(session_id, name, folder, snapshot_json, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(session_id) DO UPDATE SET name = excluded.name, folder = excluded.folder, snapshot_json = excluded.snapshot_json, updated_at = excluded.updated_at",
            params![session_id, name, folder, snapshot_json, now],
        )?;
        Ok(())
    }

    pub fn history(&self, session_id: &str) -> Result<Option<String>, IndexError> {
        Ok(self.conn.query_row("SELECT snapshot_json FROM session_history WHERE session_id = ?1", [session_id], |r| r.get(0)).optional()?)
    }

    // ── notes ───────────────────────────────────────────────────────────────────────────

    pub fn note_get(&self, folder: &str) -> Result<Option<NoteRow>, IndexError> {
        let key = key_of(folder)?;
        Ok(self
            .conn
            .query_row("SELECT folder_path, content, updated_at FROM project_notes WHERE folder_path = ?1", [&key], |r| {
                Ok(NoteRow { folder: r.get(0)?, text: r.get(1)?, updated_at: r.get(2)? })
            })
            .optional()?)
    }

    /// Saves a note (blank text removes it).
    pub fn note_set(&mut self, folder: &str, text: &str, now: i64) -> Result<(), IndexError> {
        let key = key_of(folder)?;
        if text.trim().is_empty() {
            self.conn.execute("DELETE FROM project_notes WHERE folder_path = ?1", [&key])?;
        } else {
            self.conn.execute(
                "INSERT INTO project_notes(folder_path, content, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(folder_path) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at",
                params![key, text, now],
            )?;
        }
        Ok(())
    }

    /// Case-insensitive substring search over notes (notes are short free text; FTS is overkill).
    pub fn search_notes(&self, needle: &str, limit: usize) -> Result<Vec<NoteHit>, IndexError> {
        let needle = needle.trim();
        if needle.is_empty() {
            return Ok(Vec::new());
        }
        let pat = format!("%{}%", needle.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let mut st = self.conn.prepare(
            "SELECT folder_path, content, updated_at FROM project_notes WHERE content LIKE ?1 ESCAPE '\\' OR folder_path LIKE ?1 ESCAPE '\\'
             ORDER BY updated_at DESC LIMIT ?2",
        )?;
        let rows = st.query_map(params![pat, i64::try_from(limit).unwrap_or(500)], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, i64>(2)?))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (folder, text, updated_at) = row?;
            let (excerpt, matches) = note_excerpt(&text, needle);
            out.push(NoteHit { folder, updated_at, excerpt, matches });
        }
        Ok(out)
    }

    /// Imports `project_notes` from the WPF app's `output.db`, opened read-only. A key already
    /// present is kept unless the WPF note is newer. Returns how many notes were imported.
    pub fn import_wpf_notes(&mut self, path: &Path) -> Result<usize, IndexError> {
        if !path.is_file() {
            return Err(IndexError::Message(format!("no file at {}", path.display())));
        }
        let src = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .map_err(|e| IndexError::Message(format!("could not open {} as a database: {e}", path.display())))?;
        let mut st = src
            .prepare("SELECT folder_path, content, updated_at FROM project_notes")
            .map_err(|_| IndexError::Message(format!("{} has no notes table; is it the WPF app's output.db?", path.display())))?;
        let rows: Vec<(String, String, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2).unwrap_or(0))))
            .map_err(|e| IndexError::Message(format!("could not read the notes: {e}")))?
            .filter_map(Result::ok)
            .collect();
        let tx = self.conn.transaction()?;
        let mut n = 0;
        for (folder, text, ts) in rows {
            let Some(key) = normalize_folder(&folder) else { continue };
            if text.trim().is_empty() {
                continue;
            }
            n += tx.execute(
                "INSERT INTO project_notes(folder_path, content, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT(folder_path) DO UPDATE SET content = excluded.content, updated_at = excluded.updated_at
                 WHERE excluded.updated_at > project_notes.updated_at",
                params![key, text, ts],
            )?;
        }
        tx.commit()?;
        Ok(n)
    }

    pub fn stats(&self) -> Result<SchemaStats, IndexError> {
        let one = |sql: &str| -> i64 { self.conn.query_row(sql, [], |r| r.get(0)).unwrap_or(0) };
        Ok(SchemaStats {
            schema_version: self.schema_version(),
            output_rows: one("SELECT COUNT(*) FROM output"),
            output_bytes: one("SELECT COALESCE(SUM(length(text)), 0) FROM output"),
            notes: one("SELECT COUNT(*) FROM project_notes"),
            sessions: one("SELECT COUNT(*) FROM sessions"),
        })
    }
}

fn key_of(folder: &str) -> Result<String, IndexError> {
    normalize_folder(folder).ok_or_else(|| IndexError::Message("this session has no folder to keep a note for".into()))
}

/// Deletes the oldest rows of a session until at least `excess` bytes are gone; returns the bytes freed.
fn trim(tx: &rusqlite::Transaction<'_>, session_id: &str, excess: i64) -> rusqlite::Result<i64> {
    let mut freed = 0i64;
    let mut last_id = 0i64;
    {
        let mut st = tx.prepare("SELECT id, length(text) FROM output WHERE session_id = ?1 ORDER BY id ASC")?;
        let mut rows = st.query([session_id])?;
        while let Some(r) = rows.next()? {
            last_id = r.get(0)?;
            freed += r.get::<_, i64>(1)?;
            if freed >= excess {
                break;
            }
        }
    }
    if last_id > 0 {
        tx.execute("DELETE FROM output WHERE session_id = ?1 AND id <= ?2", params![session_id, last_id])?;
    }
    Ok(freed)
}

fn fts_error(e: rusqlite::Error) -> IndexError {
    let m = e.to_string();
    if m.contains("fts5")
        || m.contains("syntax error")
        || m.contains("no such column")
        || m.contains("unterminated")
        || m.contains("unknown special query")
    {
        IndexError::Message(format!("That isn't valid search syntax ({m}). Search for plain words, or quote the phrase."))
    } else {
        IndexError::Db(format!("the search index failed: {m}"))
    }
}

/// `snippet()` output with marker characters → plain text and char ranges of the marked terms.
fn split_marks(s: &str) -> (String, Vec<[usize; 2]>) {
    let mut out = String::with_capacity(s.len());
    let mut ranges = Vec::new();
    let mut n = 0usize;
    let mut start = None;
    for c in s.chars() {
        match c {
            MARK_START => start = Some(n),
            MARK_END => {
                if let Some(a) = start.take() {
                    ranges.push([a, n]);
                }
            }
            '\n' => {
                out.push(' ');
                n += 1;
            }
            _ => {
                out.push(c);
                n += 1;
            }
        }
    }
    (out, ranges)
}

/// A window of ~120 chars around the first match of `needle` (case-insensitive), with every
/// match inside it marked.
fn note_excerpt(text: &str, needle: &str) -> (String, Vec<[usize; 2]>) {
    let chars: Vec<char> = text.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    let low: Vec<char> = chars.iter().map(|c| c.to_lowercase().next().unwrap_or(*c)).collect();
    let nd: Vec<char> = needle.chars().map(|c| c.to_lowercase().next().unwrap_or(c)).collect();
    let find = |from: usize| -> Option<usize> {
        if nd.is_empty() || low.len() < nd.len() {
            return None;
        }
        (from..=low.len() - nd.len()).find(|i| low.get(*i..*i + nd.len()) == Some(&nd[..]))
    };
    let first = find(0).unwrap_or(0);
    let from = first.saturating_sub(40);
    let to = (first + nd.len() + 80).min(chars.len());
    let mut matches = Vec::new();
    let mut i = from;
    while let Some(p) = find(i) {
        if p + nd.len() > to {
            break;
        }
        matches.push([p - from, p - from + nd.len()]);
        i = p + nd.len().max(1);
    }
    let mut ex: String = chars.get(from..to).unwrap_or(&[]).iter().collect();
    let mut shift = 0;
    if from > 0 {
        ex.insert(0, '…');
        shift = 1;
    }
    if to < chars.len() {
        ex.push('…');
    }
    for m in &mut matches {
        m[0] += shift;
        m[1] += shift;
    }
    (ex, matches)
}

const V1: &str = "
CREATE TABLE output (
    id INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    ts INTEGER NOT NULL,
    text TEXT NOT NULL
);
CREATE INDEX output_session ON output(session_id, id);
CREATE VIRTUAL TABLE output_fts USING fts5(text, content='output', content_rowid='id');
CREATE TRIGGER output_ai AFTER INSERT ON output BEGIN
    INSERT INTO output_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER output_ad AFTER DELETE ON output BEGIN
    INSERT INTO output_fts(output_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TABLE sessions (
    session_id TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    folder TEXT NOT NULL DEFAULT '',
    group_id TEXT NOT NULL DEFAULT '',
    first_seen INTEGER NOT NULL DEFAULT 0,
    last_seen INTEGER NOT NULL DEFAULT 0,
    bytes INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE session_history (
    session_id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    folder TEXT NOT NULL,
    snapshot_json TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
CREATE TABLE project_notes (
    folder_path TEXT PRIMARY KEY,
    content TEXT NOT NULL,
    updated_at INTEGER NOT NULL
);
";

#[cfg(test)]
mod tests {
    use super::*;

    fn mem() -> Db {
        Db::open(None).unwrap()
    }

    fn lines(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn migrates_an_empty_database_and_reopening_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("i.db");
        {
            let db = Db::open(Some(&p)).unwrap();
            assert_eq!(db.schema_version(), SCHEMA_VERSION);
            assert_eq!(db.stats().unwrap().output_rows, 0);
        }
        let db = Db::open(Some(&p)).unwrap();
        assert_eq!(db.schema_version(), SCHEMA_VERSION);
    }

    #[test]
    fn a_newer_schema_is_refused_not_clobbered() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("i.db");
        {
            let c = Connection::open(&p).unwrap();
            c.pragma_update(None, "user_version", 99).unwrap();
        }
        assert!(matches!(Db::open(Some(&p)), Err(IndexError::Db(_))));
    }

    #[test]
    fn finds_output_with_a_highlighted_excerpt() {
        let mut db = mem();
        db.set_session("s1", "Odulodo", "c:\\repos\\odulodo", "g", 1).unwrap();
        db.insert_output("s1", 1000, &lines(&["building the project", "error: linker failed with exit code 1", "done"]), 1 << 20).unwrap();
        let hits = db.search_output(&fts_phrase("linker failed"), 10, None).unwrap();
        assert_eq!(hits.len(), 1);
        let h = &hits[0];
        assert_eq!((h.session_name.as_str(), h.ts), ("Odulodo", 1000));
        assert!(h.matches.len() == 1 && !h.reopenable);
        let [a, b] = h.matches[0];
        let hl: String = h.excerpt.chars().skip(a).take(b - a).collect();
        assert!(hl.to_lowercase().contains("linker"), "{hl:?} in {:?}", h.excerpt);
    }

    #[test]
    fn phrase_quoting_neutralises_fts_syntax() {
        let mut db = mem();
        db.insert_output("s", 1, &lines(&["foo AND bar", "NEAR(x y)", "say \"hi\" there", "a-b c*d"]), 1 << 20).unwrap();
        for q in [
            "foo AND",
            "NEAR(",
            "\"",
            "\"unterminated",
            "a-b",
            "c*",
            "*",
            "(",
            ")",
            "OR",
            "NOT x",
            "col:x",
            "^x",
            "-",
            "'",
            "\\",
            "x\u{0}y",
            "{}",
            "a b c d e f",
        ] {
            let r = db.search_output(&fts_phrase(q), 10, None);
            assert!(r.is_ok(), "query {q:?} must not be an error: {r:?}");
        }
        assert_eq!(db.search_output(&fts_phrase("foo and"), 10, None).unwrap().len(), 1);
        assert_eq!(db.search_output(&fts_phrase("say \"hi\""), 10, None).unwrap().len(), 1);
    }

    #[test]
    fn raw_fts_syntax_errors_become_plain_messages() {
        let mut db = mem();
        db.insert_output("s", 1, &lines(&["hello world"]), 1 << 20).unwrap();
        for q in ["\"", "AND", "hello AND", "NEAR(", "col:hello", "("] {
            match db.search_output(q, 10, None) {
                Ok(_) => {}
                Err(IndexError::Message(m)) => assert!(m.contains("isn't valid search syntax"), "{m}"),
                Err(e) => panic!("{q:?} -> {e:?}"),
            }
        }
        assert_eq!(db.search_output("hello OR nothing", 10, None).unwrap().len(), 1);
        assert_eq!(db.search_output("hel*", 10, None).unwrap().len(), 1);
    }

    #[test]
    fn session_filter_and_limit() {
        let mut db = mem();
        for s in ["a", "b"] {
            for i in 0..5 {
                db.insert_output(s, i, &lines(&["needle in a haystack"]), 1 << 20).unwrap();
            }
        }
        assert_eq!(db.search_output(&fts_phrase("needle"), 100, None).unwrap().len(), 10);
        assert_eq!(db.search_output(&fts_phrase("needle"), 3, None).unwrap().len(), 3);
        let only_a = db.search_output(&fts_phrase("needle"), 100, Some(&["a".to_owned()])).unwrap();
        assert!(only_a.len() == 5 && only_a.iter().all(|h| h.session_id == "a"));
        assert!(db.search_output(&fts_phrase("needle"), 100, Some(&[])).unwrap().is_empty());
    }

    #[test]
    fn retention_keeps_the_newest_and_keeps_fts_in_step() {
        let mut db = mem();
        let cap = 2_000;
        for i in 0..200 {
            db.insert_output("s", i, &lines(&[&format!("marker{i:04} {}", "x".repeat(40))]), cap).unwrap();
        }
        let st = db.stats().unwrap();
        assert!(st.output_bytes <= i64::try_from(cap).unwrap(), "bytes {}", st.output_bytes);
        assert!(st.output_rows > 10);
        assert!(db.search_output(&fts_phrase("marker0199"), 5, None).unwrap().len() == 1, "newest kept");
        assert!(db.search_output(&fts_phrase("marker0000"), 5, None).unwrap().is_empty(), "oldest dropped from the FTS table too");
        // Another session is not trimmed by this one's cap.
        db.insert_output("other", 1, &lines(&["marker0000 kept"]), cap).unwrap();
        assert_eq!(db.search_output(&fts_phrase("marker0000"), 5, None).unwrap().len(), 1);
    }

    #[test]
    fn notes_are_keyed_by_normalised_folder_and_blank_deletes() {
        let mut db = mem();
        db.note_set("C:/Repos/App/", "remember the milk", 5).unwrap();
        assert_eq!(db.note_get("c:\\repos\\app").unwrap().unwrap().text, "remember the milk");
        assert_eq!(db.note_get("C:\\REPOS\\APP\\").unwrap().unwrap().updated_at, 5);
        assert!(db.note_get("c:\\repos\\other").unwrap().is_none());
        db.note_set("c:\\repos\\app", "  ", 6).unwrap();
        assert!(db.note_get("c:\\repos\\app").unwrap().is_none());
        assert!(matches!(db.note_set("  ", "x", 1), Err(IndexError::Message(_))));
    }

    #[test]
    fn note_search_is_literal_not_a_pattern() {
        let mut db = mem();
        db.note_set("/a", "100% sure", 1).unwrap();
        db.note_set("/b", "under_score", 2).unwrap();
        db.note_set("/c", "plain text", 3).unwrap();
        assert_eq!(db.search_notes("100%", 10).unwrap().len(), 1);
        assert_eq!(db.search_notes("_", 10).unwrap().len(), 1);
        assert_eq!(db.search_notes("%", 10).unwrap().len(), 1);
        let h = &db.search_notes("SURE", 10).unwrap()[0];
        let [a, b] = h.matches[0];
        assert_eq!(h.excerpt.chars().skip(a).take(b - a).collect::<String>(), "sure");
    }

    #[test]
    fn history_round_trips_and_marks_hits_reopenable() {
        let mut db = mem();
        db.save_history("s", "Name", "/f", "{\"Name\":\"Name\"}", 1).unwrap();
        assert_eq!(db.history("s").unwrap().as_deref(), Some("{\"Name\":\"Name\"}"));
        assert!(db.history("zz").unwrap().is_none());
        db.insert_output("s", 1, &lines(&["findme"]), 1 << 20).unwrap();
        assert!(db.search_output(&fts_phrase("findme"), 5, None).unwrap()[0].reopenable);
    }

    #[test]
    fn imports_notes_from_a_wpf_database_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("output.db");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch(
                "CREATE TABLE project_notes (folder_path TEXT PRIMARY KEY, content TEXT, updated_at INTEGER);
                 INSERT INTO project_notes VALUES ('C:\\Repos\\App', 'from wpf', 100), ('D:\\x', '', 5), ('E:\\y', 'old', 1);",
            )
            .unwrap();
        }
        let mut db = mem();
        db.note_set("e:\\y", "newer here", 50).unwrap();
        assert_eq!(db.import_wpf_notes(&p).unwrap(), 1);
        assert_eq!(db.note_get("c:\\repos\\app").unwrap().unwrap().text, "from wpf");
        assert_eq!(db.note_get("e:\\y").unwrap().unwrap().text, "newer here");
        assert!(matches!(db.import_wpf_notes(&dir.path().join("missing.db")), Err(IndexError::Message(_))));
        let junk = dir.path().join("junk.db");
        std::fs::write(&junk, b"not a database at all").unwrap();
        assert!(matches!(db.import_wpf_notes(&junk), Err(IndexError::Message(_))));
    }
}
