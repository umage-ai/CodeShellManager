//! One smoke test on a real PTY (ConPTY on Windows, openpty elsewhere): a session runs a
//! command, its output reaches the terminal, its exit is seen.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::{Duration, Instant};

use csm_core::models::AppState;
use csm_engine::{Caller, Engine, State, StateOptions, Status};
use serde_json::json;

#[test]
fn a_real_command_runs_prints_and_exits() {
    let mut app = AppState::default();
    app.settings.claude_launch_stagger_ms = 0;
    let opts = StateOptions { first_size_timeout: Duration::from_millis(50), ..StateOptions::default() };
    let engine = Engine::start(State::new(app, opts), vec![]).unwrap();
    let dir = std::env::temp_dir().to_string_lossy().into_owned();
    let (command, args) = if cfg!(windows) { ("cmd", "/c echo hi from csm") } else { ("sh", "-c 'echo hi from csm'") };
    let out = engine.call(Caller::Ui, "session_new", json!({"folder": dir, "command": command, "args": args})).unwrap();
    let id = out.data["id"].as_str().unwrap().to_owned();

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut screen = String::new();
    loop {
        if let Ok(o) = engine.call(Caller::Ui, "session_read_screen", json!({"session_id": id})) {
            screen = o.data["text"].as_str().unwrap_or_default().to_owned();
        }
        let exited = engine.view().session(&id).is_some_and(|s| s.status == Status::Exited);
        if screen.contains("hi from csm") && exited {
            break;
        }
        assert!(Instant::now() < deadline, "no output/exit in time; screen: {screen:?}; view: {:?}", engine.view().session(&id));
        std::thread::sleep(Duration::from_millis(50));
    }
    let v = engine.view();
    let s = v.session(&id).unwrap();
    assert_eq!(s.exit_code, Some(0));
    assert!(s.backend.is_some());
    engine.call(Caller::Ui, "session_close", json!({"session_id": id})).unwrap();
    engine.shutdown();
}
