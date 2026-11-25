//! The remote-access log: an append-only file, one JSON object per line, in the same private
//! folder as the keys. It records enabling and disabling, pairing, connections, revocations,
//! refusals, every non-read call a paired device makes (the tool and the session, never typed
//! content), and the reads of session content and terminal streams a device opens. Repeats of the
//! same thing (a scanner knocking, a screen that polls) are counted, not written line by line. At
//! 4 MiB it moves to `<name>.log.1`; the last [`KEEP_FILES`] older files are kept.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Map, Value, json};

const ROTATE_AT: u64 = 4 * 1024 * 1024;

/// Rotated files kept beside the live one (`.log.1` is the newest).
pub const KEEP_FILES: u32 = 5;

/// How long a repeat of the same event counts toward one line instead of writing another.
const COALESCE_WINDOW: Duration = Duration::from_secs(30);

/// Most distinct repeating events followed at once; past it every event is written as it comes.
const MAX_FOLLOWED: usize = 256;

/// Text for a log field: control characters (a newline would forge a line) removed, capped.
pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

struct Repeats {
    event: String,
    fields: Value,
    first: Instant,
    repeats: u64,
}

pub struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
    seen: Mutex<HashMap<String, Repeats>>,
}

impl std::fmt::Debug for Audit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Audit").field("path", &self.path).finish()
    }
}

impl Audit {
    pub fn new(path: PathBuf) -> Self {
        Self { path, lock: Mutex::new(()), seen: Mutex::new(HashMap::new()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `{"t": unix_ms, "event": event, ...fields}`. A failure is logged, never raised:
    /// the log must not be a way to stop the app (and it never holds a secret).
    pub fn record(&self, event: &str, fields: Value) {
        self.flush_expired(Instant::now());
        self.write(event, fields);
    }

    /// As [`Audit::record`], but the same `key` again within half a minute is only counted: the
    /// first is written at once, and a later line `<event>_repeated` says how many followed. So a
    /// flood of identical refusals or reads cannot fill the log (or push the rest out of it).
    pub fn record_coalesced(&self, event: &str, key: &str, fields: Value) {
        let now = Instant::now();
        self.flush_expired(now);
        {
            let mut seen = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(r) = seen.get_mut(key) {
                r.repeats = r.repeats.saturating_add(1);
                return;
            }
            if seen.len() < MAX_FOLLOWED {
                seen.insert(key.to_owned(), Repeats { event: event.to_owned(), fields: fields.clone(), first: now, repeats: 0 });
            }
        }
        self.write(event, fields);
    }

    /// Writes the "repeated" line of every followed event whose window has passed.
    fn flush_expired(&self, now: Instant) {
        let done: Vec<Repeats> = {
            let mut seen = self.seen.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let keys: Vec<String> = seen.iter().filter(|(_, r)| now.saturating_duration_since(r.first) >= COALESCE_WINDOW).map(|(k, _)| k.clone()).collect();
            keys.iter().filter_map(|k| seen.remove(k)).collect()
        };
        for r in done {
            if r.repeats > 0 {
                let mut fields = r.fields;
                if let Value::Object(o) = &mut fields {
                    o.insert("repeats".into(), json!(r.repeats));
                    o.insert("window_secs".into(), json!(COALESCE_WINDOW.as_secs()));
                }
                self.write(&format!("{}_repeated", r.event), fields);
            }
        }
    }

    fn write(&self, event: &str, fields: Value) {
        let _guard = self.lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut o = Map::new();
        let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        o.insert("t".into(), json!(u64::try_from(ms).unwrap_or(u64::MAX)));
        o.insert("event".into(), json!(event));
        if let Value::Object(f) = fields {
            for (k, v) in f {
                o.insert(k, v);
            }
        }
        let line = format!("{}\n", Value::Object(o));
        if let Err(e) = self.append(&line) {
            log::warn!("remote access: could not write the log {}: {e}", self.path.display());
        }
    }

    fn rotated(&self, n: u32) -> PathBuf {
        self.path.with_extension(format!("log.{n}"))
    }

    fn append(&self, line: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent().filter(|d| !d.exists()) {
            crate::fsperm::ensure_private_dir(dir)?;
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= ROTATE_AT) {
            // The oldest is dropped, each other file moves one up, the live one becomes `.log.1`.
            for n in (1..KEEP_FILES).rev() {
                let _ = std::fs::rename(self.rotated(n), self.rotated(n + 1));
            }
            let _ = std::fs::rename(&self.path, self.rotated(1));
        }
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&self.path)?.write_all(line.as_bytes())
    }

    /// The last `n` lines, parsed (what Settings shows). Unparsable lines are skipped.
    pub fn tail(&self, n: usize) -> Vec<Value> {
        self.flush_expired(Instant::now());
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(n);
        lines.iter().skip(start).filter_map(|l| serde_json::from_str(l).ok()).collect()
    }
}

impl Drop for Audit {
    /// The counts still being followed are written, so a quit does not lose them.
    fn drop(&mut self) {
        self.flush_expired(Instant::now() + COALESCE_WINDOW);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lines_are_json_in_order_and_a_newline_cannot_forge_one() {
        let d = tempfile::tempdir().unwrap();
        let a = Audit::new(d.path().join("remote-access.log"));
        a.record("enabled", json!({"by": "ui"}));
        a.record("connect", json!({"device": clean("Lap\ntop\u{7}")}));
        let t = a.tail(10);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0]["event"], "enabled");
        assert_eq!(t[1]["device"], "Laptop");
        assert!(t[1]["t"].as_u64().unwrap() > 0);
        assert_eq!(std::fs::read_to_string(a.path()).unwrap().lines().count(), 2);
        assert_eq!(a.tail(1).len(), 1);
    }

    #[test]
    fn a_flood_of_the_same_refusal_is_one_line_and_a_count() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("remote-access.log");
        let a = Audit::new(path.clone());
        for _ in 0..1000 {
            a.record_coalesced("refused", "unknown key|10.0.0.9", json!({"why": "unknown key", "from": "10.0.0.9"}));
        }
        a.record_coalesced("refused", "unknown key|10.0.0.10", json!({"why": "unknown key", "from": "10.0.0.10"}));
        assert_eq!(std::fs::read_to_string(&path).unwrap().lines().count(), 2, "one line per distinct source");
        drop(a);
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let summary = lines.iter().find(|v| v["event"] == "refused_repeated").expect("the count is written when the log closes");
        assert_eq!(summary["repeats"], 999);
    }

    #[test]
    fn five_rotated_files_are_kept() {
        let d = tempfile::tempdir().unwrap();
        let path = d.path().join("remote-access.log");
        let a = Audit::new(path.clone());
        for gen_ in 1..=7u32 {
            // A live file over the limit rotates on the next write.
            std::fs::write(&path, vec![b'x'; usize::try_from(ROTATE_AT).unwrap()]).unwrap();
            a.record("e", json!({"gen": gen_}));
        }
        for n in 1..=KEEP_FILES {
            assert!(a.rotated(n).exists(), ".log.{n} is kept");
        }
        assert!(!a.rotated(KEEP_FILES + 1).exists(), "no more than {KEEP_FILES}");
    }
}
