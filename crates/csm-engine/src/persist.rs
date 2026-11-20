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
}
