//! Run commands (`RunInstance` / `SessionRunner` in the WPF app): a session's labelled command
//! lines, each started in its **own headless PTY** in the session's folder (an SSH parent gets a
//! fresh `ssh` connection, a WSL parent a `wsl.exe` in its distro). A run never types into the
//! session's own terminal, so a Claude session is untouched.
//!
//! - Runs stay on the **inbox** ConPTY (`SpawnOptions::allow_bundled = false`): a headless PTY
//!   has no terminal to answer OpenConsole's opening DA1, and the inbox conhost answers device
//!   queries itself, which is right for a headless PTY (WPF CLAUDE.md "Bundled ConPTY").
//! - Every PTY is in a Job Object (Windows) / its own process group (Unix), so stopping a run
//!   kills its whole tree. Runs die with their session (close, sleep, restart) and the app.
//! - `Process` mode is `cmd.exe /c "<command>"` handed to CreateProcess verbatim (cmd does not
//!   parse MSVCRT quoting); `PowerShell` mode is `-EncodedCommand` (no quoting at all). The
//!   command line is the user's own: a shell is the point, and no app value is put in it.
//! - Output is ANSI-stripped and capped at 1 MB ([`output`]). Not persisted.
//! - `PostRunUrl` opens in the browser only after exit code 0, and only an absolute `http(s)` URL
//!   ([`launchable_url`]): a whole state file can be imported, and ShellExecute would happily run
//!   a program, a script or any protocol handler.
//!
//! Spawning happens on a worker; the engine thread only records state ([`start`], [`stop`]).

pub mod output;
pub mod templates;

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use csm_core::models::{RunCommandItem, RunMode, SessionKind, ShellSession};
use csm_pty::{Pty, PtySize, SpawnOptions};
use csm_services::argv::split_command_line;
use csm_services::launch as sl;
use serde::Serialize;

use crate::engine::Ctx;
use crate::session::now_ms;
use crate::state::State;
pub use output::{RunOutput, RunOutputHandle};

/// The size of a run's headless PTY (the WPF app's 200x50).
const RUN_SIZE: PtySize = PtySize { cols: 200, rows: 50 };

/// Longest command line a run command may have.
pub const MAX_COMMAND_LINE: usize = 8192;
/// Most run commands per session.
pub const MAX_RUN_COMMANDS: usize = 50;

/// Where a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunPhase {
    /// Never started in this session's life (or its output was dismissed).
    Idle,
    /// The worker is starting the process.
    Starting,
    Running,
    /// Exit code 0.
    Ok,
    /// Non-zero exit, or it could not start.
    Failed,
    /// Stopped by the person (or its session closed).
    Stopped,
}

impl RunPhase {
    pub fn is_running(self) -> bool {
        matches!(self, RunPhase::Starting | RunPhase::Running)
    }
}

/// One invocation of a run command.
pub(crate) struct RunInstance {
    pub item_id: String,
    pub label: String,
    pub post_run_url: Option<String>,
    /// Distinguishes this invocation from a later restart of the same item.
    pub seq: u64,
    pub phase: RunPhase,
    pub exit_code: Option<i32>,
    pub started_ms: i64,
    pub ended_ms: Option<i64>,
    pub output: Arc<RunOutput>,
    pub pty: Option<Arc<dyn Pty>>,
    /// Set when stopped: a spawn still in flight kills what it started.
    pub closed: Arc<AtomicBool>,
}

impl RunInstance {
    fn kill(&mut self) {
        self.closed.store(true, Ordering::Release);
        if let Some(p) = &self.pty {
            p.kill();
        }
    }
}

/// Runs by session id (runtime only).
pub(crate) type Runs = HashMap<String, Vec<RunInstance>>;

/// One run command as every surface shows it: the configured item plus its last invocation.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunView {
    pub id: String,
    pub label: String,
    pub command_line: String,
    pub is_default: bool,
    /// `process` or `powershell`.
    pub mode: &'static str,
    pub post_run_url: Option<String>,
    pub phase: RunPhase,
    pub exit_code: Option<i32>,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
}

pub fn mode_id(m: RunMode) -> &'static str {
    match m {
        RunMode::Process => "process",
        RunMode::PowerShell => "powershell",
    }
}

/// The views of a session's run commands, in list order.
pub(crate) fn views(state: &State, s: &ShellSession) -> Vec<RunView> {
    let inst = state.rt.runs.get(&s.id);
    s.run_commands
        .iter()
        .map(|item| {
            let i = inst.and_then(|v| v.iter().find(|r| r.item_id == item.id));
            RunView {
                id: item.id.clone(),
                label: item.label.clone(),
                command_line: item.command_line.clone(),
                is_default: item.is_default,
                mode: mode_id(item.mode),
                post_run_url: item.post_run_url.clone(),
                phase: i.map_or(RunPhase::Idle, |r| r.phase),
                exit_code: i.and_then(|r| r.exit_code),
                started_ms: i.map(|r| r.started_ms),
                ended_ms: i.and_then(|r| r.ended_ms),
            }
        })
        .collect()
}

/// Output handles of every invocation, by `(session id, run id)` (the snapshot carries them so
/// the UI can read output once a frame without a tool call).
pub(crate) fn handles(state: &State) -> HashMap<(String, String), RunOutputHandle> {
    state.rt.runs.iter().flat_map(|(sid, v)| v.iter().map(move |r| ((sid.clone(), r.item_id.clone()), RunOutputHandle(r.output.clone())))).collect()
}

// ── Planning (pure) ──────────────────────────────────────────────────────────────────────

/// What to start for a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunPlan {
    pub argv: Vec<String>,
    /// Windows `cmd.exe /c "<command>"`: the command line after the program, verbatim.
    pub verbatim: Option<String>,
    /// `None`: the user's home folder.
    pub cwd: Option<PathBuf>,
}

/// Why a command line can't be run (or saved).
pub fn command_line_error(cmd: &str) -> Option<&'static str> {
    if cmd.trim().is_empty() {
        return Some("The command line is empty.");
    }
    if cmd.len() > MAX_COMMAND_LINE {
        return Some("The command line is too long.");
    }
    // A newline would end cmd.exe's /c line early (and submit a second command in a shell).
    if cmd.chars().any(|c| c.is_control() && c != '\t') {
        return Some("The command line contains a line break or a control character; use && to chain commands.");
    }
    None
}

fn spec_of(s: &ShellSession) -> sl::LaunchSpec {
    sl::LaunchSpec {
        kind: match s.kind {
            SessionKind::Local => sl::SessionKind::Local,
            SessionKind::Ssh => sl::SessionKind::Ssh,
            SessionKind::Wsl => sl::SessionKind::Wsl,
        },
        command: s.command.clone(),
        args: s.args.clone(),
        working_folder: s.working_folder.clone(),
        ssh_user: s.ssh_user.clone(),
        ssh_host: s.ssh_host.clone(),
        ssh_port: s.ssh_port,
        ssh_remote_folder: s.ssh_remote_folder.clone(),
        wsl_distro: s.wsl_distro.clone(),
        wsl_user: s.wsl_user.clone(),
        wsl_working_folder: s.wsl_working_folder.clone(),
        resolved_wsl_shell: None,
    }
}

/// The argv for running `item` for session `s`. `windows` picks the local shape (tests pass it;
/// the engine passes `cfg!(windows)`); `pwsh` is the PowerShell to use; `unix_shell` the login
/// shell elsewhere. SSH and WSL parents ignore `mode`: those runs always go through a POSIX shell.
pub fn plan(s: &ShellSession, item: &RunCommandItem, windows: bool, pwsh: &str, unix_shell: &str) -> Result<RunPlan, String> {
    if let Some(e) = command_line_error(&item.command_line) {
        return Err(e.to_owned());
    }
    if let Some(e) = s.launch_validation_error() {
        return Err(e.to_owned());
    }
    let cmd = item.command_line.trim();
    let plan = match s.kind {
        SessionKind::Ssh => {
            let args = sl::build_run_ssh_args(&spec_of(s), cmd).map_err(|e| e.to_string())?;
            RunPlan { argv: std::iter::once("ssh".to_owned()).chain(split_command_line(&args)).collect(), verbatim: None, cwd: None }
        }
        SessionKind::Wsl => {
            if !windows {
                return Err("WSL runs work on Windows only.".into());
            }
            let args = sl::build_run_wsl_args(&spec_of(s), cmd).map_err(|e| e.to_string())?;
            RunPlan { argv: std::iter::once("wsl.exe".to_owned()).chain(split_command_line(&args)).collect(), verbatim: None, cwd: None }
        }
        SessionKind::Local => {
            let cwd = Some(s.working_folder.trim()).filter(|f| !f.is_empty()).map(PathBuf::from);
            match (item.mode, windows) {
                (RunMode::PowerShell, _) => RunPlan {
                    argv: std::iter::once(pwsh.to_owned()).chain(split_command_line(&sl::build_pwsh_args(cmd))).collect(),
                    verbatim: None,
                    cwd,
                },
                (RunMode::Process, true) => RunPlan { argv: vec!["cmd.exe".to_owned()], verbatim: Some(sl::build_local_cmd(cmd)), cwd },
                (RunMode::Process, false) => RunPlan { argv: vec![unix_shell.to_owned(), "-lc".to_owned(), cmd.to_owned()], verbatim: None, cwd },
            }
        }
    };
    Ok(plan)
}

/// The exact string to open after a successful run, or `None`: an absolute `http`/`https` URL
/// and nothing else (no program, script, UNC path or other protocol handler). Scheme-less input
/// (`localhost:5173`) is refused rather than guessed at. The string returned is the one checked.
pub fn launchable_url(url: &str) -> Option<String> {
    let u = url.trim_matches(' ');
    if u.is_empty() || u.len() > 2048 {
        return None;
    }
    let lower = u.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"))?;
    if u.chars().any(|c| c.is_control() || c.is_whitespace() || matches!(c, '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '}' | '|')) {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or("");
    let name = host.rsplit_once(':').map_or(host, |(h, _)| h);
    if name.is_empty() || name.starts_with('-') || name.starts_with('.') {
        return None;
    }
    Some(u.to_owned())
}

// ── Engine-thread operations ─────────────────────────────────────────────────────────────

/// The item for `run_id`, or the session's default (else the first).
pub(crate) fn pick<'a>(s: &'a ShellSession, run_id: Option<&str>) -> Option<&'a RunCommandItem> {
    match run_id.filter(|r| !r.is_empty()) {
        Some(r) => s.run_commands.iter().find(|i| i.id == r || i.label.eq_ignore_ascii_case(r)),
        None => s.run_commands.iter().find(|i| i.is_default).or_else(|| s.run_commands.first()),
    }
}

/// Starts `item` for session `s` (stopping its previous invocation first). Records the run as
/// starting and hands the spawn to a worker.
pub(crate) fn start(ctx: &mut Ctx<'_>, s: &ShellSession, item: &RunCommandItem) {
    ctx.state.rt.instance_seq = ctx.state.rt.instance_seq.wrapping_add(1);
    let seq = ctx.state.rt.instance_seq;
    let list = ctx.state.rt.runs.entry(s.id.clone()).or_default();
    if let Some(i) = list.iter().position(|r| r.item_id == item.id) {
        let mut old = list.remove(i);
        old.kill();
    }
    let output = RunOutput::new();
    let closed = Arc::new(AtomicBool::new(false));
    list.push(RunInstance {
        item_id: item.id.clone(),
        label: item.label.clone(),
        post_run_url: item.post_run_url.clone(),
        seq,
        phase: RunPhase::Starting,
        exit_code: None,
        started_ms: now_ms(),
        ended_ms: None,
        output: output.clone(),
        pty: None,
        closed: closed.clone(),
    });
    ctx.state.touch();

    let factory = ctx.state.options.pty.clone();
    let engine = ctx.engine.clone();
    let (session, item, sid, iid) = (s.clone(), item.clone(), s.id.clone(), item.id.clone());
    let worker = move || {
        // The exit callback is registered by `on_spawned`, on the engine thread, so the exit is
        // always handled after the start.
        let result = spawn_run(&session, &item, &factory, &output, &closed);
        engine.post(Box::new(move |ctx| on_spawned(ctx, &sid, &iid, seq, result)));
    };
    let spawned = std::thread::Builder::new().name("csm-run".into()).spawn(worker);
    if let Err(e) = spawned {
        on_spawned(ctx, &s.id, "", seq, Err(format!("could not start a worker thread: {e}")));
    }
}

/// On a worker: resolve, spawn on the inbox ConPTY, wire output. Blocking.
fn spawn_run(
    s: &ShellSession,
    item: &RunCommandItem,
    factory: &crate::state::PtyFactory,
    output: &Arc<RunOutput>,
    closed: &AtomicBool,
) -> Result<Arc<dyn Pty>, String> {
    let pwsh = if cfg!(windows) { crate::launch::pwsh() } else { "pwsh" };
    let unix_shell = std::env::var("SHELL").ok().filter(|v| v.starts_with('/')).unwrap_or_else(|| "/bin/sh".into());
    let mut p = plan(s, item, cfg!(windows), pwsh, &unix_shell)?;
    if p.cwd.as_deref().is_some_and(|c| !c.is_dir()) {
        p.cwd = None;
    }
    if p.cwd.is_none() {
        p.cwd = dirs::home_dir();
    }
    if closed.load(Ordering::Acquire) {
        return Err("stopped before it started".into());
    }
    let program = p.argv.first().cloned().unwrap_or_default();
    let mut opts = SpawnOptions::new(p.argv.into_iter().map(OsString::from).collect(), RUN_SIZE);
    opts.cwd = p.cwd;
    opts.allow_bundled = false;
    opts.verbatim_args = p.verbatim.map(OsString::from);
    opts.env.push(("CSM_SESSION_ID".into(), Some(s.id.clone().into())));
    log::info!("run '{}' for session {} ({program})", item.label, s.id);
    let pty = factory(opts).map_err(|e| format!("could not start `{program}`: {e}"))?;
    if closed.load(Ordering::Acquire) {
        pty.kill();
        return Err("stopped before it started".into());
    }
    let out = output.clone();
    pty.subscribe(Box::new(move |bytes: &[u8]| out.append_bytes(bytes)));
    Ok(pty)
}

fn find<'a>(state: &'a mut State, sid: &str, seq: u64) -> Option<&'a mut RunInstance> {
    state.rt.runs.get_mut(sid)?.iter_mut().find(|r| r.seq == seq)
}

pub(crate) fn on_spawned(ctx: &mut Ctx<'_>, sid: &str, _iid: &str, seq: u64, result: Result<Arc<dyn Pty>, String>) {
    let engine = ctx.engine.clone();
    let Some(r) = find(ctx.state, sid, seq) else {
        // Stopped, restarted or its session closed while starting.
        if let Ok(p) = result {
            p.kill();
        }
        return;
    };
    match result {
        Ok(pty) => {
            r.phase = RunPhase::Running;
            r.pty = Some(pty.clone());
            if r.closed.load(Ordering::Acquire) {
                pty.kill();
            }
            let (sid, engine2) = (sid.to_owned(), engine.clone());
            pty.on_exit(Box::new(move |code| {
                engine2.post(Box::new(move |ctx| on_exited(ctx, &sid, seq, code)));
            }));
        }
        Err(msg) => {
            log::warn!("run '{}' could not start: {msg}", r.label);
            r.output.append_text(&format!("Cannot start: {msg}\n"));
            r.phase = RunPhase::Failed;
            r.exit_code = Some(-1);
            r.ended_ms = Some(now_ms());
        }
    }
    ctx.state.touch();
}

pub(crate) fn on_exited(ctx: &mut Ctx<'_>, sid: &str, seq: u64, code: Option<i32>) {
    let Some(r) = find(ctx.state, sid, seq) else { return };
    if r.phase != RunPhase::Stopped {
        r.phase = if code == Some(0) { RunPhase::Ok } else { RunPhase::Failed };
        r.exit_code = code;
    }
    r.ended_ms.get_or_insert_with(now_ms);
    let url = (r.phase == RunPhase::Ok).then(|| r.post_run_url.clone()).flatten().filter(|u| !u.trim().is_empty());
    let label = r.label.clone();
    ctx.state.touch();
    if let Some(u) = url {
        match launchable_url(&u) {
            Some(safe) => crate::tools::open::open_url_detached(safe),
            None => log::warn!("run '{label}': PostRunUrl '{u}' rejected — only http and https URLs are opened"),
        }
    }
}

/// Stops one run (kills its tree). True when it was running.
pub(crate) fn stop(state: &mut State, sid: &str, item_id: &str) -> bool {
    let Some(r) = state.rt.runs.get_mut(sid).and_then(|v| v.iter_mut().find(|r| r.item_id == item_id)) else { return false };
    let was = r.phase.is_running();
    if was {
        r.phase = RunPhase::Stopped;
        r.ended_ms = Some(now_ms());
    }
    r.kill();
    state.touch();
    was
}

/// Kills every run of a session and forgets them (close, sleep, restart).
pub(crate) fn stop_all(state: &mut State, sid: &str) {
    if let Some(list) = state.rt.runs.remove(sid) {
        for mut r in list {
            r.kill();
        }
        state.touch();
    }
}

/// Kills every run (the app is closing).
pub(crate) fn kill_everything(state: &mut State) {
    let ids: Vec<String> = state.rt.runs.keys().cloned().collect();
    for id in ids {
        stop_all(state, &id);
    }
}

/// Seeds a new local (or WSL) session's run commands from its folder's project type, on a
/// worker; applied only if the list is still empty when the scan finishes.
pub(crate) fn seed_async(ctx: &mut Ctx<'_>, sid: &str) {
    let Some(s) = ctx.state.app.sessions.iter().find(|s| s.id == sid) else { return };
    if !s.run_commands.is_empty() || s.kind == SessionKind::Ssh || s.working_folder.trim().is_empty() {
        return;
    }
    let (folder, engine, sid) = (s.working_folder.clone(), ctx.engine.clone(), sid.to_owned());
    let _ = std::thread::Builder::new().name("csm-run-seed".into()).spawn(move || {
        if let Some((kind, items)) = templates::seed_for_folder(&folder) {
            engine.post(Box::new(move |ctx| {
                if let Some(s) = ctx.state.app.sessions.iter_mut().find(|s| s.id == sid)
                    && s.run_commands.is_empty()
                {
                    log::info!("run commands: seeded {kind} template for {sid}");
                    s.run_commands = items;
                    ctx.state.persist();
                }
            }));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(cmd: &str, mode: RunMode) -> RunCommandItem {
        RunCommandItem { id: "r".into(), label: "Run".into(), command_line: cmd.into(), is_default: true, mode, post_run_url: None }
    }

    fn local() -> ShellSession {
        ShellSession { id: "s".into(), command: "claude".into(), working_folder: r"C:\src\a b".into(), ..Default::default() }
    }

    #[test]
    fn process_mode_on_windows_is_cmd_with_a_verbatim_tail() {
        let p = plan(&local(), &item(r#"dotnet run --project "a b" && echo %PATH%"#, RunMode::Process), true, "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(p.argv, ["cmd.exe"]);
        assert_eq!(p.verbatim.as_deref(), Some(r#"/c "dotnet run --project "a b" && echo %PATH%""#));
        assert_eq!(p.cwd, Some(PathBuf::from(r"C:\src\a b")));
    }

    #[test]
    fn powershell_mode_is_encoded_so_nothing_needs_quoting() {
        let p = plan(&local(), &item("Get-ChildItem | Select -First 1; \"x\"", RunMode::PowerShell), true, "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(&p.argv[..6], ["pwsh.exe", "-NonInteractive", "-NoLogo", "-ExecutionPolicy", "Bypass", "-EncodedCommand"]);
        assert_eq!(p.argv.len(), 7);
        assert!(p.verbatim.is_none());
        let unix = plan(&local(), &item("ls -la", RunMode::Process), false, "pwsh", "/bin/zsh").unwrap();
        assert_eq!(unix.argv, ["/bin/zsh", "-lc", "ls -la"]);
    }

    #[test]
    fn ssh_and_wsl_parents_ignore_mode_and_keep_values_single() {
        let ssh = ShellSession {
            id: "s".into(),
            kind: SessionKind::Ssh,
            ssh_host: "box".into(),
            ssh_user: "al".into(),
            ssh_remote_folder: "/srv/a\" -oProxyCommand=calc".into(),
            ..Default::default()
        };
        let p = plan(&ssh, &item("make test", RunMode::PowerShell), true, "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(p.argv.first().map(String::as_str), Some("ssh"));
        assert!(!p.argv.iter().any(|a| a.starts_with("-oProxyCommand")), "{:?}", p.argv);
        assert!(p.argv.last().is_some_and(|a| a.contains("bash -c 'make test'")), "{:?}", p.argv);
        let wsl = ShellSession {
            id: "s".into(),
            kind: SessionKind::Wsl,
            wsl_distro: "Ubuntu".into(),
            wsl_working_folder: "/home/a".into(),
            ..Default::default()
        };
        let p = plan(&wsl, &item("echo $HOME", RunMode::Process), true, "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(&p.argv[..3], ["wsl.exe", "-d", "Ubuntu"]);
        assert!(p.argv.windows(2).any(|w| w[0] == "-e"), "-e, never --: {:?}", p.argv);
        assert_eq!(p.argv.last().map(String::as_str), Some("echo $HOME"));
        assert!(plan(&wsl, &item("x", RunMode::Process), false, "pwsh", "/bin/sh").is_err());
    }

    #[test]
    fn bad_command_lines_are_refused() {
        for bad in ["", "   ", "echo a\necho b", "echo a\r", "x\0y"] {
            assert!(plan(&local(), &item(bad, RunMode::Process), true, "pwsh.exe", "/bin/sh").is_err(), "{bad:?}");
        }
        assert!(command_line_error("npm run dev\t--port 3000").is_none());
        let no_host = ShellSession { kind: SessionKind::Ssh, ..Default::default() };
        assert!(plan(&no_host, &item("x", RunMode::Process), true, "pwsh.exe", "/bin/sh").is_err());
    }

    #[test]
    fn only_absolute_http_urls_are_launchable() {
        for ok in ["http://localhost:5173", "https://example.com/a?b=1&c=2#x", "HTTPS://Example.com", "http://user@host:8080/"] {
            assert_eq!(launchable_url(ok).as_deref(), Some(ok), "{ok}");
        }
        for bad in [
            "",
            "localhost:5173",
            "file:///C:/Windows/System32/calc.exe",
            r"C:\Windows\System32\calc.exe",
            r"\\server\share\x.exe",
            "javascript:alert(1)",
            "ms-settings:",
            "http://",
            "http:// x",
            "https://-oProxy",
            "http://a\"b",
            "http://a b",
            "http://x\n",
            "vscode://x",
        ] {
            assert_eq!(launchable_url(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn pick_finds_by_id_label_or_default() {
        let mut s = local();
        s.run_commands = vec![
            RunCommandItem { id: "a".into(), label: "Build".into(), command_line: "b".into(), is_default: false, ..Default::default() },
            RunCommandItem { id: "b".into(), label: "Test".into(), command_line: "t".into(), is_default: true, ..Default::default() },
        ];
        assert_eq!(pick(&s, None).map(|i| i.id.as_str()), Some("b"));
        assert_eq!(pick(&s, Some("a")).map(|i| i.id.as_str()), Some("a"));
        assert_eq!(pick(&s, Some("build")).map(|i| i.id.as_str()), Some("a"));
        assert!(pick(&s, Some("nope")).is_none());
    }
}
