//! Project docs: the `README.md`, `CLAUDE.md`, `AGENTS.md` (and friends) of a session's folder
//! and of the repository it is in, for the inspector's Docs tab and the `project_docs_list` /
//! `project_doc_get` tools.
//!
//! **Blocking:** these functions read the disk. Call them from a worker thread, never from the
//! engine or UI thread.
//!
//! Rules:
//! - Only the names in [`NAMES`] are looked for, case-insensitively on the file name, in the
//!   folder itself and, when the folder is inside a git repository whose top is another folder,
//!   in that top folder too (found by walking up to the nearest `.git`; no git process).
//! - [`read`] reads only a file that [`list`] lists: the requested file is resolved and compared
//!   by canonical path, so `../`, links and other spellings cannot reach anything else.
//! - Text is capped at [`MAX_BYTES`] (`truncated`) and decoded lossily (`lossy`).

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

/// The files looked for, in this order. A `/` means "in that subfolder".
pub const NAMES: [&str; 6] = ["README.md", "CLAUDE.md", "AGENTS.md", "CLAUDE.local.md", "GEMINI.md", ".claude/CLAUDE.md"];

/// The default doc, by preference (then the first found).
pub const PREFERRED: [&str; 3] = ["CLAUDE.md", "AGENTS.md", "README.md"];

/// The most text returned (1 MiB). A bigger file is cut and says so.
pub const MAX_BYTES: u64 = 1024 * 1024;

/// How far up the `.git` search goes.
const MAX_DEPTH: usize = 64;

/// Where a doc was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    /// The session's own folder.
    Folder,
    /// The top of the git repository the folder is in (a different folder).
    RepoRoot,
}

/// One doc found on disk.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DocEntry {
    /// The canonical name it matched (`CLAUDE.md`, `.claude/CLAUDE.md`).
    pub name: String,
    /// Its path relative to the folder, with `/` (`README.md`, `../../CLAUDE.md`).
    pub rel: String,
    /// Its absolute path, as found (the folder joined with the name on disk).
    pub path: String,
    pub scope: Scope,
    pub size: u64,
    /// Last modified, Unix milliseconds (0 when the system does not say).
    pub modified_ms: u64,
}

/// What [`list`] found.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize)]
pub struct DocList {
    pub entries: Vec<DocEntry>,
    /// The git repository's top folder, when it is not the folder itself.
    pub repo_root: Option<String>,
}

impl DocList {
    /// The doc to show first: `CLAUDE.md`, else `AGENTS.md`, else `README.md` (the folder's
    /// before the repository root's), else the first found.
    #[must_use]
    pub fn default_rel(&self) -> Option<&str> {
        for want in PREFERRED {
            for scope in [Scope::Folder, Scope::RepoRoot] {
                if let Some(e) = self.entries.iter().find(|e| e.scope == scope && e.name == want) {
                    return Some(&e.rel);
                }
            }
        }
        self.entries.first().map(|e| e.rel.as_str())
    }
}

/// The text of one doc.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocText {
    pub entry: DocEntry,
    pub text: String,
    /// The file is bigger than [`MAX_BYTES`]: `text` is its start.
    pub truncated: bool,
    /// The file is not valid UTF-8: bad bytes were replaced with U+FFFD.
    pub lossy: bool,
}

/// Why a doc could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocError {
    /// Not one of the listed docs (or not there any more).
    NotListed(String),
    /// The folder is unusable (empty, relative, missing).
    BadFolder(String),
    Io(String),
}

impl std::fmt::Display for DocError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DocError::NotListed(m) | DocError::BadFolder(m) | DocError::Io(m) => f.write_str(m),
        }
    }
}

fn modified_ms(m: &fs::Metadata) -> u64 {
    m.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// A folder that can be read: absolute, an existing directory.
fn check_folder(folder: &str) -> Result<PathBuf, DocError> {
    let f = folder.trim();
    if f.is_empty() || f.chars().any(char::is_control) {
        return Err(DocError::BadFolder("the folder is empty or has control characters".into()));
    }
    let p = PathBuf::from(f);
    if !p.is_absolute() {
        return Err(DocError::BadFolder(format!("`{f}` is not an absolute path")));
    }
    if !p.is_dir() {
        return Err(DocError::BadFolder(format!("{f} is not a folder (or is not reachable)")));
    }
    Ok(p)
}

/// The nearest folder at or above `folder` that holds a `.git` (a folder, or a worktree's file).
fn repo_top(folder: &Path) -> Option<PathBuf> {
    folder.ancestors().take(MAX_DEPTH).find(|a| a.join(".git").exists()).map(Path::to_path_buf)
}

/// The entry in `dir` whose name equals `name` ignoring ASCII case (the first, by name, when a
/// case-sensitive file system has several).
fn find_ci(dir: &Path, name: &str, want_dir: bool) -> Option<(PathBuf, fs::Metadata)> {
    let rd = fs::read_dir(dir).ok()?;
    let mut hits: Vec<(String, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let n = e.file_name().to_str()?.to_owned();
            n.eq_ignore_ascii_case(name).then(|| (n, e.path()))
        })
        .collect();
    hits.sort();
    hits.into_iter().find_map(|(_, p)| {
        let m = fs::metadata(&p).ok()?;
        (if want_dir { m.is_dir() } else { m.is_file() }).then_some((p, m))
    })
}

/// `to` relative to `from` (both absolute, lexically), with `/`.
fn relative(from: &Path, to: &Path) -> String {
    let f: Vec<Component<'_>> = from.components().collect();
    let t: Vec<Component<'_>> = to.components().collect();
    let common = f.iter().zip(&t).take_while(|(a, b)| a == b).count();
    let ups = f.len().saturating_sub(common);
    let mut parts: Vec<String> = std::iter::repeat_n("..".to_owned(), ups).collect();
    parts.extend(t.iter().skip(common).map(|c| c.as_os_str().to_string_lossy().into_owned()));
    parts.join("/")
}

fn docs_in(dir: &Path, base: &Path, scope: Scope, out: &mut Vec<DocEntry>) {
    for name in NAMES {
        let found = match name.split_once('/') {
            Some((sub, file)) => find_ci(dir, sub, true).and_then(|(d, _)| find_ci(&d, file, false)),
            None => find_ci(dir, name, false),
        };
        if let Some((path, meta)) = found {
            out.push(DocEntry {
                name: name.to_owned(),
                rel: relative(base, &path),
                path: path.to_string_lossy().into_owned(),
                scope,
                size: meta.len(),
                modified_ms: modified_ms(&meta),
            });
        }
    }
}

/// The docs of `folder` (and of its repository's top folder). Blocking.
pub fn list(folder: &str) -> Result<DocList, DocError> {
    let base = check_folder(folder)?;
    let mut entries = Vec::new();
    docs_in(&base, &base, Scope::Folder, &mut entries);
    let canon = |p: &Path| fs::canonicalize(p).ok();
    let root = repo_top(&base).filter(|r| canon(r) != canon(&base));
    if let Some(r) = &root {
        docs_in(r, &base, Scope::RepoRoot, &mut entries);
    }
    Ok(DocList { entries, repo_root: root.map(|r| r.to_string_lossy().into_owned()) })
}

/// The text of `file` (a listed doc's `rel`, `path` or `name`), refusing anything [`list`]
/// does not list. Blocking.
pub fn read(folder: &str, file: &str) -> Result<DocText, DocError> {
    let listed = list(folder)?;
    let f = file.trim();
    let not_listed = || DocError::NotListed(format!("`{f}` is not one of this folder's docs ({}); project_docs_list shows them", NAMES.join(", ")));
    if f.is_empty() || f.chars().any(char::is_control) {
        return Err(not_listed());
    }
    let base = PathBuf::from(folder.trim());
    let asked = Path::new(f);
    let asked = if asked.is_absolute() { asked.to_path_buf() } else { base.join(asked) };
    let asked = fs::canonicalize(&asked).map_err(|_| not_listed())?;
    let entry = listed.entries.into_iter().find(|e| fs::canonicalize(&e.path).ok().as_deref() == Some(asked.as_path())).ok_or_else(not_listed)?;
    let file = fs::File::open(&asked).map_err(|e| DocError::Io(format!("could not open {}: {e}", entry.rel)))?;
    let meta = file.metadata().map_err(|e| DocError::Io(format!("could not read {}: {e}", entry.rel)))?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes).map_err(|e| DocError::Io(format!("could not read {}: {e}", entry.rel)))?;
    let truncated = bytes.len() as u64 > MAX_BYTES;
    bytes.truncate(usize::try_from(MAX_BYTES).unwrap_or(usize::MAX));
    let (text, lossy) = decode(&bytes, truncated);
    let entry = DocEntry { size: meta.len(), modified_ms: modified_ms(&meta), ..entry };
    Ok(DocText { entry, text, truncated, lossy })
}

/// UTF-8 text, lossily. A cut file may end in the middle of a character: that tail is dropped
/// rather than counted as a bad byte.
fn decode(bytes: &[u8], cut: bool) -> (String, bool) {
    let mut b = bytes;
    if cut
        && let Err(e) = std::str::from_utf8(b)
        && e.error_len().is_none()
    {
        b = b.get(..e.valid_up_to()).unwrap_or(b);
    }
    match std::str::from_utf8(b) {
        Ok(s) => (s.to_owned(), false),
        Err(_) => (String::from_utf8_lossy(b).into_owned(), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(p: &Path, s: &str) {
        if let Some(d) = p.parent() {
            fs::create_dir_all(d).unwrap();
        }
        fs::write(p, s).unwrap();
    }

    #[test]
    fn finds_the_names_in_order_ignoring_case() {
        let d = tempfile::tempdir().unwrap();
        write(&d.path().join("agents.md"), "a");
        write(&d.path().join("Readme.MD"), "r");
        write(&d.path().join(".Claude/claude.md"), "c");
        write(&d.path().join("NOTES.md"), "n");
        let l = list(d.path().to_str().unwrap()).unwrap();
        let names: Vec<&str> = l.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["README.md", "AGENTS.md", ".claude/CLAUDE.md"]);
        assert_eq!(l.entries[0].rel, "Readme.MD", "the name on disk");
        assert_eq!(l.entries[2].rel, ".Claude/claude.md");
        assert_eq!(l.default_rel(), Some("agents.md"));
        assert!(l.repo_root.is_none());
    }

    #[test]
    fn a_subfolder_of_a_repo_also_lists_the_root() {
        let d = tempfile::tempdir().unwrap();
        fs::create_dir_all(d.path().join(".git")).unwrap();
        write(&d.path().join("CLAUDE.md"), "root");
        let sub = d.path().join("crates/x");
        write(&sub.join("README.md"), "sub");
        let l = list(sub.to_str().unwrap()).unwrap();
        let rels: Vec<(&str, Scope)> = l.entries.iter().map(|e| (e.rel.as_str(), e.scope)).collect();
        assert_eq!(rels, [("README.md", Scope::Folder), ("../../CLAUDE.md", Scope::RepoRoot)]);
        assert_eq!(l.default_rel(), Some("../../CLAUDE.md"), "CLAUDE.md wins, wherever it is");
        // At the root itself, nothing is listed twice.
        let l = list(d.path().to_str().unwrap()).unwrap();
        assert_eq!(l.entries.len(), 1);
        assert!(l.repo_root.is_none());
    }

    #[test]
    fn read_refuses_anything_not_listed() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("proj");
        write(&f.join("README.md"), "# Hi");
        write(&d.path().join("secret.txt"), "no");
        write(&f.join("other.md"), "no");
        let folder = f.to_str().unwrap();
        assert_eq!(read(folder, "README.md").unwrap().text, "# Hi");
        for bad in ["../secret.txt", "other.md", "./../proj/../secret.txt", "", "nope.md", d.path().join("secret.txt").to_str().unwrap()] {
            assert!(matches!(read(folder, bad), Err(DocError::NotListed(_))), "{bad}");
        }
        assert_eq!(read(folder, f.join("README.md").to_str().unwrap()).unwrap().text, "# Hi", "an absolute path to a listed doc");
        assert!(matches!(read("relative/path", "README.md"), Err(DocError::BadFolder(_))));
    }

    #[test]
    fn big_and_non_utf8_files() {
        let d = tempfile::tempdir().unwrap();
        let big = "é".repeat(usize::try_from(MAX_BYTES).unwrap()); // 2 MiB of 2-byte characters
        write(&d.path().join("README.md"), &big);
        let t = read(d.path().to_str().unwrap(), "README.md").unwrap();
        assert!(t.truncated && !t.lossy, "cut on a character boundary");
        assert_eq!(t.text.len() as u64, MAX_BYTES);
        assert_eq!(t.entry.size, MAX_BYTES * 2);
        fs::write(d.path().join("CLAUDE.md"), b"ok \xff\xfe bad").unwrap();
        let t = read(d.path().to_str().unwrap(), "CLAUDE.md").unwrap();
        assert!(t.lossy && !t.truncated);
        assert!(t.text.contains('\u{FFFD}') && t.text.starts_with("ok "));
    }

    #[test]
    fn relative_paths() {
        assert_eq!(relative(Path::new("/a/b/c"), Path::new("/a/README.md")), "../../README.md");
        assert_eq!(relative(Path::new("/a"), Path::new("/a/.claude/CLAUDE.md")), ".claude/CLAUDE.md");
    }
}
