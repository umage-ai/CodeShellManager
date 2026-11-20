//! Run commands end to end through the tools: a fake PTY for the lifecycle, one real
//! `cmd /c echo` (Windows) for the command line cmd.exe actually receives.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{FakePty, H, test_app};
use csm_core::models::{RunCommandItem, RunMode, ShellSession};
use csm_engine::{Caller, RunPhase, ToolError};
use csm_pty::SpawnOptions;
use serde_json::json;

/// What each spawn was asked for (session PTYs and run PTYs alike).
type Seen = Arc<Mutex<Vec<SpawnOptions>>>;

fn session(id: &str, folder: &str, runs: Vec<RunCommandItem>) -> ShellSession {
    ShellSession { id: id.into(), name: id.into(), command: "pwsh".into(), working_folder: folder.into(), run_commands: runs, ..Default::default() }
}

fn item(id: &str, label: &str, cmd: &str, default: bool) -> RunCommandItem {
    RunCommandItem { id: id.into(), label: label.into(), command_line: cmd.into(), is_default: default, mode: RunMode::Process, post_run_url: None }
}

/// An engine with one live session `s` (two run commands, `test` the default), recording spawns.
fn rig() -> (H, Seen) {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app();
    app.sessions.push(session(
        "s",
        &dir.path().display().to_string(),
        vec![item("build", "Build", "cargo build", false), item("test", "Test", "cargo test", true)],
    ));
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let h = H::start(app, move |o| {
        let inner = o.pty.clone();
        o.pty = Arc::new(move |opts: SpawnOptions| {
            s2.lock().unwrap().push(opts.clone());
            inner(opts)
        });
    });
    h.wait_until("the session is live", |v| v.session("s").is_some_and(|s| s.live));
    // A restart is refused while the start-up restore runs (by design: two loops starting Claude
    // at once race on its config), so the rig is ready only once the restore has finished.
    h.wait_until("the restore has finished", |v| v.restore.is_none());
    (h, seen)
}

/// The fake PTY of the n-th run spawn (session spawns have more than one argv element or
/// aren't `cmd.exe`/a shell with `-lc`).
fn run_pty(h: &H, nth: usize) -> Arc<FakePty> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let runs: Vec<Arc<FakePty>> = h.ptys.lock().unwrap().iter().filter(|p| is_run(&p.argv)).cloned().collect();
        if let Some(p) = runs.get(nth) {
            return p.clone();
        }
        assert!(Instant::now() < deadline, "no run PTY #{nth}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn is_run(argv: &[String]) -> bool {
    argv.first().is_some_and(|a| a == "cmd.exe") || argv.get(1).is_some_and(|a| a == "-lc")
}

fn phase(h: &H, run: &str) -> RunPhase {
    h.engine.view().session("s").and_then(|s| s.runs.iter().find(|r| r.id == run).map(|r| r.phase)).unwrap()
}

fn wait_phase(h: &H, run: &str, want: RunPhase) {
    h.wait_until(&format!("{run} {want:?}"), |v| v.session("s").and_then(|s| s.runs.iter().find(|r| r.id == run)).is_some_and(|r| r.phase == want));
}

#[test]
fn start_runs_the_default_on_the_inbox_conpty_and_records_the_exit() {
    let (h, seen) = rig();
    let out = h.ok("run_start", json!({"session_id": "s"}));
    assert_eq!(out.data["run_id"], "test", "the default");
    let pty = run_pty(&h, 0);
    wait_phase(&h, "test", RunPhase::Running);
    let opts =
        seen.lock().unwrap().iter().find(|o| is_run(&o.argv.iter().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>())).cloned().unwrap();
    assert!(!opts.allow_bundled, "runs stay on the inbox ConPTY");
    if cfg!(windows) {
        assert_eq!(opts.argv, ["cmd.exe"]);
        assert_eq!(opts.verbatim_args.as_deref().and_then(|v| v.to_str()), Some("/c \"cargo test\""));
    }
    // Output: ANSI stripped, readable through the tool and the snapshot's handle.
    pty.emit(b"\x1b[32mrunning 3 tests\x1b[0m\r\n");
    pty.exit(Some(0));
    wait_phase(&h, "test", RunPhase::Ok);
    let o = h.ok("run_output", json!({"session_id": "s"}));
    assert_eq!(o.data["output"], "running 3 tests\n");
    assert_eq!(o.data["exit_code"], 0);
    let v = h.engine.view();
    assert_eq!(v.run_output("s", "test").map(|r| r.text()).as_deref(), Some("running 3 tests\n"));
    let list = h.ok("run_list", json!({"session_id": "s"}));
    assert_eq!(list.data[1]["phase"], "ok");
    assert_eq!(list.data[0]["phase"], "idle");
    h.engine.shutdown();
}

#[test]
fn a_non_zero_exit_is_failed_and_a_stop_is_stopped() {
    let (h, _) = rig();
    h.ok("run_start", json!({"session_id": "s", "run_id": "build"}));
    run_pty(&h, 0).exit(Some(2));
    wait_phase(&h, "build", RunPhase::Failed);
    assert_eq!(h.engine.view().session("s").unwrap().runs[0].exit_code, Some(2));

    h.ok("run_start", json!({"session_id": "s", "run_id": "Test"}));
    let p = run_pty(&h, 1);
    wait_phase(&h, "test", RunPhase::Running);
    let o = h.ok("run_stop", json!({"session_id": "s", "run_id": "test"}));
    assert_eq!(o.data["was_running"], true);
    assert!(p.killed.load(std::sync::atomic::Ordering::SeqCst), "the tree is killed");
    wait_phase(&h, "test", RunPhase::Stopped);
    let again = h.ok("run_stop", json!({"session_id": "s", "run_id": "test"}));
    assert_eq!(again.data["was_running"], false);
    h.engine.shutdown();
}

#[test]
fn starting_a_running_command_restarts_it_and_ignores_the_old_exit() {
    let (h, _) = rig();
    h.ok("run_start", json!({"session_id": "s"}));
    let first = run_pty(&h, 0);
    wait_phase(&h, "test", RunPhase::Running);
    first.emit(b"old output\n");
    h.ok("run_start", json!({"session_id": "s"}));
    assert!(first.killed.load(std::sync::atomic::Ordering::SeqCst), "the previous run is killed");
    let second = run_pty(&h, 1);
    wait_phase(&h, "test", RunPhase::Running);
    second.emit(b"new\n");
    // The old run's exit (code 1 from the kill) must not mark the new one failed.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(phase(&h, "test"), RunPhase::Running);
    let o = h.ok("run_output", json!({"session_id": "s"}));
    assert_eq!(o.data["output"], "new\n", "a fresh buffer per run");
    second.exit(Some(0));
    wait_phase(&h, "test", RunPhase::Ok);
    h.engine.shutdown();
}

#[test]
fn output_is_capped_at_one_megabyte_and_max_chars_returns_the_tail() {
    let (h, _) = rig();
    h.ok("run_start", json!({"session_id": "s"}));
    let p = run_pty(&h, 0);
    let chunk = "y".repeat(4095) + "\n";
    for _ in 0..300 {
        p.emit(chunk.as_bytes());
    }
    p.emit(b"LAST LINE\n");
    let o = h.ok("run_output", json!({"session_id": "s", "max_chars": 2_000_000}));
    let text = o.data["output"].as_str().unwrap();
    assert!(text.len() <= 1_000_000, "{}", text.len());
    assert!(text.ends_with("LAST LINE\n"));
    let tail = h.ok("run_output", json!({"session_id": "s", "max_chars": 10}));
    assert_eq!(tail.data["output"], "LAST LINE\n");
    h.engine.shutdown();
}

#[test]
fn runs_die_with_their_session_and_need_it_awake() {
    let (h, _) = rig();
    h.ok("run_start", json!({"session_id": "s"}));
    let p = run_pty(&h, 0);
    wait_phase(&h, "test", RunPhase::Running);
    h.ok("session_sleep", json!({"session_id": "s"}));
    assert!(p.killed.load(std::sync::atomic::Ordering::SeqCst), "sleep kills the session's runs");
    let e = h.ui("run_start", json!({"session_id": "s"})).unwrap_err();
    assert!(matches!(e, ToolError::Refused(m) if m.contains("asleep")));
    let v = h.engine.view();
    assert_eq!(v.session("s").unwrap().runs[1].phase, RunPhase::Idle, "forgotten with the session");
    h.engine.shutdown();
}

#[test]
fn restart_and_close_kill_runs_too() {
    let (h, _) = rig();
    h.ok("run_start", json!({"session_id": "s"}));
    let p = run_pty(&h, 0);
    wait_phase(&h, "test", RunPhase::Running);
    let restarted = h.ui("session_restart", json!({"session_id": "s"}));
    assert!(killed_within(&p.killed, Duration::from_secs(10)), "restart kills the runs (restart replied {restarted:?})");
    h.wait_until("live again", |v| v.session("s").is_some_and(|s| s.live && s.status != csm_engine::Status::Starting));
    h.ok("run_start", json!({"session_id": "s"}));
    let p2 = run_pty(&h, 1);
    h.ok("session_close", json!({"session_id": "s", "force": true}));
    assert!(killed_within(&p2.killed, Duration::from_secs(10)), "close kills the runs");
    h.engine.shutdown();
}

#[test]
fn edit_validates_keeps_one_default_and_stops_removed_runs() {
    let (h, _) = rig();
    h.ok("run_start", json!({"session_id": "s", "run_id": "build"}));
    let p = run_pty(&h, 0);
    wait_phase(&h, "build", RunPhase::Running);
    let out = h.ok(
        "run_edit",
        json!({"session_id": "s", "runs": [
            {"id": "test", "label": "Test", "command_line": "cargo test", "is_default": true},
            {"label": "Dev", "command_line": "npm run dev", "is_default": true, "mode": "powershell", "post_run_url": "http://localhost:5173"},
        ]}),
    );
    let list = out.data.as_array().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list.iter().filter(|r| r["is_default"] == true).count(), 1);
    assert_eq!(list[1]["is_default"], true, "the last marked wins");
    assert_eq!(list[1]["mode"], "powershell");
    assert!(list[1]["id"].as_str().is_some_and(|i| !i.is_empty()));
    assert!(p.killed.load(std::sync::atomic::Ordering::SeqCst), "a removed command's run is stopped");

    for bad in [
        json!([{"label": "", "command_line": "x"}]),
        json!([{"label": "A", "command_line": "  "}]),
        json!([{"label": "A", "command_line": "echo a\necho b"}]),
        json!([{"label": "A", "command_line": "x", "post_run_url": "file:///C:/Windows/System32/calc.exe"}]),
        json!([{"label": "A", "command_line": "x", "post_run_url": "localhost:5173"}]),
        json!([{"label": "A", "command_line": "x", "mode": "bash"}]),
        json!([{"label": "A", "command_line": "x", "extra": 1}]),
    ] {
        let e = h.ui("run_edit", json!({"session_id": "s", "runs": bad.clone()})).unwrap_err();
        assert!(matches!(e, ToolError::InvalidParams(_)), "{bad}: {e:?}");
    }
    // MCP clients may not decide what F5 runs.
    let ext = Caller::External { client: "c".into(), session_id: None };
    let e = h.engine.call(ext, "run_edit", json!({"session_id": "s", "runs": []})).unwrap_err();
    assert!(matches!(e, ToolError::Refused(_)), "{e:?}");
    // The CLI is the person.
    assert!(h.engine.call(Caller::Cli, "run_edit", json!({"session_id": "s", "runs": []})).is_ok());
    let e = h.ui("run_start", json!({"session_id": "s"})).unwrap_err();
    assert!(matches!(e, ToolError::NotFound(_)));
    h.engine.shutdown();
}

#[test]
fn a_start_failure_is_a_failed_run_with_the_reason_in_its_output() {
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app();
    app.sessions.push(session("s", &dir.path().display().to_string(), vec![item("r", "Run", "x", true)]));
    let h = H::start(app, |o| {
        let inner = o.pty.clone();
        o.pty = Arc::new(move |opts: SpawnOptions| if opts.allow_bundled { inner(opts) } else { Err(std::io::Error::other("no such program")) });
    });
    h.wait_until("live", |v| v.session("s").is_some_and(|s| s.live));
    h.ok("run_start", json!({"session_id": "s"}));
    h.wait_until("failed", |v| v.session("s").is_some_and(|s| s.runs[0].phase == RunPhase::Failed));
    let o = h.ok("run_output", json!({"session_id": "s"}));
    assert!(o.data["output"].as_str().unwrap().contains("Cannot start"), "{}", o.data);
    assert_eq!(o.data["exit_code"], -1);
    h.engine.shutdown();
}

#[test]
fn new_sessions_are_seeded_from_the_project_type() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();
    let h = H::new();
    let out = h.ok("session_new", json!({"folder": dir.path().display().to_string(), "command": "pwsh"}));
    let id = out.data["id"].as_str().unwrap().to_owned();
    let v = h.wait_until("seeded", |v| v.session(&id).is_some_and(|s| !s.runs.is_empty()));
    let runs = &v.session(&id).unwrap().runs;
    assert_eq!(runs.iter().map(|r| r.command_line.as_str()).collect::<Vec<_>>(), ["cargo run", "cargo build", "cargo test", "cargo clippy"]);
    assert!(runs[0].is_default);
    // `detect: true` re-applies the template on demand.
    h.ok("run_edit", json!({"session_id": id, "runs": []}));
    let r = h.ok("run_edit", json!({"session_id": id, "detect": true}));
    assert_eq!(r.data["template"], "cargo");
    h.engine.shutdown();
}

#[test]
#[cfg(windows)]
fn a_real_cmd_echo_run_reaches_cmd_verbatim() {
    use csm_engine::{Engine, State, StateOptions};
    let dir = tempfile::tempdir().unwrap();
    let mut app = test_app();
    // Quotes, &&, and a %VAR% that cmd must expand: the whole line reaches cmd.exe unchanged.
    app.sessions.push(ShellSession {
        id: "s".into(),
        command: "cmd".into(),
        args: "/k".into(),
        working_folder: dir.path().display().to_string(),
        run_commands: vec![item("r", "Echo", r#"echo "a b" && echo csm-%CSM_SESSION_ID%-done"#, true)],
        ..Default::default()
    });
    let opts = StateOptions { first_size_timeout: Duration::from_millis(30), restore_on_start: true, ..StateOptions::default() };
    let e = Engine::start(State::new(app, opts), vec![]).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while !e.view().session("s").is_some_and(|s| s.live) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(20));
    }
    e.call(Caller::Ui, "run_start", json!({"session_id": "s"})).unwrap();
    let mut last = serde_json::Value::Null;
    while Instant::now() < deadline {
        last = e.call(Caller::Ui, "run_output", json!({"session_id": "s"})).unwrap().data;
        // The reader can still be draining after the exit is seen: wait for the last line.
        let done = last["phase"] == "ok" || last["phase"] == "failed";
        if done && (last["output"].as_str().is_some_and(|t| t.contains("-done")) || last["phase"] == "failed") {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(last["phase"], "ok", "{last}");
    let text = last["output"].as_str().unwrap();
    assert!(text.contains("\"a b\""), "{text:?}");
    assert!(text.contains("csm-s-done"), "{text:?}");
    e.shutdown();
}

/// The kill happens during teardown; on a slow runner give it a bounded moment rather than
/// reading the flag the instant the tool replied.
fn killed_within(flag: &std::sync::atomic::AtomicBool, limit: Duration) -> bool {
    let start = std::time::Instant::now();
    while !flag.load(std::sync::atomic::Ordering::SeqCst) {
        if start.elapsed() > limit {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    true
}
