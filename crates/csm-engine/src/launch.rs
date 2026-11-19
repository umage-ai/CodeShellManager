//! Turning a session into argv. Never a command line built by interpolation: every value goes
//! through the `csm-services` builders (MSVCRT quoting, the WSL `-e` / SSH single-argument
//! rules) and is split back into argv elements, which `csm-pty` quotes again for CreateProcess.
//!
//! The only shell payload is the user's own command (`pwsh -NoExit -Command <command> <args>`
//! on Windows, `$SHELL -l -i -c` elsewhere), exactly as the WPF app runs it, so profile functions
//! (`claudelocal`) resolve as bare commands. Anything the app adds to that payload (a
//! `--resume <id>`) is validated to a strict alphabet first.
//!
//! [`plan`] is pure. [`resolve`] does the I/O (Claude resume lookup, pwsh location, folder
//! check) and must run on a worker thread.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use csm_core::models::{SessionKind, ShellSession};
use csm_services::argv::split_command_line;
use csm_services::launch::{self as sl, LaunchSpec};

/// `ClaudeSessionService.IsClaudeCommand`.
pub fn is_claude_command(command: &str) -> bool {
    let c = command.trim();
    c.eq_ignore_ascii_case("claude") || c.get(..7).is_some_and(|p| p.eq_ignore_ascii_case("claude "))
}

/// What to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub argv: Vec<String>,
    /// `None`: the user's home folder.
    pub cwd: Option<PathBuf>,
    pub is_claude: bool,
}

fn spec_of(s: &ShellSession) -> LaunchSpec {
    LaunchSpec {
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

/// A Claude conversation id we are willing to put on a command line: what Claude names its
/// `.jsonl` files (a UUID), nothing a shell could read as syntax.
pub fn is_safe_resume_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

const PASSTHROUGH_SHELLS: [&str; 10] = ["cmd", "powershell", "pwsh", "wsl", "bash", "zsh", "sh", "ssh", "nu", "fish"];

fn is_passthrough_shell(command: &str) -> bool {
    let leaf = csm_core::models::get_file_name(command.trim());
    let stem = leaf.rsplit_once('.').map_or(leaf, |(s, _)| s).to_ascii_lowercase();
    PASSTHROUGH_SHELLS.contains(&stem.as_str())
}

/// The argv for a session. `resume_id` is appended as `--resume <id>` (validated);
/// `wrapper` is the PowerShell that hosts non-shell commands on Windows; `unix_shell` is the
/// login shell elsewhere.
pub fn plan(s: &ShellSession, resume_id: Option<&str>, wrapper: &str, unix_shell: &str) -> Result<Plan, String> {
    let is_claude = is_claude_command(&s.command);
    if let Some(msg) = s.launch_validation_error() {
        return Err(msg.to_owned());
    }
    let argv = match s.kind {
        SessionKind::Local => {
            let command = s.command.trim();
            if command.is_empty() {
                return Err("This session has no command. Edit the session and set one.".into());
            }
            let mut args = s.args.trim().to_owned();
            if let Some(id) = resume_id.filter(|id| is_safe_resume_id(id)) {
                args = if args.is_empty() { format!("--resume {id}") } else { format!("--resume {id} {args}") };
            }
            let full = if args.is_empty() { command.to_owned() } else { format!("{command} {args}") };
            if cfg!(windows) {
                split_command_line(&sl::build_cmd_line(command, &full, wrapper))
            } else if is_passthrough_shell(command) {
                split_command_line(&full)
            } else {
                vec![unix_shell.to_owned(), "-l".into(), "-i".into(), "-c".into(), full]
            }
        }
        SessionKind::Ssh => {
            let args = sl::build_ssh_args(&spec_of(s)).map_err(|e| e.to_string())?;
            std::iter::once("ssh".to_owned()).chain(split_command_line(&args)).collect()
        }
        SessionKind::Wsl => {
            if !cfg!(windows) {
                return Err("WSL sessions run on Windows only.".into());
            }
            let args = sl::build_wsl_args(&spec_of(s), None).map_err(|e| e.to_string())?;
            std::iter::once("wsl.exe".to_owned()).chain(split_command_line(&args)).collect()
        }
    };
    if argv.first().is_none_or(|a| a.trim().is_empty()) {
        return Err("Nothing to run: the command is empty.".into());
    }
    let cwd = match s.kind {
        SessionKind::Local if !s.working_folder.trim().is_empty() => Some(PathBuf::from(s.working_folder.trim())),
        _ => None,
    };
    Ok(Plan { argv, cwd, is_claude })
}

/// [`plan`] plus its I/O: the Claude resume lookup (only when `resume` is set), the PowerShell
/// to wrap with, and a folder that no longer exists falling back to home (the WPF rule).
/// Blocking; worker threads only.
pub fn resolve(s: &ShellSession, resume: bool) -> Result<Plan, String> {
    let resume_id = if resume
        && s.kind == SessionKind::Local
        && is_claude_command(&s.command)
        && !s.args.contains("--resume")
        && !s.args.contains("--continue")
    {
        let id = last_claude_session_id(s.working_folder.trim(), &claude_home());
        match &id {
            Some(id) => log::info!("auto-resume: --resume {id} for claude in '{}'", s.working_folder),
            None => log::info!("auto-resume: no prior conversation in '{}', starting fresh", s.working_folder),
        }
        id
    } else {
        None
    };
    let wrapper = if cfg!(windows) { pwsh() } else { "pwsh.exe" };
    let unix_shell = std::env::var("SHELL").ok().filter(|v| v.starts_with('/')).unwrap_or_else(|| "/bin/sh".into());
    let mut p = plan(s, resume_id.as_deref(), wrapper, &unix_shell)?;
    if p.cwd.as_deref().is_some_and(|c| !c.is_dir()) {
        log::warn!("session folder '{}' does not exist; starting in home", s.working_folder);
        p.cwd = None;
    }
    if p.cwd.is_none() {
        p.cwd = dirs::home_dir();
    }
    Ok(p)
}

/// `CLAUDE_CONFIG_DIR`, else `~/.claude` (a stale `~/.claude` must not win over the env var).
fn claude_home() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        Some(d) => PathBuf::from(d),
        None => dirs::home_dir().unwrap_or_default().join(".claude"),
    }
}

/// The newest conversation for `folder` under `<claude home>/projects/<folder as dashes>/`.
pub fn last_claude_session_id(folder: &str, claude_home: &Path) -> Option<String> {
    if folder.is_empty() {
        return None;
    }
    let dir_name: String = folder.chars().map(|c| if matches!(c, ':' | '\\' | '/') { '-' } else { c }).collect();
    let dir = claude_home.join("projects").join(dir_name);
    let entries = std::fs::read_dir(dir).ok()?;
    entries
        .filter_map(Result::ok)
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            let stem = e.path().file_stem()?.to_str()?.to_owned();
            is_safe_resume_id(&stem).then_some((modified, stem))
        })
        .max_by_key(|(m, _)| *m)
        .map(|(_, s)| s)
}

/// `pwsh.exe` when it is on PATH and actually runs, else `powershell.exe` (`PwshLocator`).
/// Resolved once per run.
fn pwsh() -> &'static str {
    static PWSH: OnceLock<&'static str> = OnceLock::new();
    PWSH.get_or_init(|| {
        let where_exe = std::env::var_os("SystemRoot")
            .map(|r| PathBuf::from(r).join("System32").join("where.exe"))
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\where.exe"));
        let out = run_with_timeout(std::process::Command::new(where_exe).arg("pwsh.exe"), Duration::from_secs(3));
        let choice = sl::choose_pwsh(out.as_deref(), &FsProbe);
        log::info!("session wrapper shell: {choice}");
        choice
    })
}

struct FsProbe;

impl sl::RunnableProbe for FsProbe {
    fn metadata(&self, path: &str) -> Option<sl::FileMeta> {
        let m = std::fs::symlink_metadata(path).ok()?;
        #[cfg(windows)]
        let reparse = {
            use std::os::windows::fs::MetadataExt;
            m.file_attributes() & 0x400 != 0
        };
        #[cfg(not(windows))]
        let reparse = m.file_type().is_symlink();
        Some(sl::FileMeta { len: m.len(), is_reparse_point: reparse })
    }

    fn can_execute(&self, path: &str) -> bool {
        let mut c = std::process::Command::new(path);
        c.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "exit 0"]);
        run_with_timeout(&mut c, Duration::from_secs(5)).is_some()
    }
}

/// Runs a probe process (no shell, no stdin) and returns its stdout when it exits 0 in time.
fn run_with_timeout(cmd: &mut std::process::Command, timeout: Duration) -> Option<String> {
    use std::io::Read;
    use std::process::Stdio;
    // A windowed app must not flash a console for a probe (CREATE_NO_WINDOW). ConPTY sessions
    // are the exception: they have a pseudo console.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut out = String::new();
                if let Some(mut so) = child.stdout.take() {
                    let _ = so.read_to_string(&mut out);
                }
                return status.success().then_some(out);
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(command: &str, args: &str) -> ShellSession {
        ShellSession { id: "s".into(), command: command.into(), args: args.into(), working_folder: r"C:\src\a b".into(), ..Default::default() }
    }

    #[test]
    fn claude_detection() {
        assert!(is_claude_command("claude"));
        assert!(is_claude_command("Claude --x"));
        assert!(!is_claude_command("claudex"));
        assert!(!is_claude_command("pwsh"));
    }

    #[test]
    #[cfg(windows)]
    fn local_commands_are_wrapped_and_shells_pass_through() {
        let p = plan(&local("claude", "--model \"a b\""), Some("0b1c-22"), "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(p.argv, ["pwsh.exe", "-NoExit", "-Command", "claude", "--resume", "0b1c-22", "--model", "a b"]);
        assert!(p.is_claude);
        assert_eq!(p.cwd.as_deref(), Some(Path::new(r"C:\src\a b")));
        let p = plan(&local("cmd", "/c echo hi"), None, "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(p.argv, ["cmd", "/c", "echo", "hi"]);
    }

    #[test]
    fn a_hostile_resume_id_is_never_added() {
        let p = plan(&local("claude", ""), Some("x; calc"), "pwsh.exe", "/bin/sh").unwrap();
        assert!(!p.argv.iter().any(|a| a.contains("calc")), "{:?}", p.argv);
    }

    #[test]
    fn ssh_values_stay_single_arguments() {
        let s = ShellSession {
            id: "s".into(),
            kind: SessionKind::Ssh,
            ssh_host: "dev.example.com".into(),
            ssh_user: "alice".into(),
            ssh_remote_folder: "/srv/a\" -oProxyCommand=calc".into(),
            command: "bash".into(),
            ..Default::default()
        };
        let p = plan(&s, None, "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(p.argv.first().map(String::as_str), Some("ssh"));
        assert!(p.argv.contains(&"alice@dev.example.com".to_owned()), "{:?}", p.argv);
        // The hostile folder stays inside the one remote-command argument.
        assert!(!p.argv.iter().any(|a| a.starts_with("-oProxyCommand")), "{:?}", p.argv);
        assert_eq!(p.cwd, None);
        let bad = ShellSession { kind: SessionKind::Ssh, ..Default::default() };
        assert!(plan(&bad, None, "pwsh.exe", "/bin/sh").is_err());
    }

    #[test]
    fn resume_lookup_takes_the_newest_safe_jsonl() {
        let home = tempfile::tempdir().unwrap();
        let dir = home.path().join("projects").join("C--src-x");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("older-1.jsonl"), "").unwrap();
        std::thread::sleep(Duration::from_millis(30));
        std::fs::write(dir.join("newer-2.jsonl"), "").unwrap();
        std::fs::write(dir.join("not a uuid.jsonl"), "").unwrap();
        let id = last_claude_session_id(r"C:\src\x", home.path());
        assert!(matches!(id.as_deref(), Some("newer-2") | Some("older-1")), "{id:?}");
        assert_eq!(last_claude_session_id(r"C:\nowhere", home.path()), None);
    }
}
