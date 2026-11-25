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

/// The app's folder name under the platform data dirs (roaming: state; local: the index), by
/// release channel (`csm_core::channel`): `CodeShellManager2` for stable builds (and every local
/// build), `CodeShellManager2-Beta` for beta builds, so a beta installed beside the stable app
/// never shares its state.
pub const APP_DIR: &str = csm_core::channel::CURRENT.app_dir();
pub use csm_core::channel::{BETA_APP_DIR, STABLE_APP_DIR};
/// The folder name before the v2 move (2026-11-20); moved to [`STABLE_APP_DIR`] once, on start
/// (stable builds only).
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

/// The WPF app's `state.json` to import on its own at start-up, if any (docs/release.md: the
/// stable MSI replaces the WPF app, so its people must find their sessions in the new app).
///
/// Only on a true first run: no state file of ours, not even a `.bak` ([`LoadSource::FirstRun`];
/// an unreadable one is not a first run), not `--clean`, and only for the default state location
/// (no `--config-dir`, `CSM_CONFIG_DIR`, `CSM_STATE_PATH` or portable install), so tests and
/// side-by-side runs never pick it up. The import reads the WPF files and never writes them;
/// once it has run our own `state.json` exists, so it runs once.
///
/// [`LoadSource::FirstRun`]: csm_core::state::LoadSource::FirstRun
pub fn first_run_wpf_import(cli_config_dir: Option<&Path>, clean: bool, source: Option<state::LoadSource>) -> Option<PathBuf> {
    first_run_wpf_import_if(is_default_location(cli_config_dir), clean, source, wpf_state_path())
}

/// True when the state lives in this channel's default folder: no `--config-dir`,
/// `CSM_CONFIG_DIR`, `CSM_STATE_PATH` or portable install. Only then does start-up touch the
/// shared data folders (the legacy move, the beta's one-time copy, the first-run import).
pub fn is_default_location(cli_config_dir: Option<&Path>) -> bool {
    let state_path_set = std::env::var_os(STATE_PATH_ENV).is_some_and(|p| !p.is_empty());
    !state_path_set && dirs::data_dir().is_some_and(|d| config_dir(cli_config_dir) == ConfigDir::Dir(d.join(APP_DIR)))
}

/// Start-up work on the data folders, before the state is loaded. Nothing at all for `--clean`
/// or a state file outside the default location. Stable builds move the pre-v2 folder
/// ([`migrate_legacy_in`]); beta builds copy the stable app's folders once
/// ([`seed_beta_in`]). Returns one line per thing done, for the log. Never fails.
pub fn prepare_data_dirs(cli_config_dir: Option<&Path>, clean: bool) -> Vec<String> {
    let roaming = dirs::data_dir();
    let local = dirs::data_local_dir();
    prepare_data_dirs_in(csm_core::channel::CURRENT, is_default_location(cli_config_dir), clean, roaming.as_deref(), local.as_deref())
}

/// [`prepare_data_dirs`] for a channel over the given parent folders (tests).
pub fn prepare_data_dirs_in(
    channel: csm_core::channel::Channel,
    default_location: bool,
    clean: bool,
    roaming: Option<&Path>,
    local: Option<&Path>,
) -> Vec<String> {
    if clean || !default_location {
        return Vec::new();
    }
    let Some(roaming) = roaming else { return Vec::new() };
    match channel {
        csm_core::channel::Channel::Stable => {
            let mut bases: Vec<PathBuf> = [Some(roaming), local].into_iter().flatten().map(Path::to_path_buf).collect();
            bases.dedup();
            migrate_legacy_in(&bases)
        }
        csm_core::channel::Channel::Beta => seed_beta_in(roaming, local),
    }
}

/// Folders of the stable app that stay with the stable app and are never copied to a beta: the
/// remote-access identity and its enabled listener (`peer`), the paired hosts (`peer-client`), and
/// the Claude config backups (they hold a copy of `~/.claude.json`). A beta starts with remote
/// access off and nobody paired.
pub const SEED_SKIP_DIRS: &[&str] = &["peer", "peer-client", "claude-config-backups"];

/// Written inside a seeded beta folder once its copy is complete (and renamed into place).
pub const SEED_DONE_MARKER: &str = ".csm-seed-done";

/// A beta build's first start: copy the stable app's data folders (`CodeShellManager2`, roaming
/// state and local index) to the beta's (`CodeShellManager2-Beta`), so testers keep their
/// sessions, notes and add-ons. Once per base folder: only where the beta's folder does not exist
/// yet. The local folder is copied only for a beta whose roaming folder was seeded (now or on an
/// earlier start: [`SEED_DONE_MARKER`]), so a roaming folder that worked and a local one that did
/// not is retried on the next start. Never the other way, and the stable folders are only read.
/// Each folder is copied into a `.copying` folder first and renamed into place, so a copy cut
/// short never looks finished; a file that cannot be read is reported and skipped. [`SEED_SKIP_DIRS`]
/// and a database's `-wal`/`-shm`/`-journal` files (the stable app may be writing them) are left out.
pub fn seed_beta_in(roaming: &Path, local: Option<&Path>) -> Vec<String> {
    let mut report = Vec::new();
    let mut bases = vec![(roaming.to_path_buf(), true)];
    if let Some(l) = local.filter(|l| *l != roaming) {
        bases.push((l.to_path_buf(), false));
    }
    let mut roaming_seeded = roaming.join(BETA_APP_DIR).join(SEED_DONE_MARKER).is_file();
    for (base, is_roaming) in bases {
        let (from, to) = (base.join(STABLE_APP_DIR), base.join(BETA_APP_DIR));
        if !from.is_dir() || to.exists() || (!is_roaming && !roaming_seeded) {
            continue;
        }
        let staging = base.join(format!("{BETA_APP_DIR}.copying"));
        if staging.exists()
            && let Err(e) = fs::remove_dir_all(&staging)
        {
            report.push(format!("could not clear {}: {e}; not copied", staging.display()));
            continue;
        }
        let mut problems = Vec::new();
        let copied = copy_tree(&from, &staging, &mut problems, true);
        report.extend(problems);
        let done = copied
            .and_then(|n| fs::write(staging.join(SEED_DONE_MARKER), b"").map(|()| n))
            .and_then(|n| fs::rename(&staging, &to).map(|()| n));
        match done {
            Ok(n) => {
                roaming_seeded |= is_roaming;
                report.push(format!("copied {n} files from {} to {} (first start of this beta)", from.display(), to.display()));
            }
            Err(e) => {
                report.push(format!("could not copy {} to {}: {e}; it is tried again on the next start", from.display(), to.display()));
                let _ = fs::remove_dir_all(&staging);
            }
        }
    }
    report
}

/// A file the seed leaves out: temp files and the side files of a live database.
fn seed_skips_file(name: &std::ffi::OsStr) -> bool {
    let n = name.to_string_lossy().to_ascii_lowercase();
    [".tmp", "-wal", "-shm", "-journal"].iter().any(|s| n.ends_with(s))
}

/// Copies a folder tree (plain files and folders; links and temp files are skipped; at the top
/// level the [`SEED_SKIP_DIRS`] folders too). Returns the number of files copied; unreadable files
/// are reported in `problems` and skipped.
fn copy_tree(from: &Path, to: &Path, problems: &mut Vec<String>, top: bool) -> io::Result<usize> {
    fs::create_dir_all(to)?;
    let mut n = 0;
    for entry in fs::read_dir(from)? {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                problems.push(format!("could not list {}: {e}", from.display()));
                continue;
            }
        };
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            if top && SEED_SKIP_DIRS.iter().any(|d| entry.file_name().to_string_lossy().eq_ignore_ascii_case(d)) {
                continue;
            }
            match copy_tree(&src, &dst, problems, false) {
                Ok(k) => n += k,
                Err(e) => problems.push(format!("could not copy {}: {e}", src.display())),
            }
        } else if kind.is_file() && !seed_skips_file(&entry.file_name()) {
            match fs::copy(&src, &dst) {
                Ok(_) => n += 1,
                Err(e) => problems.push(format!("could not copy {}: {e}", src.display())),
            }
        }
    }
    Ok(n)
}

/// [`first_run_wpf_import`]'s decision, given what it reads from the environment (tests).
pub fn first_run_wpf_import_if(default_location: bool, clean: bool, source: Option<state::LoadSource>, wpf: Option<PathBuf>) -> Option<PathBuf> {
    if !default_location || clean || source != Some(state::LoadSource::FirstRun) {
        return None;
    }
    wpf.filter(|p| p.is_file())
}

/// The beta's `state.json`: `%AppData%\CodeShellManager2-Beta\state.json`.
pub fn beta_state_path() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(BETA_APP_DIR).join("state.json"))
}

/// The beta `state.json` a stable build offers to import ([`beta_import_offer`]'s decision, given
/// what it reads from the environment): only a stable build, only on a true first run (no state of
/// our own, not even a `.bak`), only in the default place and never for `--clean`. A tester who
/// moves from a beta to the final release would otherwise start empty beside a full beta folder.
pub fn beta_import_offer_if(
    channel: csm_core::channel::Channel,
    default_location: bool,
    clean: bool,
    source: Option<state::LoadSource>,
    beta: Option<PathBuf>,
) -> Option<PathBuf> {
    if channel != csm_core::channel::Channel::Stable || !default_location || clean || source != Some(state::LoadSource::FirstRun) {
        return None;
    }
    beta.filter(|p| p.is_file())
}

/// [`beta_import_offer_if`] over this build and machine.
pub fn beta_import_offer(cli_config_dir: Option<&Path>, clean: bool, source: Option<state::LoadSource>) -> Option<PathBuf> {
    beta_import_offer_if(csm_core::channel::CURRENT, is_default_location(cli_config_dir), clean, source, beta_state_path())
}

/// Moves the pre-v2 data folders (`CodeShellManager-rs`, roaming and local) to `CodeShellManager2`,
/// once: only where the old folder exists and the new one does not (stable builds, through
/// [`prepare_data_dirs`]). Never fails: a folder that can't be moved is reported and left alone.
pub fn migrate_legacy_in(bases: &[PathBuf]) -> Vec<String> {
    let mut report = Vec::new();
    for base in bases {
        let (old, new) = (base.join(LEGACY_APP_DIR), base.join(STABLE_APP_DIR));
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

/// How many times a file that exists but cannot be read is tried, and the pause between tries
/// (a sharing violation from a backup or antivirus scan is usually over in a moment).
const READ_TRIES: u32 = 4;
const READ_PAUSE: std::time::Duration = std::time::Duration::from_millis(200);

/// Reads a file as text. `Ok(None)` only when it does not exist (`NotFound`); any other failure
/// (a sharing violation, access denied, a folder where the file should be) is an `Err`, because a
/// file that is there but unreadable must never be mistaken for a missing or a corrupt one.
/// Unreadable bytes become U+FFFD (the parser then rejects it and the backup is tried).
fn read_text(path: &Path) -> io::Result<Option<String>> {
    read_text_with(path, READ_TRIES, READ_PAUSE)
}

fn read_text_with(path: &Path, tries: u32, pause: std::time::Duration) -> io::Result<Option<String>> {
    let mut attempt = 1;
    loop {
        match fs::read(path) {
            Ok(bytes) => return Ok(Some(String::from_utf8_lossy(&bytes).into_owned())),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) if attempt >= tries => {
                log::warn!("state: could not read {}: {e}", path.display());
                return Err(e);
            }
            Err(_) => {
                attempt += 1;
                thread::sleep(pause);
            }
        }
    }
}

/// What [`load_checked`] found.
pub struct LoadOutcome {
    pub loaded: Loaded,
    /// Set when `state.json` (or the `.bak` it needed) exists but could not be read, even after
    /// retries. The state in `loaded` is then NOT the person's: the caller must start read-only
    /// (save nothing, move nothing aside), so the real file is never replaced.
    pub unreadable: Option<String>,
}

/// Primary, then `.bak`; never fails. An I/O error is not a parse error: see [`LoadOutcome`].
pub fn load_checked(path: &Path) -> LoadOutcome {
    let primary = read_text(path);
    let backup = read_text(&backup_path(path));
    let usable = |t: &io::Result<Option<String>>| t.as_ref().ok().and_then(Clone::clone);
    let loaded = state::load(usable(&primary).as_deref(), usable(&backup).as_deref());
    // The primary parsed: a `.bak` that is merely locked does not matter.
    let primary_good = loaded.source == state::LoadSource::Primary;
    let unreadable = match (&primary, &backup) {
        (Err(e), _) => Some(format!("{}: {e}", path.display())),
        (_, Err(e)) if !primary_good => Some(format!("{}: {e}", backup_path(path).display())),
        _ => None,
    };
    LoadOutcome { loaded, unreadable }
}

/// Primary, then `.bak`; never fails (an unreadable file counts as empty here; start-up uses
/// [`load_checked`]).
pub fn load(path: &Path) -> Loaded {
    load_checked(path).loaded
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
    out.sort_by_key(|e| std::cmp::Reverse(e.0));
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
        bad.sort_by_key(|e| std::cmp::Reverse(e.0));
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

// ── tasks.json (the Board) ──────────────────────────────────────────────────────────────

/// The Board's task file, beside `state.json`.
pub const TASKS_FILE: &str = "tasks.json";

/// `<folder of state.json>/tasks.json`.
pub fn tasks_path(state_path: &Path) -> PathBuf {
    state_path.parent().unwrap_or_else(|| Path::new("")).join(TASKS_FILE)
}

/// Reads `tasks.json` (primary, then `.bak`; never fails) and keeps an unreadable file aside
/// as `state.json` does, so the next save can't overwrite it. Start-up only (file I/O).
///
/// The second value is false when a file is there but could not be read (an I/O error, not a
/// parse error): nothing is moved aside then, and the caller must not save over it.
pub fn load_tasks(path: &Path, now_ms: i64) -> (csm_core::tasks::TaskFile, bool) {
    use csm_core::tasks::TaskFile;
    let (primary, backup) = (read_text(path), read_text(&backup_path(path)));
    if let Some(e) = primary.as_ref().err().or(backup.as_ref().err()) {
        log::error!("tasks: could not read {} ({e}); the Board is not loaded and not saved this run", path.display());
        return (TaskFile::default(), false);
    }
    let (primary, backup) = (primary.ok().flatten(), backup.ok().flatten());
    let parse = |t: &Option<String>| t.as_deref().map(TaskFile::parse);
    let (file, source) = match (parse(&primary), parse(&backup)) {
        (None, None) => (TaskFile::default(), state::LoadSource::FirstRun),
        (Some(Ok(f)), _) => (f, state::LoadSource::Primary),
        (_, Some(Ok(f))) => (f, state::LoadSource::Backup),
        _ => (TaskFile::default(), state::LoadSource::Unreadable),
    };
    match source {
        state::LoadSource::Backup => log::warn!("tasks: {} unreadable, recovered from the backup", path.display()),
        state::LoadSource::Unreadable => log::error!("tasks: neither {} nor its backup could be read; starting empty", path.display()),
        _ => {}
    }
    for f in keep_unreadable(path, source, now_ms) {
        log::warn!("tasks: the unreadable file was kept as {}", f.display());
    }
    (file, true)
}

/// How a saver stands, shared between its thread and the engine (which reads it on every wake-up
/// and shows a warning while it is failing). Cheap to clone.
#[derive(Clone, Default)]
pub(crate) struct SaveHealth(std::sync::Arc<std::sync::Mutex<HealthInner>>);

#[derive(Default)]
struct HealthInner {
    /// The last failure, until a save succeeds.
    failing: Option<String>,
    /// A change the engine has not seen yet.
    changed: Option<HealthChange>,
}

/// What [`SaveHealth::take_change`] reports, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum HealthChange {
    Failed(String),
    Recovered,
}

impl SaveHealth {
    fn lock(&self) -> std::sync::MutexGuard<'_, HealthInner> {
        self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn fail(&self, message: String) {
        let mut h = self.lock();
        if h.failing.is_none() {
            h.changed = Some(HealthChange::Failed(message.clone()));
        }
        h.failing = Some(message);
    }

    fn ok(&self) {
        let mut h = self.lock();
        if h.failing.take().is_some() {
            h.changed = Some(HealthChange::Recovered);
        }
    }

    /// The failure that has not been fixed yet, for the warning strip.
    pub(crate) fn failing(&self) -> Option<String> {
        self.lock().failing.clone()
    }

    /// A failure or a recovery since the last call.
    pub(crate) fn take_change(&self) -> Option<HealthChange> {
        self.lock().changed.take()
    }
}

/// The first pause before a failed save is tried again, and the longest one.
const RETRY_FIRST: std::time::Duration = std::time::Duration::from_secs(2);
const RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

/// One background thread that writes the newest state it was given. Saves are serialised (the
/// WPF app's `SemaphoreSlim`): overlapping saves would race on the shared `.tmp`. A save that
/// fails (a full disk, a locked folder) is reported through [`SaveHealth`] and retried with a
/// growing pause until it works or a newer state replaces it.
pub(crate) struct Saver {
    tx: Sender<String>,
    thread: Option<thread::JoinHandle<()>>,
    health: SaveHealth,
}

impl Saver {
    pub(crate) fn start(path: PathBuf) -> Option<Saver> {
        Self::start_with(path, true, "csm-state-saver", RETRY_FIRST)
    }

    /// A saver for another file beside the state (`tasks.json`): the same atomic write and `.bak`,
    /// without the rolling snapshots (`snapshots` is the state file's own).
    pub(crate) fn start_plain(path: PathBuf, thread_name: &str) -> Option<Saver> {
        Self::start_with(path, false, thread_name, RETRY_FIRST)
    }

    pub(crate) fn health(&self) -> SaveHealth {
        self.health.clone()
    }

    pub(crate) fn start_with(path: PathBuf, snapshots: bool, thread_name: &str, retry_first: std::time::Duration) -> Option<Saver> {
        let (tx, rx) = mpsc::channel::<String>();
        let health = SaveHealth::default();
        let h = health.clone();
        let p = path;
        let spawned = thread::Builder::new().name(thread_name.into()).spawn(move || {
            let mut pending: Option<String> = None;
            let mut pause = retry_first;
            let mut closing = false;
            while !closing {
                let got = if pending.is_some() { rx.recv_timeout(pause) } else { rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected) };
                match got {
                    Ok(t) => pending = Some(t),
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    // The app is closing: one last try at what is pending, then done.
                    Err(mpsc::RecvTimeoutError::Disconnected) => closing = true,
                }
                // Coalesce: only the newest pending state matters.
                while let Ok(newer) = rx.try_recv() {
                    pending = Some(newer);
                }
                let Some(text) = pending.take() else { continue };
                // The Saver only exists when state is saved at all (never `--clean` or isolated).
                if snapshots {
                    match snapshot_outgoing(&p, crate::session::now_ms()) {
                        Ok(Some(s)) => log::info!("state: snapshot {}", s.display()),
                        Ok(None) => {}
                        Err(e) => log::warn!("state: snapshot of {} failed: {e}", p.display()),
                    }
                }
                match write_atomic(&p, &text) {
                    Ok(()) => {
                        h.ok();
                        pause = retry_first;
                    }
                    Err(e) => {
                        log::error!("state: save to {} failed: {e}; will retry in {:?}", p.display(), pause);
                        h.fail(format!("{}: {e}", p.display()));
                        pending = Some(text);
                        pause = (pause * 2).min(RETRY_MAX);
                    }
                }
            }
        });
        match spawned {
            Ok(t) => Some(Saver { tx, thread: Some(t), health }),
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
        fs::create_dir_all(b.path().join(STABLE_APP_DIR)).unwrap();
        let report = migrate_legacy_in(&[a.path().to_path_buf(), b.path().to_path_buf()]);
        assert_eq!(report.len(), 1, "{report:?}");
        assert!(a.path().join(STABLE_APP_DIR).join("state.json").is_file());
        assert!(!a.path().join(LEGACY_APP_DIR).exists());
        assert!(b.path().join(LEGACY_APP_DIR).is_dir());
        // A second run has nothing to do.
        assert!(migrate_legacy_in(&[a.path().to_path_buf(), b.path().to_path_buf()]).is_empty());
    }


    /// A stable app's data: roaming state with a sub-folder, a local index, and a temp file.
    fn stable_data(roaming: &Path, local: &Path) {
        fs::create_dir_all(roaming.join(STABLE_APP_DIR).join("addons").join("pack")).unwrap();
        fs::write(roaming.join(STABLE_APP_DIR).join("state.json"), r#"{"Sessions":[{"Id":"s1"}]}"#).unwrap();
        fs::write(roaming.join(STABLE_APP_DIR).join("state.json.tmp"), "torn").unwrap();
        fs::write(roaming.join(STABLE_APP_DIR).join("addons").join("pack").join("addon.toml"), "id = 'x'").unwrap();
        fs::create_dir_all(local.join(STABLE_APP_DIR)).unwrap();
        fs::write(local.join(STABLE_APP_DIR).join("index.db"), "db").unwrap();
    }

    #[test]
    fn a_beta_copies_the_stable_folders_once_and_never_back() {
        use csm_core::channel::Channel;
        let (r, l) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        stable_data(r.path(), l.path());
        let report = prepare_data_dirs_in(Channel::Beta, true, false, Some(r.path()), Some(l.path()));
        assert_eq!(report.len(), 2, "{report:?}");
        let beta = r.path().join(BETA_APP_DIR);
        assert_eq!(fs::read_to_string(beta.join("state.json")).unwrap(), r#"{"Sessions":[{"Id":"s1"}]}"#);
        assert!(beta.join("addons").join("pack").join("addon.toml").is_file());
        assert!(!beta.join("state.json.tmp").exists(), "temp files are not copied");
        assert!(l.path().join(BETA_APP_DIR).join("index.db").is_file());
        assert!(!r.path().join(format!("{BETA_APP_DIR}.copying")).exists());
        // The stable folder is untouched.
        assert!(r.path().join(STABLE_APP_DIR).join("state.json.tmp").is_file());
        // Once: the beta's own changes are never overwritten by a later start.
        fs::write(beta.join("state.json"), "{}").unwrap();
        assert!(prepare_data_dirs_in(Channel::Beta, true, false, Some(r.path()), Some(l.path())).is_empty());
        assert_eq!(fs::read_to_string(beta.join("state.json")).unwrap(), "{}");
        // A stable build never copies the beta's folder back.
        fs::remove_dir_all(r.path().join(STABLE_APP_DIR)).unwrap();
        assert!(prepare_data_dirs_in(Channel::Stable, true, false, Some(r.path()), Some(l.path())).is_empty());
        assert!(!r.path().join(STABLE_APP_DIR).exists());
    }

    #[test]
    fn a_beta_without_a_stable_app_starts_empty() {
        let (r, l) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        assert!(seed_beta_in(r.path(), Some(l.path())).is_empty());
        assert!(!r.path().join(BETA_APP_DIR).exists(), "nothing made: the beta's first run (and its 0.x import) is still a first run");
        // A copy cut short by a crash is redone from scratch.
        stable_data(r.path(), l.path());
        fs::create_dir_all(r.path().join(format!("{BETA_APP_DIR}.copying")).join("junk")).unwrap();
        seed_beta_in(r.path(), Some(l.path()));
        assert!(!r.path().join(BETA_APP_DIR).join("junk").exists());
        assert!(r.path().join(BETA_APP_DIR).join("state.json").is_file());
    }

    #[test]
    fn clean_starts_touch_no_data_folder_and_import_nothing() {
        use csm_core::channel::Channel;
        let (r, l) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        stable_data(r.path(), l.path());
        fs::create_dir_all(r.path().join(LEGACY_APP_DIR)).unwrap();
        let before = |p: &Path| fs::read_dir(p).unwrap().count();
        let (nr, nl) = (before(r.path()), before(l.path()));
        for channel in [Channel::Stable, Channel::Beta] {
            assert!(prepare_data_dirs_in(channel, true, true, Some(r.path()), Some(l.path())).is_empty(), "{channel:?}");
            // Outside the default location (--config-dir, CSM_CONFIG_DIR, portable) likewise.
            assert!(prepare_data_dirs_in(channel, false, false, Some(r.path()), Some(l.path())).is_empty(), "{channel:?}");
        }
        assert_eq!((before(r.path()), before(l.path())), (nr, nl), "no folder made, moved or copied");
        assert!(!r.path().join(BETA_APP_DIR).exists());
        // No 0.x import either (sessions, settings, notes and the Open with offer all come from it).
        let wpf = r.path().join("wpf-state.json");
        fs::write(&wpf, "{}").unwrap();
        assert_eq!(first_run_wpf_import_if(true, true, Some(LoadSource::FirstRun), Some(wpf)), None);
    }

    #[test]
    fn the_wpf_state_is_imported_on_its_own_only_on_a_true_first_run_in_the_default_place() {
        let dir = tempfile::tempdir().unwrap();
        let wpf = dir.path().join("state.json");
        fs::write(&wpf, "{}").unwrap();
        let first = Some(LoadSource::FirstRun);
        assert_eq!(first_run_wpf_import_if(true, false, first, Some(wpf.clone())), Some(wpf.clone()));
        // Not a first run: our state (or only its backup) was there, or it was unreadable.
        for other in [LoadSource::Primary, LoadSource::Backup, LoadSource::Unreadable] {
            assert_eq!(first_run_wpf_import_if(true, false, Some(other), Some(wpf.clone())), None, "{other:?}");
        }
        assert_eq!(first_run_wpf_import_if(true, false, None, Some(wpf.clone())), None, "isolated");
        assert_eq!(first_run_wpf_import_if(true, true, first, Some(wpf.clone())), None, "--clean");
        assert_eq!(first_run_wpf_import_if(false, false, first, Some(wpf.clone())), None, "--config-dir, env or portable");
        assert_eq!(first_run_wpf_import_if(true, false, first, Some(dir.path().join("none.json"))), None, "no WPF state");
        assert_eq!(first_run_wpf_import_if(true, false, first, None), None);
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
        let a = AppState { sessions: ids.iter().map(|id| ShellSession { id: (*id).into(), ..Default::default() }).collect(), ..Default::default() };
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

    #[test]
    fn the_beta_seed_leaves_out_remote_access_and_the_claude_backups() {
        let (r, l) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        stable_data(r.path(), l.path());
        let stable = r.path().join(STABLE_APP_DIR);
        for d in SEED_SKIP_DIRS {
            fs::create_dir_all(stable.join(d)).unwrap();
            fs::write(stable.join(d).join("secret.json"), "identity").unwrap();
        }
        // A same-named folder deeper down is an ordinary folder and is copied.
        fs::create_dir_all(stable.join("addons").join("peer")).unwrap();
        fs::write(stable.join("addons").join("peer").join("a.txt"), "x").unwrap();
        seed_beta_in(r.path(), Some(l.path()));
        let beta = r.path().join(BETA_APP_DIR);
        for d in SEED_SKIP_DIRS {
            assert!(!beta.join(d).exists(), "{d} must stay with the stable app");
        }
        assert!(beta.join("state.json").is_file());
        assert!(beta.join("addons").join("peer").join("a.txt").is_file());
    }

    #[test]
    fn the_beta_seed_skips_the_side_files_of_a_live_database() {
        let (r, l) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        stable_data(r.path(), l.path());
        for n in ["index.db-wal", "index.db-shm", "index.db-journal"] {
            fs::write(l.path().join(STABLE_APP_DIR).join(n), "live").unwrap();
        }
        seed_beta_in(r.path(), Some(l.path()));
        let beta_local = l.path().join(BETA_APP_DIR);
        assert!(beta_local.join("index.db").is_file());
        for n in ["index.db-wal", "index.db-shm", "index.db-journal"] {
            assert!(!beta_local.join(n).exists(), "{n}");
        }
    }

    #[test]
    fn a_seed_that_got_only_part_of_the_way_is_retried_for_the_missing_base() {
        let (r, l) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        stable_data(r.path(), l.path());
        // The local base cannot be copied: its staging path is a file (copy fails, roaming works).
        fs::write(l.path().join(format!("{BETA_APP_DIR}.copying")), "in the way").unwrap();
        let report = seed_beta_in(r.path(), Some(l.path()));
        assert!(r.path().join(BETA_APP_DIR).join(SEED_DONE_MARKER).is_file(), "{report:?}");
        assert!(!l.path().join(BETA_APP_DIR).exists(), "{report:?}");
        // The next start retries the local base (the roaming one is done and left alone).
        fs::remove_file(l.path().join(format!("{BETA_APP_DIR}.copying"))).unwrap();
        fs::write(r.path().join(BETA_APP_DIR).join("state.json"), "{}").unwrap();
        seed_beta_in(r.path(), Some(l.path()));
        assert!(l.path().join(BETA_APP_DIR).join("index.db").is_file());
        assert_eq!(fs::read_to_string(r.path().join(BETA_APP_DIR).join("state.json")).unwrap(), "{}");
    }

    #[test]
    fn the_local_base_is_not_seeded_for_a_beta_that_never_was() {
        // A beta whose roaming folder exists from its own earlier runs (no seed marker) does not
        // get the stable index copied in later.
        let (r, l) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        stable_data(r.path(), l.path());
        fs::create_dir_all(r.path().join(BETA_APP_DIR)).unwrap();
        assert!(seed_beta_in(r.path(), Some(l.path())).is_empty());
        assert!(!l.path().join(BETA_APP_DIR).exists());
    }

    #[test]
    fn a_stable_first_run_offers_the_beta_state_only_in_the_default_place() {
        use csm_core::channel::Channel;
        let dir = tempfile::tempdir().unwrap();
        let beta = dir.path().join("state.json");
        fs::write(&beta, "{}").unwrap();
        let first = Some(LoadSource::FirstRun);
        let offer = |ch, default, clean, src, p: Option<PathBuf>| beta_import_offer_if(ch, default, clean, src, p);
        assert_eq!(offer(Channel::Stable, true, false, first, Some(beta.clone())), Some(beta.clone()));
        assert_eq!(offer(Channel::Beta, true, false, first, Some(beta.clone())), None, "a beta never offers itself");
        assert_eq!(offer(Channel::Stable, false, false, first, Some(beta.clone())), None);
        assert_eq!(offer(Channel::Stable, true, true, first, Some(beta.clone())), None, "--clean");
        for other in [LoadSource::Primary, LoadSource::Backup, LoadSource::Unreadable] {
            assert_eq!(offer(Channel::Stable, true, false, Some(other), Some(beta.clone())), None, "{other:?}");
        }
        assert_eq!(offer(Channel::Stable, true, false, first, Some(dir.path().join("none.json"))), None);
        assert_eq!(offer(Channel::Stable, true, false, first, None), None);
    }

    #[test]
    fn a_file_that_is_there_but_cannot_be_read_is_an_error_not_an_absence() {
        let dir = tempfile::tempdir().unwrap();
        // A folder in place of the file: reading it fails, whatever the platform words it.
        let p = dir.path().join("state.json");
        fs::create_dir(&p).unwrap();
        assert!(read_text_with(&p, 2, std::time::Duration::from_millis(1)).is_err());
        assert!(read_text_with(&dir.path().join("none"), 2, std::time::Duration::from_millis(1)).unwrap().is_none());
        let out = load_checked(&p);
        assert!(out.unreadable.is_some());
        // The `.bak` rescues the state but the run is still read-only: the primary is not ours to replace.
        fs::write(backup_path(&p), state_text(&["a"])).unwrap();
        let out = load_checked(&p);
        assert!(out.unreadable.is_some());
        assert_eq!(out.loaded.state.sessions.len(), 1);
    }

    fn until(f: &dyn Fn() -> bool) {
        let end = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while !f() {
            assert!(std::time::Instant::now() < end, "timed out");
            thread::sleep(std::time::Duration::from_millis(5));
        }
    }

    #[test]
    fn a_saver_reports_a_failure_retries_and_reports_the_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::create_dir(temp_path(&path)).unwrap();
        let saver = Saver::start_with(path.clone(), false, "test-saver", std::time::Duration::from_millis(20)).unwrap();
        let health = saver.health();
        saver.save(state_text(&["a"]));
        until(&|| health.failing().is_some());
        assert!(matches!(health.take_change(), Some(HealthChange::Failed(_))));
        assert_eq!(health.take_change(), None, "reported once");
        // Space is back: the same pending state is written without a new save.
        fs::remove_dir(temp_path(&path)).unwrap();
        until(&|| health.failing().is_none());
        assert_eq!(health.take_change(), Some(HealthChange::Recovered));
        assert_eq!(load(&path).state.sessions.len(), 1);
        saver.finish(None);
    }

    #[test]
    fn a_newer_state_replaces_a_pending_one_that_could_not_be_written() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        fs::create_dir(temp_path(&path)).unwrap();
        let saver = Saver::start_with(path.clone(), false, "test-saver", std::time::Duration::from_millis(20)).unwrap();
        let health = saver.health();
        saver.save(state_text(&["old"]));
        until(&|| health.failing().is_some());
        saver.save(state_text(&["new"]));
        fs::remove_dir(temp_path(&path)).unwrap();
        saver.finish(None);
        assert_eq!(load(&path).state.sessions[0].id, "new");
    }
}
