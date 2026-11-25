//! Reopening remote windows at start: the setting, the tool that opens a window recording it, and
//! the engine's quit. The process spawner is a fake, so no test starts a process.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};

use common::{H, test_app};
use csm_engine::{Caller, HostInfo, PeerClientHost, RemoteWindows, WindowSpawner};
use serde_json::json;

#[derive(Default)]
struct Fake {
    started: Mutex<Vec<String>>,
    ended: Mutex<Vec<Box<dyn FnOnce() + Send>>>,
}

impl WindowSpawner for Fake {
    fn spawn(&self, id: &str, ended: Box<dyn FnOnce() + Send>) -> Result<(), String> {
        self.started.lock().unwrap().push(id.to_owned());
        self.ended.lock().unwrap().push(ended);
        Ok(())
    }
}

struct Hosts(Vec<&'static str>);

impl PeerClientHost for Hosts {
    fn status(&self) -> csm_engine::ClientPairStatus {
        csm_engine::ClientPairStatus::default()
    }
    fn pair_start(&self, _: &str, _: Option<&str>) -> Result<csm_engine::ClientPairStatus, csm_engine::ToolError> {
        Err(csm_engine::ToolError::Failed("no".into()))
    }
    fn pair_cancel(&self) -> csm_engine::ClientPairStatus {
        csm_engine::ClientPairStatus::default()
    }
    fn hosts(&self) -> Result<Vec<HostInfo>, csm_engine::ToolError> {
        Ok(self.0.iter().map(|id| HostInfo { host_id: (*id).into(), name: (*id).into(), address: "tcp://10.0.0.1:1".into(), paired_at: 1, last_connected: None }).collect())
    }
    fn forget(&self, _: &str) -> Result<bool, csm_engine::ToolError> {
        Ok(true)
    }
}

fn rig(hosts: Vec<&'static str>) -> (H, tempfile::TempDir, Arc<Fake>, Arc<RemoteWindows>) {
    let dir = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let rw = RemoteWindows::new(Some(dir.path().join(csm_engine::remote_windows::FILE)), fake.clone());
    let launcher = rw.clone();
    let rw2 = rw.clone();
    let h = H::start(test_app(), move |o| {
        o.peer_client = Arc::new(Hosts(hosts));
        o.remote_window_launcher = Arc::new(move |id: &str| launcher.open(id));
        o.remote_windows = rw2;
    });
    (h, dir, fake, rw)
}

fn on_disk(dir: &tempfile::TempDir) -> Vec<String> {
    csm_engine::remote_windows::parse(&std::fs::read_to_string(dir.path().join(csm_engine::remote_windows::FILE)).unwrap_or_default())
}

#[test]
fn the_setting_is_on_by_default_is_saved_and_only_the_person_changes_it() {
    let h = H::new();
    assert_eq!(h.ok("settings_get", json!({"key": "ReopenRemoteWindows"})).data["value"], true);
    h.ok("settings_set", json!({"key": "reopen_remote_windows", "value": false}));
    assert_eq!(h.ok("settings_get", json!({"key": "ReopenRemoteWindows"})).data["value"], false);
    let ext = Caller::External { client: "t".into(), session_id: None };
    let r = h.engine.call(ext, "settings_set", json!({"key": "ReopenRemoteWindows", "value": true}));
    assert!(matches!(r, Err(csm_engine::ToolError::Refused(_))), "{r:?}");
}

#[test]
fn opening_and_closing_a_window_through_the_tool_updates_the_remembered_set() {
    let (h, dir, fake, rw) = rig(vec!["a1", "b2"]);
    h.ok("remote_window_open", json!({"host_id": "a1"}));
    h.ok("remote_window_open", json!({"host_id": "b2"}));
    assert_eq!(on_disk(&dir), ["a1", "b2"]);
    // The person closes b2's window.
    let end = fake.ended.lock().unwrap().remove(1);
    end();
    assert_eq!(on_disk(&dir), ["a1"]);
    assert_eq!(rw.open_ids(), ["a1"]);
}

#[test]
fn a_quit_keeps_the_windows_that_were_open() {
    let (h, dir, fake, _rw) = rig(vec!["a1"]);
    h.ok("remote_window_open", json!({"host_id": "a1"}));
    h.ok("app_quit", json!({}));
    // The window's process ends with the app.
    let end = fake.ended.lock().unwrap().remove(0);
    end();
    assert_eq!(on_disk(&dir), ["a1"]);
}
