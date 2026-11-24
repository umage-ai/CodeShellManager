//! `csm doctor`: what works, what does not, and the one line that fixes it. Every failing check
//! carries a remedy.

use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use csm_engine::{proc, which};
use csm_mcp::{Backend, RemoteBackend, RemoteOptions};
use serde::Serialize;
use serde_json::{Value, json};

use crate::clients::{Client, Host, SERVER_NAME, clients};
use crate::plan::{ActionKind, inspect_client};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    /// Works, but something is worth knowing or fixing.
    Warn,
    Fail,
    /// Does not apply here.
    Skipped,
}

#[derive(Debug, Clone, Serialize)]
pub struct Check {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    /// What to do about it (always set for Warn and Fail).
    pub remedy: Option<String>,
}

impl Check {
    pub fn new(name: impl Into<String>, status: CheckStatus, message: impl Into<String>) -> Check {
        Check { name: name.into(), status, message: message.into(), remedy: None }
    }

    pub fn fix(mut self, remedy: impl Into<String>) -> Check {
        self.remedy = Some(remedy.into());
        self
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<Check>,
}

impl DoctorReport {
    /// No check failed (warnings are allowed).
    pub fn ok(&self) -> bool {
        self.checks.iter().all(|c| c.status != CheckStatus::Fail)
    }

    pub fn render(&self) -> String {
        let mut out = String::new();
        for c in &self.checks {
            let tag = match c.status {
                CheckStatus::Ok => "ok  ",
                CheckStatus::Warn => "warn",
                CheckStatus::Fail => "FAIL",
                CheckStatus::Skipped => "skip",
            };
            let _ = writeln!(out, "[{tag}] {}: {}", c.name, c.message);
            if let Some(r) = &c.remedy {
                let _ = writeln!(out, "       fix: {r}");
            }
        }
        out
    }
}

/// What to check and how.
#[derive(Debug, Clone, Default)]
pub struct DoctorRequest {
    /// How to reach the app (never started by the doctor).
    pub remote: RemoteOptions,
    /// The MCP self-test command: program and arguments (`csm mcp --in-process`). `None` skips it.
    pub self_test: Option<Vec<String>>,
    /// Where the ConPTY files should be: the app's folder. `None`: beside this executable.
    pub app_dir: Option<PathBuf>,
    /// Check the Authenticode signature (spawns PowerShell; Windows only).
    pub check_signing: bool,
}

pub fn doctor(host: &Host, req: &DoctorRequest) -> DoctorReport {
    let mut checks = vec![check_app(&req.remote), check_conpty(req), check_claude(host), check_git()];
    checks.push(check_wsl());
    if let Some(cmd) = &req.self_test
        && let Some((program, args)) = cmd.split_first()
    {
        checks.push(mcp_self_test(program, args, Duration::from_secs(20)));
    } else {
        checks.push(Check::new("mcp:self-test", CheckStatus::Skipped, "skipped"));
    }
    checks.extend(check_clients(host));
    DoctorReport { checks }
}

/// What this csm looks for: its channel and the app id that names the discovery record and the
/// pipe (`craft-<user>-<app id>-<pid>`), plus what is actually published.
fn channel_note(remote: &RemoteOptions) -> String {
    let ch = csm_engine::channel::CURRENT;
    let mut note = format!("{} channel, looking for app id {} (pipe craft-<user>-{}-<pid>)", ch.id(), ch.ipc_app_id(), ch.ipc_app_id());
    match csm_mcp::published_apps(remote.runtime.as_ref()) {
        Ok(apps) if apps.is_empty() => note.push_str("; no CodeShellManager is published"),
        Ok(apps) => {
            let list: Vec<String> = apps.iter().map(|(a, p)| format!("{a} (pid {p})")).collect();
            note.push_str(&format!("; published: {}", list.join(", ")));
        }
        Err(e) => note.push_str(&format!("; could not list the published apps: {e}")),
    }
    note
}

fn check_app(remote: &RemoteOptions) -> Check {
    let name = "app";
    let note = channel_note(remote);
    let options = RemoteOptions { start: false, ..remote.clone() };
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new().name("csm-doctor-app".into()).spawn(move || {
        let _ = tx.send(RemoteBackend::new(options).tools());
    });
    if spawned.is_err() {
        return Check::new(name, CheckStatus::Fail, "could not start the check");
    }
    match rx.recv_timeout(Duration::from_secs(8)) {
        Ok(Ok(tools)) => Check::new(name, CheckStatus::Ok, format!("CodeShellManager is running ({} tools); {note}.", tools.len())),
        Ok(Err(e)) => Check::new(name, CheckStatus::Warn, format!("{e} [{note}]"))
            .fix("Start CodeShellManager, or run csm with --start. Setup and doctor work without it."),
        Err(_) => Check::new(name, CheckStatus::Fail, "CodeShellManager did not answer within 8 seconds.")
            .fix("It may be stuck: quit it from the task manager and start it again."),
    }
}

fn check_conpty(req: &DoctorRequest) -> Check {
    let name = "conpty";
    if !cfg!(windows) {
        return Check::new(name, CheckStatus::Skipped, "Terminals use the system's PTY on this platform.");
    }
    let dir = req.app_dir.clone().or_else(|| std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf))).unwrap_or_default();
    let files = ["conpty.dll", "OpenConsole.exe"];
    let missing: Vec<&str> = files.iter().copied().filter(|f| !dir.join(f).is_file()).collect();
    if !missing.is_empty() {
        return Check::new(
            name,
            CheckStatus::Warn,
            format!("{} not found beside the app in {}; terminals fall back to the older built-in ConPTY.", missing.join(" and "), dir.display()),
        )
        .fix(
            "Reinstall CodeShellManager (the installer puts both files beside the program), or run scripts\\get-conpty.ps1 for a development build.",
        );
    }
    if !req.check_signing {
        return Check::new(name, CheckStatus::Ok, "conpty.dll and OpenConsole.exe are present.");
    }
    for f in files {
        match signature(&dir.join(f)) {
            Ok((status, subject)) if status == "Valid" && subject.contains("Microsoft") => {}
            Ok((status, subject)) => {
                return Check::new(name, CheckStatus::Fail, format!("{f} is not validly signed by Microsoft (status {status}, signer {subject:?})."))
                    .fix("Replace it with the original from the Microsoft.Windows.Console.ConPTY package (scripts\\get-conpty.ps1) or reinstall.");
            }
            Err(e) => {
                return Check::new(name, CheckStatus::Warn, format!("could not check the signature of {f}: {e}"))
                    .fix("Run: Get-AuthenticodeSignature on the file in PowerShell.");
            }
        }
    }
    Check::new(name, CheckStatus::Ok, "conpty.dll and OpenConsole.exe are present and signed by Microsoft.")
}

/// `(status, signer subject)` from PowerShell. The path travels in the environment, not in the
/// script text, so no file name can become code.
fn signature(path: &Path) -> Result<(String, String), String> {
    let mut cmd = Command::new("powershell.exe");
    cmd.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$s = Get-AuthenticodeSignature -LiteralPath $env:CSM_CHECK_FILE; $s.Status; $s.SignerCertificate.Subject",
    ]);
    cmd.env("CSM_CHECK_FILE", path);
    let out = proc::run_ok(&mut cmd, Duration::from_secs(20)).ok_or("PowerShell did not answer")?;
    let mut lines = out.lines().map(str::trim);
    let status = lines.next().unwrap_or("").to_owned();
    let subject = lines.next().unwrap_or("").to_owned();
    if status.is_empty() { Err("no answer".to_owned()) } else { Ok((status, subject)) }
}

fn first_line(s: &str) -> String {
    s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").chars().take(120).collect()
}

fn version_of(program: &Path) -> Option<String> {
    let (exe, pre) = which::launch_target(program.as_os_str());
    let mut cmd = Command::new(exe);
    cmd.args(pre).arg("--version");
    proc::run_ok(&mut cmd, Duration::from_secs(10)).map(|s| first_line(&s))
}

fn check_claude(host: &Host) -> Check {
    let name = "claude";
    let fix = "Install Claude Code (https://claude.com/claude-code); if it is installed, add its folder to PATH or start CodeShellManager from a shell that has it.";
    match which::which("claude") {
        None => Check::new(name, CheckStatus::Warn, "claude was not found on PATH or in the usual install folders.").fix(fix),
        Some(p) => match version_of(&p) {
            Some(v) if !v.is_empty() => {
                let via =
                    host.var("CLAUDE_CONFIG_DIR").map(|d| format!(" (config folder {} from CLAUDE_CONFIG_DIR)", d.display())).unwrap_or_default();
                Check::new(name, CheckStatus::Ok, format!("{v} at {}{via}", p.display()))
            }
            _ => Check::new(name, CheckStatus::Warn, format!("{} was found but `--version` did not answer.", p.display()))
                .fix("Run `claude --version` in a terminal to see the error, or reinstall Claude Code."),
        },
    }
}

fn check_git() -> Check {
    let name = "git";
    match which::which("git") {
        None => Check::new(name, CheckStatus::Warn, "git was not found; the git chip and worktrees will be unavailable.")
            .fix("Install Git (https://git-scm.com/downloads) and restart CodeShellManager."),
        Some(p) => match version_of(&p) {
            Some(v) => Check::new(name, CheckStatus::Ok, format!("{v} at {}", p.display())),
            None => Check::new(name, CheckStatus::Warn, format!("{} was found but `--version` did not answer.", p.display()))
                .fix("Run `git --version` in a terminal to see the error."),
        },
    }
}

fn check_wsl() -> Check {
    let name = "wsl";
    if !cfg!(windows) {
        return Check::new(name, CheckStatus::Skipped, "WSL is a Windows feature.");
    }
    let list = csm_engine::wsl_probe::distros();
    if list.is_empty() {
        return Check::new(name, CheckStatus::Skipped, "No WSL distribution found; WSL sessions are unavailable (optional).")
            .fix("Optional: run `wsl --install` in an administrator terminal to use WSL sessions.");
    }
    let names: Vec<&str> = list.iter().map(|d| d.name.as_str()).collect();
    Check::new(name, CheckStatus::Ok, format!("{} distribution(s): {}", list.len(), names.join(", ")))
}

fn check_clients(host: &Host) -> Vec<Check> {
    let command = crate::plan::default_command().unwrap_or_else(|_| vec!["csm".to_owned(), "mcp".to_owned()]);
    clients(host)
        .iter()
        .filter(|c| c.detected())
        .map(|c| {
            let name = format!("client:{}", c.id);
            let a = inspect_client(c, &command);
            match a.kind {
                ActionKind::Unchanged => Check::new(name, CheckStatus::Ok, format!("{} is connected ({}).", c.display, c.config.display())),
                ActionKind::Blocked => Check::new(name, CheckStatus::Warn, format!("{}: {}", c.display, a.reason.unwrap_or_default()))
                    .fix(format!("Fix the file, then run: csm setup --client {}", c.id)),
                ActionKind::Update if points_at_old_folder(c, a.before.as_deref(), &command) => Check::new(
                    name,
                    CheckStatus::Warn,
                    format!("{} points {SERVER_NAME} at csm in the old install folder (from before the move to umage.ai).", c.display),
                )
                .fix(format!("Run: csm setup --client {}  (CodeShellManager also repairs this when it starts)", c.id)),
                ActionKind::Update => Check::new(name, CheckStatus::Warn, format!("{} points {SERVER_NAME} at a different csm.", c.display))
                    .fix(format!("Run: csm setup --client {}", c.id)),
                ActionKind::Create | ActionKind::Add => {
                    Check::new(name, CheckStatus::Warn, format!("{} is installed but not connected to CodeShellManager.", c.display))
                        .fix(format!("Run: csm setup --client {}  (add --dry-run to see the change first)", c.id))
                }
            }
        })
        .collect()
}

/// Start an MCP server over stdio, send `initialize`, `initialized` and `tools/list`, close stdin
/// and check the answers.
pub fn mcp_self_test(program: &str, args: &[String], timeout: Duration) -> Check {
    let name = "mcp:self-test";
    let how = "Run `csm mcp --in-process` in a terminal to see the error.";
    let requests = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "csm-doctor", "version": env!("CARGO_PKG_VERSION")}}}),
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    ];
    let mut input = String::new();
    for r in &requests {
        input.push_str(&r.to_string());
        input.push('\n');
    }
    let mut cmd = Command::new(program);
    cmd.args(args);
    proc::no_window(&mut cmd);
    let mut child = match cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() {
        Ok(c) => c,
        Err(e) => return Check::new(name, CheckStatus::Fail, format!("could not start {program}: {e}")).fix(how),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(input.as_bytes());
    }
    let Some(mut stdout) = child.stdout.take() else {
        let _ = child.kill();
        return Check::new(name, CheckStatus::Fail, "no output pipe").fix(how);
    };
    let (tx, rx) = mpsc::channel();
    let _ = std::thread::Builder::new().name("csm-doctor-read".into()).spawn(move || {
        let mut text = String::new();
        let mut bytes = Vec::new();
        let _ = stdout.by_ref().take(32 * 1024 * 1024).read_to_end(&mut bytes);
        text.push_str(&String::from_utf8_lossy(&bytes));
        let _ = tx.send(text);
    });
    let deadline = Instant::now() + timeout;
    let text = loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok(t) => break Some(t),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() < deadline => {}
            _ => break None,
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    let Some(text) = text else {
        return Check::new(name, CheckStatus::Fail, "The MCP server did not finish within the time limit.").fix(how);
    };
    let (mut init, mut list) = (None, None);
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        match v.get("id").and_then(Value::as_i64) {
            Some(1) => init = Some(v),
            Some(2) => list = Some(v),
            _ => {}
        }
    }
    let Some(init) = init else { return Check::new(name, CheckStatus::Fail, "The MCP server sent no reply to initialize.").fix(how) };
    let server = init["result"]["serverInfo"]["name"].as_str().unwrap_or_default();
    let protocol = init["result"]["protocolVersion"].as_str().unwrap_or_default();
    if server != SERVER_NAME || protocol.is_empty() {
        return Check::new(name, CheckStatus::Fail, format!("Unexpected initialize reply (server {server:?}, protocol {protocol:?}).")).fix(how);
    }
    let tools: Vec<&str> = list
        .as_ref()
        .and_then(|l| l["result"]["tools"].as_array())
        .map(|a| a.iter().filter_map(|t| t["name"].as_str()).collect())
        .unwrap_or_default();
    if tools.is_empty() {
        return Check::new(name, CheckStatus::Fail, "The MCP server listed no tools.").fix(how);
    }
    if !tools.contains(&"session_list") {
        return Check::new(name, CheckStatus::Fail, "The MCP server's tool list is missing session_list.").fix("Update csm and the app together.");
    }
    Check::new(name, CheckStatus::Ok, format!("MCP server answered (protocol {protocol}, {} tools listed).", tools.len()))
}

/// Whether the entry in `before` is one setup wrote that names the pre-2.0 install folder.
fn points_at_old_folder(c: &Client, before: Option<&str>, command: &[String]) -> bool {
    let Some(program) = command.first() else { return false };
    before
        .and_then(|t| crate::config::read_entry(c, t).ok().flatten())
        .is_some_and(|e| crate::clients::is_moved_entry(&e, Path::new(program)))
}

#[cfg(test)]
mod channel_note_tests {
    use super::*;

    #[test]
    fn the_note_names_the_channel_and_the_app_id_it_looks_for() {
        let dir = std::env::temp_dir().join(format!("csm-doctor-note-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let remote = RemoteOptions { runtime: Some(csm_mcp::RuntimeDir::at(dir.join("rt")).unwrap()), ..Default::default() };
        let note = channel_note(&remote);
        let ch = csm_engine::channel::CURRENT;
        assert!(note.contains(ch.id()) && note.contains(ch.ipc_app_id()), "{note}");
        assert!(note.contains("no CodeShellManager is published"), "{note}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
