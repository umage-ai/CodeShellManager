//! Running git: the I/O half of [`crate::git`]. **Every function here blocks**: call it from a
//! worker thread, never from the UI thread or the engine thread.
//!
//! The rules carried over from the WPF `GitService` (its CLAUDE.md, "Never spawn a process on the
//! UI thread" and "Never interpolate a value into a command line"):
//!
//! * The command line is built by the pure builders ([`super::build_local_git_command_line`],
//!   [`super::build_wsl_git_command_line`]) as argv and the program is exec'd directly. Under
//!   WSL that is `wsl.exe -d <distro> -e sh -lc '<fixed script>' git -C <linux> ...`: no value
//!   ever reaches a shell.
//! * `GIT_TERMINAL_PROMPT=0` always, `GIT_OPTIONAL_LOCKS=0` for reads ([`super::git_environment`]);
//!   under WSL both are added to `WSLENV`, or the distro's git never sees them.
//! * No console window (`CREATE_NO_WINDOW`), stdin closed, output capped, a deadline, and on
//!   timeout the **whole process tree** is killed: a fetch's real work happens in children
//!   (`git-remote-https`, `ssh`) that killing `git.exe` alone left running.
//! * Paths are compared in their real form ([`canonical`]): an 8.3 or junction spelling of a
//!   worktree folder is the same folder as git's long path.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use super::status::{GitOpResult, GitRepoStatus, parse_status_v2};
use super::worktrees::{self, WorktreeInfo};
use super::{args, text};
use crate::wsl;

/// Most of git's stdout or stderr we keep (the rest is read and dropped so git never blocks).
pub const OUTPUT_CAP: usize = 4 << 20;
/// The poll's status call. Process start is ~90% of it; the limit is for a wedged repo.
pub const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
/// Small read-only queries (rev-parse, for-each-ref, worktree list).
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(10);
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(60);
pub const PULL_TIMEOUT: Duration = Duration::from_secs(120);
pub const SWITCH_TIMEOUT: Duration = Duration::from_secs(30);
pub const WORKTREE_TIMEOUT: Duration = Duration::from_secs(60);
/// A nested-repository scan gives up (and the caller refuses) past this many folders.
pub const NESTED_SCAN_MAX_DIRS: usize = 500_000;

/// What one git process did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GitRun {
    pub stdout: String,
    pub stderr: String,
    /// `None` when it never started, was killed, or ended without a code.
    pub code: Option<i32>,
    pub timed_out: bool,
    /// Why it could not be started at all (git or wsl.exe missing, ...).
    pub spawn_error: Option<String>,
}

impl GitRun {
    #[must_use]
    pub fn ok(&self) -> bool {
        !self.timed_out && self.spawn_error.is_none() && self.code == Some(0)
    }

    /// stdout and stderr joined, the way the failure panel shows it.
    #[must_use]
    pub fn output(&self) -> String {
        let mut out = text::combine_output(&self.stdout, &self.stderr);
        if let Some(e) = &self.spawn_error {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(e);
        }
        out
    }
}

/// Where a session folder's git runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitTarget {
    /// A Windows (or Unix) folder; git runs as `git -C <folder>`.
    Local(String),
    /// A `\\wsl$\<distro>\...` folder; git runs inside the distro at the Linux path.
    Wsl { distro: String, linux: String },
}

impl GitTarget {
    /// The target for a session folder. `None` for an empty folder.
    #[must_use]
    pub fn of(folder: &str) -> Option<GitTarget> {
        let folder = folder.trim();
        if folder.is_empty() {
            return None;
        }
        match wsl::try_parse_unc_path(folder) {
            (Some(distro), linux) if !distro.is_empty() => {
                let linux = if linux.is_empty() { "/".to_string() } else { linux };
                Some(GitTarget::Wsl { distro, linux })
            }
            _ => Some(GitTarget::Local(folder.to_string())),
        }
    }

    #[must_use]
    pub fn is_wsl(&self) -> bool {
        matches!(self, GitTarget::Wsl { .. })
    }
}

/// `true` for a `\\wsl$\` / `\\wsl.localhost\` folder (polled less often; never watched).
#[must_use]
pub fn is_wsl_folder(folder: &str) -> bool {
    GitTarget::of(folder).is_some_and(|t| t.is_wsl())
}

#[cfg(windows)]
fn system32(exe: &str) -> PathBuf {
    let root = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
    root.join("System32").join(exe)
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The command for one git invocation. Argv only; see the module docs.
fn command(target: &GitTarget, git_args: &[String], read_only: bool) -> Result<Command, String> {
    let refs: Vec<&str> = git_args.iter().map(String::as_str).collect();
    let env = args::git_environment(read_only);
    match target {
        GitTarget::Local(dir) => {
            let mut c = Command::new("git");
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                // MSVCRT-quoted per element by the builder: exactly the line the WPF app ran.
                c.raw_arg(args::build_local_git_command_line(dir, &refs));
            }
            #[cfg(not(windows))]
            {
                c.arg("-C").arg(dir).args(&refs);
            }
            for (k, v) in env {
                c.env(k, v);
            }
            Ok(c)
        }
        #[cfg(windows)]
        GitTarget::Wsl { distro, linux } => {
            use std::os::windows::process::CommandExt;
            let mut c = Command::new(system32("wsl.exe"));
            c.raw_arg(args::build_wsl_git_command_line(distro, linux, &refs));
            let names: Vec<&str> = env.iter().map(|(k, _)| *k).collect();
            let existing = std::env::var("WSLENV").ok();
            c.env("WSLENV", args::merge_wsl_env(existing.as_deref(), &names));
            for (k, v) in env {
                c.env(k, v);
            }
            Ok(c)
        }
        #[cfg(not(windows))]
        GitTarget::Wsl { .. } => Err("WSL folders exist on Windows only".to_string()),
    }
}

/// Reads a pipe to the end on its own thread, keeping at most [`OUTPUT_CAP`] bytes.
fn reader<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    let Some(mut pipe) = pipe else {
        let _ = tx.send(Vec::new());
        return rx;
    };
    let spawned = thread::Builder::new().name("csm-git-pipe".into()).spawn(move || {
        let mut kept = Vec::new();
        let mut buf = [0u8; 8192];
        loop {
            match pipe.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let room = OUTPUT_CAP.saturating_sub(kept.len());
                    if let Some(chunk) = buf.get(..n.min(room)) {
                        kept.extend_from_slice(chunk);
                    }
                }
            }
        }
        let _ = tx.send(kept);
    });
    if spawned.is_err() {
        // The receiver then reports a disconnect, read as "no output".
        log::warn!("git: no pipe reader thread");
    }
    rx
}

/// Kills the process and everything it started.
fn kill_tree(child: &mut Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new(system32("taskkill.exe"))
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(CREATE_NO_WINDOW)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
    #[cfg(unix)]
    {
        // The child leads its own process group (`process_group(0)`): signal the group.
        let _ = Command::new("kill").args(["-KILL", &format!("-{pid}")]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Runs git for `folder` with a deadline. Never panics; a failure to start is in the result.
#[must_use]
pub fn run_git(folder: &str, git_args: &[String], timeout: Duration, read_only: bool) -> GitRun {
    let Some(target) = GitTarget::of(folder) else {
        return GitRun { spawn_error: Some("no folder".into()), ..GitRun::default() };
    };
    run_target(&target, git_args, timeout, read_only)
}

fn run_target(target: &GitTarget, git_args: &[String], timeout: Duration, read_only: bool) -> GitRun {
    let mut cmd = match command(target, git_args, read_only) {
        Ok(c) => c,
        Err(e) => {
            return GitRun { spawn_error: Some(e), ..GitRun::default() };
        }
    };
    cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            let what = if target.is_wsl() { "wsl.exe" } else { "git" };
            return GitRun { spawn_error: Some(format!("could not start {what}: {e}")), ..GitRun::default() };
        }
    };
    let out = reader(child.stdout.take());
    let err = reader(child.stderr.take());
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut code = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                code = status.code();
                break;
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    kill_tree(&mut child);
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(_) => {
                kill_tree(&mut child);
                break;
            }
        }
    }
    // A grandchild that outlived the tree kill could hold a pipe open: don't wait on it forever.
    let grace = Duration::from_secs(2);
    let decode = |rx: mpsc::Receiver<Vec<u8>>| rx.recv_timeout(grace).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default();
    let mut stdout = decode(out);
    let stderr = decode(err);
    if target.is_wsl() {
        stdout = args::strip_wsl_profile_noise(&stdout).to_string();
    }
    GitRun { stdout, stderr, code: if timed_out { None } else { code }, timed_out, spawn_error: None }
}

/// The folder exists (local) or the target is WSL (whose `\\wsl$` probe could boot a distro;
/// git answers "not a repo" there itself).
#[must_use]
pub fn folder_usable(folder: &str) -> bool {
    match GitTarget::of(folder) {
        Some(GitTarget::Local(f)) => Path::new(&f).is_dir(),
        Some(GitTarget::Wsl { .. }) => true,
        None => false,
    }
}

/// One `status --porcelain=v2 --branch`. `None` when the folder is not in a repo (or git failed).
#[must_use]
pub fn status(folder: &str) -> Option<GitRepoStatus> {
    if !folder_usable(folder) {
        return None;
    }
    let r = run_git(folder, &args::status_args(), STATUS_TIMEOUT, true);
    r.ok().then(|| parse_status_v2(&r.stdout))
}

/// The main working tree's root, one spelling per repo (canonical: 8.3 and junctions resolved,
/// a WSL worktree's Linux path mapped back to `\\wsl$`). `None` outside a repo.
#[must_use]
pub fn repo_root(folder: &str) -> Option<String> {
    if !folder_usable(folder) {
        return None;
    }
    let r = run_git(folder, &args::git_common_dir_args(), QUERY_TIMEOUT, true);
    if !r.ok() {
        return None;
    }
    let out = translate_out(folder, &r.stdout);
    args::repo_root_from_common_dir(Some(&out), folder).map(|root| canonical(&root))
}

/// Linux paths in WSL output back to `\\wsl$\<distro>\...`; local output unchanged.
fn translate_out(folder: &str, out: &str) -> String {
    match GitTarget::of(folder) {
        Some(GitTarget::Wsl { distro, .. }) => args::translate_linux_paths_to_unc(out, &distro),
        _ => out.to_string(),
    }
}

/// Every worktree of the repo holding `folder` (the main one first), paths canonical. **Empty on
/// any failure**: callers that decide whether something may be removed treat "not listed" as
/// "refuse", so this fails closed.
#[must_use]
pub fn worktree_list(folder: &str) -> Vec<WorktreeInfo> {
    if !folder_usable(folder) {
        return Vec::new();
    }
    let r = run_git(folder, &args::list_worktrees_args(), QUERY_TIMEOUT, true);
    if !r.ok() {
        return Vec::new();
    }
    let mut list = worktrees::parse_worktree_list(&translate_out(folder, &r.stdout));
    for w in &mut list {
        if !w.is_prunable {
            w.path = canonical(&w.path);
        }
    }
    list
}

/// Local branch names.
pub fn branches(folder: &str) -> Result<Vec<String>, String> {
    let r = run_git(folder, &args::list_branches_args(), QUERY_TIMEOUT, true);
    if r.ok() { Ok(args::parse_branch_list(&r.stdout)) } else { Err(failure_text("Listing branches", &r, QUERY_TIMEOUT)) }
}

/// A user-initiated operation: combined output, ok, timed out.
#[must_use]
pub fn op(folder: &str, git_args: &[String], timeout: Duration, read_only: bool) -> GitOpResult {
    let r = run_git(folder, git_args, timeout, read_only);
    let mut output = r.output();
    if r.timed_out {
        if !output.is_empty() {
            output.push('\n');
        }
        output.push_str(&format!(
            "git did not finish within {}s and was stopped. If the remote needs a password or key, sign in from a terminal once.",
            timeout.as_secs()
        ));
    }
    GitOpResult { ok: r.ok(), output, timed_out: r.timed_out }
}

#[must_use]
pub fn fetch(folder: &str) -> GitOpResult {
    op(folder, &args::fetch_args(), FETCH_TIMEOUT, false)
}

#[must_use]
pub fn pull_ff_only(folder: &str) -> GitOpResult {
    op(folder, &args::pull_ff_only_args(), PULL_TIMEOUT, false)
}

#[must_use]
pub fn switch_branch(folder: &str, branch: &str) -> GitOpResult {
    op(folder, &args::switch_args(branch), SWITCH_TIMEOUT, false)
}

#[must_use]
pub fn prune_worktrees(folder: &str) -> GitOpResult {
    op(folder, &args::prune_worktrees_args(), SWITCH_TIMEOUT, false)
}

/// `worktree remove -- <path>` without `--force`, run from `run_from` (the main worktree: a
/// process whose cwd is inside the folder would lock it on Windows).
#[must_use]
pub fn remove_worktree(run_from: &str, worktree_path: &str) -> GitOpResult {
    op(run_from, &args::remove_worktree_args(worktree_path), WORKTREE_TIMEOUT, false)
}

/// `branch -d -- <branch>`: git keeps an unmerged branch.
#[must_use]
pub fn delete_merged_branch(run_from: &str, branch: &str) -> GitOpResult {
    op(run_from, &args::delete_merged_branch_args(branch), SWITCH_TIMEOUT, false)
}

/// `worktree add`: a new branch (`-b <branch> -- <path> [<base>]`) or an existing one
/// (`-- <path> <branch>`). Argv; `--` before the positionals.
#[must_use]
pub fn add_worktree(run_from: &str, target_path: &str, branch: &str, create: bool, base: Option<&str>) -> GitOpResult {
    let mut a = args::worktree_add_args(target_path, branch, create);
    if create && let Some(b) = base.filter(|b| !b.is_empty()) {
        a.push(b.to_string());
    }
    op(run_from, &a, WORKTREE_TIMEOUT, false)
}

/// The failure text the panel and the tool error show: what failed, git's own words, and a hint
/// when git's message is one we recognise.
#[must_use]
pub fn failure_message(what: &str, output: &str) -> String {
    let mut msg = format!("{what} failed.");
    let out = output.trim();
    if !out.is_empty() {
        msg.push('\n');
        msg.push_str(out);
    }
    if let Some(h) = text::failure_hint(out) {
        msg.push_str("\n\n");
        msg.push_str(h);
    }
    msg
}

fn failure_text(what: &str, r: &GitRun, timeout: Duration) -> String {
    let mut out = r.output();
    if r.timed_out {
        out.push_str(&format!("\ngit did not finish within {}s and was stopped.", timeout.as_secs()));
    }
    failure_message(what, &out)
}

// ── Paths on disk ──────────────────────────────────────────────────────────────────────────

/// A path's real form: 8.3 names expanded, junctions and symlinks followed
/// (`std::fs::canonicalize` is `GetFinalPathNameByHandle` on Windows), without the `\\?\`
/// prefix. The input unchanged when it does not exist. WSL paths are returned as given: their
/// case matters and git already reports them in one spelling. Disk I/O: off the UI thread.
#[must_use]
pub fn canonical(path: &str) -> String {
    if path.trim().is_empty() || is_wsl_folder(path) {
        return path.to_string();
    }
    match std::fs::canonicalize(path) {
        Ok(p) => strip_verbatim(&p.to_string_lossy()),
        Err(_) => path.to_string(),
    }
}

fn strip_verbatim(s: &str) -> String {
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        s.to_string()
    }
}

/// Folders below `root` (not `root` itself) that are a repository of their own: a `.git`
/// folder (an independent clone) or a `.git` file (a linked worktree or a submodule). Removing
/// `root` would delete them with it, and git's clean check does not look inside an ignored
/// folder. Junctions and symlinks are not followed; `.git` folders are not descended into.
///
/// **Fails closed**: `None` when more than `max_dirs` folders had to be visited, so the caller
/// cannot vouch for the tree and must refuse. Disk I/O: off the UI thread.
#[must_use]
pub fn nested_repositories(root: &Path, max_dirs: usize) -> Option<Vec<PathBuf>> {
    let mut found = Vec::new();
    let mut pending: Vec<(PathBuf, bool)> = vec![(root.to_path_buf(), true)];
    let mut visited = 0usize;
    while let Some((dir, is_root)) = pending.pop() {
        if !is_root && dir.join(".git").is_file() {
            found.push(dir);
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // Not following: a junction or symlink reports `is_symlink` here, never `is_dir`.
            let Ok(ft) = entry.file_type() else { continue };
            if ft.is_symlink() || !ft.is_dir() {
                continue;
            }
            visited += 1;
            if visited > max_dirs {
                return None;
            }
            if entry.file_name().to_string_lossy().eq_ignore_ascii_case(".git") {
                if !is_root {
                    found.push(dir.clone());
                }
                continue;
            }
            pending.push((entry.path(), false));
        }
    }
    found.sort();
    found.dedup();
    Some(found)
}

/// The git directory of the working tree holding `folder` (walking up), with a linked
/// worktree's `gitdir:` pointer resolved, so a worktree watches its own `HEAD`. `None` outside
/// a repo. Disk I/O only, no spawn.
#[must_use]
pub fn git_dir(folder: &str) -> Option<PathBuf> {
    let mut dir = PathBuf::from(folder.trim());
    if dir.as_os_str().is_empty() {
        return None;
    }
    for _ in 0..64 {
        let dot = dir.join(".git");
        if dot.is_dir() {
            return Some(dot);
        }
        if dot.is_file() {
            let text = std::fs::read_to_string(&dot).ok()?;
            let gd = text.trim().strip_prefix("gitdir:")?.trim().to_string();
            if gd.is_empty() {
                return None;
            }
            let p = match wsl::try_parse_unc_path(folder) {
                (Some(distro), _) if gd.starts_with('/') => PathBuf::from(wsl::to_unc_path(&distro, &gd)),
                _ if Path::new(&gd).is_absolute() => PathBuf::from(&gd),
                _ => dir.join(&gd),
            };
            return Some(p);
        }
        if !dir.pop() {
            return None;
        }
    }
    None
}

/// For a linked worktree, the main repo's name (`· worktree of X`); `None` for the main
/// checkout, a submodule or a non-repo. Reads the `.git` FILE of the working tree holding
/// `folder`, no spawn.
#[must_use]
pub fn linked_worktree_of(folder: &str, repo_root: Option<&str>) -> Option<String> {
    let mut dir = PathBuf::from(folder.trim());
    for _ in 0..64 {
        let dot = dir.join(".git");
        if dot.is_dir() {
            return None;
        }
        if dot.is_file() {
            let text = std::fs::read_to_string(&dot).ok()?;
            return worktrees::linked_worktree_of(&text, repo_root);
        }
        if !dir.pop() {
            return None;
        }
    }
    None
}

/// Where `worktree_new` puts a worktree by default: a sibling of the main checkout named
/// `<repo>-<branch>` (slashes and spaces become `-`), like the WPF dialog.
#[must_use]
pub fn default_worktree_path(repo_root: &str, branch: &str) -> String {
    let root = repo_root.trim_end_matches(['/', '\\']);
    let safe: String = branch.chars().map(|c| if matches!(c, '/' | '\\' | ' ' | ':') { '-' } else { c }).collect();
    let name = worktrees::name(root);
    let wsl = is_wsl_folder(root);
    let sep = if wsl || root.contains('\\') { '\\' } else { '/' };
    let root_native = if sep == '\\' { root.replace('/', "\\") } else { root.to_string() };
    match root_native.rfind(sep) {
        Some(i) if i > 0 => format!("{}{sep}{name}-{safe}", root_native.get(..i).unwrap_or("")),
        _ => format!("{root_native}-{safe}"),
    }
}

/// Windows form of a path git printed with forward slashes (`C:/repo/wt` → `C:\repo\wt`), as
/// sessions store it. WSL UNC and Unix paths are left as they are.
#[must_use]
pub fn windows_form(path: &str) -> String {
    if cfg!(windows) { path.replace('/', "\\") } else { path.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets() {
        assert_eq!(GitTarget::of(""), None);
        assert_eq!(GitTarget::of(r"C:\repo"), Some(GitTarget::Local(r"C:\repo".into())));
        assert_eq!(GitTarget::of(r"\\wsl$\Ubuntu\home\u\proj"), Some(GitTarget::Wsl { distro: "Ubuntu".into(), linux: "/home/u/proj".into() }));
        assert!(is_wsl_folder(r"\\wsl.localhost\Ubuntu\home"));
    }

    #[test]
    fn verbatim_prefixes_are_stripped() {
        assert_eq!(strip_verbatim(r"\\?\C:\a"), r"C:\a");
        assert_eq!(strip_verbatim(r"\\?\UNC\srv\share\x"), r"\\srv\share\x");
        assert_eq!(strip_verbatim("/home/x"), "/home/x");
    }

    #[test]
    fn default_paths_are_siblings_of_the_main_checkout() {
        assert_eq!(default_worktree_path(r"C:\src\odulodo", "feat/x y"), r"C:\src\odulodo-feat-x-y");
        assert_eq!(default_worktree_path("C:/src/odulodo", "fix"), "C:/src/odulodo-fix");
        assert_eq!(default_worktree_path(r"\\wsl$\Ubuntu\home\u\repo", "b"), r"\\wsl$\Ubuntu\home\u\repo-b");
    }

    #[cfg(windows)]
    #[test]
    fn the_wsl_command_is_wsl_exe_by_full_path_with_argv_from_the_builder() {
        let t = GitTarget::of(r"\\wsl$\Ubuntu\home\u\my proj").unwrap();
        let c = command(&t, &args::status_args(), true).unwrap();
        let prog = c.get_program().to_string_lossy().to_lowercase();
        assert!(prog.ends_with(r"system32\wsl.exe"), "{prog}");
        let wslenv = c.get_envs().find(|(k, _)| k.to_string_lossy() == "WSLENV").and_then(|(_, v)| v).unwrap().to_string_lossy().into_owned();
        assert!(wslenv.contains("GIT_TERMINAL_PROMPT") && wslenv.contains("GIT_OPTIONAL_LOCKS"), "{wslenv}");
    }
}
