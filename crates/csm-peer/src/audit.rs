//! The remote-access log: an append-only file, one JSON object per line, in the same private
//! folder as the keys. It records enabling and disabling, pairing, connections, revocations,
//! refusals and every non-read call a paired device makes (the tool and the session, never
//! typed content). At 4 MiB it moves to `<name>.log.1` and starts again.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde_json::{Map, Value, json};

const ROTATE_AT: u64 = 4 * 1024 * 1024;

/// Text for a log field: control characters (a newline would forge a line) removed, capped.
pub fn clean(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).take(200).collect()
}

#[derive(Debug)]
pub struct Audit {
    path: PathBuf,
    lock: Mutex<()>,
}

impl Audit {
    pub fn new(path: PathBuf) -> Self {
        Self { path, lock: Mutex::new(()) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends `{"t": unix_ms, "event": event, ...fields}`. A failure is logged, never raised:
    /// the log must not be a way to stop the app (and it never holds a secret).
    pub fn record(&self, event: &str, fields: Value) {
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

    fn append(&self, line: &str) -> std::io::Result<()> {
        if let Some(dir) = self.path.parent().filter(|d| !d.exists()) {
            crate::fsperm::ensure_private_dir(dir)?;
        }
        if std::fs::metadata(&self.path).is_ok_and(|m| m.len() >= ROTATE_AT) {
            let _ = std::fs::rename(&self.path, self.path.with_extension("log.1"));
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
        let text = std::fs::read_to_string(&self.path).unwrap_or_default();
        let lines: Vec<&str> = text.lines().collect();
        let start = lines.len().saturating_sub(n);
        lines.iter().skip(start).filter_map(|l| serde_json::from_str(l).ok()).collect()
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
}
