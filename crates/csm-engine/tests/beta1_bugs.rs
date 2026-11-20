//! Beta.1 fixes through the real engine: the session environment (#162), option-like WSL/SSH
//! names (#123) and state snapshots / unreadable state files (#166).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use common::{H, test_app};
use csm_core::models::{SessionKind, ShellSession};
use csm_engine::{Caller, State, persist};
use serde_json::json;

#[test]
fn sessions_do_not_inherit_the_launching_terminals_session_markers() {
    let h = H::new();
    h.session("pwsh");
    let env = h.pty(0).env.clone();
    let set = |k: &str| env.iter().rev().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    assert_eq!(set("CSM_ENV"), Some(Some("1".into())));
    assert_eq!(set("TERM_PROGRAM"), Some(Some("CodeShellManager".into())));
    assert!(set("CSM_SESSION_ID").flatten().is_some());
    // Every marker present in this test's own environment (e.g. run from a Claude Code
    // terminal) is removed.
    for (k, _) in std::env::vars_os() {
        let k = k.to_string_lossy().into_owned();
        if csm_core::session_env::is_stripped(&k) {
            assert_eq!(set(&k), Some(None), "{k} must be removed");
        }
    }
}

#[test]
fn an_option_like_wsl_distro_from_the_state_file_is_refused_at_launch() {
    let mut app = test_app();
    for (id, kind, distro, host) in [("w", SessionKind::Wsl, "--help", ""), ("s", SessionKind::Ssh, "", "-oProxyCommand=calc")] {
        app.sessions.push(ShellSession {
            id: id.into(),
            kind,
            wsl_distro: distro.into(),
            ssh_host: host.into(),
            is_dormant: true,
            ..Default::default()
        });
    }
    let h = H::start(app, |_| {});
    for id in ["w", "s"] {
        let err = h.ui("session_wake", json!({"session_id": id})).unwrap_err();
        assert!(format!("{err:?}").contains("option"), "{id}: {err:?}");
    }
    assert_eq!(h.pty_count(), 0, "nothing may be spawned");
    // The tools refuse such names up front, too.
    let folder = h.dir.path().to_string_lossy().into_owned();
    let err = h.ui("session_new", json!({"folder": folder, "kind": "ssh", "ssh": {"host": "-oProxyCommand=calc"}})).unwrap_err();
    assert!(format!("{err:?}").contains("'-'"), "{err:?}");
}

#[test]
fn an_unreadable_state_file_is_moved_aside_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, "{\"Sessions\": [").unwrap();
    std::fs::write(persist::backup_path(&path), "also torn").unwrap();
    let ptys = Default::default();
    let mut o = common::fast_options(&ptys);
    o.save_path = Some(path.clone());
    // --clean never touches the files.
    let mut clean = o.clone();
    clean.clean = true;
    let (_, report) = State::load(clean);
    assert!(report.set_aside.is_empty());
    assert!(path.is_file());
    // A normal start keeps both where no save can reach them, and says so.
    let (state, report) = State::load(o);
    assert_eq!(report.source, Some(csm_core::state::LoadSource::Unreadable));
    assert_eq!(report.set_aside.len(), 2, "{:?}", report.set_aside);
    assert!(report.set_aside.iter().all(|p| p.is_file()));
    assert_eq!(std::fs::read_to_string(&report.set_aside[0]).unwrap(), "{\"Sessions\": [");
    let e = csm_engine::Engine::start(state, vec![]).unwrap();
    e.call(Caller::Ui, "group_new", json!({"name": "G"})).unwrap();
    e.shutdown();
    assert!(path.is_file());
    assert!(report.set_aside.iter().all(|p| p.is_file()), "a save must not remove the kept copies");
}

#[test]
fn the_saver_keeps_a_snapshot_and_the_tool_lists_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, test_app().to_json().unwrap()).unwrap();
    let ptys = Default::default();
    let mut o = common::fast_options(&ptys);
    o.save_path = Some(path.clone());
    let (state, _) = State::load(o);
    let e = csm_engine::Engine::start(state, vec![]).unwrap();
    e.call(Caller::Ui, "group_new", json!({"name": "G"})).unwrap();
    // The save is debounced and written on the saver thread.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while persist::list_snapshots(&path).is_empty() {
        assert!(std::time::Instant::now() < deadline, "no snapshot was taken");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let out = e.call(Caller::Ui, "state_snapshots", json!({})).unwrap();
    let list = out.data["snapshots"].as_array().unwrap();
    assert_eq!(list.len(), 1, "{}", out.data);
    // The snapshot is the generation before the first save of this run.
    let file = list[0]["file"].as_str().unwrap();
    assert_eq!(std::fs::read_to_string(file).unwrap(), test_app().to_json().unwrap());
    e.shutdown();
}
