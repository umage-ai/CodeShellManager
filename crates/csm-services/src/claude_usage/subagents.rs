//! Reads the files Claude Code keeps beside a subagent's transcript: `agent-<id>.meta.json` and,
//! in a workflow run folder (`subagents/workflows/<run id>/`), `journal.jsonl`. Not a documented
//! API: every read is capped, every failure shows less. The parsing is in
//! `csm_core::subagents`. Reads the disk: worker threads only.

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use csm_core::subagents::{Journal, MetaInfo, parse_journal, parse_meta};

/// Most bytes read of a meta file or a journal.
pub const MAX_FILE: u64 = 4 * 1024 * 1024;
/// Most agents looked at per session in one pass.
pub const MAX_SCAN: usize = 400;

/// A subagent transcript found on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub transcript: PathBuf,
    /// The workflow run folder it sits in: `(run id, folder)`.
    pub run: Option<(String, PathBuf)>,
    pub mtime_ms: Option<i64>,
}

/// The workflow run a transcript path belongs to: the folder directly below a `workflows` folder.
pub fn run_of(transcript: &Path) -> Option<(String, PathBuf)> {
    let mut dir = transcript.parent()?;
    loop {
        let parent = dir.parent()?;
        if parent.file_name().is_some_and(|n| n == "workflows") {
            let id = dir.file_name()?.to_str()?.to_owned();
            return Some((id, dir.to_path_buf()));
        }
        dir = parent;
    }
}

fn mtime_ms(p: &Path) -> Option<i64> {
    let m = fs::metadata(p).ok()?.modified().ok()?;
    i64::try_from(m.duration_since(UNIX_EPOCH).ok()?.as_millis()).ok()
}

/// The subagent transcripts of a main transcript, newest-first and capped at [`MAX_SCAN`].
pub fn find(main: &Path) -> Vec<Found> {
    let mut v: Vec<Found> = super::discover::subagent_files(main)
        .into_iter()
        .map(|(id, p)| Found { run: run_of(&p), mtime_ms: mtime_ms(&p), id, transcript: p })
        .collect();
    v.sort_by(|a, b| b.mtime_ms.cmp(&a.mtime_ms).then(a.id.cmp(&b.id)));
    v.truncate(MAX_SCAN);
    v
}

fn read_capped(p: &Path) -> Option<String> {
    let f = fs::File::open(p).ok()?;
    let mut buf = Vec::new();
    f.take(MAX_FILE).read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

type Stamp = (Option<i64>, u64);

fn stamp(p: &Path) -> Option<Stamp> {
    let m = fs::metadata(p).ok()?;
    Some((m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).and_then(|d| i64::try_from(d.as_millis()).ok()), m.len()))
}

/// Parsed meta files and journals, read again only when their size or time changed.
#[derive(Debug, Default)]
pub struct Cache {
    metas: HashMap<PathBuf, (Stamp, MetaInfo)>,
    journals: HashMap<PathBuf, (Stamp, Journal)>,
}

impl Cache {
    /// The meta of a transcript (`agent-<id>.jsonl` → `agent-<id>.meta.json`); empty when missing.
    pub fn meta(&mut self, transcript: &Path) -> MetaInfo {
        let p = transcript.with_extension("meta.json");
        let Some(s) = stamp(&p) else {
            self.metas.remove(&p);
            return MetaInfo::default();
        };
        if let Some((old, m)) = self.metas.get(&p)
            && *old == s
        {
            return m.clone();
        }
        let m = read_capped(&p).map(|t| parse_meta(&t)).unwrap_or_default();
        self.metas.insert(p, (s, m.clone()));
        m
    }

    /// The journal of a run folder; empty when missing.
    pub fn journal(&mut self, run_dir: &Path) -> Journal {
        let p = run_dir.join("journal.jsonl");
        let Some(s) = stamp(&p) else {
            self.journals.remove(&p);
            return Journal::default();
        };
        if let Some((old, j)) = self.journals.get(&p)
            && *old == s
        {
            return j.clone();
        }
        let j = read_capped(&p).map(|t| parse_journal(&t)).unwrap_or_default();
        self.journals.insert(p, (s, j.clone()));
        j
    }

    /// Forgets files not in `keep` (a session's subagents went away).
    pub fn retain(&mut self, keep: &[Found]) {
        self.metas.retain(|p, _| keep.iter().any(|f| f.transcript.with_extension("meta.json") == *p));
        self.journals.retain(|p, _| keep.iter().any(|f| f.run.as_ref().is_some_and(|(_, d)| d.join("journal.jsonl") == *p)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_folder_is_the_one_below_workflows() {
        let (id, dir) = run_of(Path::new("/p/s/subagents/workflows/wf_1/agent-a.jsonl")).unwrap();
        assert_eq!(id, "wf_1");
        assert!(dir.ends_with("wf_1"));
        assert!(run_of(Path::new("/p/s/subagents/agent-a.jsonl")).is_none());
        // Deeper nesting still names the run folder.
        assert_eq!(run_of(Path::new("/p/subagents/workflows/wf_2/x/agent-a.jsonl")).unwrap().0, "wf_2");
    }

    #[test]
    fn finds_agents_reads_meta_and_journal_and_survives_garbage() {
        let d = tempfile::tempdir().unwrap();
        let main = d.path().join("sid.jsonl");
        fs::write(&main, "").unwrap();
        let sub = d.path().join("sid").join("subagents");
        let run = sub.join("workflows").join("wf_x");
        fs::create_dir_all(&run).unwrap();
        fs::write(sub.join("agent-a1.jsonl"), "").unwrap();
        fs::write(sub.join("agent-a1.meta.json"), r#"{"description":"Explore","finished":true}"#).unwrap();
        fs::write(run.join("agent-w1.jsonl"), "").unwrap();
        fs::write(run.join("agent-w1.meta.json"), [0xff_u8, 0xfe, b'{']).unwrap();
        fs::write(run.join("agent-w2.jsonl"), "").unwrap();
        fs::write(run.join("journal.jsonl"), "{\"type\":\"started\",\"agentId\":\"w2\",\"label\":\"L\"}\n\u{0}garbage\n").unwrap();
        fs::write(sub.join("agent-.jsonl"), "").unwrap();
        fs::write(sub.join("notes.txt"), "x").unwrap();

        let found = find(&main);
        assert_eq!(found.len(), 3);
        let mut cache = Cache::default();
        let a1 = found.iter().find(|f| f.id == "a1").unwrap();
        assert_eq!(cache.meta(&a1.transcript).finished, Some(true));
        assert!(a1.run.is_none());
        let w1 = found.iter().find(|f| f.id == "w1").unwrap();
        assert_eq!(cache.meta(&w1.transcript), MetaInfo::default());
        let (rid, rdir) = w1.run.clone().unwrap();
        assert_eq!(rid, "wf_x");
        let j = cache.journal(&rdir);
        assert_eq!(j.started.len(), 1);
        assert_eq!(j.bad_lines, 1);
        // A change is noticed.
        fs::write(sub.join("agent-a1.meta.json"), r#"{"description":"Explore","finished":false,"model":"m"}"#).unwrap();
        assert_eq!(cache.meta(&a1.transcript).finished, Some(false));
        // A folder without journal or meta is just empty.
        assert_eq!(cache.journal(d.path()), Journal::default());
        cache.retain(&[]);
        assert!(cache.metas.is_empty() && cache.journals.is_empty());
    }
}
