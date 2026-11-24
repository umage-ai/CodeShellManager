//! Guarding Claude's own config file (`~/.claude.json`) across a quit.
//!
//! The Claude CLI rewrites that file unlocked on exit, so exits that overlap can lose each
//! other's updates, and a torn write can leave it truncated. A quit closes several Claude
//! sessions at once (`QuitClaudeConcurrency`), so before it starts it keeps a copy of each
//! config file, and once every Claude has exited it checks the file still parses; if it does not,
//! the copy is put back. A lost individual update is accepted; a corrupt file is not.
//!
//! Nothing here runs while a Claude is still exiting, and nothing here can fail a quit: every
//! problem is logged and the quit goes on.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// How many backups of each config file are kept.
pub const KEEP: usize = 3;
/// The folder (beside `state.json`) the copies live in.
pub const DIR: &str = "claude-config-backups";

/// Which Claude config files a quit guards.
#[derive(Debug, Clone, Default)]
pub enum ClaudeConfigs {
    /// None (tests, the demo, `--clean`): the user's real files are never touched.
    #[default]
    None,
    /// `~/.claude.json` and the `.claude.json` of every Claude config folder found.
    Auto,
    /// These files.
    Files(Vec<PathBuf>),
}

impl ClaudeConfigs {
    /// The files to guard that exist.
    pub fn resolve(&self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut push = |p: PathBuf| {
            if p.is_file() && !out.contains(&p) {
                out.push(p);
            }
        };
        match self {
            ClaudeConfigs::None => {}
            ClaudeConfigs::Files(f) => f.iter().cloned().for_each(&mut push),
            ClaudeConfigs::Auto => {
                if let Some(h) = home_dir() {
                    push(h.join(".claude.json"));
                }
                for root in csm_services::claude_usage::discover::claude_roots() {
                    push(root.join(".claude.json"));
                }
            }
        }
        out
    }
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    let h = std::env::var_os("USERPROFILE");
    #[cfg(not(windows))]
    let h = std::env::var_os("HOME");
    h.filter(|v| !v.is_empty()).map(PathBuf::from)
}

/// What the check after the quit found for one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// It parses: left alone (a file that was not there before and is not now ends here too).
    Fine,
    /// It was torn or gone, and the copy was put back.
    Restored,
    /// It was torn or gone and the copy could not be put back (the reason).
    RestoreFailed(String),
}

/// The copies taken before the quit, to check against afterwards.
#[derive(Debug, Default)]
pub struct Guard {
    items: Vec<(PathBuf, Option<PathBuf>)>,
    dir: PathBuf,
}

/// Is this a usable config: a JSON object?
fn is_good(bytes: &[u8]) -> bool {
    serde_json::from_slice::<Value>(bytes).is_ok_and(|v| v.is_object())
}

fn label(config: &Path) -> String {
    let parent = config.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut l: String = parent.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    l.truncate(40);
    let file = config.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{l}-{}", file.trim_start_matches('.').replace('.', "_"))
}

/// Copies every config file that exists and parses into `dir`, keeping the newest [`KEEP`] copies
/// of each. A file that is already unreadable gets no copy (it would only preserve the damage).
pub fn take(configs: &[PathBuf], dir: &Path, now_ms: i64) -> Guard {
    let mut guard = Guard { items: Vec::new(), dir: dir.to_path_buf() };
    for config in configs {
        let Ok(bytes) = fs::read(config) else {
            guard.items.push((config.clone(), None));
            continue;
        };
        if !is_good(&bytes) {
            log::warn!("quit: {} does not parse before the quit; no backup taken", config.display());
            guard.items.push((config.clone(), None));
            continue;
        }
        let target = dir.join(format!("{}.{now_ms}.json", label(config)));
        match fs::create_dir_all(dir).and_then(|()| fs::write(&target, &bytes)) {
            Ok(()) => {
                prune(dir, &label(config));
                guard.items.push((config.clone(), Some(target)));
            }
            Err(e) => {
                log::warn!("quit: could not back up {}: {e}", config.display());
                guard.items.push((config.clone(), None));
            }
        }
    }
    guard
}

/// Removes all but the newest [`KEEP`] copies for `label`.
fn prune(dir: &Path, label: &str) {
    let prefix = format!("{label}.");
    let Ok(rd) = fs::read_dir(dir) else { return };
    let mut mine: Vec<(i64, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter_map(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            let ms = n.strip_prefix(&prefix)?.strip_suffix(".json")?.parse::<i64>().ok()?;
            Some((ms, e.path()))
        })
        .collect();
    mine.sort_by_key(|m| std::cmp::Reverse(m.0));
    for (_, p) in mine.into_iter().skip(KEEP) {
        let _ = fs::remove_file(p);
    }
}

impl Guard {
    /// Checks each guarded file; a torn or missing one is restored from its copy. Call only once
    /// no Claude is exiting any more.
    pub fn verify(&self) -> Vec<(PathBuf, Outcome)> {
        self.items.iter().map(|(config, backup)| (config.clone(), self.verify_one(config, backup.as_deref()))).collect()
    }

    fn verify_one(&self, config: &Path, backup: Option<&Path>) -> Outcome {
        let Some(backup) = backup else { return Outcome::Fine };
        match fs::read(config) {
            Ok(bytes) if is_good(&bytes) => return Outcome::Fine,
            Ok(bytes) => {
                // Keep the torn file for a look; the restore replaces it.
                let _ = fs::write(self.dir.join(format!("{}.corrupt.json", label(config))), bytes);
            }
            Err(_) => {}
        }
        match restore(backup, config) {
            Ok(()) => {
                log::warn!("quit: {} was damaged while Claude exited; restored from {}", config.display(), backup.display());
                Outcome::Restored
            }
            Err(e) => {
                log::error!("quit: {} is damaged and could not be restored from {}: {e}", config.display(), backup.display());
                Outcome::RestoreFailed(e.to_string())
            }
        }
    }
}

/// Puts the copy back through a temp file beside the target, so the target is never half-written.
fn restore(backup: &Path, config: &Path) -> std::io::Result<()> {
    let bytes = fs::read(backup)?;
    if !is_good(&bytes) {
        return Err(std::io::Error::other("the backup does not parse either"));
    }
    let tmp = config.with_extension("json.csm-restore");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, config).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const GOOD: &str = r#"{"numStartups": 7, "projects": {}}"#;

    fn rig(content: Option<&str>) -> (tempfile::TempDir, PathBuf, PathBuf) {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        fs::create_dir_all(&home).unwrap();
        let cfg = home.join(".claude.json");
        if let Some(c) = content {
            fs::write(&cfg, c).unwrap();
        }
        let dir = d.path().join("data").join(DIR);
        (d, cfg, dir)
    }

    #[test]
    fn a_good_file_is_backed_up_and_left_untouched() {
        let (_d, cfg, dir) = rig(Some(GOOD));
        let g = take(std::slice::from_ref(&cfg), &dir, 1000);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        assert_eq!(g.verify(), vec![(cfg.clone(), Outcome::Fine)]);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), GOOD);
    }

    #[test]
    fn a_lost_update_is_accepted_but_a_corrupt_file_is_restored() {
        let (_d, cfg, dir) = rig(Some(GOOD));
        let g = take(std::slice::from_ref(&cfg), &dir, 1000);
        // Valid JSON with an update missing: fine.
        fs::write(&cfg, r#"{"numStartups": 8}"#).unwrap();
        assert_eq!(g.verify()[0].1, Outcome::Fine);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), r#"{"numStartups": 8}"#);
        // Truncated: put back.
        fs::write(&cfg, r#"{"numStartups": 8, "proj"#).unwrap();
        assert_eq!(g.verify()[0].1, Outcome::Restored);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), GOOD);
        assert!(dir.join(format!("{}.corrupt.json", label(&cfg))).is_file(), "the torn file is kept for a look");
        // Empty, and gone: put back too.
        fs::write(&cfg, "").unwrap();
        assert_eq!(g.verify()[0].1, Outcome::Restored);
        fs::remove_file(&cfg).unwrap();
        assert_eq!(g.verify()[0].1, Outcome::Restored);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), GOOD);
    }

    #[test]
    fn a_missing_file_is_ignored() {
        let (_d, cfg, dir) = rig(None);
        assert!(ClaudeConfigs::Files(vec![cfg.clone()]).resolve().is_empty());
        let g = take(std::slice::from_ref(&cfg), &dir, 1);
        assert!(!dir.exists(), "nothing to copy, nothing created");
        assert_eq!(g.verify(), vec![(cfg.clone(), Outcome::Fine)]);
        assert!(!cfg.exists());
    }

    #[test]
    fn an_already_broken_file_gets_no_backup_and_is_not_touched() {
        let (_d, cfg, dir) = rig(Some("{oops"));
        let g = take(std::slice::from_ref(&cfg), &dir, 1);
        assert!(!dir.exists());
        assert_eq!(g.verify()[0].1, Outcome::Fine);
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "{oops");
    }

    #[test]
    fn only_the_last_few_backups_are_kept() {
        let (_d, cfg, dir) = rig(Some(GOOD));
        for ms in 1..=6 {
            let _ = take(std::slice::from_ref(&cfg), &dir, ms);
        }
        let mut names: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        assert_eq!(names.len(), KEEP, "{names:?}");
        assert!(names.iter().all(|n| n.ends_with(".4.json") || n.ends_with(".5.json") || n.ends_with(".6.json")), "{names:?}");
    }

    #[test]
    fn a_backup_that_is_itself_bad_is_not_put_back() {
        let (_d, cfg, dir) = rig(Some(GOOD));
        let g = take(std::slice::from_ref(&cfg), &dir, 1);
        let backup = g.items[0].1.clone().unwrap();
        fs::write(&backup, "nope").unwrap();
        fs::write(&cfg, "{").unwrap();
        assert!(matches!(g.verify()[0].1, Outcome::RestoreFailed(_)));
        assert_eq!(fs::read_to_string(&cfg).unwrap(), "{");
    }
}
