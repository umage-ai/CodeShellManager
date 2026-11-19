//! Finding programs: `PATH` plus the folders installers use, `PATHEXT` and npm `.cmd` shims.
//! No subprocess is run.
//!
//! Adapted from craft-anything's `crates/platform/src/which.rs` (MIT OR Apache-2.0, storytold),
//! trimmed to what CodeShellManager launches: `claude` and the other CLIs a session runs. The
//! reasons it exists:
//!
//! - An app started from Explorer or the macOS Dock gets the system's short `PATH`, not the
//!   shell's, and would miss `claude` in `~/.local/bin` or `~/.claude/local`; on Windows an
//!   installer may not have put its folder on `PATH` before the app started.
//! - Only `.exe` files start by bare name on Windows; npm installs Claude Code, Codex and Gemini
//!   as `.cmd` shims, and `.cmd` re-parses its arguments ([`launch_target`] avoids that).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

#[cfg(unix)]
const USUAL_HOME_DIRS: &[&str] = &[".local/bin", ".claude/local", ".npm-global/bin", ".bun/bin", ".volta/bin", ".cargo/bin", "bin"];
#[cfg(unix)]
const USUAL_DIRS: &[&str] = &["/opt/homebrew/bin", "/opt/homebrew/sbin", "/usr/local/bin", "/home/linuxbrew/.linuxbrew/bin", "/usr/bin", "/bin"];
#[cfg(windows)]
const USUAL_HOME_DIRS: &[&str] =
    &[r".local\bin", r".claude\local", r"AppData\Roaming\npm", r"AppData\Local\Microsoft\WinGet\Links", r".bun\bin", r".cargo\bin"];
#[cfg(windows)]
const USUAL_DIRS: &[&str] = &[r"C:\Program Files\WinGet\Links", r"C:\Program Files\nodejs"];
#[cfg(not(any(unix, windows)))]
const USUAL_HOME_DIRS: &[&str] = &[];
#[cfg(not(any(unix, windows)))]
const USUAL_DIRS: &[&str] = &[];
const HOME_VAR: &str = if cfg!(windows) { "USERPROFILE" } else { "HOME" };

/// The first match for `name` on [`search_path`], or `name` itself when it is already a path.
pub fn which(name: &str) -> Option<PathBuf> {
    which_in(name, search_path()).into_iter().next()
}

/// `PATH` plus the usual folders, after it so the person's own order wins.
pub fn search_path() -> Option<OsString> {
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()).map(|d| PathBuf::from(d).join("local"));
    with_usual_dirs(std::env::var_os("PATH"), std::env::var_os(HOME_VAR).map(PathBuf::from), claude)
}

/// `path` with the usual folders added when they exist (a folder that is not there adds nothing
/// but a longer `PATH`).
pub fn with_usual_dirs(path: Option<OsString>, home: Option<PathBuf>, extra: Option<PathBuf>) -> Option<OsString> {
    let mut dirs: Vec<PathBuf> = path.as_ref().map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    let before = dirs.len();
    let in_home = home.iter().flat_map(|h| USUAL_HOME_DIRS.iter().map(move |d| h.join(d)).chain(nvm_bins(h)).chain(npm_global_bin(h)));
    for d in extra.into_iter().chain(in_home).chain(USUAL_DIRS.iter().map(PathBuf::from)) {
        if !dirs.contains(&d) && d.is_dir() {
            dirs.push(d);
        }
    }
    if dirs.len() == before {
        return path;
    }
    std::env::join_paths(&dirs).ok().or(path)
}

/// Every match for `name` on `path`, in order. A `name` that holds a separator is checked as a path.
pub fn which_in(name: &str, path: Option<OsString>) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if name.is_empty() {
        return out;
    }
    let as_path = Path::new(name);
    if as_path.components().count() > 1 || as_path.is_absolute() {
        if let Some(c) = candidates(as_path).into_iter().find(|c| is_executable(c)) {
            out.push(c);
        }
        return out;
    }
    let Some(path) = path else { return out };
    for dir in std::env::split_paths(&path).filter(|d| !d.as_os_str().is_empty()) {
        if let Some(c) = candidates(&dir.join(name)).into_iter().find(|c| is_executable(c))
            && !out.contains(&c)
        {
            out.push(c);
        }
    }
    out
}

/// How to start `program` directly, and the arguments that go before the caller's. On Windows an
/// npm `.cmd` shim is replaced by its interpreter and script (through `cmd.exe` a prompt with line
/// breaks, or past 8191 characters, would not arrive whole, and `%` expands); any other program is
/// the path it was found at, so a child with a different lookup cannot say "not found".
pub fn launch_target(program: &OsStr) -> (OsString, Vec<OsString>) {
    let path = search_path();
    let Some(found) = which_in(&program.to_string_lossy(), path.clone()).into_iter().next() else { return (program.to_owned(), Vec::new()) };
    let Some((dir, interpreter, target)) = read_shim(&found) else { return (found.into_os_string(), Vec::new()) };
    match interpreter {
        None if is_executable(&dir.join(&target)) => (dir.join(target).into_os_string(), Vec::new()),
        Some(i) if dir.join(&target).is_file() => match find_interpreter(&dir, &i, path) {
            Some(exe) => (exe.into_os_string(), vec![dir.join(target).into_os_string()]),
            None => (found.into_os_string(), Vec::new()),
        },
        _ => (found.into_os_string(), Vec::new()),
    }
}

/// The interpreter (`node`) an npm shim needs when it cannot be found.
pub fn shim_missing_interpreter(program: &OsStr) -> Option<String> {
    let path = search_path();
    let found = which_in(&program.to_string_lossy(), path.clone()).into_iter().next()?;
    let (dir, interpreter, target) = read_shim(&found)?;
    let interpreter = interpreter?;
    (dir.join(&target).is_file() && find_interpreter(&dir, &interpreter, path).is_none()).then_some(interpreter)
}

/// A `.cmd`/`.bat` npm shim's folder, interpreter and target. Windows only; `None` elsewhere.
fn read_shim(found: &Path) -> Option<(PathBuf, Option<String>, String)> {
    if !cfg!(windows) || !found.extension().is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat")) {
        return None;
    }
    let dir = found.parent().map(Path::to_path_buf).unwrap_or_default();
    let text = std::fs::metadata(found).ok().filter(|m| m.len() <= 64 * 1024).and_then(|_| std::fs::read_to_string(found).ok())?;
    let (interpreter, target) = parse_npm_shim(&text)?;
    Some((dir, interpreter, target))
}

/// `node.exe` beside the shim, as npm's shim prefers, else on `path`.
fn find_interpreter(dir: &Path, interpreter: &str, path: Option<OsString>) -> Option<PathBuf> {
    let local = dir.join(format!("{interpreter}.exe"));
    if local.is_file() { Some(local) } else { which_in(interpreter, path).into_iter().next() }
}

/// What an npm (or pnpm) `.cmd` shim runs: its interpreter (`node`; none when the target is an
/// `.exe`) and the target, relative to the shim's folder.
pub fn parse_npm_shim(text: &str) -> Option<(Option<String>, String)> {
    let run = text.lines().find(|l| l.contains("%*"))?;
    let quoted: Vec<&str> = run.split('"').skip(1).step_by(2).collect();
    let target = quoted.iter().rev().find_map(|q| q.strip_prefix(r"%dp0%\").or_else(|| q.strip_prefix(r"%~dp0\")))?;
    if target.to_ascii_lowercase().ends_with(".exe") {
        return Some((None, target.to_string()));
    }
    let interpreter = if quoted.contains(&"%_prog%") {
        // npm: `SET "_prog=%dp0%\node.exe"` when Node sits beside the shim, else `SET "_prog=node"`.
        text.lines().filter_map(|l| l.trim().strip_prefix("SET \"_prog=")?.strip_suffix('"')).find(|p| !p.contains('%'))?
    } else {
        // pnpm: `"%~dp0\node.exe"  "%~dp0\..\pnpm\bin\pnpm.cjs" %*`.
        quoted.iter().any(|q| q.to_ascii_lowercase().ends_with(r"\node.exe")).then_some("node")?
    };
    Some((Some(interpreter.to_string()), target.to_string()))
}

/// The `bin` folders of the Node versions nvm installed, newest first.
fn nvm_bins(home: &Path) -> Vec<PathBuf> {
    if cfg!(windows) {
        let root = std::env::var_os("NVM_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(r"AppData\Roaming\nvm"));
        newest_versions(&root)
    } else {
        newest_versions(&home.join(".nvm/versions/node")).into_iter().map(|v| v.join("bin")).collect()
    }
}

/// The `v<number>…` folders in `root`, newest first, at most eight.
fn newest_versions(root: &Path) -> Vec<PathBuf> {
    let Ok(versions) = std::fs::read_dir(root) else { return Vec::new() };
    let mut found: Vec<(Vec<u64>, PathBuf)> = versions
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            let number = name.strip_prefix('v').filter(|n| n.starts_with(|c: char| c.is_ascii_digit()))?;
            let path = e.path();
            path.is_dir().then(|| (number.split('.').map(|n| n.parse().unwrap_or(0)).collect(), path))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0));
    found.into_iter().map(|(_, dir)| dir).take(8).collect()
}

/// Where npm puts global installs when the person moved its prefix (`NPM_CONFIG_PREFIX`, or
/// `prefix=` in `~/.npmrc`).
fn npm_global_bin(home: &Path) -> Option<PathBuf> {
    let set = ["NPM_CONFIG_PREFIX", "npm_config_prefix"].iter().find_map(std::env::var_os).filter(|v| !v.is_empty());
    let prefix = set.map(PathBuf::from).or_else(|| npmrc_prefix(home))?;
    Some(if cfg!(windows) { prefix } else { prefix.join("bin") })
}

fn npmrc_prefix(home: &Path) -> Option<PathBuf> {
    let file = home.join(".npmrc");
    let text = std::fs::metadata(&file).ok().filter(|m| m.len() <= 64 * 1024).and_then(|_| std::fs::read_to_string(&file).ok())?;
    let value = text.lines().rev().filter_map(|l| l.split_once('=')).find(|(k, _)| k.trim() == "prefix")?.1.trim().trim_matches('"');
    if value.is_empty() {
        return None;
    }
    Some(match value.strip_prefix("~/").or_else(|| value.strip_prefix("~\\")) {
        Some(rest) => home.join(rest),
        None => PathBuf::from(value),
    })
}

#[cfg(windows)]
fn candidates(base: &Path) -> Vec<PathBuf> {
    let mut v = Vec::new();
    // CreateProcess only adds `.exe`, and npm puts an extensionless POSIX script beside every
    // `.cmd` shim: try the kinds Windows runs first, then the bare name.
    if base.extension().is_none() {
        let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        for e in exts.split(';').filter(|e| !e.is_empty()) {
            let mut s = base.as_os_str().to_os_string();
            s.push(e.to_ascii_lowercase());
            v.push(PathBuf::from(s));
        }
    }
    v.push(base.to_path_buf());
    v
}

#[cfg(not(windows))]
fn candidates(base: &Path) -> Vec<PathBuf> {
    vec![base.to_path_buf()]
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn make_executable(p: &Path) {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    #[cfg(not(unix))]
    fn make_executable(_: &Path) {}

    #[test]
    fn npm_shim_with_a_node_beside_it() {
        let shim = "@ECHO off\r\nSET dp0=%~dp0\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\"%_prog%\"  \"%dp0%\\node_modules\\@anthropic-ai\\claude-code\\cli.js\" %*\r\n";
        let (interp, target) = parse_npm_shim(shim).unwrap();
        assert_eq!(interp.as_deref(), Some("node"));
        assert_eq!(target, r"node_modules\@anthropic-ai\claude-code\cli.js");
    }

    #[test]
    fn a_shim_that_runs_an_exe_has_no_interpreter() {
        let shim = "@echo off\r\n\"%~dp0\\bin\\codex.exe\" %*\r\n";
        assert_eq!(parse_npm_shim(shim), Some((None, r"bin\codex.exe".to_string())));
        assert_eq!(parse_npm_shim("echo nothing to run"), None);
    }

    #[test]
    fn usual_folders_follow_path_and_only_when_they_exist() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join(if cfg!(windows) { r".claude\local" } else { ".claude/local" });
        std::fs::create_dir_all(&bin).unwrap();
        let base = std::env::join_paths([PathBuf::from("first")]).unwrap();
        let joined = with_usual_dirs(Some(base), Some(home.path().to_path_buf()), None).unwrap();
        let dirs: Vec<PathBuf> = std::env::split_paths(&joined).collect();
        assert_eq!(dirs.first(), Some(&PathBuf::from("first")), "the person's own order wins");
        assert!(dirs.contains(&bin), "{dirs:?}");
        assert!(!dirs.contains(&home.path().join(".local").join("bin")), "a folder that is not there is not added");
    }

    #[test]
    fn claude_config_dir_is_searched_for_a_local_install() {
        let cfg = tempfile::tempdir().unwrap();
        let local = cfg.path().join("local");
        std::fs::create_dir_all(&local).unwrap();
        let exe = local.join(if cfg!(windows) { "claude.exe" } else { "claude" });
        std::fs::write(&exe, b"").unwrap();
        make_executable(&exe);
        let joined = with_usual_dirs(None, None, Some(local));
        assert_eq!(which_in("claude", joined), vec![exe]);
    }

    #[test]
    fn which_in_finds_by_pathext_and_dedupes() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(if cfg!(windows) { "tool.exe" } else { "tool" });
        std::fs::write(&file, b"").unwrap();
        make_executable(&file);
        let p = std::env::join_paths([dir.path(), dir.path()]).unwrap();
        assert_eq!(which_in("tool", Some(p)), vec![file]);
        assert!(which_in("", None).is_empty());
        assert!(which_in("tool", None).is_empty());
    }

    #[test]
    fn npmrc_prefix_wins_and_expands_home() {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(home.path().join(".npmrc"), "prefix=/old\n; c\nprefix=~/npm-global\n").unwrap();
        assert_eq!(npmrc_prefix(home.path()), Some(home.path().join("npm-global")));
    }
}
