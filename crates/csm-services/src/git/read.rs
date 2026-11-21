//! Read-only git for viewing: a diff, one file at a revision (or in the working tree) and the file
//! list. These back the `git_diff`, `git_show_file` and `git_ls_files` tools, which the Review
//! add-on calls and agents may call.
//!
//! The first half is pure (argv builders, validators, the `--name-status -z` parser); the second
//! half spawns git and blocks, so call it from a worker thread.
//!
//! Rules this carries:
//! * Argv only. Every caller value is one element. `--literal-pathspecs` is a global option so a
//!   path like `:(top)x` or `*.rs` means exactly that file; paths go after `--`.
//! * `--no-ext-diff`, `--no-textconv` and an empty `diff.external`: viewing a diff must never run
//!   a program a repository's own config names. `cat-file blob` applies no textconv either.
//! * Revisions are checked (no leading `-`, no whitespace or control characters) before they are an
//!   argv element, and a file path is repository-relative with no `..`, drive or leading `/`.
//! * A working-tree read stays inside the repository after canonicalising, and refuses a file git
//!   ignores (so `.env` is not one tool call away).
//! * Output is capped; a cut is reported (`truncated`), never silent.

use std::io::Read;
use std::path::{Component, PathBuf};
use std::time::Duration;

use super::run::{self, GitRun, QUERY_TIMEOUT};

/// Largest `max_bytes` a caller may ask for: the git runner keeps at most this much output.
pub const MAX_BYTES_LIMIT: usize = run::OUTPUT_CAP;
/// What a patch or a file is cut at when the caller names no limit.
pub const DEFAULT_MAX_BYTES: usize = 512 * 1024;
/// Most paths a diff may be restricted to.
pub const MAX_PATHS: usize = 64;
/// Most entries returned in a file list.
pub const MAX_FILES: usize = 100_000;
/// Most changed files a diff lists.
pub const MAX_DIFF_FILES: usize = 20_000;
/// A diff or listing over a big repository can take a while.
pub const DIFF_TIMEOUT: Duration = Duration::from_secs(30);

/// Which changes `git_diff` shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiffSource {
    /// The working tree against HEAD (staged and unstaged together).
    Working,
    /// The index against HEAD.
    Staged,
    /// `base...head`: what `head` has that `base` does not (since their merge-base).
    Range,
}

impl DiffSource {
    #[must_use]
    pub fn parse(s: &str) -> Option<DiffSource> {
        match s {
            "working" => Some(DiffSource::Working),
            "staged" => Some(DiffSource::Staged),
            "range" => Some(DiffSource::Range),
            _ => None,
        }
    }
}

/// One changed file from `--name-status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangedFile {
    /// `M`, `A`, `D`, `R`, `C`, `T` or `U`.
    pub status: char,
    pub path: String,
    pub old_path: Option<String>,
}

// ── Validation ────────────────────────────────────────────────────────────────────────────

/// A revision expression that is safe as one argv element: a name, a sha, `HEAD~2`, `@{u}`.
/// `Err` says why not.
pub fn check_rev(rev: &str) -> Result<(), String> {
    if rev.is_empty() || rev.len() > 256 {
        return Err("a revision must be 1 to 256 characters".into());
    }
    if rev.starts_with('-') {
        return Err("a revision cannot start with `-`".into());
    }
    if rev.contains("..") {
        return Err("a revision cannot contain `..` (give `base` and `head` separately)".into());
    }
    let ok = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '/' | '~' | '^' | '@' | '{' | '}' | '-' | '+');
    if !rev.chars().all(ok) {
        return Err("a revision may contain letters, digits and _ . / ~ ^ @ { } - + only".into());
    }
    Ok(())
}

/// A repository-relative path with forward slashes: not empty, no `..`, not absolute, no drive,
/// no control characters or backslashes. Returns it normalised (no `./`, no doubled slashes).
pub fn check_repo_path(path: &str) -> Result<String, String> {
    if path.is_empty() || path.len() > 4096 {
        return Err("a path must be 1 to 4096 characters".into());
    }
    if path.chars().any(char::is_control) {
        return Err("a path cannot contain control characters".into());
    }
    if path.contains('\\') {
        return Err("a path is repository-relative with forward slashes (no `\\`)".into());
    }
    if path.starts_with('/') || path.as_bytes().get(1) == Some(&b':') {
        return Err("a path is relative to the repository root, not absolute".into());
    }
    let mut parts = Vec::new();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("a path cannot contain `..`".into()),
            p if p.eq_ignore_ascii_case(".git") => return Err("a path cannot go through `.git`".into()),
            p => parts.push(p),
        }
    }
    if parts.is_empty() {
        return Err("a path names no file".into());
    }
    Ok(parts.join("/"))
}

// ── Argv builders ─────────────────────────────────────────────────────────────────────────

fn base(sub: &str) -> Vec<String> {
    ["--literal-pathspecs", "-c", "core.quotePath=false", "-c", "diff.external=", "-c", "color.ui=false", sub].map(String::from).to_vec()
}

fn diff_common(sub_flags: &[&str], source: DiffSource, have_head: bool, rbase: Option<&str>, rhead: Option<&str>, paths: &[String]) -> Vec<String> {
    let mut a = base("diff");
    a.extend(["--no-color", "--no-ext-diff", "--no-textconv", "--src-prefix=a/", "--dst-prefix=b/", "-M"].map(String::from));
    a.extend(sub_flags.iter().map(|s| (*s).to_owned()));
    match source {
        DiffSource::Working if have_head => a.push("HEAD".into()),
        // An unborn branch has no HEAD to compare to: the index is all there is.
        DiffSource::Working => a.push("--cached".into()),
        DiffSource::Staged => a.push("--cached".into()),
        DiffSource::Range => a.push(format!("{}...{}", rbase.unwrap_or("HEAD"), rhead.unwrap_or("HEAD"))),
    }
    if source == DiffSource::Staged && have_head {
        a.push("HEAD".into());
    }
    a.push("--".into());
    a.extend(paths.iter().cloned());
    a
}

/// `git diff` producing the patch.
#[must_use]
pub fn diff_patch_args(source: DiffSource, have_head: bool, rbase: Option<&str>, rhead: Option<&str>, context: u32, paths: &[String]) -> Vec<String> {
    let unified = format!("--unified={}", context.min(20));
    diff_common(&[&unified], source, have_head, rbase, rhead, paths)
}

/// `git diff --name-status -z`: the changed files, one record each.
#[must_use]
pub fn diff_names_args(source: DiffSource, have_head: bool, rbase: Option<&str>, rhead: Option<&str>, paths: &[String]) -> Vec<String> {
    diff_common(&["--name-status", "-z"], source, have_head, rbase, rhead, paths)
}

/// Untracked, not-ignored files (`-z`).
#[must_use]
pub fn untracked_args(paths: &[String]) -> Vec<String> {
    let mut a = base("ls-files");
    a.extend(["--others", "--exclude-standard", "-z", "--"].map(String::from));
    a.extend(paths.iter().cloned());
    a
}

/// Tracked files in the index, or in the tree of `rev`.
#[must_use]
pub fn ls_files_args(rev: Option<&str>) -> Vec<String> {
    match rev {
        None => {
            let mut a = base("ls-files");
            a.push("-z".into());
            a
        }
        Some(r) => {
            let mut a = base("ls-tree");
            a.extend(["-r", "-z", "--name-only"].map(String::from));
            a.push(r.to_owned());
            a
        }
    }
}

/// The object `<rev>:<path>` names (`:0:<path>` is the index).
#[must_use]
pub fn object_spec(rev: &str, path: &str) -> String {
    if rev == ":0" || rev == "index" { format!(":0:{path}") } else { format!("{rev}:{path}") }
}

/// Resolve a revision or object spec to an id, quietly.
#[must_use]
pub fn rev_parse_args(spec: &str) -> Vec<String> {
    let mut a = base("rev-parse");
    a.extend(["--verify", "--quiet"].map(String::from));
    a.push(spec.to_owned());
    a
}

/// A blob's content by id. No textconv, no filters.
#[must_use]
pub fn cat_blob_args(id: &str) -> Vec<String> {
    let mut a = base("cat-file");
    a.extend(["blob".to_owned(), id.to_owned()]);
    a
}

// ── Parsing ───────────────────────────────────────────────────────────────────────────────

/// `--name-status -z`: `M\0path\0`, and for a rename or copy `R100\0old\0new\0`. Unknown or
/// broken records are skipped; at most `cap` files.
#[must_use]
pub fn parse_name_status(raw: &str, cap: usize) -> Vec<ChangedFile> {
    let mut out = Vec::new();
    let mut it = raw.split('\0');
    while let Some(tok) = it.next() {
        if out.len() >= cap {
            break;
        }
        let Some(status) = tok.chars().next() else { continue };
        match status {
            'R' | 'C' => {
                let (Some(old), Some(new)) = (it.next(), it.next()) else { break };
                if !new.is_empty() {
                    out.push(ChangedFile { status, path: new.to_owned(), old_path: Some(old.to_owned()) });
                }
            }
            'M' | 'A' | 'D' | 'T' | 'U' => {
                let Some(p) = it.next() else { break };
                if !p.is_empty() {
                    out.push(ChangedFile { status, path: p.to_owned(), old_path: None });
                }
            }
            _ => {}
        }
    }
    out
}

/// A NUL-separated path list (`ls-files -z`, `ls-tree -z --name-only`), capped.
#[must_use]
pub fn parse_nul_list(raw: &str, cap: usize) -> Vec<String> {
    raw.split('\0').filter(|s| !s.is_empty()).take(cap).map(str::to_owned).collect()
}

/// `s` cut to at most `max` bytes at a character boundary; `true` when something was cut.
#[must_use]
pub fn cut_at_char(s: &str, max: usize) -> (&str, bool) {
    if s.len() <= max {
        return (s, false);
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    (s.get(..end).unwrap_or(""), true)
}

/// Git reads a file as binary when its first 8000 bytes hold a NUL.
#[must_use]
pub fn looks_binary(s: &str) -> bool {
    let probe = cut_at_char(s, 8000).0;
    probe.contains('\0')
}

// ── Running ───────────────────────────────────────────────────────────────────────────────

fn fail(what: &str, r: &GitRun, timeout: Duration) -> String {
    let mut out = r.output();
    if r.timed_out {
        out.push_str(&format!("\ngit did not finish within {}s and was stopped.", timeout.as_secs()));
    }
    run::failure_message(what, &out)
}

/// The result of [`diff`].
#[derive(Debug, Clone, Default)]
pub struct DiffOut {
    /// The commit HEAD names (`None` on an unborn branch).
    pub head: Option<String>,
    pub files: Vec<ChangedFile>,
    pub files_truncated: bool,
    /// The patch, cut at `max_bytes` (empty when not asked for).
    pub patch: String,
    pub patch_truncated: bool,
    /// Untracked, not-ignored files (working source only).
    pub untracked: Vec<String>,
}

/// What [`diff`] was asked for.
pub struct DiffRequest<'a> {
    pub source: DiffSource,
    pub base: Option<&'a str>,
    pub head: Option<&'a str>,
    pub paths: &'a [String],
    pub context: u32,
    pub want_patch: bool,
    pub max_bytes: usize,
}

/// The head commit id, if there is one.
#[must_use]
pub fn head_id(folder: &str) -> Option<String> {
    let r = run::run_git(folder, &rev_parse_args("HEAD"), QUERY_TIMEOUT, true);
    r.ok().then(|| r.stdout.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Blocks. A diff of `folder`'s repository.
pub fn diff(folder: &str, req: &DiffRequest<'_>) -> Result<DiffOut, String> {
    // Paths are relative to the repository, whatever folder inside it was given.
    let top = toplevel(folder)?;
    let folder = top.as_str();
    let head = head_id(folder);
    let have_head = head.is_some();
    let names = run::run_git(folder, &diff_names_args(req.source, have_head, req.base, req.head, req.paths), DIFF_TIMEOUT, true);
    if !names.ok() {
        return Err(fail("Listing the changes", &names, DIFF_TIMEOUT));
    }
    let mut files = parse_name_status(&names.stdout, MAX_DIFF_FILES + 1);
    let files_truncated = files.len() > MAX_DIFF_FILES;
    files.truncate(MAX_DIFF_FILES);
    let mut out = DiffOut { head, files, files_truncated, ..DiffOut::default() };
    if req.want_patch {
        let r = run::run_git(folder, &diff_patch_args(req.source, have_head, req.base, req.head, req.context, req.paths), DIFF_TIMEOUT, true);
        if !r.ok() {
            return Err(fail("Reading the diff", &r, DIFF_TIMEOUT));
        }
        let raw_full = r.stdout.len() >= run::OUTPUT_CAP;
        let (cut, was_cut) = cut_at_char(&r.stdout, req.max_bytes.clamp(1, MAX_BYTES_LIMIT));
        out.patch = cut.to_owned();
        out.patch_truncated = was_cut || raw_full;
    }
    if req.source == DiffSource::Working {
        let u = run::run_git(folder, &untracked_args(req.paths), DIFF_TIMEOUT, true);
        if u.ok() {
            out.untracked = parse_nul_list(&u.stdout, MAX_DIFF_FILES);
        }
    }
    Ok(out)
}

/// The result of [`show_file`].
#[derive(Debug, Clone, Default)]
pub struct FileOut {
    pub blob: String,
    pub size: usize,
    pub binary: bool,
    pub truncated: bool,
    /// Empty for a binary file.
    pub text: String,
}

/// The working tree's top folder, as a path on this machine.
fn toplevel(folder: &str) -> Result<String, String> {
    if !run::folder_usable(folder) {
        return Err(format!("{folder} is not a usable folder"));
    }
    let r = run::run_git(folder, &["rev-parse".to_owned(), "--show-toplevel".to_owned()], QUERY_TIMEOUT, true);
    if !r.ok() {
        return Err(format!("{folder} is not in a git repository"));
    }
    let out = run::translate_out(folder, r.stdout.trim());
    if out.is_empty() { Err("git did not say where the repository is".into()) } else { Ok(run::windows_form(&out)) }
}

/// Blocks. One file: at `rev` (`HEAD`, a sha, `:0` or `index` for the index) or, with no `rev`,
/// as it is in the working tree. `path` must already have passed [`check_repo_path`], and `rev`
/// [`check_rev`] when it is not `:0`.
pub fn show_file(folder: &str, rev: Option<&str>, path: &str, max_bytes: usize) -> Result<FileOut, String> {
    let max = max_bytes.clamp(1, MAX_BYTES_LIMIT);
    let top = toplevel(folder)?;
    let folder = top.as_str();
    match rev {
        Some(rev) => {
            let spec = object_spec(rev, path);
            let id = run::run_git(folder, &rev_parse_args(&spec), QUERY_TIMEOUT, true);
            let blob = id.stdout.trim().to_owned();
            if !id.ok() || blob.is_empty() {
                return Err(format!("`{path}` is not in {rev}"));
            }
            let r = run::run_git(folder, &cat_blob_args(&blob), DIFF_TIMEOUT, true);
            if !r.ok() {
                return Err(fail(&format!("Reading `{path}`"), &r, DIFF_TIMEOUT));
            }
            let full = r.stdout.len() >= run::OUTPUT_CAP;
            Ok(file_out(blob, &r.stdout, max, full))
        }
        None => {
            let top = PathBuf::from(folder);
            let ignored = run::run_git(folder, &["check-ignore".to_owned(), "-q".to_owned(), "--".to_owned(), path.to_owned()], QUERY_TIMEOUT, true);
            match ignored.code {
                Some(0) => return Err(format!("`{path}` is ignored by git; this tool does not read ignored files")),
                Some(1) => {}
                // Anything else (git failed, timed out): not knowing is a refusal, not a pass.
                _ => return Err(format!("could not check whether git ignores `{path}`; not read")),
            }
            let real_top = std::fs::canonicalize(&top).map_err(|e| format!("cannot open the repository folder: {e}"))?;
            let file = std::fs::canonicalize(top.join(path)).map_err(|_| format!("`{path}` does not exist in the working tree"))?;
            if !file.starts_with(&real_top) || file.components().any(|c| matches!(c, Component::ParentDir)) {
                return Err(format!("`{path}` resolves outside the repository; refused"));
            }
            // Whatever the spelling (8.3 names, links), nothing under the repository .git folder is read.
            let inside_git = file.strip_prefix(&real_top).ok().and_then(|r| r.components().next()).is_some_and(|c| c.as_os_str().eq_ignore_ascii_case(".git"));
            if inside_git {
                return Err(format!("`{path}` is inside .git; refused"));
            }
            let meta = std::fs::metadata(&file).map_err(|e| format!("cannot read `{path}`: {e}"))?;
            if !meta.is_file() {
                return Err(format!("`{path}` is not a file"));
            }
            let mut buf = Vec::new();
            std::fs::File::open(&file)
                .and_then(|f| f.take(max as u64 + 1).read_to_end(&mut buf))
                .map_err(|e| format!("cannot read `{path}`: {e}"))?;
            let text = String::from_utf8_lossy(&buf).into_owned();
            let hashed = run::run_git(folder, &["hash-object".to_owned(), "--no-filters".to_owned(), "--".to_owned(), run::strip_verbatim(&file.to_string_lossy())], QUERY_TIMEOUT, true);
            let blob = if hashed.ok() { hashed.stdout.trim().to_owned() } else { String::new() };
            let mut out = file_out(blob, &text, max, false);
            out.size = usize::try_from(meta.len()).unwrap_or(usize::MAX);
            out.truncated = out.truncated || out.size > max;
            Ok(out)
        }
    }
}

fn file_out(blob: String, text: &str, max: usize, full: bool) -> FileOut {
    let size = text.len();
    if looks_binary(text) {
        return FileOut { blob, size, binary: true, truncated: false, text: String::new() };
    }
    let (cut, was_cut) = cut_at_char(text, max);
    FileOut { blob, size, binary: false, truncated: was_cut || full, text: cut.to_owned() }
}

/// Blocks. Tracked files (index, or the tree of `rev`), capped at [`MAX_FILES`]; `true` when cut.
pub fn ls_files(folder: &str, rev: Option<&str>) -> Result<(Vec<String>, bool), String> {
    let top = toplevel(folder)?;
    let folder = top.as_str();
    let r = run::run_git(folder, &ls_files_args(rev), DIFF_TIMEOUT, true);
    if !r.ok() {
        return Err(fail("Listing files", &r, DIFF_TIMEOUT));
    }
    let mut files = parse_nul_list(&r.stdout, MAX_FILES + 1);
    let cut = files.len() > MAX_FILES || r.stdout.len() >= run::OUTPUT_CAP;
    files.truncate(MAX_FILES);
    Ok((files, cut))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn revs_that_could_be_options_or_ranges_are_refused() {
        for ok in ["HEAD", "HEAD~2", "origin/main", "a1b2c3d", "@{u}", "v1.2.3", "feat/x_y"] {
            assert!(check_rev(ok).is_ok(), "{ok}");
        }
        for bad in ["", "-x", "--output=/tmp/x", "a..b", "a b", "a;b", "a\nb", "a:b", "$(x)", "a`b`"] {
            assert!(check_rev(bad).is_err(), "{bad:?}");
        }
        assert!(check_rev(&"x".repeat(300)).is_err());
    }

    #[test]
    fn paths_are_repository_relative_and_normalised() {
        assert_eq!(check_repo_path("src/./a//b.rs").unwrap(), "src/a/b.rs");
        for bad in ["", "/etc/passwd", "C:/x", "C:x", "../x", "a/../../x", "a\\b", "a\nb", ".", "./", "\u{7}", ".git/config", "a/.GIT/x"] {
            assert!(check_repo_path(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn diff_argv_never_runs_external_programs_and_keeps_paths_after_dashes() {
        let a = diff_patch_args(DiffSource::Working, true, None, None, 50, &["a b.rs".into(), "--evil".into()]);
        let line = a.join(" ");
        assert!(line.contains("--no-ext-diff") && line.contains("--no-textconv") && line.contains("diff.external="));
        assert!(line.contains("--literal-pathspecs"));
        assert!(line.contains("--unified=20"), "context is clamped: {line}");
        let dashes = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(&a[dashes + 1..], ["a b.rs", "--evil"]);
        assert!(a[..dashes].iter().all(|x| x != "--evil" && x != "a b.rs"));
        let range = diff_names_args(DiffSource::Range, true, Some("main"), Some("feat"), &[]);
        assert!(range.contains(&"main...feat".to_string()));
        let staged = diff_names_args(DiffSource::Staged, true, None, None, &[]);
        assert!(staged.contains(&"--cached".to_string()) && staged.contains(&"HEAD".to_string()));
        let unborn = diff_names_args(DiffSource::Working, false, None, None, &[]);
        assert!(unborn.contains(&"--cached".to_string()) && !unborn.contains(&"HEAD".to_string()));
    }

    #[test]
    fn specs_name_the_index_or_a_revision() {
        assert_eq!(object_spec("HEAD", "a/b.rs"), "HEAD:a/b.rs");
        assert_eq!(object_spec(":0", "a/b.rs"), ":0:a/b.rs");
        assert_eq!(object_spec("index", "x"), ":0:x");
    }

    #[test]
    fn name_status_handles_renames_and_garbage() {
        let raw = "M\0src/a.rs\0A\0new file.txt\0R087\0old.rs\0new.rs\0D\0gone.rs\0Z\0weird\0C100\0x";
        let files = parse_name_status(raw, 100);
        assert_eq!(files.len(), 4);
        assert_eq!(files[2], ChangedFile { status: 'R', path: "new.rs".into(), old_path: Some("old.rs".into()) });
        assert_eq!(parse_name_status(raw, 2).len(), 2);
        assert!(parse_name_status("", 10).is_empty());
        assert!(parse_name_status("\0\0\0M", 10).is_empty());
    }

    #[test]
    fn cutting_respects_characters() {
        assert_eq!(cut_at_char("héllo", 2), ("h", true));
        assert_eq!(cut_at_char("héllo", 3), ("hé", true));
        assert_eq!(cut_at_char("abc", 10), ("abc", false));
        assert_eq!(cut_at_char("é", 0), ("", true));
        assert!(looks_binary("a\0b") && !looks_binary("plain"));
    }
}
