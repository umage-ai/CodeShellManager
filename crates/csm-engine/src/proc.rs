//! Short-lived helper processes (not PTYs): the one place the engine spawns them.
//!
//! Every such child is started with `CREATE_NO_WINDOW` on Windows, so a windowed app never
//! flashes a console for a probe, and with a deadline, a capped capture and a kill on timeout.
//! `cargo xtask policy` allows `std::process::Command` only here (and in `csm-pty`, the CLI's
//! setup code and tests). Blocking: call from a worker thread, never from the engine or UI thread.

use std::ffi::OsStr;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Most bytes of stdout kept from a probe.
pub const CAPTURE_CAP: usize = 1 << 20;

/// `CREATE_NO_WINDOW`. Not a no-op for console programs started from a GUI process.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Marks `cmd` to start without a console window (Windows); a no-op elsewhere.
pub fn no_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// How a probe ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    pub success: bool,
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
}

impl Captured {
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }
}

/// Runs `cmd` (no stdin, stderr discarded) and returns its stdout and exit status, or `None` when
/// it could not start or did not finish within `timeout` (it is then killed).
pub fn run_capture(cmd: &mut Command, timeout: Duration) -> Option<Captured> {
    no_window(cmd);
    let mut child = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
    let mut pipe = child.stdout.take()?;
    // Drained on its own thread: a child that fills the pipe must not stall behind try_wait.
    let reader = std::thread::Builder::new()
        .name("csm-proc-read".into())
        .spawn(move || {
            let mut kept = Vec::new();
            let mut buf = [0u8; 8192];
            while let Ok(n) = pipe.read(&mut buf) {
                if n == 0 {
                    break;
                }
                if kept.len() < CAPTURE_CAP {
                    kept.extend_from_slice(buf.get(..n).unwrap_or_default());
                }
            }
            kept.truncate(CAPTURE_CAP);
            kept
        })
        .ok();
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let stdout = reader.and_then(|r| r.join().ok()).unwrap_or_default();
    status.map(|s| Captured { success: s.success(), code: s.code(), stdout })
}

/// Runs `program` with `args` (argv, never a command line) under the rules of [`run_capture`].
pub fn capture<I, S>(program: impl AsRef<OsStr>, args: I, timeout: Duration) -> Option<Captured>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    run_capture(Command::new(program).args(args), timeout)
}

/// [`capture`], keeping stdout only when the program exited 0.
pub fn capture_ok<I, S>(program: impl AsRef<OsStr>, args: I, timeout: Duration) -> Option<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    capture(program, args, timeout).filter(|c| c.success).map(|c| c.text())
}

/// [`run_capture`], keeping stdout only when the program exited 0.
pub fn run_ok(cmd: &mut Command, timeout: Duration) -> Option<String> {
    run_capture(cmd, timeout).filter(|c| c.success).map(|c| c.text())
}

/// Starts a GUI program (an editor, the file manager, the browser opener) with argv: no shell,
/// no console window, no stdio, and a thread that reaps it so it never lingers as a zombie. Does
/// not wait for it. `raw` (Windows only) is appended verbatim after `args`, for a program with
/// its own argument parser (Explorer: a path with a comma must arrive quoted). Callers pass only
/// values they have validated.
pub fn spawn_detached(program: &std::path::Path, args: &[String], raw: Option<&str>) -> std::io::Result<()> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    no_window(&mut cmd);
    #[cfg(windows)]
    if let Some(r) = raw {
        use std::os::windows::process::CommandExt;
        cmd.raw_arg(r);
    }
    #[cfg(not(windows))]
    let _ = raw;
    let mut child = cmd.spawn()?;
    let _ = std::thread::Builder::new().name("csm-reap".into()).spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// Starts a process add-on (`plugins.rs`): `argv[0]` with `argv[1..]`, the working folder `dir`,
/// stdin, stdout and stderr piped, no console window (Windows). No shell is involved. An empty
/// `argv` is an error, not a panic.
pub fn spawn_piped(argv: &[String], dir: &std::path::Path) -> std::io::Result<std::process::Child> {
    let (program, args) = argv.split_first().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "the add-on has no argv"))?;
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    no_window(&mut cmd);
    cmd.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(windows)]
    fn captures_and_times_out() {
        let mut c = Command::new("cmd");
        c.args(["/c", "echo hi"]);
        assert_eq!(run_ok(&mut c, Duration::from_secs(10)).as_deref().map(str::trim), Some("hi"));
        let mut slow = Command::new("ping");
        slow.args(["-n", "30", "127.0.0.1"]);
        let started = Instant::now();
        assert!(run_capture(&mut slow, Duration::from_millis(300)).is_none());
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_program_is_none() {
        assert!(run_capture(&mut Command::new("csm-no-such-program-xyz"), Duration::from_secs(1)).is_none());
    }
}
