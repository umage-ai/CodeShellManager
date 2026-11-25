//! Plan, apply and uninstall. `plan` is read-only and is what `--dry-run` prints; `apply` writes
//! with a backup, verifies by reading the file back, and rolls back if anything is off.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::clients::{Client, Host, SERVER_NAME, clients, is_owned_entry};
use crate::config;

/// What to connect.
#[derive(Debug, Clone, Default)]
pub struct PlanRequest {
    /// Client ids (`claude_code`, `codex`, ...). Empty with `all_detected` false also means the
    /// detected ones.
    pub clients: Vec<String>,
    pub all_detected: bool,
    /// The program followed by its arguments: `[<csm>, "mcp"]`.
    pub command: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionKind {
    /// The config file does not exist; it is created.
    Create,
    /// The file exists without our entry.
    Add,
    /// Our entry exists (written by setup) and differs.
    Update,
    Unchanged,
    /// Nothing is written; `reason` says why and what to do.
    Blocked,
}

/// One client's part of the plan.
#[derive(Debug, Clone, Serialize)]
pub struct Action {
    pub client: &'static str,
    pub display: &'static str,
    pub config: PathBuf,
    pub kind: ActionKind,
    pub reason: Option<String>,
    pub entry: Value,
    /// The file as it is now (`None` when it does not exist) and as it would be.
    #[serde(skip)]
    pub before: Option<String>,
    #[serde(skip)]
    pub after: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Plan {
    pub command: Vec<String>,
    pub actions: Vec<Action>,
    /// Some action is blocked.
    pub blocked: bool,
}

impl Plan {
    pub fn change_count(&self) -> usize {
        self.actions.iter().filter(|a| matches!(a.kind, ActionKind::Create | ActionKind::Add | ActionKind::Update)).count()
    }

    /// The exact change, for `--dry-run`: per client the file and the lines that change.
    pub fn describe(&self) -> String {
        let mut out = String::new();
        for a in &self.actions {
            let verb = match a.kind {
                ActionKind::Create => "create",
                ActionKind::Add => "add the entry to",
                ActionKind::Update => "update the entry in",
                ActionKind::Unchanged => "nothing to do for",
                ActionKind::Blocked => "BLOCKED:",
            };
            let _ = writeln!(out, "{} {} ({})", verb, a.display, a.config.display());
            if let Some(r) = &a.reason {
                let _ = writeln!(out, "  {r}");
            }
            if matches!(a.kind, ActionKind::Create | ActionKind::Add | ActionKind::Update)
                && let Some(after) = &a.after
            {
                out.push_str(&line_diff(a.before.as_deref().unwrap_or(""), after));
            }
        }
        out
    }
}

/// The changed lines between two texts: the common leading and trailing lines are dropped.
pub fn line_diff(before: &str, after: &str) -> String {
    let b: Vec<&str> = before.lines().collect();
    let a: Vec<&str> = after.lines().collect();
    let head = b.iter().zip(&a).take_while(|(x, y)| x == y).count();
    let tail = b.iter().rev().zip(a.iter().rev()).take(b.len().min(a.len()) - head).take_while(|(x, y)| x == y).count();
    let removed = b.get(head..b.len() - tail).unwrap_or_default();
    let added = a.get(head..a.len() - tail).unwrap_or_default();
    let mut out = String::new();
    for l in removed.iter().take(40) {
        let _ = writeln!(out, "  - {l}");
    }
    for l in added.iter().take(40) {
        let _ = writeln!(out, "  + {l}");
    }
    out
}

/// `[<csm>, "mcp"]`: this executable when it is `csm`, else a `csm` beside it, else on `PATH`.
pub fn default_command() -> Result<Vec<String>, String> {
    let name = format!("csm{}", std::env::consts::EXE_SUFFIX);
    let exe = std::env::current_exe().ok();
    let found = exe
        .as_ref()
        .filter(|e| e.file_name().is_some_and(|n| n.eq_ignore_ascii_case(&name)))
        .cloned()
        .or_else(|| exe.as_ref().and_then(|e| e.parent()).map(|d| d.join(&name)).filter(|p| p.is_file()))
        .or_else(|| csm_engine::which::which("csm"));
    let path = found.ok_or("could not find the csm program; pass --command <path to csm>")?;
    Ok(vec![path.to_string_lossy().into_owned(), "mcp".to_owned()])
}

fn select<'a>(all: &'a [Client], req_clients: &[String], all_detected: bool) -> Result<Vec<&'a Client>, String> {
    if req_clients.is_empty() || all_detected {
        return Ok(all.iter().filter(|c| c.detected()).collect());
    }
    let mut out: Vec<&Client> = Vec::new();
    for name in req_clients {
        let c = all.iter().find(|c| c.is(name)).ok_or_else(|| {
            let ids: Vec<&str> = all.iter().map(|c| c.id).collect();
            format!("unknown client `{name}`; choose from {}", ids.join(", "))
        })?;
        if !out.iter().any(|o| o.id == c.id) {
            out.push(c);
        }
    }
    Ok(out)
}

fn read_text(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read(path) {
        Ok(bytes) => String::from_utf8(bytes).map(Some).map_err(|_| "the file is not UTF-8 text".to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("could not read it: {e}")),
    }
}

fn same_command(a: &Value, b: &Value) -> bool {
    a.get("command") == b.get("command") && a.get("args") == b.get("args")
}

/// The entry to write over an existing one of ours (COR-31): the existing entry with only what
/// setup owns changed. `command` is set; `args` keep everything the person added after the
/// arguments setup owns (`mcp`); `env` and any other key stay; `type` and the like are added only
/// when missing.
fn merge_entry(existing: &Value, desired: &Value) -> Value {
    let Some(mut merged) = existing.as_object().cloned() else { return desired.clone() };
    let owned: Vec<Value> = desired.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    let theirs: Vec<Value> = existing.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    let kept = theirs.iter().zip(&owned).take_while(|(a, b)| a == b).count();
    let mut args = owned;
    args.extend(theirs.into_iter().skip(kept));
    if let Some(command) = desired.get("command") {
        merged.insert("command".to_owned(), command.clone());
    }
    merged.insert("args".to_owned(), Value::Array(args));
    for (k, v) in desired.as_object().into_iter().flatten() {
        merged.entry(k.clone()).or_insert_with(|| v.clone());
    }
    Value::Object(merged)
}

/// Whether the entry already runs `desired`: the same program, and the arguments setup owns first
/// (anything after them is the person's).
fn runs_same(existing: &Value, desired: &Value) -> bool {
    let owned = desired.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    let theirs = existing.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
    existing.get("command") == desired.get("command") && theirs.starts_with(&owned)
}

/// What setup would do for one client.
pub fn inspect_client(c: &Client, command: &[String]) -> Action {
    let entry = c.desired_entry(command);
    let mut action = Action {
        client: c.id,
        display: c.display,
        config: c.config.clone(),
        kind: ActionKind::Blocked,
        reason: None,
        entry: entry.clone(),
        before: None,
        after: None,
    };
    let blocked = |mut a: Action, why: String| {
        a.reason = Some(why);
        a
    };
    let before = match read_text(&c.config) {
        Ok(t) => t,
        Err(e) => return blocked(action, e),
    };
    let existing = match before.as_deref() {
        None => None,
        Some(text) => match config::read_entry(c, text) {
            Ok(e) => e,
            Err(e) => return blocked(action, format!("the config is not valid ({e}); fix or move it, then run setup again")),
        },
    };
    action.kind = match &existing {
        None if before.is_none() => ActionKind::Create,
        None => ActionKind::Add,
        Some(e) if runs_same(e, &entry) => ActionKind::Unchanged,
        Some(e) if is_owned_entry(e) => {
            // Our entry, now out of date: change what we own, keep the rest of what is there.
            action.entry = merge_entry(e, &entry);
            ActionKind::Update
        }
        Some(_) => {
            return blocked(action, format!("it already has an entry named `{SERVER_NAME}` that setup did not add; remove it or rename it first"));
        }
    };
    if action.kind != ActionKind::Unchanged {
        match config::set_entry(c, before.as_deref().unwrap_or(""), &action.entry) {
            Ok(after) => action.after = Some(after),
            Err(e) => {
                action.kind = ActionKind::Blocked;
                action.reason = Some(format!("could not edit the config safely ({e})"));
            }
        }
    }
    action.before = before;
    action
}

/// Read-only: what `setup` would do.
pub fn plan(host: &Host, req: &PlanRequest) -> Result<Plan, String> {
    if req.command.is_empty() {
        return Err("no command to register".to_owned());
    }
    let all = clients(host);
    let chosen = select(&all, &req.clients, req.all_detected)?;
    let actions: Vec<Action> = chosen.into_iter().map(|c| inspect_client(c, &req.command)).collect();
    let blocked = actions.iter().any(|a| a.kind == ActionKind::Blocked);
    Ok(Plan { command: req.command.clone(), actions, blocked })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "result", content = "detail")]
pub enum Outcome {
    Written,
    Unchanged,
    Skipped(String),
    /// Written, then found wrong, then the file was restored.
    RolledBack(String),
    Failed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct FileResult {
    pub client: &'static str,
    pub config: PathBuf,
    pub outcome: Outcome,
    pub backup: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ApplyReport {
    pub files: Vec<FileResult>,
}

impl ApplyReport {
    pub fn ok(&self) -> bool {
        self.files.iter().all(|f| matches!(f.outcome, Outcome::Written | Outcome::Unchanged | Outcome::Skipped(_)))
    }

    /// Every client was written or was already as wanted: nothing was skipped (blocked) or failed.
    /// `csm setup` exits non-zero when this is false (COR-30), so a script that asked for a client
    /// that was blocked does not see success.
    pub fn complete(&self) -> bool {
        self.files.iter().all(|f| matches!(f.outcome, Outcome::Written | Outcome::Unchanged))
    }
}

/// `<file>.csm-backup`, or `.csm-backup-2`, `-3`... when that exists: an older backup is never
/// overwritten.
pub fn backup_path(p: &Path) -> PathBuf {
    let base = p.as_os_str().to_owned();
    let with = |suffix: String| {
        let mut s = base.clone();
        s.push(suffix);
        PathBuf::from(s)
    };
    let first = with(".csm-backup".to_owned());
    if !first.exists() {
        return first;
    }
    (2..1000).map(|n| with(format!(".csm-backup-{n}"))).find(|c| !c.exists()).unwrap_or(first)
}

/// How many `.csm-backup*` files of a config setup keeps (the newest).
pub const BACKUPS_KEPT: usize = 3;

/// Creates a new file that did not exist (`create_new`: never through an existing file or link),
/// private to the user on Unix, and writes `bytes`. `like` is a file whose permissions the new
/// one takes after it is written (a backup or temp file must not be more open than the config).
fn create_new_like(path: &Path, bytes: &[u8], like: Option<&Path>) -> std::io::Result<()> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path)?;
    let done = f.write_all(bytes).and_then(|()| f.sync_all());
    drop(f);
    done.and_then(|()| match like.and_then(|l| std::fs::metadata(l).ok()) {
        Some(meta) => std::fs::set_permissions(path, meta.permissions()),
        None => Ok(()),
    })
    .inspect_err(|_| {
        let _ = std::fs::remove_file(path);
    })
}

/// The file a write to `path` must replace: the target of a link (the link itself stays), else
/// `path`. A link that leads nowhere is an error, not a file to replace.
fn write_target(path: &Path) -> std::io::Result<PathBuf> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path),
        _ => Ok(path.to_path_buf()),
    }
}

/// Write `text` to `path` through a temp file in the same folder (made with `create_new`, with the
/// permissions of the file it replaces). A symlinked config is written through: the link stays and
/// its target is replaced. With `expect` (what the caller last read), the file is read once more
/// just before the rename and nothing is replaced if it differs: Claude Code rewrites its config
/// constantly, and a change in that window would be lost (SEC-41).
fn write_atomic(path: &Path, text: &str, expect: Option<&Option<String>>) -> std::io::Result<()> {
    let path = write_target(path)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let like = path.is_file().then_some(path.as_path());
    let mut made = None;
    for n in 0..100u32 {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".csm-tmp-{}-{n}", std::process::id()));
        let tmp = PathBuf::from(name);
        match create_new_like(&tmp, text.as_bytes(), like) {
            Ok(()) => {
                made = Some(tmp);
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    let tmp = made.ok_or_else(|| std::io::Error::other("could not make a temp file"))?;
    if let Some(expected) = expect
        && read_text(&path).as_ref() != Ok(expected)
    {
        let _ = std::fs::remove_file(&tmp);
        return Err(std::io::Error::other("the file changed while it was being written; nothing was replaced, run setup again"));
    }
    std::fs::rename(&tmp, &path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// Keeps the newest [`BACKUPS_KEPT`] `<config>.csm-backup*` files of `config`.
fn prune_backups(config: &Path) {
    let (Some(dir), Some(name)) = (config.parent(), config.file_name().map(|n| n.to_string_lossy().into_owned())) else { return };
    let prefix = format!("{name}.csm-backup");
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut mine: Vec<(std::time::SystemTime, PathBuf)> = rd
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .collect();
    mine.sort_by_key(|m| std::cmp::Reverse(m.0));
    for (_, p) in mine.into_iter().skip(BACKUPS_KEPT) {
        let _ = std::fs::remove_file(p);
    }
}

/// Makes the backup of `config` from its current text: a new file (never overwriting an older
/// one), with the config's permissions; then prunes the old ones.
fn make_backup(config: &Path, old: &str) -> std::io::Result<PathBuf> {
    let target = write_target(config)?;
    for _ in 0..1000 {
        let backup = backup_path(config);
        match create_new_like(&backup, old.as_bytes(), Some(&target)) {
            Ok(()) => {
                prune_backups(config);
                return Ok(backup);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("too many backups"))
}

/// `value` with the entry at `path` removed, and any object that leaves empty along the way
/// (so a table created just for our entry compares equal to its absence).
fn without(mut value: Value, path: &[&str]) -> Value {
    fn go(v: &mut Value, path: &[&str]) {
        let Some((first, rest)) = path.split_first() else { return };
        let Some(map) = v.as_object_mut() else { return };
        if rest.is_empty() {
            map.remove(*first);
        } else if let Some(child) = map.get_mut(*first) {
            go(child, rest);
            if child.as_object().is_some_and(serde_json::Map::is_empty) {
                map.remove(*first);
            }
        }
    }
    go(&mut value, path);
    value
}

/// Everything but our entry is as it was, and our entry is what we meant to write.
pub fn verify(c: &Client, before: Option<&str>, written: &str, desired: &Value) -> Result<(), String> {
    let got = config::read_entry(c, written)?.ok_or("the entry is not in the file after writing")?;
    if !same_command(&got, desired) {
        return Err("the entry read back does not match what was written".to_owned());
    }
    let path = c.entry_path();
    let old = config::parse(c, before.unwrap_or(""))?;
    let new = config::parse(c, written)?;
    if without(old, &path) != without(new, &path) {
        return Err("something besides our entry changed".to_owned());
    }
    Ok(())
}

fn apply_one(a: &Action, c: Option<&Client>) -> FileResult {
    let mut result = FileResult { client: a.client, config: a.config.clone(), outcome: Outcome::Unchanged, backup: None };
    match a.kind {
        ActionKind::Unchanged => return result,
        ActionKind::Blocked => {
            result.outcome = Outcome::Skipped(a.reason.clone().unwrap_or_default());
            return result;
        }
        _ => {}
    }
    let (Some(after), Some(c)) = (&a.after, c) else {
        result.outcome = Outcome::Failed("nothing to write".to_owned());
        return result;
    };
    // The file must still be what the plan saw: Claude Code rewrites its config constantly.
    match read_text(&a.config) {
        Ok(now) if now == a.before => {}
        Ok(_) => {
            result.outcome = Outcome::Failed("the file changed since it was planned; run setup again".to_owned());
            return result;
        }
        Err(e) => {
            result.outcome = Outcome::Failed(e);
            return result;
        }
    }
    if let Some(old) = &a.before {
        match make_backup(&a.config, old) {
            Ok(backup) => result.backup = Some(backup),
            Err(e) => {
                result.outcome = Outcome::Failed(format!("could not make a backup of {}: {e}", a.config.display()));
                return result;
            }
        }
    }
    if let Err(e) = write_atomic(&a.config, after, Some(&a.before)) {
        result.outcome = Outcome::Failed(format!("could not write: {e}"));
        return result;
    }
    let check =
        read_text(&a.config).and_then(|t| t.ok_or_else(|| "the file vanished".to_owned())).and_then(|t| verify(c, a.before.as_deref(), &t, &a.entry));
    result.outcome = match check {
        Ok(()) => Outcome::Written,
        Err(why) => {
            let restored = match &a.before {
                Some(old) => write_atomic(&a.config, old, None).is_ok(),
                None => std::fs::remove_file(&a.config).is_ok(),
            };
            Outcome::RolledBack(if restored {
                why
            } else {
                format!("{why}; and the original could not be restored: copy {:?} back by hand", result.backup)
            })
        }
    };
    result
}

/// Do the plan. Each client is independent; one failing does not stop the others.
pub fn apply(host: &Host, plan: &Plan) -> ApplyReport {
    let all = clients(host);
    let files = plan.actions.iter().map(|a| apply_one(a, all.iter().find(|c| c.id == a.client))).collect();
    ApplyReport { files }
}

/// After the install folder moved (`...\umage\CodeShellManager` to `...\umage.ai\CodeShellManager`):
/// point every entry setup added that still names the old folder at `command` (`[<csm>, "mcp"]`).
/// Never adds an entry to a client that has none, never touches an entry setup did not write, and
/// goes through the same plan, backup, verify and roll back as `csm setup`. Blocking file work:
/// call it from a worker thread. Returns one result per entry it changed (or failed to).
pub fn repair_moved(host: &Host, command: &[String]) -> Vec<FileResult> {
    let Some(program) = command.first() else { return Vec::new() };
    let mut out = Vec::new();
    for c in &clients(host) {
        let Ok(Some(text)) = read_text(&c.config) else { continue };
        let Ok(Some(entry)) = config::read_entry(c, &text) else { continue };
        if !crate::clients::is_moved_entry(&entry, Path::new(program)) {
            continue;
        }
        let action = inspect_client(c, command);
        if action.kind == ActionKind::Update {
            out.push(apply_one(&action, Some(c)));
        }
    }
    out
}

/// [`repair_moved`], but not while another program may be rewriting the same files (SEC-41): Claude
/// Code rewrites `~/.claude.json` whenever it runs, and a rewrite that lands between our read and
/// our rename would be lost. `busy` says whether a Claude process runs (it is asked off the UI
/// thread: this blocks). While it does, this waits `pause` and asks again, up to `attempts` times,
/// and then gives up for this start (the next start repairs). Returns what [`repair_moved`] did,
/// or nothing when it never got a quiet moment.
pub fn repair_moved_when_idle(host: &Host, command: &[String], busy: &dyn Fn() -> bool, attempts: u32, pause: std::time::Duration) -> Vec<FileResult> {
    for attempt in 0..attempts {
        if !busy() {
            return repair_moved(host, command);
        }
        if attempt + 1 < attempts {
            std::thread::sleep(pause);
        }
    }
    Vec::new()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "result", content = "detail")]
pub enum RemoveOutcome {
    Removed,
    /// Dry run: it would be removed.
    WouldRemove,
    NotPresent,
    /// An entry with our name that setup did not add.
    LeftAlone(String),
    Failed(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct UninstallResult {
    pub client: &'static str,
    pub config: PathBuf,
    pub outcome: RemoveOutcome,
    pub backup: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UninstallReport {
    pub files: Vec<UninstallResult>,
}

/// Remove the entries setup added (and only those), from `ids` or every client when empty.
/// Backups made by setup stay.
pub fn uninstall(host: &Host, ids: &[String], dry_run: bool) -> Result<UninstallReport, String> {
    let all = clients(host);
    let chosen: Vec<&Client> = if ids.is_empty() { all.iter().collect() } else { select(&all, ids, false)? };
    let mut files = Vec::new();
    for c in chosen {
        let mut r = UninstallResult { client: c.id, config: c.config.clone(), outcome: RemoveOutcome::NotPresent, backup: None };
        match read_text(&c.config) {
            Ok(None) => {}
            Err(e) => r.outcome = RemoveOutcome::Failed(e),
            Ok(Some(text)) => match config::read_entry(c, &text) {
                Err(e) => r.outcome = RemoveOutcome::Failed(format!("the config is not valid ({e}); left as it is")),
                Ok(None) => {}
                Ok(Some(entry)) if !is_owned_entry(&entry) => {
                    r.outcome = RemoveOutcome::LeftAlone(format!("`{SERVER_NAME}` here was not added by setup"));
                }
                Ok(Some(_)) if dry_run => r.outcome = RemoveOutcome::WouldRemove,
                Ok(Some(_)) => match config::remove_entry(c, &text) {
                    Ok(Some(after)) => {
                        let expected = Some(text.clone());
                        let done = make_backup(&c.config, &text).and_then(|backup| write_atomic(&c.config, &after, Some(&expected)).map(|()| backup));
                        r.outcome = match done {
                            Ok(backup) => {
                                r.backup = Some(backup);
                                RemoveOutcome::Removed
                            }
                            Err(e) => RemoveOutcome::Failed(format!("could not write: {e}")),
                        };
                    }
                    Ok(None) => {}
                    Err(e) => r.outcome = RemoveOutcome::Failed(e),
                },
            },
        }
        files.push(r);
    }
    Ok(UninstallReport { files })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn a_file_that_changes_just_before_the_rename_is_not_replaced() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("claude.json");
        std::fs::write(&p, "{\"a\": 1}").unwrap();
        // The caller read `{"a": 1}`, but Claude Code has written since.
        std::fs::write(&p, "{\"a\": 2}").unwrap();
        let err = write_atomic(&p, "{\"ours\": true}", Some(&Some("{\"a\": 1}".to_owned()))).unwrap_err();
        assert!(err.to_string().contains("changed while"), "{err}");
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"a\": 2}");
        let left: Vec<_> = std::fs::read_dir(d.path()).unwrap().flatten().map(|e| e.file_name()).collect();
        assert_eq!(left.len(), 1, "no temp file left behind: {left:?}");
        // Unchanged: replaced.
        write_atomic(&p, "{\"ours\": true}", Some(&Some("{\"a\": 2}".to_owned()))).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"ours\": true}");
        // A file that was missing when read must still be missing.
        let q = d.path().join("new.json");
        std::fs::write(&q, "appeared").unwrap();
        assert!(write_atomic(&q, "x", Some(&None)).is_err());
        assert_eq!(std::fs::read_to_string(&q).unwrap(), "appeared");
    }

    #[test]
    fn the_temp_file_is_new_and_never_an_existing_file() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("cfg.json");
        std::fs::write(&p, "{}").unwrap();
        // Something planted at the first temp name is not written through (it is not even touched).
        let planted = d.path().join(format!("cfg.json.csm-tmp-{}-0", std::process::id()));
        std::fs::write(&planted, "planted").unwrap();
        write_atomic(&p, "{\"x\": 1}", None).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\"x\": 1}");
        assert_eq!(std::fs::read_to_string(&planted).unwrap(), "planted");
    }

    #[test]
    fn a_merge_changes_only_what_setup_owns() {
        let existing = serde_json::json!({"type": "stdio", "command": "old", "args": ["mcp", "--x"], "env": {"A": "1"}});
        let desired = serde_json::json!({"type": "stdio", "command": "new", "args": ["mcp"]});
        let m = merge_entry(&existing, &desired);
        assert_eq!(m, serde_json::json!({"type": "stdio", "command": "new", "args": ["mcp", "--x"], "env": {"A": "1"}}));
        assert!(runs_same(&m, &desired));
        assert!(!runs_same(&existing, &desired));
    }
}
