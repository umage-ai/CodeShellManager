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
        Some(e) if same_command(e, &entry) => ActionKind::Unchanged,
        Some(e) if is_owned_entry(e) => ActionKind::Update,
        Some(_) => {
            return blocked(action, format!("it already has an entry named `{SERVER_NAME}` that setup did not add; remove it or rename it first"));
        }
    };
    if action.kind != ActionKind::Unchanged {
        match config::set_entry(c, before.as_deref().unwrap_or(""), &entry) {
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

/// Write `text` to `path` through a temp file in the same folder.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".csm-tmp-{}", std::process::id()));
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
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
        let backup = backup_path(&a.config);
        if let Err(e) = std::fs::write(&backup, old) {
            result.outcome = Outcome::Failed(format!("could not make a backup at {}: {e}", backup.display()));
            return result;
        }
        result.backup = Some(backup);
    }
    if let Err(e) = write_atomic(&a.config, after) {
        result.outcome = Outcome::Failed(format!("could not write: {e}"));
        return result;
    }
    let check =
        read_text(&a.config).and_then(|t| t.ok_or_else(|| "the file vanished".to_owned())).and_then(|t| verify(c, a.before.as_deref(), &t, &a.entry));
    result.outcome = match check {
        Ok(()) => Outcome::Written,
        Err(why) => {
            let restored = match &a.before {
                Some(old) => write_atomic(&a.config, old).is_ok(),
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
                        let backup = backup_path(&c.config);
                        r.outcome = match std::fs::write(&backup, &text).and_then(|()| write_atomic(&c.config, &after)) {
                            Ok(()) => {
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
