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

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use csm_core::models::{SessionKind, ShellSession};
use csm_services::argv::split_command_line;
use csm_services::claude_usage;
use csm_services::launch::{self as sl, LaunchSpec};

use crate::{proc, which, wsl_probe};

/// The environment changes every session and run command starts with (issue #162): the
/// launching terminal's session markers (`CLAUDECODE`, `CLAUDE_CODE_ENTRYPOINT`, `WT_SESSION`,
/// `VSCODE_*`, …) removed, `TERM_PROGRAM`/`TERM_PROGRAM_VERSION`/`CSM_ENV` set. Reads the
/// current environment's names, fresh each time; call it on the spawning worker.
pub fn inherited_env_changes() -> Vec<(OsString, Option<OsString>)> {
    let names: Vec<OsString> = std::env::vars_os().map(|(k, _)| k).collect();
    csm_core::session_env::session_env_changes(names.iter().map(OsString::as_os_str), env!("CARGO_PKG_VERSION"))
}

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
    /// `PATH` for the child: the app's own plus the folders installers use, so `claude` resolves
    /// when the app was started from Explorer or the Dock with a short `PATH`. `None` leaves it.
    pub path: Option<OsString>,
    /// The Claude conversation this launch runs, when known: the `--session-id` CSM chose, the
    /// id given to `--resume`, or one the user's own arguments name. Stored on the session.
    pub claude_session_id: Option<String>,
}

fn spec_of(s: &ShellSession, wsl_shell: Option<&str>) -> LaunchSpec {
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
        ssh_config_file: s.ssh_config_file.clone(),
        ssh_remote_argv: s.ssh_remote_argv.clone(),
        wsl_distro: s.wsl_distro.clone(),
        wsl_user: s.wsl_user.clone(),
        wsl_working_folder: s.wsl_working_folder.clone(),
        resolved_wsl_shell: wsl_shell.map(str::to_owned),
    }
}

/// A Claude conversation id we are willing to put on a command line: what Claude names its
/// `.jsonl` files (a UUID), nothing a shell could read as syntax.
pub fn is_safe_resume_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// Longest session name passed to `claude --name`.
const MAX_CLAUDE_NAME: usize = 80;

/// A session name made safe to put on the shell payload as `'<name>'`: letters and digits of any
/// script, spaces, and `-_.,:[]#+@=~`. Everything else becomes a space: quotes of every kind
/// (PowerShell also reads them as string ends), backslashes, `$`, `` ` ``, control characters, and
/// `&`, `(`, `)`, `|`, `^`, `<`, `>`, `%`, `!`, which `cmd.exe` acts on when PowerShell hands an
/// argument without spaces to an npm `claude.cmd` shim unquoted. Runs of spaces collapse,
/// leading dashes go (so it can't read as an option), and it is capped at [`MAX_CLAUDE_NAME`]
/// characters. `None` when nothing is left.
pub fn safe_claude_name(name: &str) -> Option<String> {
    const EXTRA: &str = " -_.,:[]#+@=~";
    let kept: String = name.chars().map(|c| if c.is_alphanumeric() || EXTRA.contains(c) { c } else { ' ' }).collect();
    let words: Vec<&str> = kept.split(' ').filter(|w| !w.is_empty()).collect();
    let joined = words.join(" ");
    let trimmed = joined.trim_start_matches(['-', ' ']);
    let capped: String = trimmed.chars().take(MAX_CLAUDE_NAME).collect();
    let capped = capped.trim_end().to_owned();
    (!capped.is_empty()).then_some(capped)
}

/// The user already passes a name (`--name x`, `--name=x`, `-n x`).
fn has_name_flag(args: &str) -> bool {
    args.split_whitespace().any(|w| w == "-n" || w == "--name" || w.starts_with("--name="))
}

/// Settings › General "Name Claude conversations after the session" (#76): a local Claude
/// session gets `--name '<session name>'` in front of its arguments, so Claude's `/resume` list
/// and terminal title carry the name. Not when the user's arguments already name it, and not for
/// SSH or WSL sessions (their payload goes through another shell's quoting). The name is reduced
/// to [`safe_claude_name`]'s alphabet and single-quoted, which both `pwsh -Command` and
/// `sh -c` read literally.
pub fn with_claude_name(s: &ShellSession) -> ShellSession {
    let mut c = s.clone();
    if s.kind != SessionKind::Local || !is_claude_command(&s.command) || has_name_flag(&s.args) || has_name_flag(&s.command) {
        return c;
    }
    let Some(name) = safe_claude_name(&s.name) else { return c };
    let arg = format!("--name '{name}'");
    c.args = if c.args.trim().is_empty() { arg } else { format!("{arg} {}", c.args.trim()) };
    c
}

const PASSTHROUGH_SHELLS: [&str; 10] = ["cmd", "powershell", "pwsh", "wsl", "bash", "zsh", "sh", "ssh", "nu", "fish"];

fn is_passthrough_shell(command: &str) -> bool {
    let leaf = csm_core::models::get_file_name(command.trim());
    let stem = leaf.rsplit_once('.').map_or(leaf, |(s, _)| s).to_ascii_lowercase();
    PASSTHROUGH_SHELLS.contains(&stem.as_str())
}

/// Which Claude conversation a launch names (validated before it reaches the command line).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeId<'a> {
    /// `--resume <id>`: continue that conversation.
    Resume(&'a str),
    /// `--session-id <id>`: a new conversation with an id CSM chose, so its transcript is known.
    New(&'a str),
}

/// The argv for a session. `resume_id` is appended as `--resume <id>` (validated);
/// `wrapper` is the PowerShell that hosts non-shell commands on Windows; `unix_shell` is the
/// login shell elsewhere.
#[cfg(test)]
pub fn plan(s: &ShellSession, resume_id: Option<&str>, wrapper: &str, unix_shell: &str) -> Result<Plan, String> {
    plan_with(s, resume_id.map(ClaudeId::Resume), wrapper, unix_shell, None)
}

/// [`plan`] with the WSL login shell the distro probe found (`ResolvedWslShell`; `bash` when
/// `None`).
pub fn plan_with(s: &ShellSession, claude_id: Option<ClaudeId<'_>>, wrapper: &str, unix_shell: &str, wsl_shell: Option<&str>) -> Result<Plan, String> {
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
            let flag = match claude_id {
                Some(ClaudeId::Resume(id)) if is_safe_resume_id(id) => Some(format!("--resume {id}")),
                Some(ClaudeId::New(id)) if is_safe_resume_id(id) => Some(format!("--session-id {id}")),
                _ => None,
            };
            if let Some(flag) = flag {
                args = if args.is_empty() { flag } else { format!("{flag} {args}") };
            }
            let full = if args.is_empty() { command.to_owned() } else { format!("{command} {args}") };
            if cfg!(windows) {
                split_command_line(&sl::build_cmd_line(command, &full, wrapper))
            } else if is_passthrough_shell(command) {
                // POSIX words, not the Windows MSVCRT rules: `sh -c 'echo hi'` must reach sh as
                // three arguments. Windows splitting ignores single quotes, so on Unix a quoted
                // argument used to arrive in pieces ("Unterminated quoted string").
                shlex::split(&full).ok_or_else(|| format!("The command line has an unbalanced quote: {full}"))?
            } else {
                vec![unix_shell.to_owned(), "-l".into(), "-i".into(), "-c".into(), full]
            }
        }
        SessionKind::Ssh => {
            let args = sl::build_ssh_args(&spec_of(s, None)).map_err(|e| e.to_string())?;
            std::iter::once("ssh".to_owned()).chain(split_command_line(&args)).collect()
        }
        SessionKind::Wsl => {
            if !cfg!(windows) {
                return Err("WSL sessions run on Windows only.".into());
            }
            let args = sl::build_wsl_args(&spec_of(s, wsl_shell), None).map_err(|e| e.to_string())?;
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
    Ok(Plan { argv, cwd, is_claude, path: None, claude_session_id: None })
}

/// [`resolve_with`] for a session alone in its folder (tests).
#[cfg(test)]
pub fn resolve(s: &ShellSession, resume: bool, mcp: Option<&McpOffer>) -> Result<Plan, String> {
    resolve_with(s, resume, true, mcp)
}

/// [`plan_with`] plus its I/O: the Claude conversation lookup, the PowerShell to wrap with, and a
/// folder that no longer exists falling back to home (the WPF rule). `sole_in_folder`: no other
/// CSM Claude session works in this folder (false: a resume never adopts a newer transcript of
/// the folder). Blocking; worker threads only.
pub fn resolve_with(s: &ShellSession, resume: bool, sole_in_folder: bool, mcp: Option<&McpOffer>) -> Result<Plan, String> {
    // "Offer CodeShellManager's tools to Claude sessions" (Settings → MCP, off by default): a local
    // Claude session gets `--mcp-config <file>` pointing at `csm mcp --session <id>`, so its calls
    // carry the session id and the consent card can say "Claude in <name> wants to…".
    let with_mcp;
    let s = match mcp.filter(|_| s.kind == SessionKind::Local && is_claude_command(&s.command) && !s.args.contains("--mcp-config")) {
        Some(offer) => match offer.write_config(&s.id) {
            Ok(arg) => {
                let mut c = s.clone();
                c.args = if c.args.trim().is_empty() { arg } else { format!("{arg} {}", c.args.trim()) };
                with_mcp = c;
                &with_mcp
            }
            Err(e) => {
                log::warn!("not offering CodeShellManager's MCP tools to '{}': {e}", s.working_folder);
                s
            }
        },
        None => s,
    };
    let modified = |p: &std::path::Path| p.metadata().and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH);
    let choice = choose_claude_id(
        s,
        resume,
        sole_in_folder,
        |id| {
            // Claude resumes from its own config folder: look there (not other profiles).
            let home: Vec<PathBuf> = claude_usage::discover::claude_home().into_iter().collect();
            claude_usage::discover::find_transcript_near(&home, id, s.working_folder.trim()).map(|p| modified(&p))
        },
        || {
            let home: Vec<PathBuf> = claude_usage::discover::claude_home().into_iter().collect();
            claude_usage::discover::newest_transcript_for_folder(&home, s.working_folder.trim()).map(|(id, p)| (id, modified(&p)))
        },
        || uuid::Uuid::new_v4().to_string(),
    );
    match &choice {
        ClaudeChoice::Resume(id) => log::info!("claude: --resume {id} in '{}'", s.working_folder),
        ClaudeChoice::New(id) => log::info!("claude: new conversation --session-id {id} in '{}'", s.working_folder),
        ClaudeChoice::FromArgs(id) => log::info!("claude: the session's arguments name conversation {id}"),
        ClaudeChoice::None => {}
    }
    let claude_id = match &choice {
        ClaudeChoice::Resume(id) => Some(ClaudeId::Resume(id.as_str())),
        ClaudeChoice::New(id) => Some(ClaudeId::New(id.as_str())),
        ClaudeChoice::FromArgs(_) | ClaudeChoice::None => None,
    };
    let wrapper = if cfg!(windows) { pwsh() } else { "pwsh.exe" };
    let unix_shell = std::env::var("SHELL").ok().filter(|v| v.starts_with('/')).unwrap_or_else(|| "/bin/sh".into());
    let mut p = if s.kind == SessionKind::Wsl {
        let wsl = wsl_session(s)?;
        let shell = wsl_probe::login_shell(&wsl.wsl_distro, &wsl.wsl_user);
        plan_with(&wsl, claude_id, wrapper, &unix_shell, Some(&shell))?
    } else {
        plan_with(s, claude_id, wrapper, &unix_shell, None)?
    };
    p.path = which::search_path();
    p.claude_session_id = choice.id().map(str::to_owned);
    if s.kind == SessionKind::Local && !is_passthrough_shell(&s.command) && which::which(first_word(&s.command)).is_none() {
        log::warn!("`{}` was not found on PATH or in the usual install folders; the shell may not find it either", first_word(&s.command));
    }
    if p.cwd.as_deref().is_some_and(|c| !c.is_dir()) {
        log::warn!("session folder '{}' does not exist; starting in home", s.working_folder);
        p.cwd = None;
    }
    if p.cwd.is_none() {
        p.cwd = dirs::home_dir();
    }
    Ok(p)
}

fn first_word(command: &str) -> &str {
    command.split_whitespace().next().unwrap_or("")
}

/// The WSL session with its Linux folder made concrete (`WslDiscoveryService` rules): blank or
/// `~` stays blank, which `build_wsl_args` turns into an unquoted `--cd ~` (quoting it would make
/// a folder literally called `~`); `~/x` becomes `$HOME/x`, because wsl.exe expands only the bare
/// `~` token and would treat `~/x` as a Windows path. Blocking: probes `$HOME` when it needs it.
fn wsl_session(s: &ShellSession) -> Result<ShellSession, String> {
    let mut out = s.clone();
    let folder = s.wsl_working_folder.trim();
    out.wsl_working_folder = if folder == "~" {
        String::new()
    } else if let Some(rest) = folder.strip_prefix("~/") {
        let home = wsl_probe::home(&s.wsl_distro, &s.wsl_user).ok_or_else(|| {
            format!(
                "Could not find the home folder in WSL distro `{}` (is it starting?). Try again, or edit the session and type the full Linux path.",
                s.wsl_distro.trim()
            )
        })?;
        format!("{}/{rest}", home.trim_end_matches('/'))
    } else {
        folder.to_owned()
    };
    Ok(out)
}

/// What a Claude launch says about its conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeChoice {
    /// Not a local Claude session, or its arguments pick the conversation in a way we can't
    /// follow (`--continue`, a bare `--resume`).
    None,
    /// `--resume <id>`.
    Resume(String),
    /// `--session-id <id>`: a new conversation whose transcript we know in advance.
    New(String),
    /// The session's own arguments name it (`--resume <id>`, `--session-id <id>`); nothing added.
    FromArgs(String),
}

impl ClaudeChoice {
    pub fn id(&self) -> Option<&str> {
        match self {
            ClaudeChoice::None => None,
            ClaudeChoice::Resume(id) | ClaudeChoice::New(id) | ClaudeChoice::FromArgs(id) => Some(id),
        }
    }
}

/// How the user's own arguments pick a conversation: `Some(Some(id))` names one,
/// `Some(None)` picks one we can't know (`--continue`, `-c`, a bare `--resume`, `--fork-session`),
/// `None` says nothing.
fn conversation_in_args(args: &str) -> Option<Option<String>> {
    let words: Vec<&str> = args.split_whitespace().collect();
    let mut i = 0;
    while let Some(w) = words.get(i) {
        let (flag, inline) = match w.split_once('=') {
            Some((f, v)) => (f, Some(v)),
            None => (*w, None),
        };
        match flag {
            "--resume" | "-r" | "--session-id" => {
                let value = inline.or_else(|| words.get(i + 1).copied().filter(|v| !v.starts_with('-')));
                let value = value.map(|v| v.trim_matches(['"', '\''])).filter(|v| is_safe_resume_id(v));
                return Some(value.map(str::to_owned));
            }
            "--continue" | "-c" | "--fork-session" => return Some(None),
            // A name's words are not flags: `--name 'x -c'` (the session-name setting quotes it)
            // must not read as `--continue`.
            "--name" | "-n" => {
                // An opening quote whose word doesn't also close it: the name runs on.
                let opens = |v: &str| v.starts_with('\'') && !(v.len() > 1 && v.ends_with('\''));
                let mut quoted = match inline {
                    Some(v) => opens(v),
                    None if words.get(i + 1).is_some_and(|v| !v.starts_with('-')) => {
                        i += 1;
                        words.get(i).is_some_and(|v| opens(v))
                    }
                    None => false,
                };
                while quoted {
                    i += 1;
                    quoted = words.get(i).is_some_and(|v| !v.ends_with('\''));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Which conversation a launch of `s` runs (pure apart from the callbacks, which do the I/O):
/// - a new start gets a fresh `--session-id`, so its transcript is known exactly;
/// - a resume (restore, wake, restart) continues the stored id when its transcript exists, starts
///   a conversation with that id when it doesn't (nothing was ever said in it), and only for a
///   session from before CSM stored ids falls back to `guess`: the newest transcript whose `cwd`
///   is the folder;
/// - arguments that already pick a conversation are left alone (an id they name is recorded).
///
/// `exists` and `guess` return the transcript's modification time. `sole_in_folder` says no other
/// CSM Claude session works in this folder: then a transcript of the folder written after the
/// stored one is taken to be this session's (a `/clear` in Claude starts a new conversation id
/// that CSM is not told about), the rule the app used before ids were stored.
pub fn choose_claude_id(
    s: &ShellSession,
    resume: bool,
    sole_in_folder: bool,
    exists: impl Fn(&str) -> Option<SystemTime>,
    guess: impl FnOnce() -> Option<(String, SystemTime)>,
    fresh: impl FnOnce() -> String,
) -> ClaudeChoice {
    // "Re-run argv" sessions never get `--resume` or `--session-id`: the program reattaches by itself.
    if s.resume_rerun {
        return ClaudeChoice::None;
    }
    if s.kind != SessionKind::Local || !is_claude_command(&s.command) {
        return ClaudeChoice::None;
    }
    match conversation_in_args(&s.args) {
        Some(Some(id)) => return ClaudeChoice::FromArgs(id),
        Some(None) => return ClaudeChoice::None,
        None => {}
    }
    let stored = s.claude_session_id.as_deref().filter(|id| is_safe_resume_id(id));
    if resume {
        if let Some(id) = stored {
            let Some(at) = exists(id) else { return ClaudeChoice::New(id.to_owned()) };
            if sole_in_folder
                && let Some((newer, _)) = guess().filter(|(g, t)| g != id && *t > at && is_safe_resume_id(g))
            {
                return ClaudeChoice::Resume(newer);
            }
            return ClaudeChoice::Resume(id.to_owned());
        }
        if let Some((id, _)) = guess().filter(|(id, _)| is_safe_resume_id(id)) {
            return ClaudeChoice::Resume(id);
        }
    }
    let id = fresh();
    if is_safe_resume_id(&id) { ClaudeChoice::New(id) } else { ClaudeChoice::None }
}

/// `pwsh.exe` when it is on PATH and actually runs, else `powershell.exe` (`PwshLocator`).
/// Resolved once per run.
pub(crate) fn pwsh() -> &'static str {
    static PWSH: OnceLock<&'static str> = OnceLock::new();
    PWSH.get_or_init(|| {
        let where_exe = std::env::var_os("SystemRoot")
            .map(|r| PathBuf::from(r).join("System32").join("where.exe"))
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\where.exe"));
        let out = proc::capture_ok(where_exe, ["pwsh.exe"], Duration::from_secs(3));
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
        proc::capture_ok(path, ["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", "exit 0"], Duration::from_secs(5)).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(windows)]
    use std::path::Path;

    fn local(command: &str, args: &str) -> ShellSession {
        ShellSession { id: "s".into(), command: command.into(), args: args.into(), working_folder: r"C:\src\a b".into(), ..Default::default() }
    }

    fn wsl(folder: &str) -> ShellSession {
        ShellSession {
            id: "w".into(),
            kind: SessionKind::Wsl,
            wsl_distro: "Ubuntu".into(),
            wsl_working_folder: folder.into(),
            command: "claude".into(),
            ..Default::default()
        }
    }

    #[test]
    #[cfg(windows)]
    fn wsl_blank_and_tilde_folders_mean_home_unquoted() {
        for folder in ["", "  ", "~"] {
            let p = plan_with(&wsl_session_no_probe(folder), None, "pwsh.exe", "/bin/sh", Some("sh")).unwrap();
            let at = p.argv.iter().position(|a| a == "--cd").unwrap();
            assert_eq!(p.argv[at + 1], "~", "{folder:?} -> {:?}", p.argv);
        }
        let p = plan_with(&wsl("/home/a b"), None, "pwsh.exe", "/bin/sh", Some("sh")).unwrap();
        assert_eq!(p.argv[0], "wsl.exe");
        assert!(p.argv.windows(2).any(|w| w == ["-e", "sh"]), "the probed shell is the login shell: {:?}", p.argv);
        assert!(p.argv.contains(&"/home/a b".to_owned()));
        let p = plan_with(&wsl("/x"), None, "pwsh.exe", "/bin/sh", None).unwrap();
        assert!(p.argv.windows(2).any(|w| w == ["-e", "bash"]), "{:?}", p.argv);
    }

    /// What `wsl_session` does to a folder that needs no probe.
    #[cfg(windows)]
    fn wsl_session_no_probe(folder: &str) -> ShellSession {
        wsl_session(&wsl(folder)).unwrap()
    }

    #[test]
    fn wsl_tilde_folders_are_normalised_without_a_probe_when_bare() {
        assert_eq!(wsl_session(&wsl("~")).unwrap().wsl_working_folder, "");
        assert_eq!(wsl_session(&wsl("/srv/x")).unwrap().wsl_working_folder, "/srv/x");
        // `~/x` needs `$HOME`: without a probe result it is an error, never a literal `~/x`.
        if !cfg!(windows) {
            assert!(wsl_session(&wsl("~/proj")).is_err());
        }
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
    fn ssh_config_alias_and_remote_argv_become_arguments() {
        let s = ShellSession {
            id: "s".into(),
            kind: SessionKind::Ssh,
            ssh_host: "agent-1".into(),
            ssh_config_file: "/cfg dir/ssh_config".into(),
            ssh_remote_argv: ["tmux", "new-session", "-A", "-s", "ma in; rm -rf x"].map(String::from).to_vec(),
            ..Default::default()
        };
        let p = plan(&s, None, "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(&p.argv[..4], ["ssh", "-F", "/cfg dir/ssh_config", "-t"], "{:?}", p.argv);
        assert_eq!(p.argv[4], "agent-1");
        assert_eq!(p.argv[5], "tmux new-session -A -s 'ma in; rm -rf x'");
        assert_eq!(p.argv.len(), 6);
    }

    #[test]
    fn rerun_sessions_never_get_a_claude_id() {
        let mut s = claude("", Some("11111111-2222-3333-4444-555555555555"));
        s.resume_rerun = true;
        let c = choose_claude_id(&s, true, true, |_| Some(SystemTime::UNIX_EPOCH), || None, || "x".into());
        assert_eq!(c, ClaudeChoice::None);
        s.resume_rerun = false;
        assert!(matches!(choose_claude_id(&s, true, true, |_| Some(SystemTime::UNIX_EPOCH), || None, || "x".into()), ClaudeChoice::Resume(_)));
    }

    fn claude(args: &str, stored: Option<&str>) -> ShellSession {
        ShellSession { claude_session_id: stored.map(Into::into), ..local("claude", args) }
    }

    fn at(s: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(s)
    }

    /// With another Claude session in the folder: the stored id is never traded for a guess.
    fn choose(s: &ShellSession, resume: bool, exists: bool, guess: Option<&str>) -> ClaudeChoice {
        choose_claude_id(s, resume, false, |_| exists.then(|| at(10)), || guess.map(|g| (g.into(), at(20))), || "fresh-1".into())
    }

    #[test]
    fn a_resume_follows_a_clear_when_the_session_is_alone_in_its_folder() {
        let s = claude("", Some("abc-1"));
        let pick = |stored: Option<u64>, newest: Option<(&str, u64)>| {
            choose_claude_id(&s, true, true, |_| stored.map(at), || newest.map(|(g, t)| (g.into(), at(t))), || "fresh-1".into())
        };
        // A newer conversation in the folder (a `/clear`): that one.
        assert_eq!(pick(Some(10), Some(("after-clear-2", 20))), ClaudeChoice::Resume("after-clear-2".into()));
        // The stored one is the newest, or the other is older: the stored one.
        assert_eq!(pick(Some(10), Some(("abc-1", 10))), ClaudeChoice::Resume("abc-1".into()));
        assert_eq!(pick(Some(30), Some(("older-3", 20))), ClaudeChoice::Resume("abc-1".into()));
        assert_eq!(pick(Some(10), None), ClaudeChoice::Resume("abc-1".into()));
        // No transcript for the stored id (nothing said yet): never an older conversation.
        assert_eq!(pick(None, Some(("old-4", 5))), ClaudeChoice::New("abc-1".into()));
        // A hostile newest id is never used.
        assert_eq!(pick(Some(10), Some(("x; calc", 20))), ClaudeChoice::Resume("abc-1".into()));
        // Not resuming: a fresh conversation either way.
        assert_eq!(
            choose_claude_id(&s, false, true, |_| Some(at(10)), || Some(("after-clear-2".into(), at(20))), || "fresh-1".into()),
            ClaudeChoice::New("fresh-1".into())
        );
    }

    #[test]
    fn a_new_claude_session_gets_its_own_session_id() {
        assert_eq!(choose(&claude("", None), false, false, Some("old-9")), ClaudeChoice::New("fresh-1".into()));
        // Even with an id stored: not resuming means a new conversation.
        assert_eq!(choose(&claude("", Some("old-1")), false, true, None), ClaudeChoice::New("fresh-1".into()));
    }

    #[test]
    fn a_resume_continues_the_stored_id_exactly() {
        assert_eq!(choose(&claude("", Some("abc-1")), true, true, Some("other-2")), ClaudeChoice::Resume("abc-1".into()));
        // Nothing was said in it yet (no transcript): start it under the same id.
        assert_eq!(choose(&claude("", Some("abc-1")), true, false, Some("other-2")), ClaudeChoice::New("abc-1".into()));
        // An old session without an id falls back to the newest transcript of its folder.
        assert_eq!(choose(&claude("", None), true, false, Some("guess-3")), ClaudeChoice::Resume("guess-3".into()));
        assert_eq!(choose(&claude("", None), true, false, None), ClaudeChoice::New("fresh-1".into()));
        // A hostile stored id is ignored, never put on a command line.
        assert_eq!(choose(&claude("", Some("x; calc")), true, true, None), ClaudeChoice::New("fresh-1".into()));
    }

    #[test]
    fn arguments_that_pick_a_conversation_are_left_alone() {
        assert_eq!(choose(&claude("--resume abc-1 --model x", None), true, true, None), ClaudeChoice::FromArgs("abc-1".into()));
        assert_eq!(choose(&claude("--session-id=abc-2", None), false, true, None), ClaudeChoice::FromArgs("abc-2".into()));
        assert_eq!(choose(&claude("-r abc-3", None), false, true, None), ClaudeChoice::FromArgs("abc-3".into()));
        assert_eq!(choose(&claude("--continue", Some("abc-1")), true, true, None), ClaudeChoice::None);
        assert_eq!(choose(&claude("--resume", None), true, true, None), ClaudeChoice::None);
        assert_eq!(choose(&claude("--resume --model x", None), true, true, None), ClaudeChoice::None);
        // The quoted session name (#76) never reads as a flag that picks a conversation.
        assert_eq!(choose(&claude("--name 'x -c --resume abc-9' --model x", None), false, true, None), ClaudeChoice::New("fresh-1".into()));
        assert_eq!(choose(&claude("--name 'solo' -r abc-4", None), false, true, None), ClaudeChoice::FromArgs("abc-4".into()));
        assert_eq!(choose(&claude("--name='a b' --session-id abc-5", None), false, true, None), ClaudeChoice::FromArgs("abc-5".into()));
    }

    #[test]
    fn only_local_claude_sessions_get_an_id() {
        assert_eq!(choose(&local("pwsh", ""), false, false, None), ClaudeChoice::None);
        let mut w = wsl("/x");
        w.claude_session_id = Some("abc-1".into());
        assert_eq!(choose(&w, true, true, None), ClaudeChoice::None);
    }

    #[test]
    #[cfg(windows)]
    fn the_session_id_goes_before_the_users_arguments() {
        let p = plan_with(&local("claude", "--model x"), Some(ClaudeId::New("abc-1")), "pwsh.exe", "/bin/sh", None).unwrap();
        assert_eq!(p.argv, ["pwsh.exe", "-NoExit", "-Command", "claude", "--session-id", "abc-1", "--model", "x"]);
        let p = plan_with(&local("claude", ""), Some(ClaudeId::New("x; calc")), "pwsh.exe", "/bin/sh", None).unwrap();
        assert!(!p.argv.iter().any(|a| a.contains("calc")), "{:?}", p.argv);
    }

    #[test]
    fn resolve_records_the_conversation_it_starts() {
        let dir = tempfile::tempdir().unwrap();
        let s = ShellSession { working_folder: dir.path().to_string_lossy().into_owned(), ..claude("", None) };
        let p = resolve(&s, false, None).unwrap();
        let id = p.claude_session_id.clone().unwrap();
        assert!(is_safe_resume_id(&id) && id.len() == 36, "{id}");
        assert!(p.argv.iter().any(|a| a.contains(&id)), "{:?}", p.argv);
        assert!(p.argv.iter().any(|a| a.contains("--session-id")), "{:?}", p.argv);
    }
}

/// Where a Claude session's MCP config is written, and the `csm` binary it points at.
#[derive(Debug, Clone)]
pub struct McpOffer {
    pub dir: PathBuf,
    pub csm: PathBuf,
}

impl McpOffer {
    /// The `csm` beside the running app, if it is there (a dev build or test binary has none).
    pub fn beside_app(dir: PathBuf) -> Option<McpOffer> {
        let exe = std::env::current_exe().ok()?;
        let csm = exe.parent()?.join(if cfg!(windows) { "csm.exe" } else { "csm" });
        csm.is_file().then_some(McpOffer { dir, csm })
    }

    /// Writes `<dir>/mcp-<id>.json` and returns the `--mcp-config '<path>'` argument. Worker only.
    fn write_config(&self, session_id: &str) -> Result<String, String> {
        let (json, file) = mcp_config(&self.csm, session_id, &self.dir)?;
        private_dir(&self.dir).map_err(|e| format!("could not use {} safely: {e}", self.dir.display()))?;
        write_private_file(&file, json.as_bytes()).map_err(|e| format!("could not write {}: {e}", file.display()))?;
        mcp_config_arg(&file)
    }
}

/// Makes sure `dir` is a real folder (not a link) that only this user can use: created with mode
/// 0700 on Unix, and refused when it already exists with another owner or with group or other
/// access (a predictable shared folder another user planted). The owner is read by comparing with
/// a file made here, which we own by definition. On Windows the folder inherits its parent's
/// per-user permissions.
fn private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let mut b = std::fs::DirBuilder::new();
        b.recursive(true).mode(0o700);
        b.create(dir)?;
        let meta = std::fs::symlink_metadata(dir)?;
        if !meta.is_dir() {
            return Err(std::io::Error::other("not a plain folder (a link?)"));
        }
        let probe = dir.join(format!(".owner-probe-{}", std::process::id()));
        let _ = std::fs::remove_file(&probe);
        write_private_file(&probe, b"")?;
        let mine = std::fs::symlink_metadata(&probe).map(|m| m.uid());
        let _ = std::fs::remove_file(&probe);
        if mine? != meta.uid() {
            return Err(std::io::Error::other("the folder belongs to another user"));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)?;
        if std::fs::symlink_metadata(dir)?.file_type().is_symlink() {
            return Err(std::io::Error::other("not a plain folder (a link?)"));
        }
        Ok(())
    }
}

/// Writes a new file that only this user can read (0600 on Unix), never through an existing file
/// or link: a leftover of an earlier run is removed first and the file is made with `create_new`.
fn write_private_file(file: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::remove_file(file) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(file)?;
    f.write_all(bytes)
}

/// The config JSON and its file name for one session. The session id becomes part of a file name
/// and an argv element, so only `[A-Za-z0-9_-]` is accepted.
fn mcp_config(csm: &std::path::Path, session_id: &str, dir: &std::path::Path) -> Result<(String, PathBuf), String> {
    if session_id.is_empty() || !session_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
        return Err(format!("unexpected session id `{session_id}`"));
    }
    let json = serde_json::json!({
        "mcpServers": { "codeshell": { "command": csm.to_string_lossy(), "args": ["mcp", "--session", session_id] } }
    });
    Ok((json.to_string(), dir.join(format!("mcp-{session_id}.json"))))
}

/// `--mcp-config '<path>'`. The arguments reach claude through `pwsh -Command` (Windows) or
/// `sh -c` (Unix); single quotes are literal in both, so a path containing one is refused rather
/// than escaped.
fn mcp_config_arg(file: &std::path::Path) -> Result<String, String> {
    let p = file.to_string_lossy();
    if p.contains('\'') || p.chars().any(char::is_control) {
        return Err(format!("the config path can't be passed safely: {p}"));
    }
    Ok(format!("--mcp-config '{p}'"))
}

#[cfg(test)]
mod posix_split_tests {
    use super::*;

    #[test]
    fn a_shell_session_on_unix_keeps_quoted_arguments_whole() {
        if cfg!(windows) {
            return;
        }
        let s = ShellSession { id: "a".into(), command: "sh".into(), args: "-c 'echo hi from csm'".into(), ..Default::default() };
        let p = plan(&s, None, "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(p.argv, vec!["sh", "-c", "echo hi from csm"]);
        let bad = ShellSession { args: "-c 'echo hi".into(), ..s };
        assert!(plan(&bad, None, "pwsh.exe", "/bin/sh").unwrap_err().contains("unbalanced quote"));
    }
}

#[cfg(test)]
mod mcp_offer_tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn config_points_at_csm_mcp_with_the_session() {
        let (json, file) = mcp_config(Path::new("C:/apps/csm.exe"), "abc-123", Path::new("C:/state")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["mcpServers"]["codeshell"]["args"], serde_json::json!(["mcp", "--session", "abc-123"]));
        assert_eq!(v["mcpServers"]["codeshell"]["command"], "C:/apps/csm.exe");
        assert!(file.ends_with("mcp-abc-123.json"));
    }

    #[test]
    fn hostile_ids_and_paths_are_refused() {
        for id in ["", "a b", "a;b", "../x", "a'b", "$(id)"] {
            assert!(mcp_config(Path::new("csm"), id, Path::new("d")).is_err(), "{id}");
        }
        assert!(mcp_config_arg(Path::new("C:/it's/mcp.json")).is_err());
        assert_eq!(mcp_config_arg(Path::new("C:/a b/mcp.json")).unwrap(), "--mcp-config 'C:/a b/mcp.json'");
    }

    #[test]
    fn only_local_claude_sessions_get_the_offer() {
        let dir = std::env::temp_dir().join(format!("csm-mcp-offer-{}", std::process::id()));
        let offer = McpOffer { dir: dir.clone(), csm: PathBuf::from("csm") };
        let mut s =
            ShellSession { id: "s1".into(), command: "claude".into(), working_folder: dir.to_string_lossy().into_owned(), ..Default::default() };
        let p = resolve(&s, false, Some(&offer)).unwrap();
        assert!(p.argv.join(" ").contains("--mcp-config"), "{:?}", p.argv);
        assert!(dir.join("mcp-s1.json").is_file());
        s.command = "pwsh".into();
        let p = resolve(&s, false, Some(&offer)).unwrap();
        assert!(!p.argv.join(" ").contains("--mcp-config"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_config_is_made_new_and_private_never_through_a_leftover_or_a_link() {
        let tmp = tempfile::tempdir().unwrap();
        let offer = McpOffer { dir: tmp.path().join("mcp"), csm: PathBuf::from("csm") };
        std::fs::create_dir_all(&offer.dir).unwrap();
        let file = offer.dir.join("mcp-s1.json");
        std::fs::write(&file, "leftover").unwrap();
        offer.write_config("s1").unwrap();
        assert!(std::fs::read_to_string(&file).unwrap().contains("mcpServers"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(std::fs::metadata(&offer.dir).unwrap().permissions().mode() & 0o777, 0o700);
            // A planted link at the file's name is replaced, never written through.
            let victim = tmp.path().join("victim");
            std::fs::write(&victim, "mine").unwrap();
            std::fs::remove_file(&file).unwrap();
            std::os::unix::fs::symlink(&victim, &file).unwrap();
            offer.write_config("s1").unwrap();
            assert_eq!(std::fs::read_to_string(&victim).unwrap(), "mine");
            // A folder with group or other access is tightened; a link in its place is refused.
            std::fs::set_permissions(&offer.dir, std::fs::Permissions::from_mode(0o777)).unwrap();
            offer.write_config("s1").unwrap();
            assert_eq!(std::fs::metadata(&offer.dir).unwrap().permissions().mode() & 0o777, 0o700);
            let linked = McpOffer { dir: tmp.path().join("link"), csm: PathBuf::from("csm") };
            std::os::unix::fs::symlink(tmp.path(), &linked.dir).unwrap();
            assert!(linked.write_config("s1").is_err());
        }
    }

    #[test]
    fn a_session_id_cannot_climb_out_of_the_config_folder() {
        assert!(mcp_config(Path::new("csm"), "../x", Path::new("d")).is_err());
    }
}

#[cfg(test)]
mod claude_name_tests {
    use super::*;

    fn claude(name: &str, args: &str) -> ShellSession {
        ShellSession { id: "s1".into(), name: name.into(), command: "claude".into(), args: args.into(), ..Default::default() }
    }

    #[test]
    fn names_keep_letters_and_lose_shell_syntax() {
        assert_eq!(safe_claude_name("api [prod] #2").as_deref(), Some("api [prod] #2"));
        // cmd.exe's metacharacters: an npm `claude.cmd` shim would get `x&calc` unquoted.
        assert_eq!(safe_claude_name("x&calc (a|b) ^c <d> %e% !f!").as_deref(), Some("x calc a b c d e f"));
        assert_eq!(safe_claude_name("Æblegrød – ünïcode").as_deref(), Some("Æblegrød ünïcode"));
        // Quotes of every kind (PowerShell ends a '…' string at ‘ ’ ‚ ‛ too), `$`, backtick, `;`.
        assert_eq!(safe_claude_name("it's ‘x’ $(calc) `y`; z").as_deref(), Some("it s x calc y z"));
        assert_eq!(safe_claude_name("a\"b\\c\nd").as_deref(), Some("a b c d"));
        // Never an option, never empty, capped.
        assert_eq!(safe_claude_name("--resume x").as_deref(), Some("resume x"));
        assert_eq!(safe_claude_name("'\"$"), None);
        assert_eq!(safe_claude_name(&"x".repeat(200)).map(|s| s.len()), Some(MAX_CLAUDE_NAME));
    }

    #[test]
    fn a_local_claude_session_gets_its_name_in_front_of_its_args() {
        assert_eq!(with_claude_name(&claude("My repo", "")).args, "--name 'My repo'");
        assert_eq!(with_claude_name(&claude("My repo", "--model opus")).args, "--name 'My repo' --model opus");
    }

    #[test]
    fn a_name_the_user_passes_wins_and_other_sessions_are_left_alone() {
        for args in ["-n mine", "--name mine", "--name=mine"] {
            assert_eq!(with_claude_name(&claude("My repo", args)).args, args);
        }
        let shell = ShellSession { command: "pwsh".into(), ..claude("My repo", "") };
        assert_eq!(with_claude_name(&shell).args, "");
        let ssh = ShellSession { kind: SessionKind::Ssh, ..claude("My repo", "") };
        assert_eq!(with_claude_name(&ssh).args, "");
        assert_eq!(with_claude_name(&claude("'''", "")).args, "", "nothing safe left: no flag");
    }

    #[test]
    #[cfg(windows)]
    fn the_argv_on_windows_carries_the_quoted_name_to_powershell() {
        let s = with_claude_name(&ShellSession { working_folder: r"C:\src".into(), ..claude("My repo", "--model opus") });
        let p = plan(&s, Some("0b1c-22"), "pwsh.exe", "/bin/sh").unwrap();
        // pwsh joins the words after -Command with spaces: `claude --resume … --name 'My repo' …`.
        assert_eq!(p.argv, ["pwsh.exe", "-NoExit", "-Command", "claude", "--resume", "0b1c-22", "--name", "'My", "repo'", "--model", "opus"]);
    }

    #[test]
    #[cfg(not(windows))]
    fn the_argv_on_unix_is_one_shell_payload_with_the_quoted_name() {
        let s = with_claude_name(&ShellSession { working_folder: "/src".into(), ..claude("My repo", "--model opus") });
        let p = plan(&s, Some("0b1c-22"), "pwsh.exe", "/bin/sh").unwrap();
        assert_eq!(p.argv, ["/bin/sh", "-l", "-i", "-c", "claude --resume 0b1c-22 --name 'My repo' --model opus"]);
    }
}
