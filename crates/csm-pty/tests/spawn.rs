//! Spawns real processes on a real PTY. On Windows the test binary has no conpty.dll beside it,
//! so these run on the inbox ConPTY (the bundled path is exercised by the spike app).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::ffi::OsString;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use csm_pty::{PtySize, SpawnOptions, spawn};

fn argv(a: &[&str]) -> Vec<OsString> {
    a.iter().map(OsString::from).collect()
}

fn collect(pty: &Arc<dyn csm_pty::Pty>) -> Arc<Mutex<Vec<u8>>> {
    let out = Arc::new(Mutex::new(Vec::new()));
    let o = out.clone();
    pty.subscribe(Box::new(move |b| o.lock().unwrap().extend_from_slice(b)));
    out
}

fn wait_for(out: &Arc<Mutex<Vec<u8>>>, needle: &str, timeout: Duration) -> bool {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if String::from_utf8_lossy(&out.lock().unwrap()).contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

#[cfg(windows)]
#[test]
fn echo_runs_and_exit_code_is_reported() {
    let pty = spawn(SpawnOptions::new(argv(&["cmd", "/c", "echo csm-hello& exit 3"]), PtySize::new(100, 30))).unwrap();
    // Subscribe late on purpose: early output must still arrive (EarlyOutputBuffer).
    std::thread::sleep(Duration::from_millis(300));
    let out = collect(&pty);
    assert!(wait_for(&out, "csm-hello", Duration::from_secs(10)), "no output: {:?}", String::from_utf8_lossy(&out.lock().unwrap()));
    let (tx, rx) = mpsc::channel();
    pty.on_exit(Box::new(move |code| {
        let _ = tx.send(code);
    }));
    let code = rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert_eq!(code, Some(3));
    assert!(pty.has_exited());
}

#[cfg(windows)]
#[test]
fn input_reaches_the_child_and_env_and_cwd_apply() {
    let dir = std::env::temp_dir();
    let mut opts = SpawnOptions::new(argv(&["cmd", "/q", "/k"]), PtySize::new(120, 30));
    opts.cwd = Some(dir.clone());
    opts.env.push(("CSM_PTY_TEST".into(), Some("value with spaces".into())));
    let pty = spawn(opts).unwrap();
    let out = collect(&pty);
    pty.write(b"echo [%CSM_PTY_TEST%] & cd\r").unwrap();
    assert!(wait_for(&out, "[value with spaces]", Duration::from_secs(10)));
    let dir_text = dir.to_string_lossy().trim_end_matches('\\').to_string();
    assert!(wait_for(&out, &dir_text, Duration::from_secs(10)));
    pty.resize(PtySize::new(80, 20)).unwrap();
    pty.kill();
    let (tx, rx) = mpsc::channel();
    pty.on_exit(Box::new(move |c| {
        let _ = tx.send(c);
    }));
    assert!(rx.recv_timeout(Duration::from_secs(10)).is_ok());
}

/// Killing the PTY kills grandchildren too (Job Object), not just the direct child.
#[cfg(windows)]
#[test]
fn kill_takes_the_whole_tree() {
    // cmd → ping (a grandchild that would otherwise run for ~60s).
    let pty = spawn(SpawnOptions::new(argv(&["cmd", "/c", "ping -n 60 127.0.0.1"]), PtySize::new(100, 30))).unwrap();
    let out = collect(&pty);
    assert!(wait_for(&out, "127.0.0.1", Duration::from_secs(10)));
    let pid = pty.pid().unwrap();
    let grandchild = child_pids(pid);
    assert_eq!(grandchild.len(), 1, "expected exactly one ping.exe under cmd {pid}");
    drop(pty);
    // Both the direct child and the grandchild are gone.
    for victim in [pid, grandchild[0]] {
        let start = Instant::now();
        while alive(victim) {
            assert!(start.elapsed() < Duration::from_secs(10), "process {victim} still alive");
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[cfg(windows)]
fn child_pids(parent: u32) -> Vec<u32> {
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-CimInstance Win32_Process -Filter 'ParentProcessId={parent}' | Where-Object Name -eq 'PING.EXE').ProcessId"),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).split_whitespace().filter_map(|s| s.parse().ok()).collect()
}

#[cfg(windows)]
fn alive(pid: u32) -> bool {
    let list = std::process::Command::new("tasklist").args(["/FI", &format!("PID eq {pid}"), "/NH"]).output().unwrap();
    String::from_utf8_lossy(&list.stdout).contains(&format!(" {pid} "))
}

#[test]
fn empty_argv_is_an_error_not_a_panic() {
    assert!(spawn(SpawnOptions::new(Vec::new(), PtySize::new(80, 24))).is_err());
}
