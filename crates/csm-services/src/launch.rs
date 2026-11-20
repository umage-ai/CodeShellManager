//! Launch argument builders for ssh, `wsl.exe` and run commands, plus launch validation.
//!
//! Ports of `ShellSession.BuildSshArgs` / `BuildWslArgs` / `LaunchValidationError`,
//! `RunInstance.Build*`, `PseudoTerminal.BuildCmdLine` and the decision logic of
//! `PwshLocator`. Every value is untrusted (state.json, imports, a cloned repo): each one is
//! one argv element, never interpolated.

use crate::argv::{posix_single_quote, quote_for_cmd};
use crate::winpath;
use std::fmt;

/// Kind of session. Mirrors `Models.SessionKind` (unify with `csm-core` later).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionKind {
    Local,
    Ssh,
    Wsl,
}

/// The fields of a session that decide how it is launched. A minimal local stand-in for the
/// `ShellSession` model; unify with the `csm-core` type later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchSpec {
    pub kind: SessionKind,
    pub command: String,
    pub args: String,
    /// Local working folder (for WSL: the `\\wsl$` UNC mirror).
    pub working_folder: String,
    pub ssh_user: String,
    pub ssh_host: String,
    pub ssh_port: i32,
    pub ssh_remote_folder: String,
    pub wsl_distro: String,
    pub wsl_user: String,
    /// Linux-style folder inside the distro; empty means the user's home.
    pub wsl_working_folder: String,
    /// Runtime-only: the distro's login shell ("bash" or "sh") once probed; `None` means
    /// "bash". Never persisted.
    pub resolved_wsl_shell: Option<String>,
}

impl Default for LaunchSpec {
    fn default() -> Self {
        Self {
            kind: SessionKind::Local,
            command: "claude".to_string(),
            args: String::new(),
            working_folder: String::new(),
            ssh_user: String::new(),
            ssh_host: String::new(),
            ssh_port: 22,
            ssh_remote_folder: String::new(),
            wsl_distro: String::new(),
            wsl_user: String::new(),
            wsl_working_folder: String::new(),
            resolved_wsl_shell: None,
        }
    }
}

/// Why an argument builder refused. Callers validate first ([`launch_validation_error`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchError {
    /// `SshHost must be set for remote sessions.`
    MissingSshHost,
    /// `WslDistro must be set for WSL sessions.`
    MissingWslDistro,
    /// An SSH host or user that ssh would read as an option (starts with `-`) or that holds a
    /// control character.
    OptionLikeSshName,
    /// A WSL distro or user that wsl.exe would read as an option (starts with `-`; quoting does
    /// not help) or that holds a control character. Issue #123.
    OptionLikeWslName,
}

impl fmt::Display for LaunchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSshHost => f.write_str("SshHost must be set for remote sessions."),
            Self::MissingWslDistro => f.write_str("WslDistro must be set for WSL sessions."),
            Self::OptionLikeSshName => {
                f.write_str("The SSH host or user starts with '-' or contains a control character; ssh would read it as an option.")
            }
            Self::OptionLikeWslName => {
                f.write_str("The WSL distro or user name starts with '-' or contains a control character; wsl.exe would read it as an option.")
            }
        }
    }
}

impl std::error::Error for LaunchError {}

fn blank(s: &str) -> bool {
    s.trim().is_empty()
}

/// A value that may follow an option on a command line: not option-like once trimmed, no
/// control characters (blank passes; blankness is checked separately).
fn argument_safe(s: &str) -> bool {
    !s.trim_start().starts_with('-') && !s.chars().any(char::is_control)
}

fn check_ssh(spec: &LaunchSpec) -> Result<(), LaunchError> {
    if blank(&spec.ssh_host) {
        return Err(LaunchError::MissingSshHost);
    }
    if !argument_safe(&spec.ssh_host) || !argument_safe(&spec.ssh_user) {
        return Err(LaunchError::OptionLikeSshName);
    }
    Ok(())
}

fn check_wsl(spec: &LaunchSpec) -> Result<(), LaunchError> {
    if blank(&spec.wsl_distro) {
        return Err(LaunchError::MissingWslDistro);
    }
    if !argument_safe(&spec.wsl_distro) || !argument_safe(&spec.wsl_user) {
        return Err(LaunchError::OptionLikeWslName);
    }
    Ok(())
}

/// `None` when the session has everything it needs to launch, else a sentence for the user.
/// Checked before any terminal exists, because the builders refuse these and state.json and
/// imports are untrusted.
#[must_use]
pub fn launch_validation_error(spec: &LaunchSpec) -> Option<&'static str> {
    match spec.kind {
        SessionKind::Ssh if blank(&spec.ssh_host) => Some("This SSH session has no host. Edit the session and set one."),
        SessionKind::Ssh if check_ssh(spec).is_err() => Some(
            "This SSH session's host or user starts with '-' or contains a control character, so ssh would read it as an option. Edit the session and fix it.",
        ),
        SessionKind::Wsl if blank(&spec.wsl_distro) => Some("This WSL session has no distro. Edit the session and pick one."),
        SessionKind::Wsl if check_wsl(spec).is_err() => Some(
            "This WSL session's distro or user name starts with '-' or contains a control character, so wsl.exe would read it as an option. Edit the session and fix it.",
        ),
        _ => None,
    }
}

/// The ssh target as one string: `user@host`, or just the host without a user.
fn ssh_target(spec: &LaunchSpec) -> String {
    if blank(&spec.ssh_user) { spec.ssh_host.clone() } else { format!("{}@{}", spec.ssh_user, spec.ssh_host) }
}

/// `-p PORT ` (non-default only), `-t `, the quoted target, then the remote command as a
/// single quoted argv element.
fn ssh_args_with_remote(spec: &LaunchSpec, remote: &str) -> String {
    let mut out = String::new();
    if spec.ssh_port != 22 {
        out.push_str(&format!("-p {} ", spec.ssh_port));
    }
    out.push_str("-t ");
    // Quoted so it stays ONE argv element. Unquoted, an SshHost of
    // `h -oProxyCommand=calc` split into extra *ssh options*, and ProxyCommand runs locally:
    // a crafted state.json got code execution before any connection was attempted.
    out.push_str(&quote_for_cmd(&ssh_target(spec), false));
    out.push(' ');
    // Two escaping layers, and both matter. POSIX single-quoting (inside) is done by the
    // caller; this is the Windows argv layer around the whole remote command. Hand-writing
    // `" ... "` was not enough: PosixSingleQuote escapes `'` and not `"`, so a `"` in the
    // folder ended the wrapper at the Windows layer and the rest became separate ssh
    // arguments, and ssh honours options after the host (`-oProxyCommand=...` runs locally).
    out.push_str(&quote_for_cmd(remote, true));
    out
}

/// The argument string for the ssh executable, e.g.
/// `-t alice@dev.example.com "cd '/proj' && bash"`.
///
/// # Errors
/// [`LaunchError::MissingSshHost`] when the host is blank.
pub fn build_ssh_args(spec: &LaunchSpec) -> Result<String, LaunchError> {
    check_ssh(spec)?;
    let mut remote = String::new();
    if !blank(&spec.ssh_remote_folder) {
        remote.push_str(&format!("cd {} && ", posix_single_quote(&spec.ssh_remote_folder)));
    }
    // Command/Args are arbitrary remote execution by design; becoming *local* execution is
    // not, which is why they sit inside the quoted element too.
    remote.push_str(if blank(&spec.command) { "bash" } else { &spec.command });
    if !blank(&spec.args) {
        remote.push(' ');
        remote.push_str(&spec.args);
    }
    Ok(ssh_args_with_remote(spec, &remote))
}

/// ssh args for a run command: `... "cd '/folder' && bash -c '<escaped>'"` (`RunInstance`).
///
/// # Errors
/// [`LaunchError::MissingSshHost`] when the host is blank. (The C# original would produce
/// a malformed line instead; callers validate first, so this only makes that explicit.)
pub fn build_run_ssh_args(spec: &LaunchSpec, command_line: &str) -> Result<String, LaunchError> {
    check_ssh(spec)?;
    let mut remote = String::new();
    if !blank(&spec.ssh_remote_folder) {
        remote.push_str(&format!("cd {} && ", posix_single_quote(&spec.ssh_remote_folder)));
    }
    remote.push_str("bash -c ");
    remote.push_str(&posix_single_quote(command_line));
    Ok(ssh_args_with_remote(spec, &remote))
}

/// The argument string for `wsl.exe`:
/// `-d <distro> [-u <user>] --cd <linux-folder|~> -e <shell> -lc "<payload>"`.
///
/// The payload is command + args, or `inner` when given (run commands). It is wrapped in
/// `<shell> -lc` so PATH-resolved tools (nvm node, pyenv, ...) behave as in a login shell.
///
/// `-e`, not `--`, deliberately: `wsl.exe <cmd> -- ...` runs the trailing command through the
/// distro's *default* login shell first, so a payload built for our shell is expanded twice,
/// the first time in the wrong environment. Verified empirically:
/// `wsl -d Ubuntu -- bash -lc 'for t in a b; do echo "L=$t"; done'` printed `L=` / `L=`;
/// the same with `-e bash` printed `L=a` / `L=b`. Resist changing it back.
///
/// A blank Linux folder emits `--cd ~` (unquoted: quoting would make it a literal directory
/// name). Omitting `--cd` makes wsl inherit the *Windows* cwd and land in `/mnt/c/...`.
///
/// # Errors
/// [`LaunchError::MissingWslDistro`] when the distro is blank.
pub fn build_wsl_args(spec: &LaunchSpec, inner: Option<&str>) -> Result<String, LaunchError> {
    check_wsl(spec)?;
    let login_shell = spec.resolved_wsl_shell.as_deref().unwrap_or("bash");
    let mut out = String::new();
    out.push_str("-d ");
    out.push_str(&quote_for_cmd(&spec.wsl_distro, false));
    if !blank(&spec.wsl_user) {
        out.push_str(" -u ");
        out.push_str(&quote_for_cmd(&spec.wsl_user, false));
    }
    out.push_str(" --cd ");
    if blank(&spec.wsl_working_folder) {
        out.push('~');
    } else {
        out.push_str(&quote_for_cmd(&spec.wsl_working_folder, false));
    }
    let payload: String = match inner {
        Some(i) => i.to_string(),
        None => {
            let shell = if blank(&spec.command) { login_shell } else { &spec.command };
            if blank(&spec.args) { shell.to_string() } else { format!("{shell} {}", spec.args) }
        }
    };
    out.push_str(" -e ");
    out.push_str(&quote_for_cmd(login_shell, false));
    out.push_str(" -lc ");
    out.push_str(&quote_for_cmd(&payload, true));
    Ok(out)
}

/// Command line for display (error dialogs, tooltips). Never fails: an incomplete session
/// shows just the executable.
#[must_use]
pub fn full_command_line(spec: &LaunchSpec) -> String {
    match spec.kind {
        SessionKind::Ssh => match build_ssh_args(spec) {
            Ok(a) => format!("ssh {a}"),
            Err(_) => "ssh".to_string(),
        },
        SessionKind::Wsl => match build_wsl_args(spec, None) {
            Ok(a) => format!("wsl.exe {a}"),
            Err(_) => "wsl.exe".to_string(),
        },
        SessionKind::Local => {
            if blank(&spec.args) {
                spec.command.clone()
            } else {
                format!("{} {}", spec.command, spec.args)
            }
        }
    }
}

// ── Run commands (RunInstance) ──────────────────────────────────────────────────────────

/// Wraps a single-statement command line for `cmd.exe`: `/c "<command>"`.
#[must_use]
pub fn build_local_cmd(command_line: &str) -> String {
    format!("/c \"{command_line}\"")
}

/// `-EncodedCommand` arguments: the payload is UTF-16LE base64, so there is no quoting to
/// get wrong.
#[must_use]
pub fn build_pwsh_args(command_line: &str) -> String {
    let mut bytes = Vec::with_capacity(command_line.len() * 2);
    for unit in command_line.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    format!("-NonInteractive -NoLogo -ExecutionPolicy Bypass -EncodedCommand {}", base64_encode(&bytes))
}

/// Standard base64 with padding (RFC 4648), as `Convert.ToBase64String`.
#[must_use]
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = u32::from(chunk.first().copied().unwrap_or(0));
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let n = (b0 << 16) | (b1 << 8) | b2;
        let idx = |shift: u32| -> char {
            let i = ((n >> shift) & 0x3F) as usize;
            TABLE.get(i).copied().map_or('?', char::from)
        };
        out.push(idx(18));
        out.push(idx(12));
        out.push(if chunk.len() > 1 { idx(6) } else { '=' });
        out.push(if chunk.len() > 2 { idx(0) } else { '=' });
    }
    out
}

/// wsl.exe args for a run in the parent's distro. One implementation with the session
/// launcher so the two cannot disagree about quoting or a blank distro.
///
/// # Errors
/// [`LaunchError::MissingWslDistro`] when the distro is blank.
pub fn build_run_wsl_args(spec: &LaunchSpec, command_line: &str) -> Result<String, LaunchError> {
    build_wsl_args(spec, Some(command_line))
}

// ── PseudoTerminal.BuildCmdLine ─────────────────────────────────────────────────────────

/// Shells passed through verbatim (they initialise the console themselves).
const PASSTHROUGH_SHELLS: [&str; 10] = ["cmd", "powershell", "pwsh", "wsl", "bash", "zsh", "sh", "ssh", "nu", "fish"];

/// The command line handed to CreateProcess for a session.
///
/// Shells go through unchanged; anything else is wrapped in `<wrapper> -NoExit -Command ...`
/// so the shell sets up the Win32 console before the target runs (Electron/Node SEA apps such
/// as `claude.exe` crash with STATUS_DLL_INIT_FAILED when launched directly in a ConPTY) and
/// loads the user's PowerShell profile, so functions defined there resolve as bare commands.
#[must_use]
pub fn build_cmd_line(command: &str, full_user_cmd: &str, wrapper_shell: &str) -> String {
    let exe = winpath::file_name_without_extension(command).to_lowercase();
    if PASSTHROUGH_SHELLS.contains(&exe.as_str()) {
        return full_user_cmd.to_string();
    }
    format!("{wrapper_shell} -NoExit -Command {full_user_cmd}")
}

// ── PwshLocator decisions ───────────────────────────────────────────────────────────────

/// What a metadata lookup of a candidate executable found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    pub len: u64,
    pub is_reparse_point: bool,
}

/// The two I/O questions `PwshLocator.IsRunnable` asks. Implemented over the real file system
/// later; faked in tests.
pub trait RunnableProbe {
    /// `None` when the file does not exist or cannot be read.
    fn metadata(&self, path: &str) -> Option<FileMeta>;
    /// Runs the program with a trivial no-op and reports a clean exit.
    fn can_execute(&self, path: &str) -> bool;
}

/// True when `path` can actually be executed.
///
/// An ordinary executable (non-zero length, not a reparse point) is decided from metadata
/// alone, with no process spawned. A zero-byte reparse point is a Microsoft Store App
/// Execution Alias, which is *ambiguous*, not bad: a working Store PowerShell 7 looks exactly
/// like a stub left by an uninstalled app, so only that case is settled by probing.
/// Rejecting the shape outright silently downgraded Store-PS7 users to 5.1. An unreadable or
/// malformed path is not runnable: falling back to powershell.exe is always safe.
#[must_use]
pub fn is_runnable(path: &str, probe: &dyn RunnableProbe) -> bool {
    if blank(path) || path.contains('\0') {
        return false;
    }
    let Some(meta) = probe.metadata(path) else {
        return false;
    };
    if meta.len > 0 && !meta.is_reparse_point {
        return true;
    }
    probe.can_execute(path)
}

/// `"pwsh.exe"` when `where.exe pwsh.exe` succeeded and its first line is runnable, else
/// `"powershell.exe"`. `where_stdout` is `None` when `where.exe` was missing, hung or failed.
/// `where.exe` proves a *name* resolves, not that it runs (issue #104).
#[must_use]
pub fn choose_pwsh(where_stdout: Option<&str>, probe: &dyn RunnableProbe) -> &'static str {
    let first = where_stdout.and_then(|s| s.split('\n').next()).map_or("", str::trim);
    if where_stdout.is_some() && is_runnable(first, probe) { "pwsh.exe" } else { "powershell.exe" }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::argv::split_command_line;
    use std::cell::Cell;
    use std::collections::HashMap;

    fn split(cmd: &str) -> Vec<String> {
        split_command_line(cmd)
    }

    fn ssh(user: &str, host: &str, port: i32, folder: &str, command: &str) -> LaunchSpec {
        LaunchSpec {
            kind: SessionKind::Ssh,
            ssh_user: user.into(),
            ssh_host: host.into(),
            ssh_port: port,
            ssh_remote_folder: folder.into(),
            command: command.into(),
            ..LaunchSpec::default()
        }
    }

    fn wsl(distro: &str) -> LaunchSpec {
        LaunchSpec { kind: SessionKind::Wsl, wsl_distro: distro.into(), ..LaunchSpec::default() }
    }

    // ── ShellSessionTests: ssh ─────────────────────────────────────────────────────────

    #[test]
    fn ssh_default_port_omits_port_flag() {
        let s = ssh("alice", "dev.example.com", 22, "/home/alice/project", "bash");
        assert_eq!(build_ssh_args(&s).unwrap(), "-t alice@dev.example.com \"cd '/home/alice/project' && bash\"");
    }

    #[test]
    fn ssh_non_default_port_includes_port_flag() {
        let s = ssh("bob", "192.168.1.100", 2222, "", "pwsh");
        assert_eq!(build_ssh_args(&s).unwrap(), "-p 2222 -t bob@192.168.1.100 \"pwsh\"");
    }

    #[test]
    fn ssh_no_user_omits_at_sign() {
        let s = ssh("", "myserver", 22, "", "bash");
        assert_eq!(build_ssh_args(&s).unwrap(), "-t myserver \"bash\"");
    }

    #[test]
    fn ssh_no_remote_folder_omits_cd() {
        let s = ssh("ci", "build-server", 22, "", "bash");
        assert_eq!(build_ssh_args(&s).unwrap(), "-t ci@build-server \"bash\"");
    }

    #[test]
    fn full_command_line_remote_starts_with_ssh() {
        let s = ssh("alice", "dev.example.com", 22, "/proj", "bash");
        assert!(full_command_line(&s).starts_with("ssh "));
    }

    #[test]
    fn ssh_folder_with_spaces_produces_valid_command() {
        let s = ssh("alice", "dev.example.com", 22, "/home/alice/my project", "bash");
        assert_eq!(build_ssh_args(&s).unwrap(), "-t alice@dev.example.com \"cd '/home/alice/my project' && bash\"");
    }

    #[test]
    fn full_command_line_local_returns_command_and_args() {
        let s = LaunchSpec { command: "claude".into(), args: "--continue".into(), ..LaunchSpec::default() };
        assert_eq!(full_command_line(&s), "claude --continue");
    }

    #[test]
    fn ssh_empty_host_is_an_error() {
        let s = ssh("alice", "", 22, "", "bash");
        assert_eq!(build_ssh_args(&s), Err(LaunchError::MissingSshHost));
        assert_eq!(full_command_line(&s), "ssh");
    }

    #[test]
    fn ssh_blank_command_falls_back_to_bash_and_args_are_appended() {
        let mut s = ssh("", "h", 22, "", "  ");
        assert_eq!(build_ssh_args(&s).unwrap(), "-t h \"bash\"");
        s.command = "claude".into();
        s.args = "--continue".into();
        assert_eq!(build_ssh_args(&s).unwrap(), "-t h \"claude --continue\"");
    }

    // ── ShellSessionTests: wsl ─────────────────────────────────────────────────────────

    #[test]
    fn wsl_happy_path_builds_expected_shape() {
        let s = LaunchSpec { wsl_user: "alice".into(), wsl_working_folder: "/home/alice/proj".into(), command: "claude".into(), ..wsl("Ubuntu") };
        assert_eq!(build_wsl_args(&s, None).unwrap(), "-d Ubuntu -u alice --cd /home/alice/proj -e bash -lc \"claude\"");
    }

    #[test]
    fn wsl_no_user_omits_user_flag() {
        let s = LaunchSpec { wsl_working_folder: "/srv".into(), command: "bash".into(), ..wsl("Debian") };
        assert_eq!(build_wsl_args(&s, None).unwrap(), "-d Debian --cd /srv -e bash -lc \"bash\"");
    }

    #[test]
    fn wsl_no_working_folder_falls_back_to_home_tilde() {
        // "(optional)" means "your home directory". Omitting --cd would inherit the
        // launching Windows process's cwd (/mnt/c/...) on the slow 9p mount.
        let s = LaunchSpec { command: "bash".into(), ..wsl("Ubuntu") };
        assert_eq!(build_wsl_args(&s, None).unwrap(), "-d Ubuntu --cd ~ -e bash -lc \"bash\"");
    }

    #[test]
    fn wsl_home_tilde_is_unquoted_so_wsl_expands_it() {
        let args = build_wsl_args(&wsl("Ubuntu"), None).unwrap();
        assert!(args.contains(" --cd ~ "));
        assert!(!args.contains("--cd \"~\""));
    }

    #[test]
    fn wsl_uses_exec_flag_not_bare_dash_dash() {
        let s = LaunchSpec { command: "claude".into(), ..wsl("Ubuntu") };
        let args = build_wsl_args(&s, None).unwrap();
        assert!(args.contains(" -e "));
        // a substring check alone would not catch "--cd"
        assert!(!args.split(' ').any(|t| t == "--"));
    }

    #[test]
    fn wsl_resolved_shell_is_used_as_exec_shell() {
        let s = LaunchSpec { command: "ls".into(), resolved_wsl_shell: Some("sh".into()), ..wsl("docker-desktop") };
        assert_eq!(build_wsl_args(&s, None).unwrap(), "-d docker-desktop --cd ~ -e sh -lc \"ls\"");
    }

    #[test]
    fn wsl_resolved_shell_unset_defaults_to_bash() {
        let s = LaunchSpec { command: "ls".into(), ..wsl("Ubuntu") };
        assert_eq!(build_wsl_args(&s, None).unwrap(), "-d Ubuntu --cd ~ -e bash -lc \"ls\"");
    }

    #[test]
    fn wsl_empty_command_inner_payload_uses_resolved_shell_too() {
        // Otherwise an empty command box on an sh-only distro would emit `bash` as the
        // *inner* payload, failing identically to the outer bug.
        let s = LaunchSpec { command: String::new(), resolved_wsl_shell: Some("sh".into()), ..wsl("docker-desktop") };
        assert_eq!(build_wsl_args(&s, None).unwrap(), "-d docker-desktop --cd ~ -e sh -lc \"sh\"");
    }

    #[test]
    fn wsl_args_are_appended_to_the_command() {
        let s = LaunchSpec { command: "claude".into(), args: "--continue".into(), ..wsl("Ubuntu") };
        assert!(build_wsl_args(&s, None).unwrap().contains("bash -lc \"claude --continue\""));
    }

    #[test]
    fn wsl_empty_distro_is_an_error() {
        let s = LaunchSpec { command: "bash".into(), ..wsl("") };
        assert_eq!(build_wsl_args(&s, None), Err(LaunchError::MissingWslDistro));
        assert_eq!(full_command_line(&s), "wsl.exe");
    }

    #[test]
    fn full_command_line_wsl_starts_with_wsl_exe() {
        let s = LaunchSpec { command: "claude".into(), ..wsl("Ubuntu") };
        assert!(full_command_line(&s).starts_with("wsl.exe "));
    }

    #[test]
    fn wsl_linux_path_with_spaces_quotes_cd_value() {
        let s = LaunchSpec { wsl_working_folder: "/home/alice/my proj".into(), command: "claude".into(), ..wsl("Ubuntu") };
        assert_eq!(build_wsl_args(&s, None).unwrap(), "-d Ubuntu --cd \"/home/alice/my proj\" -e bash -lc \"claude\"");
    }

    // ── LaunchValidationError ──────────────────────────────────────────────────────────

    #[test]
    fn option_like_wsl_names_are_refused_by_validation_and_builders() {
        // Issue #123: quoting does not help, `wsl.exe -d "--help"` still reads an option.
        for (d, u) in [("--help", ""), ("-x", ""), (" -d", ""), ("Ubuntu", "--exec"), ("Ubuntu", "-u"), ("Ubu\ntu", "")] {
            let s = LaunchSpec { wsl_user: u.into(), ..wsl(d) };
            assert!(launch_validation_error(&s).is_some_and(|m| m.contains("option")), "{d:?}/{u:?}");
            assert_eq!(build_wsl_args(&s, None), Err(LaunchError::OptionLikeWslName), "{d:?}/{u:?}");
            assert_eq!(build_run_wsl_args(&s, "ls"), Err(LaunchError::OptionLikeWslName));
            assert_eq!(full_command_line(&s), "wsl.exe");
        }
        let ok = LaunchSpec { wsl_user: "a-lice".into(), ..wsl("Ubuntu-22.04") };
        assert!(launch_validation_error(&ok).is_none());
        assert!(build_wsl_args(&ok, None).is_ok());
    }

    #[test]
    fn option_like_ssh_names_are_refused_by_validation_and_builders() {
        for (u, h) in [("", "-oProxyCommand=calc"), ("-oProxyCommand=calc", "dev"), ("", " -v"), ("", "de\u{1b}v")] {
            let s = ssh(u, h, 22, "", "");
            assert!(launch_validation_error(&s).is_some_and(|m| m.contains("option")), "{u:?}/{h:?}");
            assert_eq!(build_ssh_args(&s), Err(LaunchError::OptionLikeSshName));
            assert_eq!(build_run_ssh_args(&s, "ls"), Err(LaunchError::OptionLikeSshName));
        }
        assert!(build_ssh_args(&ssh("alice", "dev-box", 22, "", "")).is_ok());
    }

    #[test]
    fn launch_validation_local_is_none() {
        assert!(launch_validation_error(&LaunchSpec::default()).is_none());
    }

    #[test]
    fn launch_validation_ssh_blank_host_reports() {
        let s = LaunchSpec { kind: SessionKind::Ssh, ..LaunchSpec::default() };
        assert!(launch_validation_error(&s).unwrap().to_lowercase().contains("host"));
    }

    #[test]
    fn launch_validation_wsl_blank_distro_reports() {
        let msg = launch_validation_error(&wsl("")).unwrap();
        assert!(msg.to_lowercase().contains("distro"));
    }

    #[test]
    fn launch_validation_wsl_with_distro_is_none() {
        assert!(launch_validation_error(&wsl("Ubuntu")).is_none());
    }

    #[test]
    fn launch_validation_whitespace_only_counts_as_blank() {
        assert!(launch_validation_error(&wsl("  \t")).is_some());
    }

    // ── Win32CommandLineTests: builders through the tokenizer ──────────────────────────

    #[test]
    fn run_wsl_bash_payload_arrives_intact() {
        for command_line in ["cargo test", "echo \"hi\"", "sed -i 's/\\\"//g' f.txt", "cp -r /src /dst\\", "printf '%s\\n' \"$HOME\""] {
            let p = LaunchSpec { wsl_working_folder: "/home/a b".into(), ..wsl("Ubuntu") };
            let argv = split(&build_run_wsl_args(&p, command_line).unwrap());
            assert_eq!(argv, vec!["-d", "Ubuntu", "--cd", "/home/a b", "-e", "bash", "-lc", command_line]);
        }
    }

    #[test]
    fn wsl_distro_with_space_and_user_tokenizes() {
        let s = LaunchSpec {
            wsl_user: "alice".into(),
            wsl_working_folder: "/home/alice".into(),
            command: "claude".into(),
            args: "--prompt \"fix the \\\"foo\\\" bug\"".into(),
            ..wsl("My Distro")
        };
        assert_eq!(
            split(&build_wsl_args(&s, None).unwrap()),
            vec!["-d", "My Distro", "-u", "alice", "--cd", "/home/alice", "-e", "bash", "-lc", "claude --prompt \"fix the \\\"foo\\\" bug\""]
        );
    }

    #[test]
    fn run_wsl_blank_distro_errors() {
        assert_eq!(build_run_wsl_args(&wsl(""), "ls"), Err(LaunchError::MissingWslDistro));
    }

    // ── UntrustedInputTests: ssh remote folder and host ────────────────────────────────

    fn ssh_hostile(host: &str, folder: &str) -> LaunchSpec {
        ssh("", host, 22, folder, "bash")
    }

    #[test]
    fn ssh_remote_folder_is_single_quote_escaped() {
        for folder in [
            "/home/alice/proj",
            "/home/alice/my proj",
            "/x'; id; :'", // closes the quote and runs `id` on the remote host
            "/x'",
            "/it's/here", // legitimate path that also used to break the command
        ] {
            let args = build_ssh_args(&ssh_hostile("example.invalid", folder)).unwrap();
            assert!(args.contains(&format!("cd {}", posix_single_quote(folder))), "{folder}");
            if folder.contains('\'') {
                assert!(!args.contains(&format!("cd '{folder}' &&")));
            }
        }
    }

    #[test]
    fn the_whole_remote_command_stays_one_windows_argument() {
        // A `"` is the escape PosixSingleQuote does NOT handle: before the outer quoting it
        // terminated the hand-written wrapper, and ProxyCommand runs LOCALLY.
        for folder in ["/t\" -oProxyCommand=calc \"x", "/t\" -oPermitLocalCommand=yes \"x", "/plain"] {
            let argv = split(&build_ssh_args(&ssh_hostile("example.invalid", folder)).unwrap());
            assert_eq!(argv.len(), 3, "{folder}");
            assert_eq!(argv[0], "-t");
            assert_eq!(argv[1], "example.invalid");
            assert!(argv[2].starts_with("cd "));
            assert!(!argv.iter().any(|a| a.starts_with("-o")));
        }
    }

    #[test]
    fn a_hostile_host_stays_one_windows_argument() {
        for host in ["h -oProxyCommand=calc", "h\" -oProxyCommand=calc \"x"] {
            let argv = split(&build_ssh_args(&ssh_hostile(host, "")).unwrap());
            assert_eq!(argv.len(), 3);
            assert_eq!(argv[1], host);
            assert!(!argv.iter().any(|a| a.starts_with("-o")));
        }
    }

    #[test]
    fn both_escaping_layers_nest_correctly() {
        // POSIX single-quoting inside, Windows argv quoting around the whole of it. These
        // values exercise both: a quote for the inner layer, a double quote and a trailing
        // backslash for the outer one (MSVCRT's 2n / 2n+1 backslash rules).
        for folder in ["/it's/here", "/a\"b", "/a'b\"c", "/trailing\\", "/a'b\"c\\", "/x'; id; :'"] {
            let argv = split(&build_ssh_args(&ssh_hostile("example.invalid", folder)).unwrap());
            assert_eq!(argv.len(), 3, "{folder}");
            assert_eq!(argv[1], "example.invalid");
            assert_eq!(argv[2], format!("cd {} && bash", posix_single_quote(folder)));
        }
    }

    #[test]
    fn hostile_user_and_command_stay_inside_their_elements() {
        let s = ssh("al ice\" -oProxyCommand=x", "h", 2222, "/p", "bash -c \"echo \\\"x\\\"\"");
        let argv = split(&build_ssh_args(&s).unwrap());
        assert_eq!(argv.len(), 5);
        assert_eq!(&argv[..2], ["-p", "2222"]);
        assert_eq!(argv[3], "al ice\" -oProxyCommand=x@h");
        assert_eq!(argv[4], "cd '/p' && bash -c \"echo \\\"x\\\"\"");
    }

    // ── RunInstanceTests ───────────────────────────────────────────────────────────────

    #[test]
    fn local_cmd_wraps_for_cmd() {
        assert_eq!(build_local_cmd("dotnet test --filter X"), "/c \"dotnet test --filter X\"");
    }

    #[test]
    fn run_ssh_local_folder_builds_expected_shape() {
        let p = ssh("alice", "dev.example.com", 22, "/proj", "");
        assert_eq!(build_run_ssh_args(&p, "cargo test").unwrap(), "-t alice@dev.example.com \"cd '/proj' && bash -c 'cargo test'\"");
    }

    #[test]
    fn run_ssh_non_default_port_includes_port_flag() {
        let p = ssh("bob", "h", 2222, "", "");
        assert!(build_run_ssh_args(&p, "ls").unwrap().starts_with("-p 2222 "));
    }

    #[test]
    fn run_ssh_command_with_apostrophe_is_escaped() {
        let p = ssh("u", "h", 22, "/p", "");
        let args = build_run_ssh_args(&p, "echo it's me").unwrap();
        assert!(args.contains(r"bash -c 'echo it'\''s me'"));
    }

    #[test]
    fn run_ssh_stays_three_arguments_for_hostile_command_and_folder() {
        let p = ssh("u", "h", 22, "/a\" -oProxyCommand=calc \"", "");
        let argv = split(&build_run_ssh_args(&p, "echo \"$x\" 'y' \\").unwrap());
        assert_eq!(argv.len(), 3);
        assert!(!argv.iter().any(|a| a.starts_with("-o")));
    }

    #[test]
    fn run_ssh_blank_host_errors() {
        assert_eq!(build_run_ssh_args(&ssh("", "", 22, "", ""), "ls"), Err(LaunchError::MissingSshHost));
    }

    #[test]
    fn pwsh_args_round_trip_command_line_via_base64() {
        // -EncodedCommand expects UTF-16LE base64. Round-trip a string with tricky chars
        // ($env: would otherwise be eaten by cmd.exe parsing).
        let cmd = "Write-Host \"hi $env:USERNAME!\" | Out-Default";
        let args = build_pwsh_args(cmd);
        assert!(args.starts_with("-NonInteractive -NoLogo -ExecutionPolicy Bypass -EncodedCommand "));
        let b64 = args.rsplit(' ').next().unwrap();
        let bytes = base64_decode(b64);
        let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        assert_eq!(String::from_utf16(&units).unwrap(), cmd);
    }

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn pwsh_args_non_bmp_text_is_utf16_surrogates() {
        // U+1F600 is D83D DE00 in UTF-16LE: 3D D8 00 DE
        assert_eq!(base64_encode(&[0x3D, 0xD8, 0x00, 0xDE]), "PdgA3g==");
        assert!(build_pwsh_args("\u{1F600}").ends_with("PdgA3g=="));
    }

    fn base64_decode(s: &str) -> Vec<u8> {
        let table = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = Vec::new();
        let mut buf = 0u32;
        let mut bits = 0;
        for c in s.bytes().filter(|&c| c != b'=') {
            let v = table.iter().position(|&t| t == c).unwrap() as u32;
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push(((buf >> bits) & 0xFF) as u8);
            }
        }
        out
    }

    // ── PseudoTerminalBuildCmdLineTests ────────────────────────────────────────────────

    #[test]
    fn passthrough_shells_are_not_wrapped() {
        for shell in
            ["pwsh", "pwsh.exe", "PWSH.EXE", "powershell", "powershell.exe", "cmd", "cmd.exe", "ssh", "bash", "wsl", "zsh", "sh", "nu", "fish"]
        {
            let full = format!("{shell} --version");
            assert_eq!(build_cmd_line(shell, &full, "pwsh.exe"), full);
        }
    }

    #[test]
    fn passthrough_detection_uses_the_file_stem_of_a_path() {
        assert_eq!(build_cmd_line(r"C:\Windows\System32\cmd.exe", "x", "pwsh.exe"), "x");
    }

    #[test]
    fn non_shell_command_is_wrapped_in_pwsh() {
        assert_eq!(build_cmd_line("claudelocal", "claudelocal", "pwsh.exe"), "pwsh.exe -NoExit -Command claudelocal");
    }

    #[test]
    fn non_shell_command_falls_back_to_windows_powershell() {
        assert_eq!(build_cmd_line("claudelocal", "claudelocal", "powershell.exe"), "powershell.exe -NoExit -Command claudelocal");
    }

    #[test]
    fn non_shell_command_with_arguments_preserves_args() {
        assert_eq!(build_cmd_line("claude", "claude --resume abc123", "pwsh.exe"), "pwsh.exe -NoExit -Command claude --resume abc123");
    }

    #[test]
    fn powershell_function_name_is_wrapped_so_the_profile_loads() {
        assert_eq!(build_cmd_line("claudework", "claudework", "pwsh.exe"), "pwsh.exe -NoExit -Command claudework");
    }

    // ── PwshLocatorTests ───────────────────────────────────────────────────────────────

    #[derive(Default)]
    struct FakeFs {
        files: HashMap<String, FileMeta>,
        executable: HashMap<String, bool>,
        probes: Cell<u32>,
    }

    impl RunnableProbe for FakeFs {
        fn metadata(&self, path: &str) -> Option<FileMeta> {
            self.files.get(path).copied()
        }
        fn can_execute(&self, path: &str) -> bool {
            self.probes.set(self.probes.get() + 1);
            self.executable.get(path).copied().unwrap_or(false)
        }
    }

    fn fs_with(path: &str, len: u64, reparse: bool, executes: bool) -> FakeFs {
        let mut fs = FakeFs::default();
        fs.files.insert(path.into(), FileMeta { len, is_reparse_point: reparse });
        fs.executable.insert(path.into(), executes);
        fs
    }

    #[test]
    fn runnable_real_executable_is_accepted_without_a_probe() {
        let fs = fs_with("pwsh.exe", 4, false, false);
        assert!(is_runnable("pwsh.exe", &fs));
        assert_eq!(fs.probes.get(), 0, "the metadata path must not spawn anything");
    }

    #[test]
    fn runnable_zero_byte_stub_is_rejected_by_the_probe() {
        // The shape of a Store alias stub never backed by an install.
        let fs = fs_with("pwsh.exe", 0, false, false);
        assert!(!is_runnable("pwsh.exe", &fs));
        assert_eq!(fs.probes.get(), 1, "decided by the probe, not by the length check");
    }

    #[test]
    fn runnable_working_store_alias_is_accepted() {
        // Same shape as the dead stub (zero bytes, reparse point), but it executes.
        let fs = fs_with("pwsh.exe", 0, true, true);
        assert!(is_runnable("pwsh.exe", &fs));
        assert_eq!(fs.probes.get(), 1);
    }

    #[test]
    fn runnable_non_empty_reparse_point_is_probed_too() {
        let fs = fs_with("pwsh.exe", 10, true, false);
        assert!(!is_runnable("pwsh.exe", &fs));
        assert_eq!(fs.probes.get(), 1);
    }

    #[test]
    fn runnable_blank_path_is_rejected() {
        let fs = FakeFs::default();
        assert!(!is_runnable("", &fs));
        assert!(!is_runnable("   ", &fs));
    }

    #[test]
    fn runnable_missing_file_is_rejected() {
        assert!(!is_runnable("not-here.exe", &FakeFs::default()));
    }

    #[test]
    fn runnable_malformed_path_is_rejected_rather_than_panicking() {
        assert!(!is_runnable("C:\\a\0b", &fs_with("C:\\a\0b", 4, false, true)));
        assert!(!is_runnable("::::", &FakeFs::default()));
    }

    #[test]
    fn choose_pwsh_prefers_pwsh_when_where_finds_a_runnable_one() {
        let fs = fs_with(r"C:\Program Files\PowerShell\7\pwsh.exe", 9, false, false);
        let out = "C:\\Program Files\\PowerShell\\7\\pwsh.exe\r\nC:\\other\\pwsh.exe\r\n";
        assert_eq!(choose_pwsh(Some(out), &fs), "pwsh.exe");
    }

    #[test]
    fn choose_pwsh_falls_back_when_where_failed_or_the_hit_is_not_runnable() {
        let fs = fs_with("C:\\stub\\pwsh.exe", 0, true, false);
        assert_eq!(choose_pwsh(None, &fs), "powershell.exe");
        assert_eq!(choose_pwsh(Some("C:\\stub\\pwsh.exe\r\n"), &fs), "powershell.exe");
        assert_eq!(choose_pwsh(Some(""), &fs), "powershell.exe");
    }
}
