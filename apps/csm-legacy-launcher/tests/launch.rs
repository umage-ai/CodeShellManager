//! The launcher end to end: the real `CodeShellManager` binary, copied into a folder beside a
//! stand-in `codeshell.exe` (this test binary itself, which, started with `CSM_LAUNCHER_ECHO`,
//! writes the arguments it received to that file and exits). The arguments must arrive exactly
//! as given: spaces, quotes, backslashes, empty ones, cmd metacharacters.
//!
//! `harness = false`: this file's `main` is both the test runner and the stand-in.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const ECHO: &str = "CSM_LAUNCHER_ECHO";
/// Separates the arguments in the echo file (cannot occur in a command-line argument).
const SEP: char = '\u{1}';

fn main() {
    if let Some(out) = std::env::var_os(ECHO) {
        let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
        let text: String = args.iter().map(|a| format!("{a}{SEP}")).collect();
        // Written then renamed, so the test never reads half a file.
        let tmp = PathBuf::from(&out).with_extension("tmp");
        std::fs::write(&tmp, text).unwrap();
        std::fs::rename(&tmp, &out).unwrap();
        return;
    }
    arguments_pass_through_unchanged();
    no_arguments_is_no_arguments();
    the_app_beside_the_launcher_is_started_without_path();
    println!("test result: ok. 3 passed (launch)");
}

/// A folder with the launcher and the stand-in app.
fn install() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let launcher = dir.path().join(format!("CodeShellManager{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(env!("CARGO_BIN_EXE_CodeShellManager"), &launcher).unwrap();
    std::fs::copy(std::env::current_exe().unwrap(), dir.path().join(csm_legacy_launcher::target_name())).unwrap();
    (dir, launcher)
}

/// Runs the launcher with `args` and returns what the stand-in app received.
fn run(dir: &Path, launcher: &Path, args: &[&str], path_env: Option<&Path>) -> Vec<String> {
    let out = dir.join("received.txt");
    let mut cmd = Command::new(launcher);
    cmd.args(args).env(ECHO, &out);
    if let Some(p) = path_env {
        cmd.env("PATH", p);
    }
    let status = cmd.status().unwrap();
    assert!(status.success(), "the launcher exited with {status}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(text) = std::fs::read_to_string(&out) {
            return text.split_terminator(SEP).map(str::to_owned).collect();
        }
        assert!(Instant::now() < deadline, "the app was never started");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn arguments_pass_through_unchanged() {
    let (dir, launcher) = install();
    let args = [
        "--config-dir",
        r"C:\Users\Some One\My Data",
        "a \"quoted\" word",
        "\"",
        r"trailing\",
        r#"back\\"slash"#,
        r"C:\path with space\",
        "",
        "&|<>^%PATH% !x!",
        "--font-size",
        "14",
    ];
    assert_eq!(run(dir.path(), &launcher, &args, None), args, "argv arrives unchanged");
    println!("arguments_pass_through_unchanged ... ok");
}

fn no_arguments_is_no_arguments() {
    let (dir, launcher) = install();
    assert!(run(dir.path(), &launcher, &[], None).is_empty());
    println!("no_arguments_is_no_arguments ... ok");
}

fn the_app_beside_the_launcher_is_started_without_path() {
    // PATH points at an empty folder: the launcher must not need it.
    let (dir, launcher) = install();
    let empty = tempfile::tempdir().unwrap();
    assert_eq!(run(dir.path(), &launcher, &["x"], Some(empty.path())), ["x"]);
    println!("the_app_beside_the_launcher_is_started_without_path ... ok");
}
