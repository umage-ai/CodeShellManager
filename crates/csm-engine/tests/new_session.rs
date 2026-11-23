#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! New session forms (`[[new_session]]`) through the engine, with the echo plugin as the add-on:
//! a form is offered while its process runs, its picker loads (ready, empty, failed, a bad answer),
//! a submit runs the add-on's tool with the chosen id and the fixed params, a failed submit is
//! shown, nothing runs before a load, and a stopped add-on offers nothing. A pending submit is polled
//! by its request_id until it is final, its progress shows, a launch's `open_remote` opens the remote
//! (and a missing one leaves the form failed), and a stopped add-on ends the poll without a stale write.
//! The plugin is `csm-echo-plugin`; its mode (the first argument) picks the form's answers.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::{H, test_app};
use serde_json::json;

const EXE: &str = env!("CARGO_BIN_EXE_csm-echo-plugin");

/// The test add-on: a process add-on with one New session form whose tools are its own.
fn manifest(mode: &str) -> String {
    let exe = serde_json::to_string(EXE).unwrap();
    format!(
        r#"
[addon]
id = "echo-test"
name = "Echo test"
version = "0.1.0"
description = "A process add-on for the tests"
api = 1
kind = "process"

[process]
argv = [{exe}, "{mode}"]
effects = ["launches_process"]

[[new_session]]
id = "agent"
title = "New echo agent"
submit = "Create agent"
choices_tool = "echo-test.form_choices"
choice_title = "Project"
choice_param = "project"
tool = "echo-test.form_launch"
[new_session.params]
open = true
"#
    )
}

fn fixture(dir: &Path, mode: &str) -> PathBuf {
    let state = dir.join("state.json");
    let folder = dir.join("addons").join("echo-test");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("addon.toml"), manifest(mode)).unwrap();
    state
}

fn start(state: &Path) -> H {
    let state = state.to_path_buf();
    H::start(test_app(), move |o| {
        o.save_path = Some(state);
        o.restore_on_start = false;
    })
}

/// Enables the add-on the way the consent dialog does, then waits until its form is offered
/// (the form is offered only while the add-on's process runs).
fn enable_and_offer(h: &H) {
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    let list = h.ok("addon_list", json!({})).data["addons"].clone();
    let a = list.as_array().unwrap().iter().find(|a| a["id"] == "echo-test").cloned().unwrap_or_else(|| panic!("no echo-test in {list}"));
    h.ok("addon_enable", json!({"id": "echo-test", "version": a["version"], "digest": a["digest"]}));
    h.wait_until("the form to be offered", |s| !s.new_sessions.is_empty());
}

#[test]
fn a_form_loads_its_choices_and_submit_runs_its_tool_with_the_choice() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    enable_and_offer(&h);

    let listed = h.ok("new_session_sources", json!({})).data["sources"].clone();
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!((listed[0]["addon"].as_str(), listed[0]["source"].as_str()), (Some("echo-test"), Some("agent")));
    assert_eq!((listed[0]["title"].as_str(), listed[0]["submit_label"].as_str(), listed[0]["load"].as_str()), (Some("New echo agent"), Some("Create agent"), Some("idle")));

    // Nothing runs before the picker is loaded.
    let early = h.ui("new_session_submit", json!({"addon": "echo-test", "source": "agent", "choice": "beta"}));
    assert!(early.is_err(), "a submit before a load must be refused");

    h.ok("new_session_load", json!({"addon": "echo-test", "source": "agent"}));
    let s = h.wait_until("the choices", |s| s.new_sessions.first().is_some_and(|f| f.load == "ready"));
    let ids: Vec<&str> = s.new_sessions[0].choices.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["alpha", "beta"], "the entry without an id is dropped");
    assert_eq!(s.new_sessions[0].choices[0].detail, "repo a");

    // A choice that was not loaded is refused, and nothing is submitted.
    assert!(h.ui("new_session_submit", json!({"addon": "echo-test", "source": "agent", "choice": "gamma"})).is_err());
    assert_eq!(h.wait_until("still idle", |s| s.new_sessions[0].submit == "idle").new_sessions[0].submit, "idle");

    // The launch names the remote to open; the add-on offers it, so CodeShellManager opens it.
    h.ok("echo-test.offer", json!({"remotes": [agent_x()]}));
    h.wait_until("the remote", |s| s.remotes.len() == 1);
    let out = h.ok("new_session_submit", json!({"addon": "echo-test", "source": "agent", "choice": "beta"}));
    assert_eq!(out.data["args"], json!({"open": true, "project": "beta"}), "the fixed params and the choice reach the tool");
    assert_eq!(out.data["remote_id"], "agent-x");
    let s = h.wait_until("the submit to finish", |s| s.new_sessions[0].submit == "done");
    assert_eq!(s.new_sessions[0].submit_text.as_deref(), Some("Launched the agent."));
}

#[test]
fn an_empty_list_a_failed_load_and_a_bad_answer_are_states_the_dialog_can_show() {
    let cases = [("formempty", "ready", None), ("formerror", "failed", Some("the host is asleep")), ("formjunk", "failed", Some("has no `choices` list"))];
    for (mode, want, error) in cases {
        let dir = tempfile::tempdir().unwrap();
        let h = start(&fixture(dir.path(), mode));
        enable_and_offer(&h);
        // A failed load is an error answer to the caller too; the state is what the dialog reads.
        let _ = h.ui("new_session_load", json!({"addon": "echo-test", "source": "agent"}));
        let s = h.wait_until(mode, |s| s.new_sessions.first().is_some_and(|f| f.load != "idle" && f.load != "loading"));
        let f = &s.new_sessions[0];
        assert_eq!(f.load, want, "{mode}");
        match error {
            None => assert!(f.choices.is_empty(), "{mode}"),
            Some(text) => {
                let shown = f.load_error.clone().unwrap_or_default();
                assert!(shown.contains(text), "{mode}: {shown}");
                assert!(f.choices.is_empty(), "{mode}");
            }
        }
    }
}

#[test]
fn a_failed_submit_is_shown_and_the_form_can_be_tried_again() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "formfail"));
    enable_and_offer(&h);
    h.ok("new_session_load", json!({"addon": "echo-test", "source": "agent"}));
    h.wait_until("the choices", |s| s.new_sessions.first().is_some_and(|f| f.load == "ready"));
    let _ = h.ui("new_session_submit", json!({"addon": "echo-test", "source": "agent", "choice": "alpha"}));
    let s = h.wait_until("the failed submit", |s| s.new_sessions[0].submit == "failed");
    assert!(s.new_sessions[0].submit_text.as_deref().unwrap_or_default().contains("the host refused the launch"));
    // Loading again starts the form afresh, and the choices are still there.
    h.ok("new_session_load", json!({"addon": "echo-test", "source": "agent"}));
    let s = h.wait_until("reloaded", |s| s.new_sessions[0].submit == "idle" && s.new_sessions[0].load == "ready");
    assert_eq!(s.new_sessions[0].choices.len(), 2);
}

#[test]
fn an_unknown_form_is_not_found_and_a_stopped_add_on_offers_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    enable_and_offer(&h);
    assert!(h.ui("new_session_load", json!({"addon": "echo-test", "source": "nope"})).is_err());
    assert!(h.ui("new_session_load", json!({"addon": "other", "source": "agent"})).is_err());
    assert!(h.ui("new_session_load", json!({"addon": "echo-test", "source": "agent", "extra": 1})).is_err(), "unknown fields are refused");
    h.ok("addon_disable", json!({"id": "echo-test"}));
    h.wait_until("the form to go", |s| s.new_sessions.is_empty());
    let listed = h.ok("new_session_sources", json!({})).data["sources"].clone();
    assert_eq!(listed, json!([]));
}

/// A remote the echo add-on offers under the id its launch names (`agent-x`), with a session.
fn agent_x() -> serde_json::Value {
    json!({"id": "agent-x", "label": "Agent X", "detail": "idle", "session": {
        "name": "Agent X",
        "ssh": {"host": "agent-alias", "config_file": "cfg/ssh_config", "remote_argv": ["tmux", "new-session", "-A", "-s", "main"]},
        "resume": "rerun"
    }})
}

/// Enables the add-on, offers its remote and loads the form's choices.
fn ready_form(h: &H) {
    enable_and_offer(h);
    h.ok("echo-test.offer", json!({"remotes": [agent_x()]}));
    h.wait_until("the remote", |s| s.remotes.len() == 1);
    h.ok("new_session_load", json!({"addon": "echo-test", "source": "agent"}));
    h.wait_until("the choices", |s| s.new_sessions.first().is_some_and(|f| f.load == "ready"));
}

fn submit_beta() -> serde_json::Value {
    json!({"addon": "echo-test", "source": "agent", "choice": "beta"})
}

#[test]
fn a_pending_launch_is_polled_by_its_request_id_and_its_session_opens_before_the_submit_is_done() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "formpend"));
    ready_form(&h);
    // The first ask is answered pending with a request_id; the poll (the same tool, with that id)
    // is answered final, and the add-on's own csm/call remote_open has opened the session by then.
    let out = h.ok("new_session_submit", submit_beta());
    assert_eq!(out.data["request_id"], "req-echo-0001", "the poll carried the request_id");
    let s = h.wait_until("the submit to finish", |s| s.new_sessions[0].submit == "done");
    assert_eq!(s.new_sessions[0].submit_text.as_deref(), Some("Launched the agent."));
    let opened = h.engine.view().sessions.iter().any(|v| v.owner_addon.as_deref() == Some("echo-test") && v.name == "Agent X");
    assert!(opened, "the add-on's remote_open made the session during the form's tool");
}

#[test]
fn a_pending_launch_shows_its_progress_while_it_runs() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "formpend"));
    ready_form(&h);
    std::thread::scope(|scope| {
        let submit = scope.spawn(|| h.ui("new_session_submit", submit_beta()));
        // The pending answer's message is the form's progress line until the poll is answered.
        h.wait_until("the progress line", |s| s.new_sessions[0].submit == "running" && s.new_sessions[0].submit_text.is_some());
        let s = h.engine.view();
        assert_eq!(s.new_sessions[0].submit_text.as_deref(), Some("The launch is still running on the host."));
        assert!(submit.join().unwrap().is_ok());
    });
    assert_eq!(h.engine.view().new_sessions[0].submit, "done");
}

#[test]
fn a_pending_launch_that_fails_later_shows_the_failure_on_the_form() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "formpendfail"));
    ready_form(&h);
    let err = h.ui("new_session_submit", submit_beta()).expect_err("the poll fails");
    assert!(err.to_string().contains("refused the launch after waiting"), "{err}");
    let s = h.engine.view();
    assert_eq!(s.new_sessions[0].submit, "failed");
    assert!(s.new_sessions[0].submit_text.as_deref().unwrap_or_default().contains("after waiting"));
    assert!(!h.engine.view().sessions.iter().any(|v| v.name == "Agent X"), "no session for a failed launch");
}

#[test]
fn a_pending_answer_without_a_request_id_is_a_failure_not_a_success() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "formpendnoid"));
    ready_form(&h);
    let err = h.ui("new_session_submit", submit_beta()).expect_err("pending with no id");
    assert!(err.to_string().contains("no request_id"), "{err}");
    assert_eq!(h.engine.view().new_sessions[0].submit, "failed");
}

#[test]
fn a_stopped_add_on_ends_a_pending_launch_and_a_restart_starts_the_form_afresh() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "formpendforever"));
    ready_form(&h);
    std::thread::scope(|scope| {
        let submit = scope.spawn(|| h.ui("new_session_submit", submit_beta()));
        h.wait_until("the launch to be running", |s| s.new_sessions[0].submit == "running");
        h.ok("addon_disable", json!({"id": "echo-test"}));
        h.wait_until("the form to go", |s| s.new_sessions.is_empty());
        // The poll stops on its own (the add-on is gone); its answer is an error.
        assert!(submit.join().unwrap().is_err(), "the stopped launch answers with an error");
    });
    // Enabled again, the form is fresh: no progress or outcome from the old process.
    enable_and_offer(&h);
    let s = h.wait_until("the form again", |s| !s.new_sessions.is_empty());
    assert_eq!((s.new_sessions[0].load, s.new_sessions[0].submit), ("idle", "idle"), "the old launch left nothing behind");
    assert_eq!(s.new_sessions[0].submit_text, None);
}

#[test]
fn a_launch_whose_session_does_not_open_stays_a_failure_on_the_form() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    // No remote is offered, so the `open_remote` the launch names cannot open.
    ready_form(&h);
    h.ok("echo-test.offer", json!({"remotes": []}));
    h.wait_until("no remote", |s| s.remotes.is_empty());
    let out = h.ok("new_session_submit", submit_beta());
    assert_eq!(out.data["remote_id"], "agent-x", "the launch itself happened");
    let s = h.engine.view();
    assert_eq!(s.new_sessions[0].submit, "failed", "the dialog keeps the form open for this");
    let text = s.new_sessions[0].submit_text.clone().unwrap_or_default();
    assert!(text.contains("Launched the agent.") && text.contains("did not open"), "{text}");
}

#[test]
fn addon_list_says_which_forms_an_add_on_declares_for_the_consent_card() {
    let dir = tempfile::tempdir().unwrap();
    let h = start(&fixture(dir.path(), "ok"));
    h.wait_until("the add-on folder", |s| s.addons.len() == 1);
    let list = h.ok("addon_list", json!({})).data["addons"].clone();
    let form = &list[0]["new_sessions"][0];
    assert_eq!(form["id"], "agent");
    assert_eq!(form["title"], "New echo agent");
    assert_eq!(form["submit"], "Create agent");
    assert_eq!(form["choices_tool"], "echo-test.form_choices");
    assert_eq!(form["tool"], "echo-test.form_launch");
}
