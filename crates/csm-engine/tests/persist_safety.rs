//! Persistence fails loudly and safely (COR-5, COR-6, COR-36): a save that cannot be written
//! shows a standing warning until one works, a `state.json` that is there but cannot be read
//! starts the app read-only instead of being moved aside and replaced, and a first run of the
//! stable app offers a beta's sessions. Temp folders only.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::time::{Duration, Instant};

use common::{H, test_app};
use csm_core::models::ShellSession;
use csm_engine::{Caller, Engine, State, persist};
use serde_json::json;

fn wait(what: &str, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn warning_ids(e: &Engine) -> Vec<String> {
    e.view().warnings.iter().map(|w| w.id.clone()).collect()
}

#[test]
fn a_failing_save_shows_a_warning_until_a_save_works() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    // The saver writes `state.json.tmp` first: a folder there makes every save fail, as a full
    // disk does.
    let tmp = dir.path().join("state.json.tmp");
    std::fs::create_dir(&tmp).unwrap();
    let ptys = Default::default();
    let mut o = common::fast_options(&ptys);
    o.save_path = Some(path.clone());
    let (state, _) = State::load(o);
    let e = Engine::start(state, vec![]).unwrap();
    assert!(warning_ids(&e).is_empty());
    e.call(Caller::Ui, "group_new", json!({"name": "G"})).unwrap();
    wait("the save warning", || warning_ids(&e).contains(&"state_save_failed".to_string()));
    let text = e.view().warnings.iter().find(|w| w.id == "state_save_failed").unwrap().text.clone();
    assert!(text.contains("NOT being saved"), "{text}");
    assert!(!path.exists());
    // The disk is fixed: the saver's own retry writes the state and the warning goes.
    std::fs::remove_dir(&tmp).unwrap();
    wait("the warning to go", || warning_ids(&e).is_empty());
    wait("the state file", || path.is_file());
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"G\""));
    e.shutdown();
}

#[test]
fn an_unreadable_state_file_starts_read_only_and_is_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    // A folder where the file should be: present, but reading it is an I/O error, not bad JSON.
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("keep.txt"), "mine").unwrap();
    let ptys = Default::default();
    let mut o = common::fast_options(&ptys);
    o.save_path = Some(path.clone());
    let (state, report) = State::load(o);
    assert!(report.set_aside.is_empty(), "nothing is moved aside: {:?}", report.set_aside);
    assert!(state.options().read_only.is_some());
    let e = Engine::start(state, vec![]).unwrap();
    assert_eq!(warning_ids(&e), vec!["state_read_only".to_string()]);
    e.call(Caller::Ui, "group_new", json!({"name": "G"})).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    e.shutdown();
    assert!(path.is_dir() && path.join("keep.txt").is_file(), "the real file is untouched");
    assert!(!dir.path().join("state.json.bak").exists() && !dir.path().join("state.json.tmp").exists());
}

#[test]
fn a_corrupt_state_file_is_still_a_parse_error_not_a_read_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    std::fs::write(&path, "{\"Sessions\": [").unwrap();
    let out = persist::load_checked(&path);
    assert!(out.unreadable.is_none());
    assert_eq!(out.loaded.source, csm_core::state::LoadSource::Unreadable);
    // Missing is a first run, as before.
    assert!(persist::load_checked(&dir.path().join("none.json")).unreadable.is_none());
}

#[test]
fn the_first_run_of_a_stable_build_offers_a_betas_sessions() {
    let dir = tempfile::tempdir().unwrap();
    let beta = dir.path().join("state.json");
    let mut b = test_app();
    b.sessions.push(ShellSession { id: "from-beta".into(), name: "Beta work".into(), is_dormant: true, ..Default::default() });
    std::fs::write(&beta, b.to_json().unwrap()).unwrap();
    let h = H::new();
    h.engine.offer_beta_import(beta.clone());
    let v = h.wait_until("the offer", |v| v.warnings.iter().any(|w| w.id == "beta_import"));
    let w = v.warnings.iter().find(|w| w.id == "beta_import").unwrap();
    assert_eq!(w.action.as_ref().unwrap().tool, "state_import_beta");
    // An MCP client may not import.
    let ext = Caller::External { client: "an-agent".into(), session_id: None };
    assert!(h.engine.call(ext, "state_import_beta", json!({})).is_err());
    let out = h.ok("state_import_beta", json!({}));
    assert_eq!(out.data["sessions"], 1);
    let v = h.wait_until("the offer to go", |v| v.warnings.is_empty());
    assert!(v.sessions.iter().any(|s| s.id == "from-beta"));
    // The beta's file is only read.
    assert_eq!(std::fs::read_to_string(&beta).unwrap(), b.to_json().unwrap());
}

#[test]
fn the_beta_offer_can_be_dismissed() {
    let dir = tempfile::tempdir().unwrap();
    let beta = dir.path().join("state.json");
    std::fs::write(&beta, test_app().to_json().unwrap()).unwrap();
    let h = H::new();
    h.engine.offer_beta_import(beta);
    h.wait_until("the offer", |v| !v.warnings.is_empty());
    h.ok("state_import_beta", json!({"dismiss": true}));
    h.wait_until("the offer to go", |v| v.warnings.is_empty());
}

#[test]
fn an_old_api_key_is_removed_from_the_file_its_backup_and_the_snapshots_and_never_written_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut app = test_app();
    app.settings.anthropic_api_key = "sk-ant-plaintext".into();
    app.sessions.push(ShellSession { id: "keep".into(), is_dormant: true, ..Default::default() });
    let text = app.to_json().unwrap();
    assert!(text.contains("sk-ant-plaintext"));
    std::fs::write(&path, &text).unwrap();
    std::fs::write(persist::backup_path(&path), &text).unwrap();
    let snaps = persist::snapshots_dir(&path);
    std::fs::create_dir_all(&snaps).unwrap();
    std::fs::write(snaps.join(csm_core::state::snapshot_file_name(1_800_000_000_000)), &text).unwrap();
    let ptys = Default::default();
    let mut o = common::fast_options(&ptys);
    o.save_path = Some(path.clone());
    let (state, _) = State::load(o);
    assert!(state.app.settings.anthropic_api_key.is_empty());
    let e = Engine::start(state, vec![]).unwrap();
    e.call(Caller::Ui, "group_new", json!({"name": "G"})).unwrap();
    e.shutdown();
    let mut files = vec![path.clone(), persist::backup_path(&path)];
    files.extend(persist::list_snapshots(&path).into_iter().map(|(_, p)| p));
    assert!(files.len() >= 3);
    for f in files {
        let t = std::fs::read_to_string(&f).unwrap();
        assert!(!t.contains("sk-ant-plaintext"), "{} still holds the key", f.display());
    }
    assert!(std::fs::read_to_string(&path).unwrap().contains("\"keep\""), "the sessions survive");
}

#[test]
fn a_state_file_from_a_newer_app_is_refused_read_only_and_never_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut app = test_app();
    app.sessions.push(ShellSession { id: "future".into(), name: "From the future".into(), is_dormant: true, ..Default::default() });
    app.csm_rs_version = Some(persist::STATE_FILE_VERSION + 1);
    let text = app.to_json().unwrap();
    std::fs::write(&path, &text).unwrap();
    let ptys = Default::default();
    let mut o = common::fast_options(&ptys);
    o.save_path = Some(path.clone());
    let (state, report) = State::load(o);
    assert!(report.set_aside.is_empty(), "{:?}", report.set_aside);
    assert!(state.options().read_only.is_some());
    let e = Engine::start(state, vec![]).unwrap();
    assert_eq!(warning_ids(&e), vec!["state_newer".to_string()]);
    assert!(e.view().warnings[0].text.contains("newer CodeShellManager"));
    e.call(Caller::Ui, "group_new", json!({"name": "G"})).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    e.shutdown();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), text, "the newer file is untouched");
    // The current version and an unversioned (WPF) file load as before.
    assert!(persist::newer_state_reason(Some(persist::STATE_FILE_VERSION)).is_none());
    assert!(persist::newer_state_reason(None).is_none());
}
