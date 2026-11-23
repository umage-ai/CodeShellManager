//! The New session dialog's add-on forms (`[[new_session]]`): the chip an enabled add-on's form adds,
//! its picker states (loading, the reason it failed, empty, the choices), the submit that sends the
//! picked choice to the add-on's tool, and the dialog closing once that submit is done. The engine
//! is the demo's; the forms come through the model, as the engine would publish them.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::*;
use csm_engine::{Choice, NewSessionView};
use csm_ui::Action;
use csm_ui::theme::ThemeKind;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use serde_json::json;

const CHIP: &str = "New session form: New NEXUS agent";

fn form(load: &'static str, choices: &[(&str, &str)], submit: &'static str, load_error: Option<&str>, submit_text: Option<&str>) -> NewSessionView {
    NewSessionView {
        addon: "nexus".into(),
        addon_name: "NEXUS".into(),
        source: "agent".into(),
        title: "New NEXUS agent".into(),
        submit_label: "Create agent".into(),
        choice_title: "Project".into(),
        load,
        choices: choices.iter().map(|(id, label)| Choice { id: (*id).into(), label: (*label).into(), detail: "repo".into() }).collect(),
        load_error: load_error.map(str::to_owned),
        submit,
        submit_text: submit_text.map(str::to_owned),
    }
}

/// A harness whose model offers `f` (as an enabled, running add-on would), with the quick-open
/// dialog not yet open.
fn with_form(f: NewSessionView) -> Harness<'static, csm_ui::CsmApp> {
    let mut b = backend(ThemeKind::Mocha);
    b.model_mut().new_sessions = vec![f];
    b.publish();
    let mut h = harness(app_with(b));
    settle(&mut h);
    h
}

fn open_dialog(h: &mut Harness<'static, csm_ui::CsmApp>) {
    let ctx = h.ctx.clone();
    h.state_mut().run(&ctx, Action::OpenQuickOpen { group: None });
    settle(h);
}


/// The newest call of tool `id` the dialog made (other calls, such as `worktree_list`, can follow it).
fn call_of(h: &Harness<'static, csm_ui::CsmApp>, id: &str) -> Option<serde_json::Value> {
    h.state().submitted().iter().rev().find(|c| c.id == id).map(|c| c.params.clone())
}

fn has(h: &Harness<'static, csm_ui::CsmApp>, label: &str) -> bool {
    h.query_by_label(label).is_some()
}

#[test]
fn the_chip_appears_and_opening_it_asks_the_add_on_for_its_choices() {
    let mut h = with_form(form("idle", &[], "idle", None, None));
    open_dialog(&mut h);
    assert!(has(&h, CHIP), "the form's chip is in the dialog");
    h.get_by_label(CHIP).click();
    settle(&mut h);
    assert_eq!(call_of(&h, "new_session_load"), Some(json!({"addon": "nexus", "source": "agent"})));
    assert!(has(&h, "Create agent") || h.query_by_label_contains("Loading").is_some(), "the form is open");
}

#[test]
fn an_empty_picker_says_so_and_the_failed_load_shows_why_and_can_be_retried() {
    let mut h = with_form(form("ready", &[], "idle", None, None));
    open_dialog(&mut h);
    h.state_mut().ui.quick.new_form = Some(("nexus".into(), "agent".into()));
    settle(&mut h);
    assert!(has(&h, "Nothing to choose from yet."), "the empty state is said");

    let mut h = with_form(form("failed", &[], "idle", Some("the NEXUS host did not answer"), None));
    open_dialog(&mut h);
    h.state_mut().ui.quick.new_form = Some(("nexus".into(), "agent".into()));
    settle(&mut h);
    assert!(h.query_by_label_contains("the NEXUS host did not answer").is_some(), "the reason is shown");
    h.get_by_label("Try again").click();
    settle(&mut h);
    assert!(call_of(&h, "new_session_load").is_some());
}

#[test]
fn picking_a_choice_submits_it_to_the_add_on_and_the_dialog_closes_when_done() {
    let mut h = with_form(form("ready", &[("alpha", "Alpha"), ("beta", "Beta")], "idle", None, None));
    open_dialog(&mut h);
    h.state_mut().ui.quick.new_form = Some(("nexus".into(), "agent".into()));
    settle(&mut h);
    // Nothing is sent before a choice is picked: the submit is disabled.
    h.get_by_label("Create agent").click();
    settle(&mut h);
    assert!(call_of(&h, "new_session_submit").is_none(), "no choice, no submit");

    h.get_by_label("Project: Beta").click();
    settle(&mut h);
    assert_eq!(h.state().ui.quick.new_choice.as_deref(), Some("beta"), "the choice was picked");
    h.get_by_label("Create agent").click();
    settle(&mut h);
    assert_eq!(call_of(&h, "new_session_submit"), Some(json!({"addon": "nexus", "source": "agent", "choice": "beta"})));
    assert!(h.state().ui.quick.new_pending, "the dialog waits for the submit");
}

#[test]
fn a_finished_submit_closes_the_dialog() {
    // The engine reports the submit done (the add-on opened its session); the dialog that asked closes.
    let mut h = with_form(form("ready", &[("alpha", "Alpha")], "done", None, Some("Launched the agent.")));
    open_dialog(&mut h);
    h.state_mut().ui.quick.new_form = Some(("nexus".into(), "agent".into()));
    h.state_mut().ui.quick.new_pending = true;
    settle(&mut h);
    assert!(!has(&h, CHIP), "the dialog closed");
    assert!(!h.state().ui.quick.new_pending);
}

#[test]
fn a_failed_submit_stays_open_and_says_why() {
    let mut h = with_form(form("ready", &[("alpha", "Alpha")], "failed", None, Some("the host refused the launch")));
    open_dialog(&mut h);
    h.state_mut().ui.quick.new_form = Some(("nexus".into(), "agent".into()));
    h.state_mut().ui.quick.new_pending = true;
    settle(&mut h);
    assert!(has(&h, CHIP), "the dialog stays open");
    assert!(h.query_by_label_contains("the host refused the launch").is_some());
}
