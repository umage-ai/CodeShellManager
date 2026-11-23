#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! New session forms (`[[new_session]]`) through the engine, with the echo plugin as the add-on:
//! a form is offered while its process runs, its picker loads (ready, empty, failed, a bad answer),
//! a submit runs the add-on's tool with the chosen id and the fixed params, a failed submit is
//! shown, nothing runs before a load, and a stopped add-on offers nothing.
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
