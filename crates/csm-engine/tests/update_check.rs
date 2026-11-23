//! The update check (`update_check`, `update_dismiss`, `CheckForUpdates`) through the engine, with
//! a fake GitHub: no test touches the network.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{H, test_app};
use csm_core::update::{LATEST_URL, LIST_URL};
use csm_engine::{Caller, ToolError, UpdateOptions};
use serde_json::json;

const LATEST_210: &str = r#"{"tag_name": "v2.1.0", "html_url": "https://evil.example/", "draft": false, "prerelease": false}"#;

/// A fake GitHub answering `body`, and the URLs it was asked for.
fn fake(body: &'static str) -> (csm_engine::UpdateFetch, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let log = asked.clone();
    let f: csm_engine::UpdateFetch = Arc::new(move |url: &str| {
        log.lock().unwrap().push(url.to_owned());
        Ok(body.to_owned())
    });
    (f, asked)
}

fn engine(current: &str, body: &'static str, tweak: impl FnOnce(&mut csm_engine::StateOptions)) -> (H, Arc<Mutex<Vec<String>>>) {
    let (fetch, asked) = fake(body);
    let current = current.to_owned();
    let h = H::start(test_app(), move |o| {
        o.update = UpdateOptions { fetch: Some(fetch), current, ..UpdateOptions::default() };
        tweak(o);
    });
    (h, asked)
}

#[test]
fn a_check_finds_a_newer_release_and_the_badge_links_to_its_page() {
    let (h, asked) = engine("2.0.0", LATEST_210, |_| {});
    assert!(h.engine.view().update.is_none(), "nothing before a check");
    let out = h.ok("update_check", json!({}));
    assert_eq!(out.data["update_available"], true);
    assert_eq!(out.data["current"], "2.0.0");
    assert_eq!(out.data["latest"], "2.1.0");
    assert_eq!(out.data["feed"], "latest");
    // Built from the tag: the payload's html_url is never used.
    assert_eq!(out.data["url"], "https://github.com/umage-ai/CodeShellManager/releases/tag/v2.1.0");
    assert_eq!(out.message, "CodeShellManager 2.1.0 is available (this is 2.0.0).");
    assert_eq!(*asked.lock().unwrap(), vec![LATEST_URL.to_owned()]);
    let badge = h.engine.view().update.clone().expect("the badge");
    assert_eq!(badge.version, "2.1.0");
    assert_eq!(badge.current, "2.0.0");
    assert!(!badge.prerelease);
}

#[test]
fn an_up_to_date_build_shows_no_badge() {
    let (h, _) = engine("2.1.0", LATEST_210, |_| {});
    let out = h.ok("update_check", json!({}));
    assert_eq!(out.data["update_available"], false);
    assert_eq!(out.data["latest"], serde_json::Value::Null);
    assert_eq!(out.message, "CodeShellManager 2.1.0 is up to date.");
    assert!(h.engine.view().update.is_none());
}

#[test]
fn a_beta_reads_the_release_list_and_sees_a_newer_beta() {
    const LIST: &str = r#"[{"tag_name": "v2.1.0-rc.1", "prerelease": true}, {"tag_name": "v2.1.0-beta.4", "prerelease": true}, {"tag_name": "v2.0.0"}]"#;
    let (h, asked) = engine("2.1.0-beta.3", LIST, |_| {});
    let out = h.ok("update_check", json!({}));
    assert_eq!(*asked.lock().unwrap(), vec![LIST_URL.to_owned()]);
    assert_eq!(out.data["latest"], "2.1.0-beta.4", "never the rc");
    assert_eq!(out.data["prerelease"], true);
    assert_eq!(out.data["feed"], "list");
}

#[test]
fn dismissing_hides_the_badge_for_that_version() {
    let (h, _) = engine("2.0.0", LATEST_210, |_| {});
    h.ok("update_check", json!({}));
    assert!(h.engine.view().update.is_some());
    let out = h.ok("update_dismiss", json!({}));
    assert_eq!(out.data["version"], "2.1.0");
    assert!(h.engine.view().update.is_none());
    // Checking again finds the same version: still hidden.
    h.ok("update_check", json!({}));
    assert!(h.engine.view().update.is_none());
    assert_eq!(h.ok("update_dismiss", json!({})).message, "No update notice is shown.");
}

#[test]
fn garbage_from_github_is_an_error_and_no_badge() {
    let (h, _) = engine("2.0.0", "<html>rate limited</html>", |_| {});
    let e = h.ui("update_check", json!({})).unwrap_err();
    assert!(matches!(&e, ToolError::Failed(m) if m.starts_with("could not check for updates")), "{e:?}");
    assert!(h.engine.view().update.is_none());
    let (h, _) = engine("2.0.0", r#"{"tag_name": "v2.1.0\" & calc"}"#, |_| {});
    assert_eq!(h.ok("update_check", json!({})).data["update_available"], false);
}

#[test]
fn a_failed_request_is_an_error() {
    let h = H::start(test_app(), |o| {
        o.update = UpdateOptions { fetch: Some(Arc::new(|_: &str| Err("could not reach GitHub: timed out".into()))), current: "2.0.0".into(), ..UpdateOptions::default() };
    });
    let e = h.ui("update_check", json!({})).unwrap_err();
    assert_eq!(e, ToolError::Failed("could not check for updates: could not reach GitHub: timed out".into()));
}

#[test]
fn an_engine_without_a_check_says_so() {
    let h = H::new();
    let e = h.ui("update_check", json!({})).unwrap_err();
    assert_eq!(e, ToolError::Failed("this copy of the app does not check for updates".into()));
}

#[test]
fn the_check_is_network_but_not_a_consent_question_and_the_setting_is_open_to_mcp() {
    let h = H::new();
    let spec = h.engine.registry().get("update_check").copied().unwrap();
    assert!(spec.effects.contains(csm_engine::Effects::ACCESSES_NETWORK));
    assert!(!spec.effects.needs_consent());
    assert_eq!(h.ok("settings_get", json!({"key": "CheckForUpdates"})).data["value"], true);
    let mcp = Caller::External { client: "agent".into(), session_id: None };
    h.engine.call(mcp, "settings_set", json!({"key": "check_for_updates", "value": false})).unwrap();
    assert_eq!(h.ok("settings_get", json!({"key": "CheckForUpdates"})).data["value"], false);
}

/// A run that saves state, checking at once (no first delay).
fn scheduled(current: &str, tweak: impl FnOnce(&mut csm_engine::StateOptions)) -> (H, Arc<Mutex<Vec<String>>>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let (h, asked) = engine(current, LATEST_210, move |o| {
        o.save_path = Some(path);
        o.update.auto = true;
        o.update.first_delay = Duration::ZERO;
        tweak(o);
    });
    (h, asked, dir)
}

#[test]
fn a_saving_run_checks_on_its_own_once() {
    let (h, asked, _dir) = scheduled("2.0.0", |_| {});
    let v = h.wait_until("the scheduled check", |s| s.update.is_some());
    assert_eq!(v.update.as_ref().unwrap().version, "2.1.0");
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(asked.lock().unwrap().len(), 1, "then not again for a day");
}

#[test]
fn clean_isolated_local_builds_and_the_setting_off_never_check_on_their_own() {
    let quiet = |what: &str, h: &H, asked: &Arc<Mutex<Vec<String>>>| {
        std::thread::sleep(Duration::from_millis(500));
        assert!(asked.lock().unwrap().is_empty(), "{what} asked GitHub");
        assert!(h.engine.view().update.is_none(), "{what}");
    };
    let (h, asked, _d) = scheduled("2.0.0", |o| o.clean = true);
    quiet("--clean", &h, &asked);
    // An explicit check still runs under --clean.
    assert_eq!(h.ok("update_check", json!({})).data["update_available"], true);

    let (h, asked, _d) = scheduled("2.0.0", |o| o.save_path = None);
    quiet("an isolated run", &h, &asked);
    let (h, asked, _d) = scheduled("0.0.1", |_| {});
    quiet("a local build", &h, &asked);

    let dir = tempfile::tempdir().unwrap();
    let (fetch, asked) = fake(LATEST_210);
    let mut app = test_app();
    app.settings.check_for_updates = false;
    let h = H::start(app, |o| {
        o.save_path = Some(dir.path().join("state.json"));
        o.update = UpdateOptions { fetch: Some(fetch), auto: true, current: "2.0.0".into(), first_delay: Duration::ZERO, ..UpdateOptions::default() };
    });
    quiet("CheckForUpdates off", &h, &asked);
    // Turned on, it checks.
    h.ok("settings_set", json!({"key": "CheckForUpdates", "value": true}));
    h.wait_until("the check after turning it on", |s| s.update.is_some());
}
