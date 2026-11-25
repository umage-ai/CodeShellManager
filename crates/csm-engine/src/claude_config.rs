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

/// A short, stable hash of a path (FNV-1a, folded to 32 bits; case-insensitive, as Windows paths
/// are), so two config folders with the same name get different backup labels.
fn short_hash(path: &Path) -> u32 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in path.to_string_lossy().to_lowercase().replace('\\', "/").bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    u32::try_from((h >> 32) ^ (h & 0xffff_ffff)).unwrap_or(0)
}

/// The backup name stem of a config file: its folder's name and a short hash of the full path
/// (two profiles in folders both called `.claude` must not share, and so overwrite, backups).
fn label(config: &Path) -> String {
    let parent = config.parent().and_then(Path::file_name).map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut l: String = parent.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    l.truncate(40);
    let file = config.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{l}-{:08x}-{}", short_hash(config), file.trim_start_matches('.').replace('.', "_"))
}

/// Where the copies live for a state file: for the default install (state in the roaming
/// `CodeShellManager2` folder) the *local* app data folder, because the copies hold a copy of
/// `~/.claude.json` (tokens, project paths) and must not roam with the profile; anywhere else
/// (portable, `--config-dir`, tests) beside the state file.
pub fn backup_dir_for(save_path: &Path) -> Option<PathBuf> {
    let state_dir = save_path.parent()?;
    let roaming_default = dirs::data_dir().map(|d| d.join(crate::persist::APP_DIR));
    if roaming_default.as_deref() == Some(state_dir)
        && let Some(local) = dirs::data_local_dir()
    {
        return Some(local.join(crate::persist::APP_DIR).join(DIR));
    }
    Some(state_dir.join(DIR))
}

/// Creates `dir` (0700 on Unix) and writes `bytes` to `path` (0600 on Unix): the copies hold what
/// Claude keeps in its own private file.
fn write_private(dir: &Path, path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    #[cfg(not(unix))]
    fs::create_dir_all(dir)?;
    let mut o = fs::OpenOptions::new();
    o.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)?.write_all(bytes)
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
        match write_private(dir, &target, &bytes) {
            Ok(()) => {
                prune(dir, &label(config), "");
                prune(dir, &label(config), "corrupt-");
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

/// Removes all but the newest [`KEEP`] copies for `label`: the plain copies (`kind` empty) or the
/// torn files kept for a look (`kind` `corrupt-`).
fn prune(dir: &Path, label: &str, kind: &str) {
    let prefix = format!("{label}.{kind}");
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
                // Keep the torn file for a look (the newest few); the restore replaces it.
                let stem = label(config);
                let ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
                let _ = write_private(&self.dir, &self.dir.join(format!("{stem}.corrupt-{ms}.json")), &bytes);
                prune(&self.dir, &stem, "corrupt-");
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
    write_private(config.parent().unwrap_or_else(|| Path::new(".")), &tmp, &bytes)?;
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
        let kept = |kind: &str| fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with(&format!("{}.{kind}", label(&cfg)))).count();
        assert!(kept("corrupt-") >= 1, "the torn file is kept for a look");
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

    #[test]
    fn same_named_folders_get_different_backup_names() {
        let a = Path::new("C:/Users/a/work/.claude/.claude.json");
        let b = Path::new("C:/Users/a/home/.claude/.claude.json");
        assert_ne!(label(a), label(b));
        let spelled = "c:/Users/a/work/.claude/.claude.json".replace('/', &char::from(92u8).to_string());
        assert_eq!(label(a), label(Path::new(&spelled)), "the same path, spelled another way");
        // Two profiles do not overwrite each other's copies.
        let d = tempfile::tempdir().unwrap();
        let (x, y) = (d.path().join("x").join(".claude"), d.path().join("y").join(".claude"));
        for p in [&x, &y] {
            fs::create_dir_all(p).unwrap();
            fs::write(p.join(".claude.json"), GOOD).unwrap();
        }
        let dir = d.path().join("backups");
        let _ = take(&[x.join(".claude.json"), y.join(".claude.json")], &dir, 5);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 2);
    }

    #[test]
    fn torn_copies_kept_for_a_look_are_pruned_too() {
        let (_d, cfg, dir) = rig(Some(GOOD));
        let g = take(std::slice::from_ref(&cfg), &dir, 1000);
        for i in 0..8 {
            fs::write(&cfg, format!("{{torn{i}")).unwrap();
            assert_eq!(g.verify()[0].1, Outcome::Restored);
            std::thread::sleep(std::time::Duration::from_millis(3));
        }
        let corrupt = fs::read_dir(&dir).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().contains(".corrupt-")).count();
        assert_eq!(corrupt, KEEP);
    }

    #[test]
    fn the_default_installs_copies_live_in_local_not_roaming_app_data() {
        let (Some(roaming), Some(local)) = (dirs::data_dir(), dirs::data_local_dir()) else { return };
        let state = roaming.join(crate::persist::APP_DIR).join("state.json");
        let got = backup_dir_for(&state).unwrap();
        assert_eq!(got, local.join(crate::persist::APP_DIR).join(DIR));
        // Anywhere else: beside the state file.
        let t = tempfile::tempdir().unwrap();
        assert_eq!(backup_dir_for(&t.path().join("state.json")).unwrap(), t.path().join(DIR));
    }

    #[cfg(unix)]
    #[test]
    fn copies_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let (_d, cfg, dir) = rig(Some(GOOD));
        let g = take(std::slice::from_ref(&cfg), &dir, 1);
        assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
        let backup = g.items[0].1.clone().unwrap();
        assert_eq!(fs::metadata(&backup).unwrap().permissions().mode() & 0o777, 0o600);
    }
}
