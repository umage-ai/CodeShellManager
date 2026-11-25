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

/// [`run_capture`] for `program` with the argv `args` (no shell).
pub fn capture_program(program: &str, args: &[String], timeout: Duration) -> Option<Captured> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    run_capture(&mut cmd, timeout)
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

/// Starts `program` with `args` (argv, no shell) for a long-lived helper the caller stops itself
/// (an ssh port forward, `port_forward.rs`): stdin and stdout are null, stderr is discarded, no
/// console window (Windows).
pub fn spawn_child(program: &str, args: &[String]) -> std::io::Result<std::process::Child> {
    let mut cmd = Command::new(program);
    cmd.args(args).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    no_window(&mut cmd);
    cmd.spawn()
}

/// Starts a process add-on (`plugins.rs`): `argv[0]` with `argv[1..]`, the working folder `dir`,
/// stdin, stdout and stderr piped, no console window (Windows). No shell is involved. An empty
/// `argv` is an error, not a panic.
///
/// A bare program name (`csm-review`) is looked up in the app's own folder first, where the
/// packages install the bundled add-on programs, then on PATH. Windows' CreateProcess already
/// searches the exe's folder first; Unix exec does not (a .app bundle or a tarball is not on PATH).
/// A relative path with a separator (`./bin/server`) is relative to the add-on's folder `dir`
/// (never to this process's working folder, which is where CreateProcess would look) and must
/// stay inside it. On Unix the child leads a process group of its own, so [`kill_tree`] reaches
/// the workers it starts.
pub fn spawn_piped(argv: &[String], dir: &std::path::Path) -> std::io::Result<std::process::Child> {
    let (program, args) = argv.split_first().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "the add-on has no argv"))?;
    let program = resolve_program(program, dir)?;
    let mut cmd = Command::new(program);
    cmd.args(args).current_dir(dir).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    no_window(&mut cmd);
    cmd.spawn()
}

/// The program an add-on's `argv[0]` names: a bare name is looked up beside the app, then left to
/// PATH; an absolute path is used as written; a relative path with a separator is joined to the
/// add-on's folder and refused when it leaves it (`../x`).
pub fn resolve_program(program: &str, dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    use std::path::{Component, Path, PathBuf};
    let path = Path::new(program);
    if path.is_absolute() || !program.contains(['/', '\\']) {
        return Ok(beside_app(program).unwrap_or_else(|| PathBuf::from(program)));
    }
    let mut kept: Vec<Component<'_>> = Vec::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::Normal(_) => kept.push(part),
            Component::ParentDir => {
                if kept.pop().is_none() {
                    return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("`{program}` leaves the add-on's folder")));
                }
            }
            // A drive-relative or rooted path (`C:foo`, `\foo`) is not inside the folder.
            Component::Prefix(_) | Component::RootDir => {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("`{program}` is not a path inside the add-on's folder")));
            }
        }
    }
    let mut out = dir.to_path_buf();
    out.extend(kept);
    Ok(out)
}

/// Stops `child` and everything it started. Windows: `taskkill /T /F` by full path (a Job Object
/// would need `unsafe`, which this crate forbids). Unix: SIGTERM to the child's process group,
/// SIGKILL after a moment. Falls back to killing just the child. The caller still `wait`s.
pub fn kill_tree(child: &mut std::process::Child) {
    let pid = child.id();
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("SystemRoot") {
            let exe = std::path::PathBuf::from(root).join("System32").join("taskkill.exe");
            let mut cmd = Command::new(exe);
            cmd.args(["/T", "/F", "/PID", &pid.to_string()]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            no_window(&mut cmd);
            let _ = cmd.status();
        }
    }
    #[cfg(unix)]
    {
        let kill = ["/bin/kill", "/usr/bin/kill"].into_iter().find(|p| std::path::Path::new(p).is_file());
        if let Some(kill) = kill {
            let group = format!("-{pid}");
            let signal = |sig: &str| {
                let _ = Command::new(kill).args([sig, "--", &group]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
            };
            signal("-TERM");
            let until = Instant::now() + Duration::from_millis(400);
            while Instant::now() < until && matches!(child.try_wait(), Ok(None)) {
                std::thread::sleep(Duration::from_millis(20));
            }
            // The group may outlive its leader: workers that ignored TERM get KILL.
            signal("-KILL");
        }
    }
    let _ = child.kill();
}

/// `<app folder>/<program><EXE_SUFFIX>` when `program` is a bare name and that file exists.
fn beside_app(program: &str) -> Option<std::path::PathBuf> {
    if program.is_empty() || program.contains(['/', '\\']) || program == "." || program == ".." {
        return None;
    }
    let path = std::env::current_exe().ok()?.parent()?.join(format!("{program}{}", std::env::consts::EXE_SUFFIX));
    path.is_file().then_some(path)
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

    #[test]
    fn a_relative_add_on_program_is_joined_to_its_folder_and_stays_inside() {
        let dir = std::path::Path::new("addons").join("x");
        assert_eq!(resolve_program("./bin/server.exe", &dir).unwrap(), dir.join("bin").join("server.exe"));
        assert_eq!(resolve_program("bin/a/../b.exe", &dir).unwrap(), dir.join("bin").join("b.exe"));
        assert!(resolve_program("../evil.exe", &dir).is_err());
        assert!(resolve_program("bin/../../evil.exe", &dir).is_err());
        // A bare name is not joined: PATH (or the app folder) finds it.
        assert_eq!(resolve_program("python", &dir).unwrap(), std::path::PathBuf::from("python"));
    }

    #[test]
    fn spawn_piped_runs_a_relative_program_from_the_add_on_folder() {
        let dir = std::env::temp_dir().join(format!("csm-relargv-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let name = format!("server{}", std::env::consts::EXE_SUFFIX);
        std::fs::copy(std::env::current_exe().unwrap(), bin.join(&name)).unwrap();
        // The test binary run with `--list` exits at once; it is found only through the folder.
        let argv = vec![format!("./bin/{name}"), "--list".to_string()];
        let mut child = spawn_piped(&argv, &dir).unwrap();
        let status = child.wait().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(status.success());
    }

    /// The program that outlives its parent's death unless the whole tree is killed: a shell that
    /// starts a long sleeper holding the same stdout pipe.
    #[test]
    fn kill_tree_stops_the_workers_too_so_the_pipe_closes() {
        let dir = std::env::temp_dir();
        #[cfg(windows)]
        let argv: Vec<String> = ["cmd", "/c", "ping -n 60 127.0.0.1"].iter().map(ToString::to_string).collect();
        #[cfg(not(windows))]
        let argv: Vec<String> = ["sh", "-c", "sleep 60; echo"].iter().map(ToString::to_string).collect();
        let mut child = spawn_piped(&argv, &dir).unwrap();
        let mut stdout = child.stdout.take().unwrap();
        std::thread::sleep(Duration::from_millis(500));
        kill_tree(&mut child);
        let _ = child.wait();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = stdout.read_to_end(&mut sink);
            let _ = tx.send(());
        });
        assert!(rx.recv_timeout(Duration::from_secs(15)).is_ok(), "a worker kept the pipe open: the tree was not killed");
    }
}
