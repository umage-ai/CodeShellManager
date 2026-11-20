//! `state.json` on disk: where it lives, the atomic write with one `.bak` generation, and the
//! saver thread (saves never run on the engine or UI thread).
//!
//! The write mirrors the WPF `StateService` (issue #88): serialise to `.tmp`, rotate the current
//! file to `.bak`, move the `.tmp` into place. Loading falls back to `.bak` when the primary
//! won't parse (`csm_core::state::load`), so a torn write never costs the sessions.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread;

use csm_core::state::{self, Loaded};

/// Env override for the state file itself (tests, side-by-side runs). Wins over everything.
pub const STATE_PATH_ENV: &str = "CSM_STATE_PATH";
/// Env override for the config folder. Set but empty = isolated: nothing is read or written.
pub const CONFIG_DIR_ENV: &str = "CSM_CONFIG_DIR";
/// A file with this name beside the executable makes the install portable (state beside it).
pub const PORTABLE_MARKER: &str = "portable";
/// The persisted-file version written into `state.json` (`CsmRsVersion`).
pub const STATE_FILE_VERSION: u32 = 1;

/// The app's folder name under the platform data dirs (roaming: state; local: the index).
pub const APP_DIR: &str = "CodeShellManager2";
/// The folder name before the v2 move (2026-11-20); moved to [`APP_DIR`] once, on start.
pub const LEGACY_APP_DIR: &str = "CodeShellManager-rs";

/// Where the config folder is, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigDir {
    /// Isolated (`CSM_CONFIG_DIR` set but empty, or no data dir at all): never touch disk.
    Isolated,
    Dir(PathBuf),
}

/// The config folder, by the Craft settings-storage precedence: `--config-dir` (the caller passes
/// it), then `CSM_CONFIG_DIR` (set but empty = isolated), then a `portable` marker beside the
/// executable, then `<platform data dir>/CodeShellManager2` (never the WPF app's folder).
pub fn config_dir(cli_config_dir: Option<&Path>) -> ConfigDir {
    if let Some(d) = cli_config_dir {
        return ConfigDir::Dir(d.to_path_buf());
    }
    if let Some(v) = std::env::var_os(CONFIG_DIR_ENV) {
        return if v.is_empty() { ConfigDir::Isolated } else { ConfigDir::Dir(PathBuf::from(v)) };
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf))
        && dir.join(PORTABLE_MARKER).is_file()
    {
        return ConfigDir::Dir(dir.join("data"));
    }
    match dirs::data_dir() {
        Some(d) => ConfigDir::Dir(d.join(APP_DIR)),
        None => ConfigDir::Isolated,
    }
}

/// `CSM_STATE_PATH`, else `state.json` in [`config_dir`]; `None` when isolated.
pub fn state_path(cli_config_dir: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(STATE_PATH_ENV).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(p));
    }
    match config_dir(cli_config_dir) {
        ConfigDir::Dir(d) => Some(d.join("state.json")),
        ConfigDir::Isolated => None,
    }
}

/// [`state_path`] without a `--config-dir`.
pub fn default_state_path() -> Option<PathBuf> {
    state_path(None)
}

/// Where the WPF app keeps its state: `%AppData%\CodeShellManager\state.json`.
pub fn wpf_state_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("CodeShellManager").join("state.json"))
}

/// Moves the pre-v2 data folders (`CodeShellManager-rs`, roaming and local) to `CodeShellManager2`,
/// once: only where the old folder exists and the new one does not. Call it at start-up, before
/// the state is loaded (not for `--clean`). It does nothing unless the state lives in the default
/// location (no `--config-dir`, `CSM_CONFIG_DIR`, `CSM_STATE_PATH` or portable install). Returns
/// what happened, one line per folder, for the log. Never fails: a folder that can't be moved is
/// reported and left alone.
pub fn migrate_legacy_data_dirs(cli_config_dir: Option<&Path>) -> Vec<String> {
    if std::env::var_os(STATE_PATH_ENV).is_some_and(|p| !p.is_empty()) {
        return Vec::new();
    }
    let Some(default) = dirs::data_dir().map(|d| d.join(APP_DIR)) else { return Vec::new() };
    if config_dir(cli_config_dir) != ConfigDir::Dir(default) {
        return Vec::new();
    }
    let mut bases: Vec<PathBuf> = [dirs::data_dir(), dirs::data_local_dir()].into_iter().flatten().collect();
    bases.dedup();
    migrate_legacy_in(&bases)
}

/// [`migrate_legacy_data_dirs`] over the given parent folders (tests).
pub fn migrate_legacy_in(bases: &[PathBuf]) -> Vec<String> {
    let mut report = Vec::new();
    for base in bases {
        let (old, new) = (base.join(LEGACY_APP_DIR), base.join(APP_DIR));
        if !old.is_dir() || new.exists() {
            continue;
        }
        match fs::rename(&old, &new) {
            Ok(()) => report.push(format!("moved {} to {}", old.display(), new.display())),
            Err(e) => report.push(format!("could not move {} to {}: {e}; it was left where it is", old.display(), new.display())),
        }
    }
    report
}

pub fn backup_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".bak");
    PathBuf::from(s)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(".tmp");
    PathBuf::from(s)
}

/// Reads a file as text; `None` when it does not exist. Unreadable bytes become U+FFFD (the
/// parser then rejects it and the backup is tried).
fn read_text(path: &Path) -> Option<String> {
    match fs::read(path) {
        Ok(bytes) => Some(String::from_utf8_lossy(&bytes).into_owned()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            log::warn!("state: could not read {}: {e}", path.display());
            Some(String::new())
        }
    }
}

/// Primary, then `.bak`; never fails.
pub fn load(path: &Path) -> Loaded {
    let primary = read_text(path);
    let backup = read_text(&backup_path(path));
    state::load(primary.as_deref(), backup.as_deref())
}

/// `.tmp` → rotate the current file to `.bak` → move `.tmp` into place.
pub fn write_atomic(path: &Path, text: &str) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        fs::create_dir_all(dir)?;
    }
    let tmp = temp_path(path);
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
    }
    if path.exists() {
        // `rename` replaces an existing `.bak` on every platform std supports.
        fs::rename(path, backup_path(path))?;
    }
    fs::rename(&tmp, path)
}

// ── Rolling snapshots and unreadable files (issue #166) ─────────────────────────────────

/// The snapshot folder's name, beside `state.json`.
pub const SNAPSHOT_DIR: &str = "snapshots";
/// How many set-aside copies of an unreadable file are kept, per file.
pub const BAD_COPIES_KEPT: usize = 3;

/// `<folder of state.json>/snapshots`.
pub fn snapshots_dir(path: &Path) -> PathBuf {
    path.parent().unwrap_or_else(|| Path::new("")).join(SNAPSHOT_DIR)
}

/// The snapshots beside `path`, newest first, as `(stamp ms, file)`. Empty when there are none
/// or the folder can't be read. Files that are not ours are ignored.
pub fn list_snapshots(path: &Path) -> Vec<(i64, PathBuf)> {
    let Ok(entries) = fs::read_dir(snapshots_dir(path)) else { return Vec::new() };
    let mut out: Vec<(i64, PathBuf)> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name();
            state::parse_snapshot_file_name(name.to_str()?).map(|t| (t, e.path()))
        })
        .collect();
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out
}

/// Before a save replaces `path`: when a snapshot is due ([`state::snapshot_due`]), copies the
/// outgoing file into `snapshots/` (only when it parses and differs from the newest snapshot),
/// then prunes the folder ([`state::snapshots_to_keep`]). Returns the new snapshot, if any.
/// Errors are the caller's to log; a failed snapshot never stops the save.
pub fn snapshot_outgoing(path: &Path, now_ms: i64) -> io::Result<Option<PathBuf>> {
    let existing = list_snapshots(path);
    let newest = existing.first();
    if !state::snapshot_due(newest.map(|(t, _)| *t), now_ms) {
        return Ok(None);
    }
    let current = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    if state::parse(&String::from_utf8_lossy(&current)).is_err() {
        return Ok(None);
    }
    if let Some((_, p)) = newest
        && fs::read(p).is_ok_and(|b| b == current)
    {
        return Ok(None);
    }
    let dir = snapshots_dir(path);
    fs::create_dir_all(&dir)?;
    let target = dir.join(state::snapshot_file_name(now_ms));
    let tmp = dir.join(format!("{}.tmp", state::snapshot_file_name(now_ms)));
    {
        let mut f = fs::File::create(&tmp)?;
        f.write_all(&current)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, &target)?;
    let mut times: Vec<i64> = existing.iter().map(|(t, _)| *t).collect();
    times.push(now_ms);
    let keep = state::snapshots_to_keep(&times, now_ms);
    for (t, p) in &existing {
        if !keep.contains(t)
            && let Err(e) = fs::remove_file(p)
        {
            log::warn!("state: could not prune snapshot {}: {e}", p.display());
        }
    }
    Ok(Some(target))
}

/// `<file>.bad-<ms>`: where an unreadable file is set aside.
fn bad_path(file: &Path, now_ms: i64) -> PathBuf {
    let mut s = file.as_os_str().to_owned();
    s.push(format!(".bad-{now_ms}"));
    PathBuf::from(s)
}

/// Sets an unreadable `file` aside as `<file>.bad-<ms>`, by renaming it (`keep_original` false:
/// the next save can no longer overwrite it) or by copying it (true: it stays where it is).
/// Keeps the newest [`BAD_COPIES_KEPT`] set-aside copies of that file. `Ok(None)` when the file
/// does not exist.
pub fn set_aside(file: &Path, now_ms: i64, keep_original: bool) -> io::Result<Option<PathBuf>> {
    if !file.exists() {
        return Ok(None);
    }
    let target = bad_path(file, now_ms);
    if keep_original {
        fs::copy(file, &target)?;
    } else {
        fs::rename(file, &target)?;
    }
    if let (Some(dir), Some(name)) = (file.parent(), file.file_name().and_then(|n| n.to_str())) {
        let prefix = format!("{name}.bad-");
        let dir = if dir.as_os_str().is_empty() { Path::new(".") } else { dir };
        let mut bad: Vec<(i64, PathBuf)> = fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(Result::ok)
                    .filter_map(|e| {
                        let n = e.file_name();
                        let t = n.to_str()?.strip_prefix(&prefix)?.parse::<i64>().ok()?;
                        Some((t, e.path()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        bad.sort_by(|a, b| b.0.cmp(&a.0));
        for (_, p) in bad.iter().skip(BAD_COPIES_KEPT) {
            let _ = fs::remove_file(p);
        }
    }
    Ok(Some(target))
}

/// After a load that could not use the primary file: keep the unreadable file(s) where a save
/// cannot overwrite them. `Backup` (the `.bak` was used): the primary is *copied* aside (the next
/// save rotates it into `.bak`). `Unreadable` (nothing usable): the primary and the `.bak` are
/// *moved* aside, so the empty state's first saves cannot replace either. Returns the copies
/// made, for the log and the load report. Never fails: a file that can't be set aside is logged.
pub fn keep_unreadable(path: &Path, source: state::LoadSource, now_ms: i64) -> Vec<PathBuf> {
    let files: Vec<(PathBuf, bool)> = match source {
        state::LoadSource::Backup => vec![(path.to_path_buf(), true)],
        state::LoadSource::Unreadable => vec![(path.to_path_buf(), false), (backup_path(path), false)],
        state::LoadSource::Primary | state::LoadSource::FirstRun => Vec::new(),
    };
    let mut out = Vec::new();
    for (f, keep) in files {
        match set_aside(&f, now_ms, keep) {
            Ok(Some(p)) => out.push(p),
            Ok(None) => {}
            Err(e) => log::error!("state: could not set the unreadable {} aside: {e}", f.display()),
        }
    }
    out
}

/// One background thread that writes the newest state it was given. Saves are serialised (the
/// WPF app's `SemaphoreSlim`): overlapping saves would race on the shared `.tmp`.
pub(crate) struct Saver {
    tx: Sender<String>,
    thread: Option<thread::JoinHandle<()>>,
}

impl Saver {
    pub(crate) fn start(path: PathBuf) -> Option<Saver> {
        let (tx, rx) = mpsc::channel::<String>();
        let p = path;
        let spawned = thread::Builder::new().name("csm-state-saver".into()).spawn(move || {
            while let Ok(mut text) = rx.recv() {
                // Coalesce: only the newest pending state matters.
                while let Ok(newer) = rx.try_recv() {
                    text = newer;
                }
                // The Saver only exists when state is saved at all (never `--clean` or isolated).
                match snapshot_outgoing(&p, crate::session::now_ms()) {
                    Ok(Some(s)) => log::info!("state: snapshot {}", s.display()),
                    Ok(None) => {}
                    Err(e) => log::warn!("state: snapshot of {} failed: {e}", p.display()),
                }
                if let Err(e) = write_atomic(&p, &text) {
                    log::error!("state: save to {} failed: {e}", p.display());
                }
            }
        });
        match spawned {
            Ok(h) => Some(Saver { tx, thread: Some(h) }),
            Err(e) => {
                log::error!("state: could not start the saver thread: {e}; state will not be saved");
                None
            }
        }
    }

    pub(crate) fn save(&self, text: String) {
        let _ = self.tx.send(text);
    }

    /// Saves `text` last and waits until it is on disk (app exit).
    pub(crate) fn finish(mut self, text: Option<String>) {
        if let Some(t) = text {
            let _ = self.tx.send(t);
        }
        let Saver { tx, thread, .. } = &mut self;
        drop(std::mem::replace(tx, mpsc::channel().0));
        if let Some(h) = thread.take() {
            let _ = h.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use csm_core::models::{AppState, ShellSession};
    use csm_core::state::LoadSource;

    #[test]
    fn the_pre_v2_data_folder_moves_once_and_never_over_a_new_one() {
        let (a, b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        // a: only the old folder, with a state file -> moved.
        fs::create_dir_all(a.path().join(LEGACY_APP_DIR)).unwrap();
        fs::write(a.path().join(LEGACY_APP_DIR).join("state.json"), "{}").unwrap();
        // b: both exist -> the new one wins and the old one is left alone.
        fs::create_dir_all(b.path().join(LEGACY_APP_DIR)).unwrap();
        fs::create_dir_all(b.path().join(APP_DIR)).unwrap();
        let report = migrate_legacy_in(&[a.path().to_path_buf(), b.path().to_path_buf()]);
        assert_eq!(report.len(), 1, "{report:?}");
        assert!(a.path().join(APP_DIR).join("state.json").is_file());
        assert!(!a.path().join(LEGACY_APP_DIR).exists());
        assert!(b.path().join(LEGACY_APP_DIR).is_dir());
        // A second run has nothing to do.
        assert!(migrate_legacy_in(&[a.path().to_path_buf(), b.path().to_path_buf()]).is_empty());
    }

    #[test]
    fn atomic_write_keeps_a_backup_and_load_falls_back_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("state.json");
        let mut a = AppState::default();
        a.sessions.push(ShellSession { id: "one".into(), ..Default::default() });
        write_atomic(&path, &a.to_json().unwrap()).unwrap();
        a.sessions.push(ShellSession { id: "two".into(), ..Default::default() });
        write_atomic(&path, &a.to_json().unwrap()).unwrap();
        let l = load(&path);
        assert_eq!((l.source, l.state.sessions.len()), (LoadSource::Primary, 2));
        // Tear the primary: the backup (the previous generation) is used.
        fs::write(&path, "{\"Sessions\":[{\"Id\":").unwrap();
        let l = load(&path);
        assert_eq!((l.source, l.state.sessions.len()), (LoadSource::Backup, 1));
        assert_eq!(load(&dir.path().join("none.json")).source, LoadSource::FirstRun);
    }

    const HOUR: i64 = 3_600_000;
    const T0: i64 = 1_800_000_000_000;

    fn state_text(ids: &[&str]) -> String {
        let mut a = AppState::default();
        a.sessions = ids.iter().map(|id| ShellSession { id: (*id).into(), ..Default::default() }).collect();
        a.to_json().unwrap()
    }

    #[test]
    fn snapshots_respect_the_interval_and_skip_unchanged_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        // Nothing to snapshot before the first file exists.
        assert_eq!(snapshot_outgoing(&path, T0).unwrap(), None);
        write_atomic(&path, &state_text(&["a"])).unwrap();
        let first = snapshot_outgoing(&path, T0).unwrap().unwrap();
        assert_eq!(first, snapshots_dir(&path).join("state-1800000000000.json"));
        assert_eq!(fs::read_to_string(&first).unwrap(), state_text(&["a"]));
        // Within the hour: none, even though the content changed.
        write_atomic(&path, &state_text(&["a", "b"])).unwrap();
        assert_eq!(snapshot_outgoing(&path, T0 + HOUR - 1).unwrap(), None);
        // An hour later, changed: one more.
        assert!(snapshot_outgoing(&path, T0 + HOUR).unwrap().is_some());
        // Another hour, unchanged: skipped.
        assert_eq!(snapshot_outgoing(&path, T0 + 2 * HOUR).unwrap(), None);
        assert_eq!(list_snapshots(&path).len(), 2);
        // A torn file is never snapshotted.
        fs::write(&path, "{\"Sessions\": [").unwrap();
        assert_eq!(snapshot_outgoing(&path, T0 + 3 * HOUR).unwrap(), None);
        // Strangers in the folder are ignored, never deleted.
        fs::write(snapshots_dir(&path).join("notes.txt"), "mine").unwrap();
        assert_eq!(list_snapshots(&path).len(), 2);
        assert!(snapshots_dir(&path).join("notes.txt").is_file());
    }

    #[test]
    fn snapshots_are_pruned_to_the_retention() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        // Ten days of hourly saves, each with different content.
        for h in 0..240 {
            write_atomic(&path, &state_text(&[&format!("s{h}")])).unwrap();
            snapshot_outgoing(&path, T0 + h * HOUR).unwrap();
        }
        let list = list_snapshots(&path);
        assert!(list.len() <= csm_core::state::SNAPSHOT_MAX, "{}", list.len());
        assert!(list.len() >= 25, "{}", list.len());
        let now = T0 + 239 * HOUR;
        assert!(list.iter().all(|(t, _)| now - t < 8 * 24 * HOUR));
    }

    #[test]
    fn an_unreadable_primary_is_kept_aside() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        // Backup case: the primary is copied aside and stays.
        write_atomic(&path, &state_text(&["a"])).unwrap();
        write_atomic(&path, &state_text(&["a", "b"])).unwrap();
        fs::write(&path, "torn").unwrap();
        let kept = keep_unreadable(&path, LoadSource::Backup, T0);
        assert_eq!(kept, vec![dir.path().join("state.json.bad-1800000000000")]);
        assert_eq!(fs::read_to_string(&kept[0]).unwrap(), "torn");
        assert!(path.is_file());
        // Unreadable case: both files are moved aside, so no save can overwrite them.
        fs::write(backup_path(&path), "also torn").unwrap();
        let kept = keep_unreadable(&path, LoadSource::Unreadable, T0 + 1);
        assert_eq!(kept.len(), 2, "{kept:?}");
        assert!(!path.exists() && !backup_path(&path).exists());
        assert_eq!(fs::read_to_string(dir.path().join("state.json.bak.bad-1800000000001")).unwrap(), "also torn");
        // Only the newest three copies per file are kept.
        for i in 2..10 {
            fs::write(&path, "torn again").unwrap();
            keep_unreadable(&path, LoadSource::Unreadable, T0 + i);
        }
        let bad =
            fs::read_dir(dir.path()).unwrap().filter(|e| e.as_ref().unwrap().file_name().to_string_lossy().starts_with("state.json.bad-")).count();
        assert_eq!(bad, BAD_COPIES_KEPT);
        // Nothing to do for a good load.
        assert!(keep_unreadable(&path, LoadSource::Primary, T0).is_empty());
    }
}
