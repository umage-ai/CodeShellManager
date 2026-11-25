//! The threads, one file per project in the add-on's data folder:
//! `<data_dir>/projects/<key>/threads.json`, `key` = the first 16 hex characters of the SHA-256 of
//! the project id (the key tasks use). With no data folder (`--clean`, an isolated run) nothing is
//! written and the threads live in memory.
//!
//! Writes are atomic (a temp file and a rename) and keep one `.bak`. A file that does not parse is
//! moved aside as `threads.json.bad-<ms>` and the project starts empty, as `tasks.json` does.
//! Caps: 2,000 threads per project, 200 comments per thread, 16 KiB per comment, and at most 100
//! threads started by one agent session.
//!
//! The store trusts nothing it reads: it is written by this program, but a person (or an agent
//! with file access) can edit it.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::anchor::Anchor;

pub const MAX_THREADS: usize = 2000;
pub const MAX_COMMENTS: usize = 200;
pub const MAX_BODY: usize = 16 * 1024;
pub const MAX_AGENT_THREADS: usize = 100;
const FILE_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Addressed,
    Resolved,
}

impl Status {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Status::Open => "open",
            Status::Addressed => "addressed",
            Status::Resolved => "resolved",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "open" => Some(Status::Open),
            "addressed" => Some(Status::Addressed),
            "resolved" => Some(Status::Resolved),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Author {
    /// `person` or `agent`.
    pub kind: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client: Option<String>,
}

impl Author {
    #[must_use]
    pub fn person(name: &str) -> Author {
        Author { kind: "person".into(), name: name.to_owned(), session_id: None, client: None }
    }

    #[must_use]
    pub fn is_agent(&self) -> bool {
        self.kind == "agent"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Comment {
    pub id: String,
    pub author: Author,
    pub body: String,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Thread {
    pub id: String,
    pub status: Status,
    pub created_ms: u64,
    pub updated_ms: u64,
    pub anchor: Anchor,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Project {
    #[serde(default)]
    pub version: u32,
    /// The project id (what tasks call a project).
    #[serde(default)]
    pub project: String,
    /// The repository folder, for display.
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub threads: Vec<Thread>,
}

/// Why a write was refused (the message goes to the caller).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StoreError {
    NotFound(String),
    Refused(String),
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StoreError::NotFound(m) | StoreError::Refused(m) => f.write_str(m),
        }
    }
}

/// The folder name of a project: 16 hex characters of the SHA-256 of its id.
#[must_use]
pub fn project_key(project: &str) -> String {
    let digest = Sha256::digest(project.as_bytes());
    digest.iter().take(8).map(|b| format!("{b:02x}")).collect()
}

/// A comment body: control characters (but not newlines and tabs) dropped, CRLF as LF, trimmed.
/// `Err` when empty or over the cap.
pub fn clean_body(body: &str) -> Result<String, StoreError> {
    let cleaned: String = body.replace("\r\n", "\n").chars().filter(|c| !c.is_control() || matches!(c, '\n' | '\t')).collect();
    let cleaned = cleaned.trim().to_owned();
    if cleaned.is_empty() {
        return Err(StoreError::Refused("the comment is empty".into()));
    }
    if cleaned.len() > MAX_BODY {
        return Err(StoreError::Refused(format!("the comment is over {} KiB", MAX_BODY / 1024)));
    }
    Ok(cleaned)
}

#[derive(Debug, Default)]
pub struct Store {
    dir: Option<PathBuf>,
    projects: HashMap<String, Project>,
    /// Projects whose file is there but could not be read (an I/O error, not bad JSON), by key,
    /// with the reason. Never treated as empty: changing them is refused, so the real file is
    /// not replaced by one that holds only what this run added.
    unreadable: HashMap<String, String>,
    seq: u64,
}

/// What reading a project's file found.
enum Read {
    /// The file (parsed), or a bad one moved aside, or no file at all: a project to work with.
    Project(Option<Project>),
    /// The file exists but cannot be read.
    Unreadable(String),
}

impl Store {
    #[must_use]
    pub fn new(data_dir: Option<PathBuf>) -> Store {
        Store { dir: data_dir, ..Store::default() }
    }

    #[must_use]
    pub fn persistent(&self) -> bool {
        self.dir.is_some()
    }

    fn file_of(&self, key: &str) -> Option<PathBuf> {
        self.dir.as_ref().map(|d| d.join("projects").join(key).join("threads.json"))
    }

    /// Loads (once) the project's file. `now` names a bad file's new name.
    fn load(&mut self, project: &str, now: u64) -> &mut Project {
        let key = project_key(project);
        if !self.projects.contains_key(&key) {
            match self.read_file(&key, now) {
                Read::Project(p) => {
                    self.projects.insert(key.clone(), p.unwrap_or_default());
                }
                Read::Unreadable(why) => {
                    self.unreadable.insert(key.clone(), why);
                    self.projects.insert(key.clone(), Project::default());
                }
            }
        }
        self.projects.entry(key).or_default()
    }

    /// Only a file that does not exist (`NotFound`) is "no threads yet". A file that cannot be
    /// read for another reason (a sharing violation, access denied) is [`Read::Unreadable`]; a file
    /// that is read but is not valid is set aside as `threads.json.bad-<now>`.
    fn read_file(&self, key: &str, now: u64) -> Read {
        let Some(path) = self.file_of(key) else { return Read::Project(None) };
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Read::Project(None),
            // Not UTF-8 is a damaged file, not an unreadable one.
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => String::new(),
            Err(e) => return Read::Unreadable(format!("cannot read {}: {e}", path.display())),
        };
        match serde_json::from_str::<Project>(&text) {
            Ok(p) => Read::Project(Some(p)),
            Err(_) => {
                let bad = path.with_file_name(format!("threads.json.bad-{now}"));
                let _ = fs::rename(&path, bad);
                Read::Project(None)
            }
        }
    }

    /// A copy of the project to change, or why it may not be changed (its file could not be read).
    fn editable(&mut self, project: &str, now: u64) -> Result<Project, StoreError> {
        let key = project_key(project);
        let copy = self.load(project, now).clone();
        match self.unreadable.get(&key) {
            Some(why) => Err(StoreError::Refused(format!("{why}; the threads of this project are left as they are"))),
            None => Ok(copy),
        }
    }

    /// Saves `p` and, only when that worked, makes it the project's state in memory: a failed save
    /// leaves memory and disk as they were.
    fn commit(&mut self, project: &str, p: Project) -> Result<(), StoreError> {
        let key = project_key(project);
        if let Some(path) = self.file_of(&key) {
            let text = serde_json::to_string_pretty(&p).map_err(|e| StoreError::Refused(format!("cannot write the threads: {e}")))?;
            write_atomic(&path, &text).map_err(|e| StoreError::Refused(format!("cannot save the threads: {e}")))?;
        }
        self.projects.insert(key, p);
        Ok(())
    }

    /// Every project id with a file on disk (and those in memory), for a lookup by thread id.
    fn known_projects(&mut self, now: u64) -> Vec<String> {
        let mut ids: Vec<String> = self.projects.values().map(|p| p.project.clone()).collect();
        if let Some(dir) = self.dir.as_ref().map(|d| d.join("projects"))
            && let Ok(rd) = fs::read_dir(dir)
        {
            let keys: Vec<String> = rd.flatten().filter_map(|e| e.file_name().to_str().map(str::to_owned)).take(500).collect();
            for key in keys {
                if self.projects.contains_key(&key) {
                    continue;
                }
                if let Read::Project(Some(p)) = self.read_file(&key, now) {
                    ids.push(p.project.clone());
                    self.projects.insert(key, p);
                }
            }
        }
        ids.sort();
        ids.dedup();
        ids.retain(|i| !i.is_empty());
        ids
    }

    /// The threads of a project (empty when it has none).
    pub fn threads(&mut self, project: &str, now: u64) -> &[Thread] {
        &self.load(project, now).threads
    }

    /// The project holding thread `id`: `project` when given, else a search of every project.
    pub fn locate_project(&mut self, project: Option<&str>, id: &str, now: u64) -> Option<String> {
        let candidates = match project {
            Some(p) => vec![p.to_owned()],
            None => self.known_projects(now),
        };
        candidates.into_iter().find(|p| self.load(p, now).threads.iter().any(|t| t.id == id))
    }

    pub fn thread(&mut self, project: &str, id: &str, now: u64) -> Option<&Thread> {
        self.load(project, now).threads.iter().find(|t| t.id == id)
    }

    fn next_id(&mut self, prefix: &str, now: u64) -> String {
        self.seq += 1;
        format!("{prefix}_{now:011x}{:03x}", self.seq % 4096)
    }

    /// A new thread with its first comment. `root` is the repository folder (for display).
    pub fn create(&mut self, project: &str, root: &str, anchor: Anchor, body: &str, author: Author, now: u64) -> Result<Thread, StoreError> {
        let body = clean_body(body)?;
        let tid = self.next_id("t", now);
        let cid = self.next_id("c", now);
        let mut p = self.editable(project, now)?;
        if p.threads.len() >= MAX_THREADS {
            return Err(StoreError::Refused(format!("this project has {MAX_THREADS} threads; resolve and delete some first")));
        }
        if author.is_agent()
            && let Some(sid) = &author.session_id
        {
            let mine = p.threads.iter().filter(|t| t.comments.first().is_some_and(|c| c.author.session_id.as_ref() == Some(sid))).count();
            if mine >= MAX_AGENT_THREADS {
                return Err(StoreError::Refused(format!("a session may start at most {MAX_AGENT_THREADS} threads")));
            }
        }
        p.version = FILE_VERSION;
        p.project = project.to_owned();
        if !root.is_empty() {
            p.root = root.to_owned();
        }
        let t = Thread { id: tid, status: Status::Open, created_ms: now, updated_ms: now, anchor, comments: vec![Comment { id: cid, author, body, created_ms: now }] };
        p.threads.push(t.clone());
        self.commit(project, p)?;
        Ok(t)
    }

    /// Adds a comment. `status` (when given) sets the status afterwards; otherwise a reply from the
    /// other party reopens an addressed or resolved thread (the person answering an agent's
    /// "addressed", or an agent writing on a resolved thread).
    pub fn reply(&mut self, project: &str, id: &str, body: &str, author: Author, status: Option<Status>, now: u64) -> Result<Thread, StoreError> {
        let body = clean_body(body)?;
        let cid = self.next_id("c", now);
        let mut p = self.editable(project, now)?;
        let t = p.threads.iter_mut().find(|t| t.id == id).ok_or_else(|| StoreError::NotFound(format!("no thread `{id}`")))?;
        if t.comments.len() >= MAX_COMMENTS {
            return Err(StoreError::Refused(format!("a thread holds at most {MAX_COMMENTS} comments")));
        }
        let agent = author.is_agent();
        t.comments.push(Comment { id: cid, author, body, created_ms: now });
        t.updated_ms = now;
        t.status = match (status, t.status, agent) {
            (Some(s), _, _) => s,
            (None, Status::Addressed | Status::Resolved, false) | (None, Status::Resolved, true) => Status::Open,
            (None, s, _) => s,
        };
        let out = t.clone();
        self.commit(project, p)?;
        Ok(out)
    }

    pub fn set_status(&mut self, project: &str, id: &str, status: Status, now: u64) -> Result<Thread, StoreError> {
        let mut p = self.editable(project, now)?;
        let t = p.threads.iter_mut().find(|t| t.id == id).ok_or_else(|| StoreError::NotFound(format!("no thread `{id}`")))?;
        t.status = status;
        t.updated_ms = now;
        let out = t.clone();
        self.commit(project, p)?;
        Ok(out)
    }

    pub fn delete(&mut self, project: &str, id: &str, now: u64) -> Result<(), StoreError> {
        let mut p = self.editable(project, now)?;
        let before = p.threads.len();
        p.threads.retain(|t| t.id != id);
        if p.threads.len() == before {
            return Err(StoreError::NotFound(format!("no thread `{id}`")));
        }
        self.commit(project, p)
    }

    /// The repository folder recorded for a project, if it has threads.
    pub fn root_of(&mut self, project: &str, now: u64) -> Option<String> {
        Some(self.load(project, now).root.clone()).filter(|r| !r.is_empty())
    }

    /// Where the project's file is (for tests and messages).
    #[must_use]
    pub fn path_of(&self, project: &str) -> Option<PathBuf> {
        self.file_of(&project_key(project))
    }
}

/// Writes `text` to `path` through a temp file and a rename, keeping the previous file as `.bak`.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, text)?;
    if path.exists() {
        let _ = fs::copy(path, path.with_extension("json.bak"));
    }
    fs::rename(&tmp, path)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn anchor() -> Anchor {
        Anchor { kind: "lines".into(), path: "src/a.rs".into(), start: 1, end: 2, snippet: "x\ny".into(), ..Anchor::default() }
    }

    fn agent(sid: &str) -> Author {
        Author { kind: "agent".into(), name: "Claude".into(), session_id: Some(sid.into()), client: Some("claude-code".into()) }
    }

    #[test]
    fn threads_survive_a_restart_and_keep_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::new(Some(dir.path().to_path_buf()));
        let t = s.create("repo:C:/x", "C:/x", anchor(), "  look \u{7}here \r\n", Author::person("Allan"), 1000).unwrap();
        assert_eq!(t.comments[0].body, "look here");
        s.reply("repo:C:/x", &t.id, "fixed", agent("s1"), Some(Status::Addressed), 2000).unwrap();
        let mut again = Store::new(Some(dir.path().to_path_buf()));
        let got = again.thread("repo:C:/x", &t.id, 3000).unwrap();
        assert_eq!((got.status, got.comments.len()), (Status::Addressed, 2));
        assert!(s.path_of("repo:C:/x").unwrap().with_extension("json.bak").exists());
        assert!(again.thread("repo:other", &t.id, 3000).is_none(), "another project has its own file");
        // A search without a project finds it on disk.
        let mut fresh = Store::new(Some(dir.path().to_path_buf()));
        assert_eq!(fresh.locate_project(None, &t.id, 4000).as_deref(), Some("repo:C:/x"));
    }

    #[test]
    fn a_broken_file_is_moved_aside_and_the_project_starts_empty() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::new(Some(dir.path().to_path_buf()));
        let path = s.path_of("p").unwrap();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "{ not json").unwrap();
        assert!(s.threads("p", 777).is_empty());
        assert!(path.with_file_name("threads.json.bad-777").exists());
        s.create("p", "", anchor(), "ok", Author::person("A"), 1).unwrap();
        assert_eq!(s.threads("p", 2).len(), 1);
    }

    #[test]
    fn no_data_folder_means_memory_only() {
        let mut s = Store::new(None);
        assert!(!s.persistent());
        let t = s.create("p", "", anchor(), "hello", Author::person("A"), 1).unwrap();
        assert_eq!(s.threads("p", 2).len(), 1);
        s.delete("p", &t.id, 3).unwrap();
        assert!(s.threads("p", 4).is_empty());
        assert!(s.delete("p", &t.id, 5).is_err());
    }

    #[test]
    fn replies_reopen_for_the_other_party_and_the_status_can_be_set_with_the_reply() {
        let mut s = Store::new(None);
        let t = s.create("p", "", anchor(), "fix", Author::person("A"), 1).unwrap();
        let r = s.reply("p", &t.id, "done", agent("s1"), Some(Status::Addressed), 2).unwrap();
        assert_eq!(r.status, Status::Addressed);
        let r = s.reply("p", &t.id, "still wrong", Author::person("A"), None, 3).unwrap();
        assert_eq!(r.status, Status::Open, "the person answering an addressed thread reopens it");
        s.set_status("p", &t.id, Status::Resolved, 4).unwrap();
        let r = s.reply("p", &t.id, "one more thing", agent("s1"), None, 5).unwrap();
        assert_eq!(r.status, Status::Open, "an agent writing on a resolved thread reopens it");
        s.set_status("p", &t.id, Status::Addressed, 6).unwrap();
        let r = s.reply("p", &t.id, "also", agent("s1"), None, 7).unwrap();
        assert_eq!(r.status, Status::Addressed, "an agent adding to its own addressed thread leaves it");
    }

    #[test]
    fn caps_hold() {
        let mut s = Store::new(None);
        assert!(s.create("p", "", anchor(), "   ", Author::person("A"), 1).is_err());
        assert!(s.create("p", "", anchor(), &"x".repeat(MAX_BODY + 1), Author::person("A"), 1).is_err());
        let t = s.create("p", "", anchor(), "x".repeat(MAX_BODY).as_str(), Author::person("A"), 1).unwrap();
        for i in 1..MAX_COMMENTS {
            s.reply("p", &t.id, "c", Author::person("A"), None, 2 + i as u64).unwrap();
        }
        assert!(s.reply("p", &t.id, "one too many", Author::person("A"), None, 9999).is_err());
        for i in 0..MAX_AGENT_THREADS {
            s.create("p", "", anchor(), "n", agent("s9"), 10_000 + i as u64).unwrap();
        }
        assert!(s.create("p", "", anchor(), "n", agent("s9"), 99_999).is_err(), "one session may start 100 threads");
        assert!(s.create("p", "", anchor(), "n", agent("s10"), 99_999).is_ok(), "another session may still");
        assert!(s.create("p", "", anchor(), "n", Author::person("A"), 99_999).is_ok());
    }

    #[test]
    fn ids_are_unique_and_sort_by_time() {
        let mut s = Store::new(None);
        let a = s.create("p", "", anchor(), "a", Author::person("A"), 5).unwrap();
        let b = s.create("p", "", anchor(), "b", Author::person("A"), 5).unwrap();
        let c = s.create("p", "", anchor(), "c", Author::person("A"), 6).unwrap();
        assert!(a.id != b.id && b.id != c.id);
        assert!(a.id < c.id);
        assert_eq!(project_key("repo:C:/x").len(), 16);
        assert_ne!(project_key("a"), project_key("b"));
    }

    #[test]
    fn a_file_that_cannot_be_read_is_not_empty_and_is_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::new(Some(dir.path().to_path_buf()));
        let path = s.path_of("p").unwrap();
        // A folder where the file should be: there, but reading it is an I/O error, not NotFound.
        fs::create_dir_all(path.join("keep")).unwrap();
        assert!(s.threads("p", 1).is_empty());
        let err = s.create("p", "", anchor(), "mine", Author::person("A"), 2).unwrap_err();
        assert!(format!("{err:?}").contains("cannot read"), "{err:?}");
        assert!(s.reply("p", "t_x", "r", Author::person("A"), None, 3).is_err());
        assert!(s.set_status("p", "t_x", Status::Resolved, 4).is_err());
        assert!(s.delete("p", "t_x", 5).is_err());
        assert!(path.is_dir() && path.join("keep").is_dir(), "the real file is untouched");
        assert!(!path.with_file_name("threads.json.bad-1").exists(), "and not moved aside as corrupt");
        // Another project is unaffected.
        assert!(s.create("q", "", anchor(), "fine", Author::person("A"), 6).is_ok());
    }

    #[test]
    fn a_missing_file_is_just_a_project_without_threads() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::new(Some(dir.path().to_path_buf()));
        assert!(s.threads("nothing-yet", 1).is_empty());
        assert!(s.create("nothing-yet", "", anchor(), "first", Author::person("A"), 2).is_ok());
    }

    #[test]
    fn a_failed_save_changes_nothing_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let mut s = Store::new(Some(dir.path().to_path_buf()));
        let first = s.create("p", "", anchor(), "one", Author::person("A"), 1).unwrap();
        // The temp file's name taken by a folder: every save fails (as a full disk would).
        let tmp = s.path_of("p").unwrap().with_extension("json.tmp");
        fs::create_dir(&tmp).unwrap();
        assert!(s.create("p", "", anchor(), "two", Author::person("A"), 2).is_err());
        assert!(s.reply("p", &first.id, "reply", Author::person("A"), Some(Status::Resolved), 3).is_err());
        assert!(s.set_status("p", &first.id, Status::Resolved, 4).is_err());
        assert!(s.delete("p", &first.id, 5).is_err());
        let t = s.thread("p", &first.id, 6).unwrap();
        assert_eq!((t.status, t.comments.len()), (Status::Open, 1), "memory still matches the disk");
        assert_eq!(s.threads("p", 6).len(), 1);
        // The disk recovers: the next change is built on what is really there.
        fs::remove_dir(&tmp).unwrap();
        s.create("p", "", anchor(), "three", Author::person("A"), 7).unwrap();
        let mut again = Store::new(Some(dir.path().to_path_buf()));
        assert_eq!(again.threads("p", 8).len(), 2);
    }
}
