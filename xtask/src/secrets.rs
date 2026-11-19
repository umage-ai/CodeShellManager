//! Token patterns in tracked files (or staged ones, for the pre-commit hook).
//!
//! Not a replacement for a real scanner: it catches the shapes of the keys this repository could
//! plausibly hold (GitHub, Anthropic, OpenAI-style, AWS, Slack, npm, Google, PEM private keys) and
//! key files that should never be tracked. A line may carry `csm-secret-ok` to opt out.

use std::process::Command;

use crate::Findings;

struct Pattern {
    name: &'static str,
    prefix: &'static str,
    /// Characters that must follow, at least `min` of them (and exactly `min` when `exact`).
    min: usize,
    exact: bool,
    class: fn(char) -> bool,
}

fn alnum(c: char) -> bool {
    c.is_ascii_alphanumeric()
}
fn word(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}
fn upper_digit(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit()
}

const PATTERNS: &[Pattern] = &[
    Pattern { name: "GitHub token", prefix: "ghp_", min: 36, exact: false, class: alnum },
    Pattern { name: "GitHub token", prefix: "gho_", min: 36, exact: false, class: alnum },
    Pattern { name: "GitHub token", prefix: "ghu_", min: 36, exact: false, class: alnum },
    Pattern { name: "GitHub token", prefix: "ghs_", min: 36, exact: false, class: alnum },
    Pattern { name: "GitHub token", prefix: "ghr_", min: 36, exact: false, class: alnum },
    Pattern { name: "GitHub fine-grained token", prefix: "github_pat_", min: 22, exact: false, class: word },
    Pattern { name: "Anthropic key", prefix: "sk-ant-", min: 20, exact: false, class: word },
    Pattern { name: "API key (sk-)", prefix: "sk-", min: 32, exact: false, class: word },
    Pattern { name: "AWS access key id", prefix: "AKIA", min: 16, exact: true, class: upper_digit },
    Pattern { name: "Google API key", prefix: "AIza", min: 35, exact: true, class: word },
    Pattern { name: "npm token", prefix: "npm_", min: 36, exact: false, class: alnum },
    Pattern { name: "Slack token", prefix: "xoxb-", min: 10, exact: false, class: word },
    Pattern { name: "Slack token", prefix: "xoxp-", min: 10, exact: false, class: word },
    Pattern { name: "Slack token", prefix: "xoxa-", min: 10, exact: false, class: word },
];

/// `(line number, what)` for each hit in `text`.
pub fn scan(text: &str) -> Vec<(usize, &'static str)> {
    let private_key = concat!("-----BEGIN ", "PRIVATE KEY-----");
    let rsa_key = concat!("-----BEGIN ", "RSA PRIVATE KEY-----");
    let ec_key = concat!("-----BEGIN ", "EC PRIVATE KEY-----");
    let ssh_key = concat!("-----BEGIN ", "OPENSSH PRIVATE KEY-----");
    let mut hits = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.contains("csm-secret-ok") {
            continue;
        }
        if [private_key, rsa_key, ec_key, ssh_key].iter().any(|k| line.contains(k)) {
            hits.push((i + 1, "PEM private key"));
            continue;
        }
        if let Some(name) = scan_line(line) {
            hits.push((i + 1, name));
        }
    }
    hits
}

fn scan_line(line: &str) -> Option<&'static str> {
    for p in PATTERNS {
        for (at, _) in line.match_indices(p.prefix) {
            // A token starts at a boundary: `task-` is not `sk-`.
            if line[..at].chars().next_back().is_some_and(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let rest = &line[at + p.prefix.len()..];
            let n = rest.chars().take_while(|c| (p.class)(*c)).count();
            let ends_clean = rest.chars().nth(n).is_none_or(|c| !(p.class)(c));
            if n >= p.min && (!p.exact || (n == p.min && ends_clean)) {
                return Some(p.name);
            }
        }
    }
    None
}

fn is_key_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".pfx", ".p12", ".pem", ".key", ".keystore", ".jks"].iter().any(|e| lower.ends_with(e))
}

fn git_lines(args: &[&str]) -> Result<Vec<String>, String> {
    let out = Command::new("git").args(args).current_dir(crate::root()).output().map_err(|e| format!("could not run git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).lines().map(str::to_owned).collect())
}

pub fn check(staged: bool) -> Result<Findings, String> {
    let files = if staged { git_lines(&["diff", "--cached", "--name-only", "--diff-filter=ACM"])? } else { git_lines(&["ls-files"])? };
    let mut problems = Vec::new();
    for f in files {
        if is_key_file(&f) {
            problems.push(format!("{f}: looks like a key or certificate file; do not track it"));
            continue;
        }
        if f == "Cargo.lock" || f.ends_with(".png") || f.ends_with(".ico") || f.ends_with(".ttf") || f.ends_with(".otf") {
            continue;
        }
        let path = crate::root().join(&f);
        let Ok(meta) = std::fs::metadata(&path) else { continue };
        if meta.len() > 2 * 1024 * 1024 {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else { continue };
        for (line, what) in scan(&text) {
            problems.push(format!("{f}:{line}: looks like a {what}"));
        }
    }
    Ok(problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_caught_and_prose_is_not() {
        let gh = format!("token = \"{}{}\"", "ghp_", "a".repeat(36));
        assert_eq!(scan(&gh), vec![(1, "GitHub token")]);
        let aws = format!("id: {}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        assert_eq!(scan(&aws).len(), 1);
        assert!(scan(&format!("id: {}{}0", "AKIA", "ABCDEFGHIJKLMNOP")).is_empty(), "17 characters is not an access key id");
        assert_eq!(scan(&format!("{}{}", "sk-ant-", "x".repeat(30))).len(), 1);
        assert_eq!(scan(&format!("a\n{}\nb", concat!("-----BEGIN ", "PRIVATE KEY-----"))), vec![(2, "PEM private key")]);
        for fine in ["the task-queue-processing-pipeline-is-fast-enough-now", "ghp_short", "sk-learn", "skip-this-long-identifier-with-dashes-abcdefghijklmnop"] {
            assert!(scan(fine).is_empty(), "{fine}");
        }
    }

    #[test]
    fn a_line_can_opt_out() {
        let line = format!("{}{} // csm-secret-ok: test fixture", "ghp_", "b".repeat(40));
        assert!(scan(&line).is_empty());
    }

    #[test]
    fn key_files_are_refused() {
        assert!(is_key_file("signing/cert.PFX") && is_key_file("a/b.pem") && !is_key_file("src/keymap.rs"));
    }
}
