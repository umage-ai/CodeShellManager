//! Where Claude Code keeps its transcripts (ContextCost `discover.js`).
//!
//! `<root>/projects/<folder>/<session id>.jsonl` is a conversation's main thread;
//! `<root>/projects/<folder>/<session id>/subagents/**/agent-<agent id>.jsonl` are its subagents.
//! Roots: `$CLAUDE_CONFIG_DIR`, `~/.claude`, and every other `~/.claude-*` folder that has a
//! `projects` folder (a second profile such as `~/.claude-work`).
//!
//! Everything here reads the disk: worker threads only.

use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Claude Code's config folder for a process started with this environment:
/// `CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn claude_home() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => home_dir().map(|h| h.join(".claude")),
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let h = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let h = std::env::var_os("HOME");
    h.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// Every root with a `projects` folder: [`claude_home`] first, then `~/.claude` and the other
/// `~/.claude-*` profiles. Duplicates removed.
pub fn claude_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut push = |p: PathBuf| {
        if p.join("projects").is_dir() && !out.iter().any(|o| same_path(o, &p)) {
            out.push(p);
        }
    };
    if let Some(h) = claude_home() {
        push(h);
    }
    if let Some(home) = home_dir() {
        push(home.join(".claude"));
        if let Ok(rd) = fs::read_dir(&home) {
            let mut extra: Vec<PathBuf> = rd
                .filter_map(Result::ok)
                .filter(|e| e.file_name().to_str().is_some_and(|n| n.starts_with(".claude-")))
                .map(|e| e.path())
                .collect();
            extra.sort();
            for p in extra {
                push(p);
            }
        }
    }
    out
}

fn same_path(a: &Path, b: &Path) -> bool {
    if cfg!(windows) { a.to_string_lossy().eq_ignore_ascii_case(&b.to_string_lossy()) } else { a == b }
}

/// A conversation id that can be a file name and a command-line value: a UUID-like token.
pub fn is_safe_session_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The main transcript of conversation `sid` under any of `roots`.
pub fn find_transcript(roots: &[PathBuf], sid: &str) -> Option<PathBuf> {
    if !is_safe_session_id(sid) {
        return None;
    }
    let file = format!("{sid}.jsonl");
    roots.iter().find_map(|root| {
        let projects = root.join("projects");
        fs::read_dir(&projects).ok()?.filter_map(Result::ok).map(|e| e.path().join(&file)).find(|p| p.is_file())
    })
}

/// The subagent transcripts of a main transcript: `(agent id, path)`, sorted by path.
pub fn subagent_files(main: &Path) -> Vec<(String, PathBuf)> {
    let Some(stem) = main.file_stem() else { return Vec::new() };
    let Some(dir) = main.parent() else { return Vec::new() };
    let mut out = Vec::new();
    walk(&dir.join(stem).join("subagents"), 0, &mut out);
    out.sort_by(|a, b| a.1.cmp(&b.1));
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<(String, PathBuf)>) {
    if depth > 4 {
        return;
    }
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.filter_map(Result::ok) {
        let p = e.path();
        let Ok(ft) = e.file_type() else { continue };
        if ft.is_dir() {
            walk(&p, depth + 1, out);
        } else if ft.is_file()
            && let Some(name) = p.file_name().and_then(|n| n.to_str())
            && let Some(agent) = name.strip_prefix("agent-").and_then(|n| n.strip_suffix(".jsonl"))
            && !agent.is_empty()
        {
            out.push((agent.to_owned(), p.clone()));
        }
    }
}

/// The names Claude Code may give a folder's project directory: every character that is not
/// ASCII alphanumeric becomes `-` (current versions), and the older form that only replaced
/// `:` `\` `/`.
pub fn project_dir_names(folder: &str) -> Vec<String> {
    let folder = folder.trim().trim_end_matches(['\\', '/']);
    if folder.is_empty() {
        return Vec::new();
    }
    let all: String = folder.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let old: String = folder.chars().map(|c| if matches!(c, ':' | '\\' | '/') { '-' } else { c }).collect();
    if all == old { vec![all] } else { vec![all, old] }
}

/// Two working folders are the same folder (case-insensitive on Windows, trailing separators
/// and slash direction ignored).
pub fn same_folder(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let s = s.trim().trim_end_matches(['\\', '/']).replace('\\', "/");
        if cfg!(windows) { s.to_lowercase() } else { s }
    };
    !a.trim().is_empty() && norm(a) == norm(b)
}

/// The `cwd` of a transcript: the first line (within the first 256 KiB) that names one.
pub fn transcript_cwd(path: &Path) -> Option<String> {
    let f = fs::File::open(path).ok()?;
    let r = BufReader::new(f.take(256 * 1024));
    for line in r.lines().map_while(Result::ok) {
        if !line.contains("\"cwd\"") {
            continue;
        }
        if let crate::claude_usage::Line::Request(crate::claude_usage::ParsedRequest { cwd: Some(c), .. })
        | crate::claude_usage::Line::Other { cwd: Some(c) } = crate::claude_usage::parse_line(&line)
        {
            return Some(c);
        }
    }
    None
}

/// The newest conversation (by modification time) whose transcript says it ran in `folder`,
/// under `roots`: `(session id, path)`. The fallback for sessions started before CSM recorded
/// the id; two conversations in one folder make it a guess.
pub fn newest_transcript_for_folder(roots: &[PathBuf], folder: &str) -> Option<(String, PathBuf)> {
    let names = project_dir_names(folder);
    let mut best: Option<(SystemTime, String, PathBuf)> = None;
    for root in roots {
        for name in &names {
            let Ok(rd) = fs::read_dir(root.join("projects").join(name)) else { continue };
            for e in rd.filter_map(Result::ok) {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "jsonl") {
                    continue;
                }
                let Some(stem) = p.file_stem().and_then(|s| s.to_str()).map(str::to_owned) else { continue };
                if !is_safe_session_id(&stem) {
                    continue;
                }
                let Ok(modified) = e.metadata().and_then(|m| m.modified()) else { continue };
                if best.as_ref().is_some_and(|(m, ..)| *m >= modified) {
                    continue;
                }
                if transcript_cwd(&p).is_some_and(|c| same_folder(&c, folder)) {
                    best = Some((modified, stem, p));
                }
            }
        }
    }
    best.map(|(_, s, p)| (s, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(cwd: &str) -> String {
        serde_json::json!({"type": "user", "cwd": cwd, "message": {"role": "user", "content": "x"}}).to_string() + "\n"
    }

    #[test]
    fn project_dir_names_cover_both_encodings() {
        assert_eq!(project_dir_names(r"C:\src\x"), vec!["C--src-x"]);
        assert_eq!(project_dir_names(r"C:\src\my app.v2\"), vec!["C--src-my-app-v2", "C--src-my app.v2"]);
        assert_eq!(project_dir_names("/home/me/x_y"), vec!["-home-me-x-y", "-home-me-x_y"]);
        assert!(project_dir_names("  ").is_empty());
    }

    #[test]
    fn same_folder_ignores_separators_and_on_windows_case() {
        assert!(same_folder("C:/src/x/", r"C:\src\x"));
        assert_eq!(same_folder(r"c:\SRC\x", r"C:\src\x"), cfg!(windows));
        assert!(!same_folder("", ""));
        assert!(!same_folder("/a", "/b"));
    }

    #[test]
    fn transcripts_and_subagents_are_found_by_id() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("projects").join("C--src-x");
        fs::create_dir_all(dir.join("abc-1").join("subagents").join("workflows").join("w1")).unwrap();
        fs::write(dir.join("abc-1.jsonl"), line(r"C:\src\x")).unwrap();
        fs::write(dir.join("abc-1").join("subagents").join("agent-a1.jsonl"), "").unwrap();
        fs::write(dir.join("abc-1").join("subagents").join("workflows").join("w1").join("agent-a2.jsonl"), "").unwrap();
        fs::write(dir.join("abc-1").join("subagents").join("journal.jsonl"), "").unwrap();
        let roots = vec![root.path().to_path_buf()];
        let main = find_transcript(&roots, "abc-1").unwrap();
        assert_eq!(main, dir.join("abc-1.jsonl"));
        assert_eq!(find_transcript(&roots, "nope"), None);
        assert_eq!(find_transcript(&roots, "../abc-1"), None, "only safe ids");
        let subs: Vec<String> = subagent_files(&main).into_iter().map(|(a, _)| a).collect();
        assert_eq!(subs, ["a1", "a2"]);
        assert_eq!(transcript_cwd(&main).as_deref(), Some(r"C:\src\x"));
    }

    #[test]
    fn the_fallback_takes_the_newest_transcript_whose_cwd_matches() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("projects").join("C--src-x");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("older-1.jsonl"), line(r"C:\src\x")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        fs::write(dir.join("newer-2.jsonl"), line(r"C:\src\x")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(30));
        // Newest, but another folder that encodes the same (`C:\src-x`): not ours.
        fs::write(dir.join("other-3.jsonl"), line(r"C:\src-x")).unwrap();
        fs::write(dir.join("not a uuid.jsonl"), line(r"C:\src\x")).unwrap();
        let roots = vec![root.path().to_path_buf()];
        let (id, _) = newest_transcript_for_folder(&roots, r"C:\src\x").unwrap();
        assert!(matches!(id.as_str(), "newer-2" | "older-1"), "{id}");
        assert_ne!(id, "other-3");
        assert_eq!(newest_transcript_for_folder(&roots, r"C:\nowhere"), None);
    }

    #[test]
    fn safe_ids() {
        assert!(is_safe_session_id("0b1c-22"));
        assert!(!is_safe_session_id(""));
        assert!(!is_safe_session_id("x; calc"));
        assert!(!is_safe_session_id(&"a".repeat(65)));
    }
}
